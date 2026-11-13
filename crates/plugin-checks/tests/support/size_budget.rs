// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The size budget for plugin markdown, and the ratchet that keeps it shrinking.
//!
//! A main file (a command or a `SKILL.md`) loads whole on every run, so it has
//! a tight limit. A reference file loads on demand and has a looser one. A file
//! over its limit needs a row in `size_budget_allowlist.txt`; the row is a
//! ceiling that only goes down. Every function takes its inputs as arguments,
//! so a test runs it on literals before it runs on the real tree.

/// The longest a main file may be, in lines.
pub(crate) const MAIN_MAX_LINES: usize = 200;
/// The largest a main file may be, in bytes.
pub(crate) const MAIN_MAX_BYTES: usize = 20_000;
/// The longest a reference file may be, in lines.
pub(crate) const REF_MAX_LINES: usize = 300;
/// The largest a reference file may be, in bytes.
pub(crate) const REF_MAX_BYTES: usize = 24_000;

/// The phrase that opens a fallback block: what to do without the binary.
pub(crate) const FALLBACK_PHRASE: &str = "Otherwise do this";

/// Main files whose slimming PR has landed. Each holds no fenced block that
/// calls `gh api` or `jq`. Each slimming PR adds its file.
pub(crate) const SLIMMED: &[&str] = &[
    "commands/fix.md",
    "commands/handoff.md",
    "commands/session-handoff.md",
    "commands/session-start.md",
    "commands/pr-comments.md",
    "commands/review.md",
    "skills/atlassian/SKILL.md",
    "skills/delegating-subagents/SKILL.md",
];

/// The paths that had a row when the budget landed, frozen. Each one has a
/// slimming PR, so a row never leaves by a byte-only shrink: once a target has
/// no row left it must be in `SLIMMED`.
pub(crate) const SLIMMED_TARGETS: &[&str] = &[
    "commands/address-pr-comments.md",
    "commands/adr.md",
    "commands/deep-review.md",
    "commands/fix.md",
    "commands/implement.md",
    "commands/learn-project.md",
    "commands/plan.md",
    "commands/quick-review.md",
    "commands/repo-audit.md",
    "commands/review.md",
    "commands/session-handoff.md",
    "commands/session-start.md",
    "commands/setup.md",
    "skills/atlassian/SKILL.md",
    "skills/delegating-subagents/SKILL.md",
];

/// Text that moved from a main file to a reference file: (old path, new path).
pub(crate) const MOVED: &[(&str, &str)] = &[
    (
        "commands/deep-review.md",
        "skills/grounding-review/references/deep-pass.md",
    ),
    (
        "commands/quick-review.md",
        "skills/grounding-review/references/quick-pass.md",
    ),
];

/// Which budget a file gets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FileKind {
    Main,
    Reference,
}

/// One allowlist row.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Row {
    /// A file allowed up to this many lines and bytes.
    Size {
        path: String,
        max_lines: usize,
        max_bytes: usize,
    },
    /// A file allowed up to this many fallback blocks.
    Fallbacks { path: String, max: usize },
}

impl Row {
    pub(crate) fn path(&self) -> &str {
        match self {
            Row::Size { path, .. } | Row::Fallbacks { path, .. } => path,
        }
    }
}

/// Parse the allowlist: `size <path> <max_lines> <max_bytes>` and
/// `fallbacks <path> <max>` rows, with `#` comments and blank lines ignored.
pub(crate) fn parse_rows(text: &str) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = index + 1;
        let parts: Vec<&str> = line.split_whitespace().collect();
        let number = |s: &str| {
            s.parse::<usize>()
                .map_err(|_| format!("line {at}: `{s}` is not a number"))
        };
        let row = match parts.as_slice() {
            ["size", path, lines, bytes] => Row::Size {
                path: (*path).to_owned(),
                max_lines: number(lines)?,
                max_bytes: number(bytes)?,
            },
            ["fallbacks", path, max] => Row::Fallbacks {
                path: (*path).to_owned(),
                max: number(max)?,
            },
            _ => {
                return Err(format!(
                    "line {at}: `{line}` is not a size or fallbacks row"
                ))
            }
        };
        rows.push(row);
    }
    Ok(rows)
}

/// How many fallback blocks `text` holds.
pub(crate) fn count_fallbacks(text: &str) -> usize {
    text.matches(FALLBACK_PHRASE).count()
}

