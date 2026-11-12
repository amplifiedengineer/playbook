// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook commit run`: commit the staged changes, rebase onto the base
//! when behind, and push.
//!
//! A rejected plain push is never retried as a force, on any branch. The lease
//! of a `--force-with-lease` run right after a failed push compares against a
//! ref that push already refreshed, so it matches and the force succeeds
//! anyway. A force-with-lease is used only when this run itself amended or
//! rebased, where the lease is checked against a ref this run fetched. In auto
//! mode nothing is ever forced.

use super::{current_branch, git_in, git_in_long, git_success, git_text};
use crate::common::re::static_regex;
use std::path::Path;

/// Flags of one run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub amend: bool,
    pub auto: bool,
    pub no_signoff: bool,
    /// Skip the conventional commit type guard (`typecheck`).
    pub no_type_check: bool,
    /// Skip the prose check (`common::prose`).
    pub no_prose_check: bool,
}

/// Whether a hook script's source WRITES a `Signed-off-by` trailer: an
/// interpret-trailers call, `--signoff` passed to git, or an append of a
/// trailer line. A hook that only checks for the trailer does not count,
/// because dropping `--signoff` then would fail every commit.
pub fn hook_writes_signoff(source: &str) -> bool {
    // Join backslash continuations, then drop comment lines.
    let joined = source.replace("\\\n", "");
    let src: String = joined
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let trailers = static_regex(r#"(?i)interpret-trailers.*--trailer[ =]*["']?signed-off-by"#);
    let flag = static_regex(r#"(^|[;&|{(])\s*(exec\s+)?git\s[^"']*--signoff"#);
    let append = static_regex(r"(?i)signed-off-by:.*>>|>>.*signed-off-by");
    src.lines()
        .any(|l| trailers.is_match(l) || flag.is_match(l) || append.is_match(l))
}

/// Whether `--signoff` should be passed: `commit.signOff` is on, the caller
/// did not opt out, the message has no trailer yet and no hook writes one.
/// `deliver` shares this decision.
pub fn signoff_wanted(
    dir: &Path,
    message: &str,
    no_signoff: bool,
    config_on: bool,
    git: impl Fn(&[&str]) -> Option<String>,
) -> bool {
    if !config_on || no_signoff {
        return false;
    }
    let has_trailer = message
        .lines()
        .any(|l| l.to_lowercase().starts_with("signed-off-by:"));
    if has_trailer {
        return false;
    }
    for name in ["prepare-commit-msg", "commit-msg"] {
        let path = git(&["rev-parse", "--git-path", &format!("hooks/{name}")]).map(|p| {
            if Path::new(&p).is_absolute() {
                p.into()
            } else {
                dir.join(p)
            }
        });
        let Some(path) = path else { continue };
        if is_executable(&path) {
            if let Ok(src) = std::fs::read_to_string(&path) {
                if hook_writes_signoff(&src) {
                    return false;
                }
            }
        }
    }
    true
}

/// The `git commit` flags that sign off and sign: `--signoff` when
/// [`signoff_wanted`], `--gpg-sign` when a signing key is configured.
pub(crate) fn signing_flags(
    dir: &Path,
    message: &str,
    no_signoff: bool,
    config_signoff: bool,
) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if signoff_wanted(dir, message, no_signoff, config_signoff, |a| {
        git_text(dir, a)
    }) {
        flags.push("--signoff");
    }
    if git_success(dir, &["config", "--get", "user.signingkey"]) {
        flags.push("--gpg-sign");
    }
    flags
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// The `origin/main` or `origin/master` ref, whichever exists.
fn base_ref(dir: &Path) -> Option<&'static str> {
    ["origin/main", "origin/master"]
        .into_iter()
        .find(|r| git_success(dir, &["rev-parse", "--verify", r]))
}

fn show(out: &std::process::Output) -> String {
    let mut t = String::from_utf8_lossy(&out.stdout).into_owned();
    t.push_str(&String::from_utf8_lossy(&out.stderr));
    t.trim_end().to_string()
}

/// The default branch as a remote ref: `origin/HEAD`, else `origin/main` or
/// `origin/master`.
fn default_ref(dir: &Path) -> Option<String> {
    git_text(
        dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .filter(|r| !r.is_empty())
    .or_else(|| base_ref(dir).map(str::to_string))
}

/// A remote branch the current one is built on: an ancestor of HEAD that the
/// default branch does not contain yet. That is what a stacked PR is.
fn stack_parent(dir: &Path, branch: &str, default: &str) -> Option<String> {
    let own = format!("origin/{branch}");
    let merged = git_text(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "--merged=HEAD",
            &format!("--no-merged={default}"),
            "refs/remotes/origin",
        ],
    )
    .unwrap_or_default();
    merged
        .lines()
        .find(|r| r.starts_with("origin/") && !r.ends_with("/HEAD") && *r != default && **r != *own)
        .map(str::to_string)
}

/// Checks that the commit title carries the branch's ticket in its scope.
/// The ticket comes from the branch name, then the scopes of the commits
/// this branch added (not the parent's, and not the commit being amended).
/// A stacked branch blocks, with the exact fix, before anything is
/// committed; otherwise the rule's level decides. `Err` is a block, `Ok` an
/// optional warning to print.
fn scope_findings(
    dir: &Path,
    branch: &str,
    message: &str,
    amend: bool,
) -> Result<Option<String>, String> {
    use crate::pr::body_rules::{detect_ticket, enforce, Input, Rules};
    let slug = crate::common::repo_slug();
    let rules = Rules::load(
        &crate::common::home_dir(),
        (!slug.is_empty()).then_some(slug.as_str()),
    );
    let default = default_ref(dir);
    let parent = default
        .as_deref()
        .and_then(|d| stack_parent(dir, branch, d));
    let tip = if amend { "HEAD~1" } else { "HEAD" };
    let mut log = vec![
        "log".to_string(),
        tip.to_string(),
        "--format=%s".to_string(),
    ];
    log.push("--not".to_string());
    log.extend(default.iter().chain(parent.iter()).cloned());
    let log: Vec<&str> = log.iter().map(String::as_str).collect();
    let prior: Vec<String> = git_text(dir, &log)
        .map(|out| out.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let ticket = detect_ticket(None, branch, &prior, &rules.patterns);
    let title = message.lines().next().unwrap_or("").trim().to_string();
    let input = Input {
        title: None,
        body: None,
        branch,
        subjects: std::slice::from_ref(&title),
        stacked: ticket.is_some() && parent.is_some(),
        ticket_arg: ticket.as_deref(),
        issue_arg: None,
    };
    match enforce(&input, &rules) {
        Ok(warns) => Ok(warns.first().map(|w| format!("WARN: {w}"))),
        Err(blocks) => Err(format!(
            "{}\nFix the first line of the message and run again. Nothing was committed.",
            blocks.join("\n")
        )),
    }
}

/// Commit, rebase when behind, push. `Ok` is the text to print, `Err` the
/// text for stderr (the exit is 1).
pub fn run(
    dir: &Path,
    message_file: &Path,
    opts: Options,
    config_signoff: bool,
) -> Result<String, String> {
    let branch = current_branch(dir);
    let message = std::fs::read_to_string(message_file)
        .map_err(|e| format!("could not read the message: {e}"))?;
    let mut log = Vec::new();

    if !opts.no_prose_check {
        let problems = crate::common::prose::check(&message);
        if !problems.is_empty() {
            return Err(format!(
                "The commit message breaks the writing rules:\n{}\nFix the message and run again. Nothing was committed. Pass --no-prose-check only when the text is a quotation.",
                problems.join("\n")
            ));
        }
    }

    if let Some(warning) = scope_findings(dir, &branch, &message, opts.amend)? {
        log.push(warning);
    }

    if !opts.amend && !opts.no_type_check {
        let staged = git_text(dir, &["diff", "--cached", "--name-only"]).unwrap_or_default();
        let files: Vec<&str> = staged.lines().filter(|l| !l.is_empty()).collect();
        super::typecheck::check(&message, &files)
            .map_err(|e| format!("{e}\nNothing was committed."))?;
    }

    let mut args: Vec<String> = vec!["commit".into()];
    if opts.amend {
        args.push("--amend".into());
    }
    args.extend(
        signing_flags(dir, &message, opts.no_signoff, config_signoff)
            .into_iter()
            .map(String::from),
    );
    args.push("--file".into());
    args.push(message_file.to_string_lossy().into_owned());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = git_in_long(dir, &argv).ok_or("could not run git (or it timed out)")?;
    if !out.status.success() {
        return Err(format!(
            "git commit failed, nothing was pushed:\n{}",
            show(&out)
        ));
    }
    log.push(show(&out));

    let mut rebased = false;
    if let Some(base) = base_ref(dir) {
        if branch != "main" && branch != "master" {
            let short = base.trim_start_matches("origin/");
            let _ = git_in_long(dir, &["fetch", "origin", short, "--quiet"]);
            let behind: u32 = git_text(dir, &["rev-list", "--count", &format!("HEAD..{base}")])
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            if behind > 0 {
                log.push(format!(
                    "Branch is {behind} commits behind {base}. Rebasing..."
                ));
                if git_in_long(dir, &["rebase", base, "--quiet"])
                    .is_some_and(|o| o.status.success())
                {
                    rebased = true;
                } else {
                    log.push(format!(
                        "Rebase conflict. Aborting rebase. Run 'git rebase {base}' manually."
                    ));
                    let _ = git_in(dir, &["rebase", "--abort"]);
                }
            }
            let merges = git_text(dir, &["rev-list", "--merges", &format!("{base}..HEAD")])
                .map_or(0, |t| t.lines().count());
            if merges > 0 {
                log.push(format!(
                    "ERROR: {merges} merge commit(s) on this branch. Run 'git rebase {base}' to remove them."
                ));
                return Err(log.join("\n"));
            }
        }
    }

    let refspec = format!("HEAD:refs/heads/{branch}");
    let push = |extra: &[&str]| -> Result<std::process::Output, String> {
        let mut a = vec!["push"];
        a.extend_from_slice(extra);
        a.push("origin");
        a.push(&refspec);
        git_in_long(dir, &a).ok_or_else(|| "could not run git push (or it timed out)".to_string())
    };
    let remote_absent = opts.auto
        && git_in_long(
            dir,
            &["ls-remote", "--exit-code", "--heads", "origin", &branch],
        )
        .is_some_and(|o| o.status.code() == Some(2));
    if opts.auto && remote_absent {
        let o = push(&[])?;
        log.push(show(&o));
        if !o.status.success() {
            log.push(format!(
                "ERROR: push to '{branch}' was rejected. Nothing was forced."
            ));
            return Err(log.join("\n"));
        }
    } else if opts.auto && (opts.amend || rebased) {
        log.push(format!(
            "PARKED: pushing '{branch}' would need a force, and auto mode never forces. The commit is local only. Push it by hand when you have checked the remote."
        ));
        return Err(log.join("\n"));
    } else if opts.amend || rebased {
        let o = push(&["--force-with-lease"])?;
        log.push(show(&o));
        if !o.status.success() {
            return Err(log.join("\n"));
        }
    } else {
        let o = push(&[])?;
        log.push(show(&o));
        if !o.status.success() {
            log.push(format!(
                "ERROR: push to '{branch}' was rejected. Run 'git pull --rebase origin {branch}', resolve any conflict by hand, then push again. This command never auto-escalates a rejected push to force-with-lease."
            ));
            return Err(log.join("\n"));
        }
    }
    let last = git_text(dir, &["log", "-1", "--oneline"]).unwrap_or_default();
    log.push(format!("Pushed: {last} -> origin/{branch}"));
    Ok(log
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hook_that_writes_the_trailer_counts() {
        assert!(hook_writes_signoff(
            "git interpret-trailers --trailer 'Signed-off-by: x' \"$1\""
        ));
        assert!(hook_writes_signoff("exec git commit --signoff"));
        assert!(hook_writes_signoff("echo 'Signed-off-by: x' >> \"$1\""));
        assert!(hook_writes_signoff(
            "git interpret-trailers \\\n  --trailer=Signed-off-by:x"
        ));
    }

    #[test]
    fn a_hook_that_only_checks_does_not_count() {
        assert!(!hook_writes_signoff(
            "grep -q '^Signed-off-by:' \"$1\" || exit 1"
        ));
        assert!(!hook_writes_signoff("# git commit --signoff\nexit 0"));
        assert!(!hook_writes_signoff(""));
    }
}
