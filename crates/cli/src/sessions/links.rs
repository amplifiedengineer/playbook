// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The slow, fallible lookups behind a session record: the working directory
//! a transcript recorded, what git says about that directory, and the pull
//! request of the branch. Every call is bounded by a timeout and every failure
//! reads as "unknown". Nothing here writes or changes anything.

use super::{is_trunk, Place, Pr, PrKind};
use crate::common::git;
use crate::common::proc::run_with_timeout;
use crate::usage::run::Paths;
use crate::usage::UsageEvent;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Lines of a transcript read while looking for its working directory.
const HEAD_LINES: usize = 200;
const GH_TIMEOUT: Duration = Duration::from_secs(8);

/// The `cwd` of the first transcript line that has one, for the Claude Code
/// transcript of `session_id` under `projects`. The prompt text on those lines
/// is never kept.
pub fn transcript_cwd(projects: &Path, session_id: &str) -> Option<String> {
    let file = std::fs::read_dir(projects)
        .ok()?
        .flatten()
        .map(|d| d.path().join(format!("{session_id}.jsonl")))
        .find(|p| p.is_file())?;
    let reader = BufReader::new(std::fs::File::open(file).ok()?);
    reader
        .lines()
        .take(HEAD_LINES)
        .map_while(Result::ok)
        .find_map(|line| cwd_of_line(&line))
}

fn cwd_of_line(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    value
        .get("cwd")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The place of a working directory: linked worktree or main checkout, the
/// playbook convention, and whether it holds uncommitted changes.
pub fn place_of(cwd: Option<String>, home: &Path) -> Place {
    let Some(dir) = cwd else {
        return Place::default();
    };
    let path = PathBuf::from(&dir);
    if !path.is_dir() {
        return Place {
            cwd: Some(dir),
            gone: true,
            ..Place::default()
        };
    }
    let dirty = git::raw(&path, &["status", "--porcelain"]).map(|s| !s.trim().is_empty());
    let parsed = git::raw(
        &path,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-dir",
            "--git-common-dir",
        ],
    )
    .and_then(|out| parse_rev_parse(&out));
    let (worktree, convention) = match parsed {
        Some((top, true)) => {
            let convention = convention_label(crate::worktree::classify(Path::new(&top), home));
            (Some(top), convention)
        }
        _ => (None, ""),
    };
    Place {
        cwd: Some(dir),
        worktree,
        convention,
        gone: false,
        dirty,
    }
}

/// `(toplevel, is_linked_worktree)` from the three lines of
/// `git rev-parse --show-toplevel --git-dir --git-common-dir`.
pub fn parse_rev_parse(out: &str) -> Option<(String, bool)> {
    let mut lines = out.lines();
    let top = lines.next()?.trim().to_string();
    let git_dir = lines.next()?.trim();
    let common = lines.next()?.trim();
    (!top.is_empty()).then_some((top, git_dir != common))
}

fn convention_label(c: crate::worktree::Convention) -> &'static str {
    use crate::worktree::Convention::*;
    match c {
        Wu => "wu",
        AgentTool => "agent",
        CcLauncher => "cc",
        Review => "review",
        Unmanaged => "",
    }
}

/// The newest pull request of `branch`, read with `gh` from `dir`. `None` when
/// `gh` is missing, offline or finds none.
pub fn pr_of(dir: &Path, branch: &str) -> Option<Pr> {
    let mut command = Command::new("gh");
    command
        .current_dir(dir)
        .args([
            "pr", "list", "--head", branch, "--state", "all", "--limit", "1",
        ])
        .args(["--json", "number,state"]);
    let out = run_with_timeout(&mut command, GH_TIMEOUT).filter(|o| o.status.success())?;
    parse_pr_list(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_pr_list(json: &str) -> Option<Pr> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let first = value.as_array()?.first()?;
    let kind = match first.get("state")?.as_str()? {
        "OPEN" => PrKind::Open,
        "MERGED" => PrKind::Merged,
        "CLOSED" => PrKind::Closed,
        _ => return None,
    };
    Some(Pr {
        number: first.get("number")?.as_u64()?,
        kind,
    })
}

/// Looks up the place and the PR of every distinct session in `newest`, one
/// scoped thread each so slow `gh` calls overlap. Keyed by session id.
pub fn gather(
    newest: &[&UsageEvent],
    paths: &Paths,
    home: &Path,
) -> HashMap<String, (Place, Option<Pr>)> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = newest
            .iter()
            .map(|e| {
                scope.spawn(move || {
                    let cwd = transcript_cwd(&paths.claude_projects, &e.session_id);
                    let place = place_of(cwd, home);
                    let pr = match (&place.cwd, place.gone) {
                        (Some(dir), false) if !is_trunk(&e.branch) => {
                            pr_of(Path::new(dir), &e.branch)
                        }
                        _ => None,
                    };
                    (e.session_id.clone(), (place, pr))
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pr_list_reads_the_newest_state() {
        let pr = parse_pr_list(r#"[{"number":540,"state":"MERGED"}]"#).unwrap();
        assert_eq!((pr.number, pr.kind), (540, PrKind::Merged));
        assert_eq!(parse_pr_list("[]"), None);
        assert_eq!(parse_pr_list("not json"), None);
        assert_eq!(parse_pr_list(r#"[{"number":1,"state":"DRAFT"}]"#), None);
    }

    #[test]
    fn a_linked_worktree_has_a_git_dir_apart_from_the_common_dir() {
        let linked = "/w/repo-wt\n/r/.git/worktrees/repo-wt\n/r/.git\n";
        assert_eq!(parse_rev_parse(linked), Some(("/w/repo-wt".into(), true)));
        let main = "/r\n/r/.git\n/r/.git\n";
        assert_eq!(parse_rev_parse(main), Some(("/r".into(), false)));
        assert_eq!(parse_rev_parse(""), None);
    }

    #[test]
    fn the_cwd_comes_from_the_first_line_that_has_one_and_not_from_prompts() {
        assert_eq!(cwd_of_line(r#"{"type":"summary"}"#), None);
        assert_eq!(
            cwd_of_line(r#"{"cwd":"/w/x","message":{"content":"secret prompt"}}"#),
            Some("/w/x".into())
        );
    }

    #[test]
    fn a_transcript_is_found_by_session_id_under_any_project_folder() {
        let dir = std::env::temp_dir().join(format!("pb-sessions-{}", std::process::id()));
        let proj = dir.join("-w-x");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("abc.jsonl"),
            "{\"type\":\"summary\"}\n{\"cwd\":\"/w/x\"}\n",
        )
        .unwrap();
        assert_eq!(transcript_cwd(&dir, "abc"), Some("/w/x".into()));
        assert_eq!(transcript_cwd(&dir, "missing"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_directory_that_is_gone_reads_as_gone_and_never_as_clean() {
        let place = place_of(Some("/definitely/not/here".into()), Path::new("/h"));
        assert!(place.gone);
        assert_eq!(place.dirty, None);
    }
}
