// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Pieces `pr prepare` and `pr create` share: the scratch directory both
//! read and write, and the `gh` seam both call through.

use crate::common::paths::{repo_scoped_dir, RepoScope};
use crate::common::proc::run_with_timeout;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// How long a `gh` call may take; PR creation talks to the network.
const GH_TIMEOUT: Duration = Duration::from_secs(60);

/// Every non-alphanumeric character becomes `-`, the same rule
/// `common::paths::worktree_id` uses for its own path segment.
fn branch_slug(branch: &str) -> String {
    branch
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The per-branch scratch directory both subcommands read and write, under
/// this worktree's repo-scoped storage.
pub fn pr_state_dir(branch: &str) -> Result<PathBuf, String> {
    let base = repo_scoped_dir(RepoScope::Worktree).ok_or_else(|| {
        "could not resolve a worktree-scoped storage location; this repo needs a git \
         checkout with an `origin` remote"
            .to_string()
    })?;
    Ok(base.join("pr").join(branch_slug(branch)))
}

/// How long a `git` call that talks to the network may take.
const NET_TIMEOUT: Duration = Duration::from_secs(120);

/// Runs a local `git` command, returning stdout without trailing whitespace.
pub(crate) fn git(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

/// Runs a `git` command that talks to the network, bounded by `NET_TIMEOUT`.
pub(crate) fn git_net(args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command.args(args);
    let output = run_with_timeout(&mut command, NET_TIMEOUT)
        .ok_or_else(|| format!("git {} did not finish in time", args.join(" ")))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

/// Runs a local `git` command with stdout going straight to `path`, so a
/// diff of any size never passes through memory or a pipe.
pub(crate) fn git_to_file(args: &[&str], path: &Path) -> Result<(), String> {
    let file =
        fs::File::create(path).map_err(|e| format!("could not create {}: {e}", path.display()))?;
    let status = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .status()
        .map_err(|e| format!("could not run git {}: {e}", args.join(" ")))?;
    if !status.success() {
        return Err(format!("git {} failed", args.join(" ")));
    }
    Ok(())
}

/// Refuses a branch or base name that starts with `-`: git and gh would read
/// it as an option, not a name.
pub(crate) fn reject_option_like(kind: &str, value: &str) -> Result<(), String> {
    if value.starts_with('-') {
        return Err(format!(
            "{kind} {value:?} starts with '-', so git and gh would read it as an option"
        ));
    }
    Ok(())
}

/// Changes the process directory to `dir`, which must exist and sit inside a
/// git work tree. The CLI is single threaded and short lived, so this is safe.
pub fn enter_dir(dir: &str) -> Result<(), String> {
    let path = Path::new(dir)
        .canonicalize()
        .map_err(|e| format!("--dir {dir}: {e}"))?;
    if !path.is_dir() {
        return Err(format!("--dir {dir} is not a directory"));
    }
    std::env::set_current_dir(&path).map_err(|e| format!("--dir {dir}: {e}"))?;
    match git(&["rev-parse", "--is-inside-work-tree"]) {
        Ok(out) if out == "true" => Ok(()),
        _ => Err(format!("--dir {dir} is not inside a git work tree")),
    }
}

/// The checked-out branch, or an error on a detached HEAD.
pub(crate) fn current_branch() -> Result<String, String> {
    let branch = git(&["branch", "--show-current"])?;
    if branch.is_empty() {
        return Err("detached HEAD; checkout a branch first".to_string());
    }
    reject_option_like("branch", &branch)?;
    Ok(branch)
}

/// Resolves the PR's base branch and says where it came from: the flag,
/// then the repo's default branch, then `origin/HEAD`, then `main`.
pub fn resolve_base(
    gh: &dyn GhClient,
    base_arg: Option<&str>,
) -> Result<(String, &'static str), String> {
    let (base, source) = pick_base(gh, base_arg)?;
    reject_option_like("base branch", &base)?;
    Ok((base, source))
}

fn pick_base(gh: &dyn GhClient, base_arg: Option<&str>) -> Result<(String, &'static str), String> {
    if let Some(base) = base_arg.filter(|b| !b.is_empty()) {
        return Ok((base.to_string(), "--base flag"));
    }
    if let Some(base) = gh.repo_default_branch()?.filter(|b| !b.is_empty()) {
        return Ok((base, "repo default"));
    }
    if let Ok(reference) = git(&["symbolic-ref", "refs/remotes/origin/HEAD"]) {
        if let Some(base) = reference.strip_prefix("refs/remotes/origin/") {
            if !base.is_empty() {
                return Ok((base.to_string(), "git symbolic-ref"));
            }
        }
    }
    Ok(("main".to_string(), "fallback"))
}

/// An open or closed PR already attached to a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingPr {
    pub url: String,
    pub state: String,
}

/// The `gh` operations the PR subcommands need, one method per operation so
/// a test fake declares exactly which ones it touches.
pub trait GhClient {
    fn pr_view(&self, branch: &str) -> Result<Option<ExistingPr>, String>;
    fn repo_default_branch(&self) -> Result<Option<String>, String>;
    /// Head branch names of the open PRs, used to find a stacked base. A
    /// failed call is an empty list: no stack is assumed.
    fn open_pr_heads(&self) -> Vec<String> {
        Vec::new()
    }
}

/// The nearest open PR branch that `branch` is built on: an ancestor of HEAD
/// by `is_ancestor(remote ref)`, the fewest commits behind HEAD by
/// `count(range)`. One rule for `pr prepare` and `deliver`.
pub fn nearest_parent(
    heads: &[String],
    branch: &str,
    is_ancestor: &dyn Fn(&str) -> bool,
    count: &dyn Fn(&str) -> Option<u64>,
) -> Option<String> {
    let mut best: Option<(u64, &String)> = None;
    for head in heads.iter().filter(|h| h.as_str() != branch) {
        let remote = format!("refs/remotes/origin/{head}");
        if !is_ancestor(&remote) {
            continue;
        }
        let n = count(&format!("{remote}..HEAD")).unwrap_or(u64::MAX);
        if best.as_ref().is_none_or(|(m, _)| n < *m) {
            best = Some((n, head));
        }
    }
    best.map(|(_, head)| head.clone())
}

/// Reads `gh pr view --json url,state` output. A PR missing either field is
/// treated as no PR; output that is not JSON is an error.
fn parse_pr_view(out: &str) -> Result<Option<ExistingPr>, String> {
    let value: serde_json::Value =
        serde_json::from_str(out).map_err(|e| format!("gh pr view returned bad JSON: {e}"))?;
    let field = |name: &str| value.get(name).and_then(|v| v.as_str()).map(str::to_string);
    match (field("url"), field("state")) {
        (Some(url), Some(state)) => Ok(Some(ExistingPr { url, state })),
        _ => Ok(None),
    }
}

/// Shells out to the real `gh` binary.
pub struct RealGhClient;

/// Runs `gh` with `args`; `Ok` is trimmed stdout, `Err` the trimmed stderr.
pub(crate) fn gh(args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("gh");
    command.args(args);
    let output = run_with_timeout(&mut command, GH_TIMEOUT).ok_or_else(|| {
        format!(
            "gh {} did not finish (missing, or timed out)",
            args.join(" ")
        )
    })?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

impl GhClient for RealGhClient {
    fn pr_view(&self, branch: &str) -> Result<Option<ExistingPr>, String> {
        // A branch with no PR makes `gh pr view` exit non-zero, which is the
        // normal "nothing there yet" answer, not a failure.
        let Ok(out) = gh(&["pr", "view", "--json", "url,state", "--", branch]) else {
            return Ok(None);
        };
        parse_pr_view(&out)
    }

    fn repo_default_branch(&self) -> Result<Option<String>, String> {
        let out = gh(&[
            "repo",
            "view",
            "--json",
            "defaultBranchRef",
            "-q",
            ".defaultBranchRef.name",
        ]);
        Ok(out.ok().filter(|name| !name.is_empty()))
    }

    fn open_pr_heads(&self) -> Vec<String> {
        let Ok(out) = gh(&[
            "pr",
            "list",
            "--state",
            "open",
            "--json",
            "headRefName",
            "--limit",
            "100",
        ]) else {
            return Vec::new();
        };
        let value: serde_json::Value = serde_json::from_str(&out).unwrap_or_default();
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.get("headRefName")?.as_str().map(String::from))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Output shapes of `gh pr view --json url,state`, as gh prints them.
    const OPEN_PR: &str = r#"{"state":"OPEN","url":"https://github.com/o/r/pull/9"}"#;
    const MERGED_PR: &str = r#"{"state":"MERGED","url":"https://github.com/o/r/pull/3"}"#;

    #[test]
    fn an_open_pr_parses_to_its_url_and_state() {
        assert_eq!(
            parse_pr_view(OPEN_PR),
            Ok(Some(ExistingPr {
                url: "https://github.com/o/r/pull/9".to_string(),
                state: "OPEN".to_string(),
            }))
        );
    }

    #[test]
    fn a_merged_pr_keeps_its_state_so_the_caller_can_ignore_it() {
        let got = parse_pr_view(MERGED_PR).unwrap().unwrap();
        assert_eq!(got.state, "MERGED");
    }

    #[test]
    fn a_pr_missing_a_field_counts_as_no_pr() {
        assert_eq!(parse_pr_view(r#"{"state":"OPEN"}"#), Ok(None));
        assert_eq!(parse_pr_view(r#"{"url":"https://x/pull/1"}"#), Ok(None));
        assert_eq!(parse_pr_view("{}"), Ok(None));
    }

    #[test]
    fn output_that_is_not_json_is_an_error() {
        let err = parse_pr_view("no pull requests found").expect_err("not JSON");
        assert!(err.starts_with("gh pr view returned bad JSON"), "got {err}");
    }

    #[test]
    fn a_name_starting_with_a_dash_is_refused() {
        assert!(reject_option_like("base branch", "--upload-pack=x").is_err());
        assert!(reject_option_like("branch", "-x").is_err());
        assert_eq!(reject_option_like("branch", "feat/-x"), Ok(()));
        assert_eq!(reject_option_like("branch", "main"), Ok(()));
    }

    struct DefaultBranch(&'static str);

    impl GhClient for DefaultBranch {
        fn pr_view(&self, _branch: &str) -> Result<Option<ExistingPr>, String> {
            Ok(None)
        }
        fn repo_default_branch(&self) -> Result<Option<String>, String> {
            Ok(Some(self.0.to_string()))
        }
    }

    #[test]
    fn a_dash_prefixed_base_is_refused_from_the_flag_and_from_the_repo_default() {
        let flag = resolve_base(&DefaultBranch("main"), Some("--upload-pack=x"));
        let repo = resolve_base(&DefaultBranch("-evil"), None);

        assert!(flag.expect_err("flag").contains("starts with '-'"));
        assert!(repo.expect_err("repo default").contains("starts with '-'"));
    }
}