/// The problems with one file against the budget and its rows.
pub(crate) fn check_file(
    kind: FileKind,
    path: &str,
    lines: usize,
    bytes: usize,
    fallbacks: usize,
    rows: &[Row],
) -> Vec<String> {
    let (max_lines, max_bytes) = match kind {
        FileKind::Main => (MAIN_MAX_LINES, MAIN_MAX_BYTES),
        FileKind::Reference => (REF_MAX_LINES, REF_MAX_BYTES),
    };
    let within = lines <= max_lines && bytes <= max_bytes;
    let size_row = rows.iter().find_map(|r| match r {
        Row::Size {
            path: p,
            max_lines,
            max_bytes,
        } if p == path => Some((*max_lines, *max_bytes)),
        _ => None,
    });
    let fallback_row = rows.iter().find_map(|r| match r {
        Row::Fallbacks { path: p, max } if p == path => Some(*max),
        _ => None,
    });
    let mut problems = Vec::new();
    match (within, size_row) {
        (true, Some(_)) => problems.push(format!(
            "{path}: has a size row but is within budget ({lines} lines, {bytes} bytes): delete the row"
        )),
        (false, None) => problems.push(format!(
            "{path}: {lines} lines and {bytes} bytes are over the budget of {max_lines} lines and {max_bytes} bytes, and it has no size row"
        )),
        (false, Some((row_lines, row_bytes))) if lines > row_lines || bytes > row_bytes => {
            problems.push(format!(
                "{path}: {lines} lines and {bytes} bytes grew past its size row of {row_lines} lines and {row_bytes} bytes"
            ));
        }
        _ => {}
    }
    match fallback_row {
        Some(_) if fallbacks == 0 => problems.push(format!(
            "{path}: has a fallbacks row but no fallback block: delete the row"
        )),
        Some(max) if fallbacks > max => problems.push(format!(
            "{path}: {fallbacks} fallback blocks grew past its row of {max}"
        )),
        None if fallbacks > 0 && kind == FileKind::Main => problems.push(format!(
            "{path}: a main file holds {fallbacks} fallback block(s) and has no fallbacks row: move them to without-binary.md"
        )),
        _ => {}
    }
    problems
}

/// The rows that name a path no file has.
pub(crate) fn check_rows_exist(rows: &[Row], existing: &[String]) -> Vec<String> {
    rows.iter()
        .filter(|r| !existing.iter().any(|e| e == r.path()))
        .map(|r| format!("{}: a row names a file that does not exist", r.path()))
        .collect()
}

/// Where `path` lives now: followed through `moved` and then through the
/// rename pairs (old path, new path).
pub(crate) fn resolve_path(
    path: &str,
    moved: &[(&str, &str)],
    renamed: &[(String, String)],
) -> String {
    let mut now = path.to_owned();
    if let Some((_, to)) = moved.iter().find(|(from, _)| *from == now) {
        now = (*to).to_owned();
    }
    if let Some((_, to)) = renamed.iter().find(|(from, _)| *from == now) {
        now = to.clone();
    }
    now
}

/// The targets that have no row left and are not in `slimmed`.
pub(crate) fn check_slimmed_targets(
    targets: &[&str],
    slimmed: &[&str],
    rows: &[Row],
    moved: &[(&str, &str)],
    renamed: &[(String, String)],
) -> Vec<String> {
    targets
        .iter()
        .map(|t| resolve_path(t, moved, renamed))
        .filter(|now| !rows.iter().any(|r| r.path() == now) && !slimmed.contains(&now.as_str()))
        .map(|now| format!("{now}: its rows are gone but it is not in SLIMMED"))
        .collect()
}

/// Whether `text` holds a fenced block that calls `gh api` or `jq`.
pub(crate) fn has_gh_api_or_jq_block(text: &str) -> bool {
    let mut in_block = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_block = !in_block;
        } else if in_block
            && (line.contains("gh api")
                || line
                    .split(|c: char| !c.is_alphanumeric())
                    .any(|w| w == "jq"))
        {
            return true;
        }
    }
    false
}

/// The problems with a `without-binary.md` reference and the files that point
/// into it: every `##` heading is unique, and every `without-binary.md#<id>`
/// anchor in `pointers` names one.
pub(crate) fn check_anchors(reference: &str, pointers: &[(String, String)]) -> Vec<String> {
    let mut headings: Vec<&str> = Vec::new();
    let mut problems = Vec::new();
    for heading in reference.lines().filter_map(|l| l.strip_prefix("## ")) {
        let heading = heading.trim();
        if headings.contains(&heading) {
            problems.push(format!(
                "without-binary.md: the heading `{heading}` appears twice"
            ));
        }
        headings.push(heading);
    }
    const MARK: &str = "without-binary.md#";
    for (path, text) in pointers {
        for (at, _) in text.match_indices(MARK) {
            let id: String = text[at + MARK.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            if !headings.contains(&id.as_str()) {
                problems.push(format!("{path}: `{MARK}{id}` names no heading"));
            }
        }
    }
    problems
}
