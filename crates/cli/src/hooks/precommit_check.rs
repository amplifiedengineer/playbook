// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Ports the retired shell original: a mechanical sanity pass over the staged
//! diff before a commit.
//!
//! `/playbook:commit` drafts the message from the staged diff with no
//! model, so nothing in that flow judges the diff it is about to commit. This covers the half that needs
//! no judgement: debug leftovers, secret-shaped filenames, an oversized commit.
//!
//! Warn, never block. A debug statement can be deliberate and a large commit
//! can be a legitimate refactor, and only the author knows which.

use crate::common::emit_pre_context;
use crate::common::payload::Payload;
use crate::hooks::starts_a_command;

/// House rule is small single-concern commits. Matches the shell thresholds.
const MAX_FILES: usize = 20;
const MAX_CHANGED_LINES: u64 = 600;

/// Filename shapes that usually mean a credential. Names only, never contents.
const SECRET_SUFFIXES: [&str; 5] = [".pem", ".p12", ".pfx", ".keystore", "credentials.json"];
const SECRET_NAMES: [&str; 2] = ["id_rsa", "id_ed25519"];

/// Debug statements across the languages this repo touches.
const DEBUG_MARKERS: [&str; 9] = [
    "console.log",
    "console.debug",
    "debugger;",
    "dbg!(",
    "breakpoint()",
    "pdb.set_trace",
    "binding.pry",
    "fmt.Println",
    "System.out.println",
];

pub fn run(payload: &Payload) {
    if std::env::var("PRECOMMIT_CHECK").as_deref() == Ok("0") {
        return;
    }
    let cmd = payload.field(".tool_input.command");
    if cmd.is_empty() || !is_commit(&cmd) {
        return;
    }
    let findings = findings_in(None);
    if !findings.is_empty() {
        let findings = findings.iter().map(|f| format!("{f} ")).collect::<String>();
        emit_pre_context(
            "PreToolUse",
            &format!(
                "Staged-diff check before this commit: {findings}Warning only, nothing is \
                 blocked. Review the staged diff and either fix it or proceed deliberately. \
                 Disable this guard with PRECOMMIT_CHECK=0."
            ),
        );
    }
}

/// The staged-diff warnings for the repo at `dir` (the process cwd when
/// `None`), one sentence each. Empty when nothing is staged or git fails.
/// `commit run -A` reports these in its `warnings`, because a commit made inside
/// the binary never fires the hook.
pub fn findings_in(dir: Option<&std::path::Path>) -> Vec<String> {
    // Outside a repo this exits non-zero, which is the same silent return.
    let Some(staged) = git_in(dir, &["diff", "--cached", "--name-only"]) else {
        return Vec::new();
    };
    let files: Vec<&str> = staged.lines().filter(|l| !l.trim().is_empty()).collect();
    if files.is_empty() {
        return Vec::new();
    }
    let mut findings = Vec::new();

    let secrets: Vec<&str> = files.iter().copied().filter(|f| looks_secret(f)).collect();
    if !secrets.is_empty() {
        findings.push(format!(
            "Secret-shaped files are staged: {}. Confirm these belong in the repo.",
            secrets.join(" ")
        ));
    }

    let debug_lines = count_debug_lines(dir);
    if debug_lines > 0 {
        findings.push(format!(
            "{debug_lines} added line(s) look like debug output (console.log, debugger, dbg!, breakpoint, pdb, pry, println)."
        ));
    }

    let changed = changed_line_count(dir);
    if files.len() > MAX_FILES || changed > MAX_CHANGED_LINES {
        findings.push(format!(
            "Large commit: {} file(s), {changed} changed line(s). Consider splitting it into single-concern commits.",
            files.len()
        ));
    }
    findings
}

/// True for a real commit. `git commit --amend` counts; `git log` and
/// `git commit --help` do not.
fn is_commit(cmd: &str) -> bool {
    if cmd.split_whitespace().any(|t| t == "--help") {
        return false;
    }
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    tokens.iter().enumerate().any(|(i, tok)| {
        *tok == "git"
            && starts_a_command(&tokens, i)
            // Skip git's own global flags, so `git -C dir commit` still counts.
            && tokens[i + 1..]
                .iter()
                .find(|t| !t.starts_with('-'))
                .is_some_and(|verb| *verb == "commit")
    })
}

fn looks_secret(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    name == ".env"
        || name.starts_with(".env.")
        || SECRET_NAMES.contains(&name.as_str())
        || SECRET_SUFFIXES.iter().any(|s| name.ends_with(s))
}

fn count_debug_lines(dir: Option<&std::path::Path>) -> usize {
    let Some(diff) = git_in(dir, &["diff", "--cached", "--unified=0"]) else {
        return 0;
    };
    diff.lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .filter(|l| DEBUG_MARKERS.iter().any(|m| l.contains(m)))
        .count()
}

/// Added plus deleted, matching the shell's `awk '{a+=$1; d+=$2}'` over
/// `--numstat`. Binary files report `-`, which parses as 0 there and here.
fn changed_line_count(dir: Option<&std::path::Path>) -> u64 {
    let Some(numstat) = git_in(dir, &["diff", "--cached", "--numstat"]) else {
        return 0;
    };
    numstat
        .lines()
        .map(|line| {
            let mut cols = line.split_whitespace();
            let add = cols.next().and_then(|c| c.parse::<u64>().ok()).unwrap_or(0);
            let del = cols.next().and_then(|c| c.parse::<u64>().ok()).unwrap_or(0);
            add + del
        })
        .sum()
}

/// `None` when git is missing, times out, or exits non-zero, which the caller
/// treats as "nothing to say" rather than an error.
fn git_in(dir: Option<&std::path::Path>, args: &[&str]) -> Option<String> {
    crate::common::git::untrimmed(crate::common::git::run(
        dir,
        args,
        crate::common::git::TIMEOUT,
    ))
}
