// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Every `playbook`, `pb` and `pbtop` command and every `/playbook:<name>`
//! slash command that the user docs name must exist. A guide that names a
//! command the CLI does not have fails here, with the file and the line.
//!
//! Three classes of name:
//! - a primary name passes;
//! - a legacy name warns, or fails when `alias_is_error` is true. Legacy
//!   names are clap aliases, hidden subcommands and the names in
//!   `LEGACY_EXTRA`;
//! - any other name fails.
//!
//! A line or block that follows a `<!-- names: legacy -->` line may name a
//! legacy name in either mode, for a migration note such as "older versions
//! call this `upgrade`". An unknown name after the marker still fails.
//!
//! `pbdoc` and `pbsync` lines are not walked: no clap tree for them is
//! reachable from `crates/cli`. `playbook hook` is allowed by name because it
//! dispatches before clap (`crates/cli/src/fastpath.rs`).

use clap::{Command, CommandFactory};
use playbook::renames::{Rename, COMMAND_RENAMES};
use playbook::Cli;
use regex::Regex;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Whether a legacy name fails: a doc that names one outside a
/// `<!-- names: legacy -->` block is an error.
const ALIAS_IS_ERROR: bool = true;

/// Old names that clap no longer knows, as a space joined command path after
/// the program name (`"pr finish-context"`). The command rename table adds
/// its old names here.
const LEGACY_EXTRA: &[&str] = &[];

/// The legacy paths a rename table adds: each row's old path.
fn legacy_paths(table: &[Rename]) -> Vec<&'static str> {
    table.iter().map(|row| row.old).collect()
}

/// Names a change that is not on main yet will add. They count as legacy
/// names, so they warn instead of failing. Delete an entry once the command
/// exists, because a real command passes as a primary name.
const PENDING_NAMES: &[&str] = &[];

/// Pages in scope: files, or `dir/*.md` for the Markdown files in a folder.
const SCOPE: &[&str] = &[
    "README.md",
    "docs/*.md",
    "docs/guides/*.md",
    "docs/recipes/*.md",
    "docs/reference/*.md",
];

/// In scope by folder, but generated from the clap tree, so it names every
/// command by construction.
const SKIP: &[&str] = &["docs/reference/cli-and-config.md"];

/// Pages that may name a plugin slash command that no longer exists: the page
/// that lists every renamed or removed command. `CHANGELOG.md` and `docs/adr/`
/// are outside `SCOPE`, so they need no entry. The page's `playbook` command
/// names are still checked.
const SLASH_EXEMPT: &[&str] = &["docs/renamed.md"];

fn root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT"))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Report {
    errors: Vec<String>,
    warnings: Vec<String>,
}

/// How a name resolved.
enum Class {
    Primary,
    Legacy,
}

struct Rules<'a> {
    root: &'a Command,
    alias_is_error: bool,
    legacy_extra: &'a [&'a str],
    /// Does `/playbook:<name>` match a command file or a skill folder?
    slash_exists: &'a dyn Fn(&str) -> bool,
}

/// The names the `/playbook:` namespace holds: command files and skill
/// folders of the plugin.
fn plugin_names() -> BTreeSet<String> {
    let base = root().join("plugins/playbook");
    let mut names = BTreeSet::new();
    if let Ok(rd) = fs::read_dir(base.join("commands")) {
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "md") {
                if let Some(stem) = p.file_stem() {
                    names.insert(stem.to_string_lossy().into_owned());
                }
            }
        }
    }
    if let Ok(rd) = fs::read_dir(base.join("skills")) {
        for e in rd.filter_map(Result::ok) {
            if e.path().is_dir() {
                names.insert(e.file_name().to_string_lossy().into_owned());
            }
        }
    }
    names
}

/// The text of a trailing `# comment`, which starts at a `#` that follows
/// whitespace.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'#' && i > 0 && bytes[i - 1].is_ascii_whitespace() {
            return &line[..i];
        }
    }
    line
}

