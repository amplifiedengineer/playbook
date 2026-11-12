// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The drafting commit behind `playbook commit run -A` and the hidden legacy
//! `playbook commit all`: stage, draft a conventional message from the staged
//! files with no model, then commit, rebase when behind and push through the
//! same code as the message-file path of `commit run`.

use super::typecheck::{kind_of, Kind};
use super::{current_branch, git_text, prepare, run};
use std::path::Path;

/// Longest commit header the drafter writes.
const MAX_HEADER: usize = 72;
/// What `prepare` prints when nothing is staged.
const NOTHING_STAGED: &str = "NO_STAGED_CHANGES";
/// Folder that holds the crates, so the crate name is the area.
const CRATES_DIR: &str = "crates";
const DEPS_SCOPE: &str = "deps";

/// Flags of one run.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub message: Option<String>,
    pub auto: bool,
    pub no_signoff: bool,
    /// `git add -A` first.
    pub stage_all: bool,
    /// `git add -u` first.
    pub stage_update: bool,
    /// Replace the last commit; its message is kept unless `message` is set.
    pub amend: bool,
    /// Skip the conventional commit type guard.
    pub no_type_check: bool,
    /// Skip the prose check on a given message; a drafted one is never checked.
    pub no_prose_check: bool,
}

/// One staged file: its git status letter and path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub status: char,
    pub path: String,
}

/// Reads `git diff --name-status` lines. A rename keeps its new path.
pub fn parse_changes(name_status: &str) -> Vec<Change> {
    name_status
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let status = parts.next()?.chars().next()?;
            let path = parts.next_back()?;
            Some(Change {
                status,
                path: path.to_string(),
            })
        })
        .collect()
}

/// The area a path belongs to: the crate for `crates/<name>/...`, `plugin`
/// for `plugins/...`, `ci` for `.github/...`, else the top folder. A file in
/// the repo root has no area.
fn area_of(path: &str) -> Option<String> {
    // A file in the repo root has no folder, so no area.
    let (top, rest) = path.split_once('/')?;
    let second = rest.split('/').next().unwrap_or(rest);
    let area = match top {
        CRATES_DIR => second,
        "plugins" => "plugin",
        ".github" => "ci",
        other => other,
    };
    Some(area.to_string())
}

