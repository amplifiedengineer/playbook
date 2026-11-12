// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook segment`: the git recipes `/playbook:implement` runs for a
//! Segment. `branch` puts HEAD on the right branch for the delivery topology
//! and `size` measures a Segment against its base. They live here so the
//! branch names and the base choice are tested code, not paragraphs the
//! model re-derives each run.

pub mod unit;

use crate::review::size::parse_numstat;
use std::path::Path;

/// The exit code and envelope code of a failed or differing verify.
const VERIFY_FAILED: i32 = 1;

/// Longest the title part of a branch name gets.
const SLUG_MAX: usize = 40;

/// How the Segments of a plan are delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topology {
    Stacked,
    Independent,
    Single,
}

/// Lowercase, dash separated, at most `SLUG_MAX` characters, no edge dashes.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(SLUG_MAX);
    out.trim_matches('-').to_string()
}

/// `<type>/<plan-slug>-s<N>-<seg-slug>`.
pub fn branch_name(kind: &str, plan_slug: &str, n: u32, title: &str) -> String {
    format!("{kind}/{plan_slug}-s{n}-{}", slug(title))
}

/// Longest one git call may run. Generous because fetch and switch can be slow.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = crate::common::git::run(Some(dir), args, GIT_TIMEOUT)
        .ok_or_else(|| format!("git {}: could not run or timed out", args.join(" ")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn ref_exists(dir: &Path, name: &str) -> bool {
    git(dir, &["rev-parse", "--verify", "--quiet", name]).is_ok()
}

/// Everything `branch` needs to know.
pub struct BranchArgs<'a> {
    pub kind: &'a str,
    pub plan_slug: &'a str,
    pub n: u32,
    pub title: &'a str,
    pub topology: Topology,
    pub default_branch: &'a str,
    pub prev_branch: Option<&'a str>,
    pub land: bool,
}

/// The ref a Segment branches from. Under `land` it is always the fetched
/// `origin/<default>`: the previous Segment's branch is already merged and may
/// be deleted, and the local default branch is stale. A stacked Segment past
/// the first branches off the previous Segment's branch, fetched when it is
/// not local (a resume after `/clear` or on a fresh checkout).
fn base_ref(dir: &Path, a: &BranchArgs<'_>) -> Result<String, String> {
    if a.land {
        git(dir, &["fetch", "origin", a.default_branch])?;
        return Ok(format!("origin/{}", a.default_branch));
    }
    match (a.topology, a.prev_branch) {
        (Topology::Stacked, Some(prev)) if a.n > 1 => {
            if ref_exists(dir, prev) {
                return Ok(prev.to_string());
            }
            git(dir, &["fetch", "origin", prev])?;
            Ok(format!("origin/{prev}"))
        }
        _ => Ok(a.default_branch.to_string()),
    }
}

/// Put HEAD on the Segment's branch and say where it started. Under the
/// single topology the plan shares one branch, so nothing is created and the
/// base is the current tip. Prints `branch=<name> base=<sha> base_ref=<ref>`.
pub fn branch(dir: &Path, a: &BranchArgs<'_>) -> Result<String, String> {
    if a.topology == Topology::Single && !a.land {
        let name = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        if name == a.default_branch {
            return Err(format!(
                "HEAD is on the default branch {name}; switch to the plan's branch first"
            ));
        }
        let sha = git(dir, &["rev-parse", "HEAD"])?;
        return Ok(format!("branch={name} base={sha} base_ref=HEAD"));
    }
    let name = branch_name(a.kind, a.plan_slug, a.n, a.title);
    if ref_exists(dir, &format!("refs/heads/{name}")) {
        return Err(format!(
            "branch {name} already exists; resume on it with git switch"
        ));
    }
    let base = base_ref(dir, a)?;
    let sha = git(dir, &["rev-parse", &base])?;
    git(dir, &["switch", "-c", &name, &sha])?;
    Ok(format!("branch={name} base={sha} base_ref={base}"))
}

/// Changed lines and files of `base...head`.
fn measure(dir: &Path, base: &str, head: &str) -> Result<(u64, u64), String> {
    let text = git(dir, &["diff", "--numstat", &format!("{base}...{head}")])?;
    Ok(parse_numstat(&text))
}

/// `lines=N files=M` for `base...HEAD`.
pub fn size(dir: &Path, base: &str) -> Result<String, String> {
    let (lines, files) = measure(dir, base, "HEAD")?;
    Ok(format!("lines={lines} files={files}"))
}

/// The listed files that differ between `origin/<default>` (fetched first)
/// and `branch`. Empty means the branch's content landed.
pub fn verify(
    dir: &Path,
    branch: &str,
    files: &[String],
    default_branch: &str,
) -> Result<Vec<String>, String> {
    for (kind, value) in [("branch", branch), ("default branch", default_branch)] {
        crate::pr::shared::reject_option_like(kind, value)?;
    }
    git(dir, &["fetch", "origin", default_branch])?;
    if !ref_exists(dir, branch) {
        return Err(format!("branch {branch} not found"));
    }
    let remote = format!("origin/{default_branch}");
    let mut args = vec!["diff", "--name-only", &remote, branch, "--"];
    args.extend(files.iter().map(String::as_str));
    Ok(git(dir, &args)?.lines().map(str::to_string).collect())
}

/// Print the verdict of `verify` and return the exit code: 0 on a match, 1 on
/// a difference or an error. With `json` the code rides the envelope and the
/// exit code is always 0.
pub fn run_verify(
    dir: &Path,
    branch: &str,
    files: &[String],
    default_branch: &str,
    json: bool,
) -> i32 {
    let result = verify(dir, branch, files, default_branch);
    let (code, error) = match &result {
        Ok(differing) if differing.is_empty() => (0, String::new()),
        Ok(differing) => (VERIFY_FAILED, format!("{} files differ", differing.len())),
        Err(error) => (VERIFY_FAILED, error.clone()),
    };
    if json {
        let fields = match &result {
            Ok(differing) => {
                serde_json::json!({"matches": differing.is_empty(), "differing": differing})
            }
            Err(_) => serde_json::Value::Null,
        };
        println!("{}", crate::json::envelope::wrap(code, &error, fields));
        return 0;
    }
    match result {
        Ok(differing) if differing.is_empty() => println!("match=true"),
        Ok(differing) => println!("match=false differing={}", differing.join(",")),
        Err(error) => eprintln!("error: {error}"),
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_lowercase_dashed_and_bounded() {
        assert_eq!(slug("Add the Parser (v2)!"), "add-the-parser-v2");
        assert_eq!(slug("  --Edge--  "), "edge");
        assert_eq!(slug(&"a".repeat(80)).len(), SLUG_MAX);
        assert_eq!(slug(&format!("{} b", "a".repeat(39))), "a".repeat(39));
    }

    #[test]
    fn branch_names_follow_the_plan_convention() {
        assert_eq!(
            branch_name("feat", "my-plan", 2, "Parser core"),
            "feat/my-plan-s2-parser-core"
        );
    }
}