/// The command segments of one shell line: split on `&&`, `||`, `;` and `|`.
fn segments(line: &str) -> Vec<&str> {
    line.split("&&")
        .flat_map(|s| s.split("||"))
        .flat_map(|s| s.split([';', '|']))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// The words of a segment, after any leading `NAME=value` assignments and a
/// `$ ` prompt.
fn words(segment: &str) -> Vec<&str> {
    let seg = segment.strip_prefix("$ ").unwrap_or(segment);
    seg.split_whitespace()
        .skip_while(|w| {
            w.split_once('=').is_some_and(|(k, _)| {
                !k.is_empty()
                    && k.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            })
        })
        .collect()
}

fn find_sub<'a>(cmd: &'a Command, name: &str) -> Option<(&'a Command, Class)> {
    if let Some(s) = cmd.get_subcommands().find(|s| s.get_name() == name) {
        let class = if s.is_hide_set() {
            Class::Legacy
        } else {
            Class::Primary
        };
        return Some((s, class));
    }
    cmd.get_subcommands()
        .find(|s| s.get_all_aliases().any(|a| a == name))
        .map(|s| (s, Class::Legacy))
}

fn is_name(token: &str) -> bool {
    let mut chars = token.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Walk `words` (the tokens after the program name) down `start`. Returns the
/// name that failed and how, or the legacy names met on the way.
struct Walk {
    unknown: Option<(String, String)>,
    legacy: Vec<String>,
}

fn walk(start: &Command, program: &str, tokens: &[&str], rules: &Rules) -> Walk {
    let mut cur = start;
    let mut path = String::new();
    let mut out = Walk {
        unknown: None,
        legacy: Vec::new(),
    };
    for token in tokens {
        if !cur.has_subcommands() || !is_name(token) {
            break;
        }
        if cur.get_name() == start.get_name() && path.is_empty() && *token == "hook" {
            break;
        }
        if *token == "help" {
            break;
        }
        let full = if path.is_empty() {
            (*token).to_string()
        } else {
            format!("{path} {token}")
        };
        match find_sub(cur, token) {
            Some((sub, Class::Primary)) => {
                cur = sub;
                path = full;
            }
            Some((sub, _)) => {
                out.legacy.push(format!("{program} {full}"));
                cur = sub;
                path = full;
            }
            None if rules.legacy_extra.contains(&full.as_str()) => {
                out.legacy.push(format!("{program} {full}"));
                break;
            }
            None => {
                let known: Vec<&str> = cur
                    .get_subcommands()
                    .filter(|s| !s.is_hide_set())
                    .map(Command::get_name)
                    .collect();
                out.unknown = Some((
                    format!("{program} {full}"),
                    format!(
                        "`{token}` is not a subcommand of `{}`. Known: {}",
                        if path.is_empty() {
                            program.to_string()
                        } else {
                            format!("{program} {path}")
                        },
                        known.join(", ")
                    ),
                ));
                break;
            }
        }
    }
    out
}

/// Check the command words of one shell line against the tree.
fn check_line(
    file: &str,
    line_no: usize,
    shell: &str,
    legacy_ok: bool,
    rules: &Rules,
    report: &mut Report,
) {
    for seg in segments(strip_comment(shell)) {
        let w = words(seg);
        let Some((first, rest)) = w.split_first() else {
            continue;
        };
        let (start, program) = match *first {
            f if f == rules.root.get_name() || f == "pb" => {
                (Some(rules.root), rules.root.get_name())
            }
            "pbtop" => (
                rules
                    .root
                    .get_subcommands()
                    .find(|s| s.get_name() == "usage"),
                "pbtop",
            ),
            _ => continue,
        };
        let Some(start) = start else { continue };
        let result = walk(start, program, rest, rules);
        if let Some((cmd, why)) = result.unknown {
            report
                .errors
                .push(format!("{file}:{line_no}: unknown command `{cmd}`: {why}"));
        }
        for name in result.legacy {
            let msg = format!("{file}:{line_no}: `{name}` is a legacy name. Use the primary name, or mark the line with `<!-- names: legacy -->`.");
            if legacy_ok {
                continue;
            }
            if rules.alias_is_error {
                report.errors.push(msg);
            } else {
                report.warnings.push(msg);
            }
        }
    }
}

/// The text of each inline code span on a line.
fn inline_code(line: &str) -> Vec<&str> {
    let mut spans = Vec::new();
    let mut rest = line;
    while let Some(i) = rest.find('`') {
        let after = &rest[i + 1..];
        match after.find('`') {
            Some(j) => {
                spans.push(&after[..j]);
                rest = &after[j + 1..];
            }
            None => break,
        }
    }
    spans
}

fn fence_lang(line: &str) -> Option<String> {
    let t = line.trim_start();
    let rest = t.strip_prefix("```").or_else(|| t.strip_prefix("~~~"))?;
    Some(rest.trim().to_lowercase())
}

const MARKER: &str = "<!-- names: legacy -->";

/// Check one page.
fn check_page(file: &str, text: &str, rules: &Rules) -> Report {
    let mut report = Report::default();
    let slash = Regex::new(r"(^|[^A-Za-z0-9_./-])/playbook:([a-z][a-z0-9-]*)").expect("regex");
    let pbcode = Regex::new(r"\bpbcode\b").expect("regex");
    let lines: Vec<&str> = text.lines().collect();
    let mut fence: Option<String> = None;
    let mut legacy_next = false; // the marker covers the next line or block
    let mut legacy_block = false; // inside a block the marker covers
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let no = i + 1;
        if line.trim() == MARKER {
            legacy_next = true;
            i += 1;
            continue;
        }
        if let Some(lang) = fence_lang(line) {
            if fence.is_some() {
                fence = None;
                legacy_block = false;
            } else {
                fence = Some(lang);
                legacy_block = legacy_next;
                legacy_next = false;
            }
            i += 1;
            continue;
        }
        let legacy_ok = legacy_block || legacy_next;
        if !line.trim().is_empty() {
            legacy_next = false;
        }
        if pbcode.is_match(line) {
            report.errors.push(format!(
                "{file}:{no}: `pbcode` is not a command. Use `playbook code`."
            ));
        }
        for cap in slash.captures_iter(line) {
            let name = &cap[2];
            if !(rules.slash_exists)(name) && !SLASH_EXEMPT.contains(&file) {
                report.errors.push(format!(
                    "{file}:{no}: unknown slash command `/playbook:{name}`. It matches no file in plugins/playbook/commands and no folder in plugins/playbook/skills."
                ));
            }
        }
        match fence.as_deref() {
            Some("bash" | "sh" | "console" | "shell") => {
                // Join lines that end in a backslash.
                let mut joined = line.trim_end().to_string();
                while joined.ends_with('\\') && i + 1 < lines.len() {
                    joined.pop();
                    i += 1;
                    joined.push(' ');
                    joined.push_str(lines[i].trim());
                }
                check_line(file, no, &joined, legacy_ok, rules, &mut report);
            }
            Some(_) => {}
            None => {
                for span in inline_code(line) {
                    check_line(file, no, span, legacy_ok, rules, &mut report);
                }
            }
        }
        i += 1;
    }
    report
}