/// The shared area of all files, or `None` when they differ.
fn common_area(changes: &[Change]) -> Option<String> {
    let mut areas = changes.iter().map(|c| area_of(&c.path));
    let first = areas.next()??;
    areas
        .all(|a| a.as_deref() == Some(first.as_str()))
        .then_some(first)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A conventional commit message for the staged changes. It is a guide, the
/// PR title and body carry the meaning.
pub fn draft(changes: &[Change]) -> String {
    let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
    let area = common_area(changes);
    let kind = kind_of(&paths);
    let added: Vec<&Change> = changes.iter().filter(|c| c.status == 'A').collect();

    let (ty, scope, subject) = match kind {
        Some(Kind::Deps) => (
            "chore",
            Some(DEPS_SCOPE.to_string()),
            "update dependencies".to_string(),
        ),
        Some(Kind::Docs) => ("docs", None, subject_for(changes, &area)),
        Some(Kind::Ci) => ("ci", None, subject_for(changes, &area)),
        Some(Kind::Tests) => ("test", area.clone(), subject_for(changes, &area)),
        None => (
            if added.is_empty() { "fix" } else { "feat" },
            area.clone(),
            subject_for(changes, &area),
        ),
    };
    let head = match scope {
        Some(scope) => format!("{ty}({scope}): "),
        None => format!("{ty}: "),
    };
    let room = MAX_HEADER.saturating_sub(head.chars().count());
    let subject: String = subject.chars().take(room).collect();
    format!("{head}{subject}")
}

fn subject_for(changes: &[Change], area: &Option<String>) -> String {
    let mut added = changes.iter().filter(|c| c.status == 'A');
    if let (Some(only), None, 1) = (added.next(), added.next(), changes.len()) {
        return format!("add {}", file_name(&only.path));
    }
    match area {
        Some(area) => format!("update {area}"),
        None => "update files".to_string(),
    }
}

/// Stage everything, commit, rebase when behind and push. `Ok` is the text to
/// print, `Err` the text for stderr.
pub fn run(dir: &Path, opts: Options, config_signoff: bool) -> Result<String, String> {
    let staged = prepare::run(
        dir,
        prepare::Options {
            stage_all: opts.stage_all,
            stage_update: opts.stage_update,
            amend: opts.amend,
        },
    )?;
    if staged == NOTHING_STAGED {
        return Ok(staged);
    }
    let changes = parse_changes(
        &git_text(
            dir,
            &[
                "-c",
                "core.quotepath=off",
                "diff",
                "--staged",
                "--name-status",
            ],
        )
        .unwrap_or_default(),
    );
    let kept = if opts.amend && opts.message.is_none() {
        git_text(dir, &["log", "-1", "--format=%B"]).filter(|m| !m.is_empty())
    } else {
        None
    };
    let drafted = opts.message.is_none() && kept.is_none();
    let message = opts.message.or(kept).unwrap_or_else(|| draft(&changes));

    let file = std::env::temp_dir().join(format!("playbook-commit-all-{}.txt", std::process::id()));
    std::fs::write(&file, format!("{message}\n"))
        .map_err(|e| format!("could not store the message: {e}"))?;
    let result = run::run(
        dir,
        &file,
        run::Options {
            amend: opts.amend,
            auto: opts.auto,
            no_signoff: opts.no_signoff,
            no_prose_check: drafted || opts.no_prose_check,
            no_type_check: opts.no_type_check,
        },
        config_signoff,
    );
    let _ = std::fs::remove_file(&file);
    let pushed = result?;

    let sha = git_text(dir, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    Ok(format!(
        "{pushed}\nBRANCH={}\nCOMMIT={sha}\nMESSAGE={}",
        current_branch(dir),
        message.lines().next().unwrap_or("")
    ))
}

/// The `--json` form. Parses `text` (the command's arguments and the
/// worktree phrase), runs in the named root and reports in the shared
/// envelope. `base` carries the message and the check skips given on the
/// command line; staging, mode and amend come from `text`. It never fails the
/// process: the outcome is in `verdict`.
pub fn run_inline(text: &str, base: Options, config_signoff: bool) -> serde_json::Value {
    use crate::common::inline_args;
    use crate::json::envelope::wrap;
    use serde_json::json;

    let refuse = |verdict: &str, code: i32, why: String, mode: &str| {
        wrap(
            code,
            &why,
            json!({
                "verdict": verdict, "branch": null, "root": null, "commit": "",
                "message": "", "pushed": false, "rebased": false, "forced": false,
                "mode": mode, "warnings": [], "errors": [why],
            }),
        )
    };
    let parsed = match inline_args::parse(text) {
        Ok(p) => p,
        Err(e) => return refuse("REFUSED", 1, format!("{}: {}", e.code, e.message), "ask"),
    };
    let home = crate::common::home_dir();
    let slug = crate::common::repo_slug();
    let mode = parsed
        .mode_at(
            std::env::var("PLAYBOOK_MODE").ok().as_deref(),
            &home,
            (!slug.is_empty()).then_some(slug.as_str()),
        )
        .mode;
    let auto = mode == crate::common::mode::Mode::Auto;
    let mode_text = if auto { "auto" } else { "ask" };
    let dir = std::path::PathBuf::from(
        parsed
            .root
            .clone()
            .or(parsed.dir.clone())
            .unwrap_or_else(|| ".".into()),
    );
    if let (Some(root), Some(branch)) = (&parsed.root, &parsed.branch) {
        if let Err(why) = inline_args::guard(Path::new(root), branch) {
            return refuse("WRONG_DIRECTORY", 1, why, mode_text);
        }
    }
    let top = git_text(&dir, &["rev-parse", "--show-toplevel"]);
    let Some(top) = top else {
        return refuse(
            "REFUSED",
            1,
            format!("{} is not a git worktree", dir.display()),
            mode_text,
        );
    };
    let root = Path::new(&top);
    let opts = Options {
        message: base.message.or_else(|| parsed.message.clone()),
        auto,
        no_signoff: parsed.no_signoff,
        stage_all: parsed.stage_all,
        stage_update: parsed.tracked,
        amend: parsed.amend,
        no_type_check: base.no_type_check,
        no_prose_check: base.no_prose_check,
    };
    // Before the commit: afterwards nothing is staged to look at. A protected
    // branch is refused by `run` without touching the index.
    let on_protected = prepare::is_protected(&current_branch(root), &prepare::default_branch(root));
    let warnings = if on_protected {
        Vec::new()
    } else {
        if parsed.stage_all {
            let _ = git_text(root, &["add", "-A"]);
        } else if parsed.tracked {
            let _ = git_text(root, &["add", "-u"]);
        }
        crate::hooks::precommit_check::findings_in(Some(root))
    };
    let result = run(root, opts, config_signoff);
    let branch = current_branch(root);
    let sha = git_text(root, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let message = git_text(root, &["log", "-1", "--format=%B"]).unwrap_or_default();
    let (verdict, code, log, error) = match &result {
        Ok(text) if text.trim() == NOTHING_STAGED => {
            (NOTHING_STAGED, 0, String::new(), String::new())
        }
        // An aborted rebase still pushes the branch as it was, but the caller
        // must know it is not on the base yet.
        Ok(text) if text.contains("Rebase conflict") => (
            "CONFLICT",
            4,
            text.clone(),
            "rebase conflict: the branch was pushed without the rebase; run the git rebase it names by hand".to_string(),
        ),
        Ok(text) => ("PUSHED", 0, text.clone(), String::new()),
        Err(text) if text.contains("PARKED") => ("PARKED", 1, text.clone(), text.clone()),
        Err(text)
            if text.contains("never commits there")
                || text.contains("breaks the writing rules")
                || text.contains("commit type check") =>
        {
            ("REFUSED", 1, text.clone(), text.clone())
        }
        Err(text) => ("FAILED", 1, text.clone(), text.clone()),
    };
    let pushed = verdict == "PUSHED" || verdict == "CONFLICT";
    let rebased = log.contains("Rebasing") && !log.contains("Rebase conflict");
    let forced = pushed && !auto && (parsed.amend || rebased);
    // After a refusal the HEAD commit is not ours, so it is not reported.
    let (commit, message) = if pushed {
        (sha, message.lines().next().unwrap_or("").to_string())
    } else {
        (String::new(), String::new())
    };
    wrap(
        code,
        &error,
        json!({
            "verdict": verdict, "branch": branch, "root": top, "commit": commit,
            "message": message, "pushed": pushed, "rebased": rebased, "forced": forced,
            "mode": mode_text, "warnings": warnings,
            "errors": if error.is_empty() { vec![] } else { vec![error.clone()] },
            "log": log,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes(lines: &str) -> Vec<Change> {
        parse_changes(lines)
    }

    #[test]
    fn a_rename_keeps_its_new_path() {
        let c = changes("R100\told.rs\tnew.rs\nM\ta.rs");
        assert_eq!(c[0].path, "new.rs");
        assert_eq!(c[1].status, 'M');
    }

    #[test]
    fn the_crate_is_the_area_of_a_crate_file() {
        assert_eq!(area_of("crates/cli/src/a.rs").as_deref(), Some("cli"));
        assert_eq!(area_of("plugins/playbook/a.md").as_deref(), Some("plugin"));
        assert_eq!(area_of(".github/workflows/x.yml").as_deref(), Some("ci"));
        assert_eq!(area_of("README.md"), None);
    }

    #[test]
    fn the_header_never_passes_the_limit() {
        let name = "x".repeat(200);
        let msg = draft(&changes(&format!("A\tcrates/cli/src/{name}.rs")));
        assert!(msg.chars().count() <= MAX_HEADER, "{msg}");
    }

    #[test]
    fn a_workflow_change_is_ci() {
        assert_eq!(
            draft(&changes("M\t.github/workflows/a.yml")),
            "ci: update ci"
        );
    }
}
