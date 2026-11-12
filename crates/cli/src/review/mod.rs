// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook review`: the setup both review commands share. It turns the raw
//! arguments into a PR, decides report-only or posting, and decides whether to
//! review in place or in an isolated worktree. Before this, each command did
//! it in a bash block.

use crate::pr::shared::gh;
use std::process::Command;

pub mod checks;
pub mod lens;
pub mod lenses;
pub mod size;
pub mod suggest;

/// Which review command is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Quick,
    Deep,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Quick => "quick",
            Kind::Deep => "deep",
        }
    }
}

/// What the arguments name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A PR number (`12` or `#12`).
    Number(String),
    /// A branch name to resolve to its open PR.
    Branch(String),
    /// Nothing: the current branch's PR.
    Current,
}

/// The parsed arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub target: Target,
    pub self_flag: bool,
}

/// Split the raw argument string. `--self`, `--auto`, `--ask` and any other
/// `--` flag are not a target; the first remaining word is.
pub fn parse(args: &str) -> Parsed {
    let mut self_flag = false;
    let mut target = Target::Current;
    for word in args.split_whitespace() {
        if word.starts_with("--") {
            self_flag |= word == "--self" || word == "--no-post";
            continue;
        }
        if !matches!(target, Target::Current) {
            continue;
        }
        let digits = word.strip_prefix('#').unwrap_or(word);
        target = if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            Target::Number(digits.to_string())
        } else {
            Target::Branch(word.to_string())
        };
    }
    Parsed { target, self_flag }
}

/// Report-only only when the user passed `--self` or `--no-post`. The run
/// mode, naming no PR and reviewing your own PR do not skip the post
/// question: the command narrows the submit verbs for your own PR instead.
/// Pipelines pass `--self`.
pub fn self_mode(parsed: &Parsed) -> bool {
    parsed.self_flag
}

fn git_ok(args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn git_text(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `nameWithOwner` and `url` from one `gh repo view --json` call.
fn parse_repo_json(raw: &str) -> Result<(String, String), String> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("gh repo view returned bad JSON: {e}"))?;
    let field = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    Ok((field("nameWithOwner"), field("url")))
}

/// Everything the command file needs, printed as `KEY=value` lines.
pub fn prepare(kind: Kind, args: &str, _run_mode_auto: bool) -> Result<String, String> {
    let parsed = parse(args);
    let pr_number = match &parsed.target {
        Target::Number(n) => n.clone(),
        Target::Branch(b) => {
            if !git_ok(&["check-ref-format", "--branch", b]) {
                return Err(
                    "pass an integer PR number, a branch name, --self, or no args (report-only)"
                        .to_string(),
                );
            }
            let n = gh(&[
                "pr",
                "list",
                "--head",
                b,
                "--json",
                "number",
                "-q",
                ".[0].number",
            ])
            .unwrap_or_default();
            if n.is_empty() {
                return Err(format!(
                    "no open PR for branch {b}; create one first or pass a PR number"
                ));
            }
            n
        }
        Target::Current => gh(&["pr", "view", "--json", "number", "-q", ".number"])
            .ok()
            .filter(|n| !n.is_empty())
            .ok_or("no PR found for current branch; create one first or pass a PR number")?,
    };
    // Four independent read-only calls: run them together, then read the
    // results in the old order so the first failure reported is unchanged.
    let calls: [Vec<&str>; 4] = [
        vec!["repo", "view", "--json", "nameWithOwner,url"],
        vec![
            "pr",
            "view",
            &pr_number,
            "--json",
            "headRefOid",
            "-q",
            ".headRefOid",
        ],
        vec![
            "pr",
            "view",
            &pr_number,
            "--json",
            "author",
            "-q",
            ".author.login",
        ],
        vec!["api", "/user", "-q", ".login"],
    ];
    let mut results =
        crate::common::par::map(&calls, crate::common::par::MAX_CONCURRENT, |a| gh(a)).into_iter();
    let (Some(repo_json), Some(head_sha), Some(author), Some(me)) = (
        results.next(),
        results.next(),
        results.next(),
        results.next(),
    ) else {
        return Err("gh returned fewer results than calls".to_string());
    };
    let (repo_json, head_sha, author, me) = (repo_json?, head_sha?, author?, me?);
    let (repo, repo_url) = parse_repo_json(&repo_json)?;
    let self_review = author == me;
    let report_only = self_mode(&parsed);

    let in_place = git_text(&["rev-parse", "HEAD"]) == head_sha
        && git_text(&["status", "--porcelain", "--untracked-files=no"]).is_empty();
    let (mode, wt) = if in_place {
        (
            "in-place (HEAD matches, tree clean)".to_string(),
            String::new(),
        )
    } else {
        let path = crate::worktree::review::setup_with_url(
            &pr_number,
            &head_sha,
            Some(&repo_url),
            crate::common::time::now_secs(),
        )
        .map_err(|e| format!("worktree setup failed: {e}"))?;
        if path.is_empty() {
            return Err("worktree setup failed: no path returned".to_string());
        }
        (format!("worktree at {path}"), path)
    };

    let review_json = format!("/tmp/{repo}/{}-review-{pr_number}.json", kind.name());
    if let Some(dir) = std::path::Path::new(&review_json).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    Ok(format!(
        "PR={repo}#{pr_number}\nREPO={repo}\nPR_NUMBER={pr_number}\nHEAD_SHA={head_sha}\n\
         AUTHOR={author}\nSELF_REVIEW={self_review}\nSELF_MODE={report_only}\n\
         MODE={mode}\nWT={wt}\nREVIEW_JSON={review_json}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_branches_and_nothing_parse() {
        assert_eq!(parse("42").target, Target::Number("42".into()));
        assert_eq!(parse("#42 --self").target, Target::Number("42".into()));
        assert_eq!(parse("feat/x").target, Target::Branch("feat/x".into()));
        assert_eq!(parse("").target, Target::Current);
        assert_eq!(parse("--auto --ask").target, Target::Current);
    }

    #[test]
    fn only_the_first_word_is_the_target() {
        assert_eq!(parse("12 feat/x").target, Target::Number("12".into()));
        assert_eq!(parse("feat/x 12").target, Target::Branch("feat/x".into()));
    }

    #[test]
    fn the_self_flag_is_read_wherever_it_sits() {
        assert!(parse("--self 12").self_flag);
        assert!(parse("12 --self").self_flag);
        assert!(!parse("12").self_flag);
    }

    #[test]
    fn report_only_follows_the_flags_only() {
        let p = parse("12");
        assert!(!self_mode(&p));
        assert!(self_mode(&parse("12 --self")));
        assert!(self_mode(&parse("12 --no-post")));
        // No target and `--auto` still ask what to post.
        assert!(!self_mode(&parse("")));
        assert!(!self_mode(&parse("12 --auto")));
    }
}