fn files_in_scope() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in SCOPE {
        match entry.strip_suffix("/*.md") {
            Some(dir) => {
                if let Ok(rd) = fs::read_dir(root().join(dir)) {
                    let mut found: Vec<PathBuf> = rd
                        .filter_map(Result::ok)
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|x| x == "md"))
                        .collect();
                    found.sort();
                    files.extend(found);
                }
            }
            None => files.push(root().join(entry)),
        }
    }
    files.retain(|f| {
        let rel = relative(f);
        !SKIP.contains(&rel.as_str())
    });
    files
}

fn relative(path: &Path) -> String {
    path.strip_prefix(root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn the_docs_name_only_real_commands() {
    let cli = Cli::command();
    let slash_names = plugin_names();
    let slash_exists = |n: &str| slash_names.contains(n);
    let renamed = legacy_paths(COMMAND_RENAMES);
    let legacy: Vec<&str> = LEGACY_EXTRA
        .iter()
        .chain(PENDING_NAMES)
        .chain(&renamed)
        .copied()
        .collect();
    let rules = Rules {
        root: &cli,
        alias_is_error: ALIAS_IS_ERROR,
        legacy_extra: &legacy,
        slash_exists: &slash_exists,
    };
    let mut report = Report::default();
    for file in files_in_scope() {
        let text = fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
        let page = check_page(&relative(&file), &text, &rules);
        report.errors.extend(page.errors);
        report.warnings.extend(page.warnings);
    }
    for w in &report.warnings {
        eprintln!("warning: {w}");
    }
    assert!(
        report.errors.is_empty(),
        "the docs name commands that do not exist:\n{}",
        report.errors.join("\n")
    );
}

#[cfg(test)]
mod synthetic {
    use super::*;
    use clap::Command;

    /// `tool` with `code search`, `usage status`, an alias, a hidden command
    /// and a command with no subcommands.
    fn tool() -> Command {
        Command::new("tool")
            .subcommand(
                Command::new("code")
                    .subcommand(Command::new("search"))
                    .subcommand(Command::new("index")),
            )
            .subcommand(Command::new("usage").subcommand(Command::new("status")))
            .subcommand(Command::new("update").alias("upgrade"))
            .subcommand(Command::new("old").hide(true))
            .subcommand(Command::new("doctor"))
    }

    fn run(text: &str, alias_is_error: bool, extra: &[&str]) -> Report {
        let cli = tool();
        let slash = |n: &str| n == "plan" || n == "writing-style";
        let rules = Rules {
            root: &cli,
            alias_is_error,
            legacy_extra: extra,
            slash_exists: &slash,
        };
        check_page("page.md", text, &rules)
    }

    fn ok(text: &str) {
        let r = run(text, false, &[]);
        assert_eq!(r, Report::default(), "for {text:?}");
    }

    fn fails(text: &str, needle: &str) {
        let r = run(text, false, &[]);
        assert_eq!(r.errors.len(), 1, "for {text:?}: {r:?}");
        assert!(r.errors[0].contains(needle), "{}", r.errors[0]);
    }

    #[test]
    fn a_known_path_passes_and_an_unknown_subcommand_fails() {
        ok("```bash\ntool code search foo\n```\n");
        fails(
            "intro\n\n```bash\ntool nope\n```\n",
            "page.md:4: unknown command `tool nope`",
        );
        fails("`tool code nope`", "unknown command `tool code nope`");
    }

    #[test]
    fn an_alias_and_a_hidden_subcommand_warn_then_fail_when_strict() {
        for text in ["`tool upgrade`", "`tool old`"] {
            let lax = run(text, false, &[]);
            assert!(lax.errors.is_empty(), "{lax:?}");
            assert_eq!(lax.warnings.len(), 1, "{lax:?}");
            assert!(lax.warnings[0].starts_with("page.md:1:"));
            let strict = run(text, true, &[]);
            assert_eq!(strict.errors.len(), 1, "{strict:?}");
            assert!(strict.errors[0].contains("legacy name"));
        }
    }

    #[test]
    fn the_legacy_marker_allows_an_alias_in_both_modes() {
        let text = "<!-- names: legacy -->\nOlder versions call it `tool upgrade`.\n";
        for strict in [false, true] {
            assert_eq!(run(text, strict, &[]), Report::default());
        }
        let block = "<!-- names: legacy -->\n```bash\ntool upgrade\ntool old\n```\n";
        assert_eq!(run(block, true, &[]), Report::default());
    }

    #[test]
    fn the_renamed_page_is_exempt_from_the_slash_check() {
        // Arrange
        let cli = tool();
        let slash = |n: &str| n == "plan" || n == "writing-style";
        let rules = Rules {
            root: &cli,
            alias_is_error: false,
            legacy_extra: &[],
            slash_exists: &slash,
        };
        let gone = "`/playbook:nope` is now `/playbook:plan`.";
        let rows: [(&str, &str, &[&str]); 4] = [
            ("docs/renamed.md", gone, &[]),
            (
                "docs/guides/x.md",
                gone,
                &["docs/guides/x.md:1: unknown slash command `/playbook:nope`"],
            ),
            (
                "docs/renamed.md",
                "run `tool nope` for it",
                &["unknown command `tool nope`"],
            ),
            (
                "docs/renamed.md",
                "`/playbook:nope` was renamed; run `tool nope`.",
                &["unknown command `tool nope`"],
            ),
        ];
        let mut problems = Vec::new();

        // Act
        for (file, text, expected) in rows {
            let report = check_page(file, text, &rules);
            if expected.is_empty() && report != Report::default() {
                problems.push(format!(
                    "{file} {text:?}: want a clean report, got {report:?}"
                ));
            }
            for fragment in expected {
                if !report.errors.iter().any(|e| e.contains(fragment)) {
                    problems.push(format!(
                        "{file} {text:?}: no error holds {fragment:?}: {report:?}"
                    ));
                }
            }
            if file == "docs/renamed.md"
                && report
                    .errors
                    .iter()
                    .any(|e| e.contains("unknown slash command"))
            {
                problems.push(format!("{file} {text:?}: the exemption failed: {report:?}"));
            }
        }

        // Assert
        assert!(problems.is_empty(), "{problems:#?}");
    }

    #[test]
    fn the_marker_covers_only_the_next_line_or_block() {
        let text = "<!-- names: legacy -->\n`tool upgrade`\n`tool upgrade`\n";
        let r = run(text, true, &[]);
        assert_eq!(r.errors.len(), 1, "{r:?}");
        assert!(r.errors[0].starts_with("page.md:3:"));
    }

    #[test]
    fn an_unknown_name_after_the_marker_still_fails() {
        let text = "<!-- names: legacy -->\n`tool nope`\n";
        let r = run(text, false, &[]);
        assert_eq!(r.errors.len(), 1, "{r:?}");
    }

    #[test]
    fn a_name_in_the_extra_list_warns_fails_strict_and_passes_after_the_marker() {
        let extra = ["gone"];
        let lax = run("`tool gone`", false, &extra);
        assert!(lax.errors.is_empty() && lax.warnings.len() == 1, "{lax:?}");
        let strict = run("`tool gone`", true, &extra);
        assert_eq!(strict.errors.len(), 1, "{strict:?}");
        let marked = run("<!-- names: legacy -->\n`tool gone`\n", true, &extra);
        assert_eq!(marked, Report::default());
    }

    #[test]
    fn a_hidden_legacy_command_is_an_error_outside_the_marker_and_passes_after_it() {
        // Arrange
        let cli = Cli::command();
        let slash = |_: &str| true;
        let rules = |alias_is_error| Rules {
            root: &cli,
            alias_is_error,
            legacy_extra: &[],
            slash_exists: &slash,
        };
        let bare = "Run `playbook pr land` to merge.\n";
        let marked = "<!-- names: legacy -->\nRun `playbook pr land` to merge.\n";

        // Act
        let lax = check_page("p.md", bare, &rules(false));
        let strict = check_page("p.md", bare, &rules(true));
        let after_marker = check_page("p.md", marked, &rules(true));

        // Assert
        assert!(lax.errors.is_empty() && lax.warnings.len() == 1, "{lax:?}");
        assert_eq!(strict.errors.len(), 1, "{strict:?}");
        assert_eq!(after_marker, Report::default());
    }

    #[test]
    fn every_hidden_legacy_command_is_a_legacy_name_in_the_real_tree() {
        // Arrange
        let cli = Cli::command();
        let slash = |_: &str| true;
        let rules = Rules {
            root: &cli,
            alias_is_error: true,
            legacy_extra: &[],
            slash_exists: &slash,
        };
        let names = [
            "pr land",
            "pr ci-wait",
            "pr land-wait",
            "commit all",
            "review suggest",
        ];

        for name in names {
            // Act
            let report = check_page("p.md", &format!("`playbook {name}`"), &rules);

            // Assert
            assert_eq!(report.errors.len(), 1, "{name}: {report:?}");
        }
    }

    #[test]
    fn a_real_command_passes_even_when_the_extra_list_names_it() {
        let r = run("`tool doctor`", true, &["doctor"]);
        assert_eq!(r, Report::default());
    }

    #[test]
    fn a_placeholder_a_flag_and_a_leaf_command_end_the_walk() {
        ok("`tool <command> --help`");
        ok("`tool code search --limit 3 nope`");
        ok("`tool doctor check nothing`");
        ok("`tool --json nope`");
        ok("`tool code search \"a query\" nope`");
    }

    #[test]
    fn a_prompt_is_stripped_and_every_block_kind_is_read() {
        fails("```console\n$ tool nope\n```\n", "tool nope");
        fails("```sh\ntool nope\n```\n", "tool nope");
        fails("```bash\n$ tool nope\n```\n", "tool nope");
        fails("Run `tool nope` now.", "tool nope");
        ok("```text\ntool nope\n```\n");
    }

    #[test]
    fn a_trailing_backslash_joins_the_next_line() {
        fails("```bash\ntool code \\\n  nope\n```\n", "page.md:2:");
        ok("```bash\ntool code \\\n  search foo\n```\n");
    }

    #[test]
    fn env_assignments_pipes_and_comments_are_handled() {
        ok("```bash\nFOO=1 tool code search x | head\n```\n");
        fails("```bash\ncurl x | tool nope\n```\n", "tool nope");
        ok("```bash\ntool doctor # tool nope\n```\n");
        fails("```bash\ntool doctor && tool nope\n```\n", "tool nope");
    }

    #[test]
    fn pbtop_walks_the_usage_subtree() {
        ok("`pbtop status`");
        fails("`pbtop nope`", "unknown command `pbtop nope`");
        ok("`pbtop --summary --range month`");
        ok("`pbtop`");
    }

    #[test]
    fn pb_is_the_same_program() {
        fails("`pb nope`", "unknown command `tool nope`");
    }

    #[test]
    fn playbook_hook_passes_by_name() {
        let cli = Command::new("playbook").subcommand(Command::new("doctor"));
        let slash = |_: &str| true;
        let rules = Rules {
            root: &cli,
            alias_is_error: false,
            legacy_extra: &[],
            slash_exists: &slash,
        };
        let r = check_page("p.md", "`playbook hook pre-tool-use`", &rules);
        assert_eq!(r, Report::default());
    }

    #[test]
    fn slash_commands_must_match_a_command_or_a_skill() {
        ok("Run `/playbook:plan` first.");
        ok("The `/playbook:writing-style` skill.");
        fails(
            "Run `/playbook:nope`.",
            "unknown slash command `/playbook:nope`",
        );
        ok("See plugins/playbook/commands and a/playbook:nope.");
    }

    #[test]
    fn pbcode_fails_anywhere() {
        fails("Use pbcode to search.", "`pbcode` is not a command");
        fails("```text\npbcode\n```\n", "`pbcode` is not a command");
        ok("pbcodex is another word.");
    }

    #[test]
    fn pbdoc_and_pbsync_lines_are_skipped() {
        ok("```bash\npbdoc nope\npbsync nothing here\n```\n`pbsync nope`");
    }

    #[test]
    fn the_message_names_the_file_the_line_and_the_command() {
        let r = run("one\ntwo\n`tool code nope`\n", false, &[]);
        assert!(
            r.errors[0].starts_with("page.md:3: unknown command `tool code nope`"),
            "{}",
            r.errors[0]
        );
    }

    #[test]
    fn the_real_tree_knows_setup_and_the_hidden_update_alias() {
        let cli = Cli::command();
        assert!(cli.get_subcommands().any(|s| s.get_name() == "setup"));
        let upgrade = cli.get_subcommands().find(|s| s.get_name() == "upgrade");
        assert!(upgrade.is_some_and(|u| u.get_all_aliases().any(|a| a == "update")));
        assert!(plugin_names().contains("plan"));
    }

    #[test]
    fn an_old_path_in_the_rename_table_is_a_legacy_name() {
        // Arrange
        let table = [Rename {
            old: "code find",
            new: "code search",
            extra: &[],
        }];
        let legacy = legacy_paths(&table);
        let text = "Run `tool code find foo`.";

        // Act
        let lax = run(text, false, &legacy);
        let strict = run(text, true, &legacy);

        // Assert
        assert!(lax.errors.is_empty(), "{lax:?}");
        assert_eq!(lax.warnings.len(), 1, "{lax:?}");
        assert!(lax.warnings[0].contains("legacy name"), "{lax:?}");
        assert_eq!(strict.errors.len(), 1, "{strict:?}");
    }
}
