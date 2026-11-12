// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Lints the help text of every command and argument in the clap tree.
//!
//! The rules (H1 to H13) are pure functions on text, so each has a table test.
//! The live tree is checked against one allowlist file per rewrite area under
//! `tests/help_lint_allowlist/`. The allowlists only shrink: a stale entry
//! fails here, and `scripts/check-help-lint-shrink.sh` fails a PR that adds one.

use clap::{Arg, Command, CommandFactory, Parser};
use playbook::Cli;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

// RULES-BEGIN
/// Words allowed in the summary of a top-level command (H3).
const TOP_LEVEL_WORDS: (usize, usize) = (6, 12);
/// Words allowed in the summary of a nested command (H3).
const NESTED_WORDS: (usize, usize) = (3, 14);
/// Longest summary in characters (H4).
const MAX_SUMMARY_CHARS: usize = 70;
/// First words a summary must not open with (H5).
const FILLER_OPENERS: &[&str] = &["Helpers", "Checks", "Mode", "The", "A", "An"];
/// Characters allowed in the first sentence of argument help (H6).
const ARG_SENTENCE_CHARS: (usize, usize) = (2, 100);
/// Leading articles argument help must not start with (H7).
const ARTICLES: &[&str] = &["The", "A", "An"];
/// Text that repeats a default clap prints itself (H8).
const DEFAULT_MARKERS: &[&str] = &["(default", "default:"];
/// Visible arguments that make a leaf need an `Example:` line (H9).
const ARGS_NEEDING_EXAMPLE: usize = 3;
/// Words that name the unit of a numeric default (H11).
const UNIT_WORDS: &[&str] = &[
    "seconds",
    "results",
    "rows",
    "times",
    "calls",
    "dollars",
    "tokens",
    "lines",
    "bytes",
    "characters",
    "days",
];
/// A word or phrase that help must not use (H12).
struct Banned {
    text: &'static str,
    /// Match the exact case; otherwise any case.
    exact_case: bool,
}
/// Variants of the one word for each concept (H12).
const BANNED: &[Banned] = &[
    Banned {
        text: "folder",
        exact_case: false,
    },
    Banned {
        text: "folders",
        exact_case: false,
    },
    Banned {
        text: "pull request",
        exact_case: false,
    },
    Banned {
        text: "pull requests",
        exact_case: false,
    },
    Banned {
        text: "repository",
        exact_case: false,
    },
    Banned {
        text: "WU",
        exact_case: true,
    },
    Banned {
        text: "WUs",
        exact_case: true,
    },
    Banned {
        text: "segment",
        exact_case: true,
    },
    Banned {
        text: "segments",
        exact_case: true,
    },
    Banned {
        text: "dir",
        exact_case: false,
    },
];
// RULES-END

const EXAMPLE_PREFIX: &str = "Example:";
const ALLOWLIST_DIR: &str = "tests/help_lint_allowlist";
/// Each allowlist file with the top-level commands whose violations it holds.
const AREAS: &[(&str, &[&str])] = &[
    (
        "user-core.txt",
        &[
            "doctor",
            "upgrade",
            "uninstall",
            "migrate",
            "hooks",
            "deps",
            "version",
            "trust",
            "commands",
            "completions",
        ],
    ),
    ("setup.txt", &["setup", "init"]),
    (
        "launcher.txt",
        &["cc", "ccd", "usage", "sessions", "skills"],
    ),
    (
        "config.txt",
        &["config", "effort", "mode", "memory", "state"],
    ),
    ("delivery.txt", &["pr", "deliver", "commit"]),
    ("review.txt", &["review", "worktree", "handoff", "gate"]),
    (
        "planning.txt",
        &[
            "plan",
            "plans",
            "segment",
            "glossary",
            "adr",
            "learn",
            "atlassian",
            "prose",
        ],
    ),
    (
        "plumbing.txt",
        &["agents", "manifest", "settings", "sanitize", "ci", "code"],
    ),
    (
        "utilities.txt",
        &["route", "path", "doc", "tools", "eval", "statusline"],
    ),
];

// ---- text rules -----------------------------------------------------------

fn first_paragraph(text: &str) -> String {
    let lines: Vec<&str> = text
        .trim()
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .collect();
    lines.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ")
}

fn first_sentence(text: &str) -> String {
    let paragraph = first_paragraph(text);
    let sentence = paragraph.split(". ").next().unwrap_or_default();
    sentence.trim().trim_end_matches('.').to_string()
}

fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

fn first_word(text: &str) -> &str {
    text.split_whitespace().next().unwrap_or_default()
}

/// Rule ids broken by a command summary (H3 to H5).
fn summary_violations(summary: &str, top_level: bool) -> Vec<&'static str> {
    let (min, max) = if top_level {
        TOP_LEVEL_WORDS
    } else {
        NESTED_WORDS
    };
    let mut out = Vec::new();
    if !(min..=max).contains(&words(summary)) {
        out.push("H3");
    }
    if summary.chars().count() > MAX_SUMMARY_CHARS {
        out.push("H4");
    }
    if FILLER_OPENERS.contains(&first_word(summary)) {
        out.push("H5");
    }
    out
}

/// Rule ids broken by the first sentence of argument help (H6, H7).
fn arg_help_violations(help: &str) -> Vec<&'static str> {
    let sentence = first_sentence(help);
    let mut out = Vec::new();
    let (min, max) = ARG_SENTENCE_CHARS;
    if !(min..=max).contains(&sentence.chars().count()) {
        out.push("H6");
    }
    let opens_well = sentence
        .chars()
        .next()
        .is_some_and(|c| c.is_uppercase() || c == '`');
    if !opens_well || ARTICLES.contains(&first_word(&sentence)) {
        out.push("H7");
    }
    out
}

fn repeats_default(help: &str) -> bool {
    let lower = help.to_lowercase();
    DEFAULT_MARKERS.iter().any(|m| lower.contains(m))
}

fn has_example(text: &str) -> bool {
    text.lines()
        .any(|l| l.trim_start().starts_with(EXAMPLE_PREFIX))
}

/// The command of each `Example:` line: the first backtick span, else the rest.
fn example_commands(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim_start().strip_prefix(EXAMPLE_PREFIX))
        .map(|rest| match rest.split('`').nth(1) {
            Some(span) => span.trim().to_string(),
            None => rest.trim().to_string(),
        })
        .collect()
}

/// Splits on spaces. A single or double quoted run is one word and the quotes
/// are dropped. The line is cut at the first unquoted `>`. A backslash is an
/// ordinary character. An unterminated quote is an error, so H10 reports the
/// line instead of guessing.
fn split_command_line(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for ch in line.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '>') => break,
            (None, '"' | '\'') => {
                quote = Some(ch);
                in_word = true;
            }
            (None, ' ') => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            (None, c) => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err(format!("unterminated quote in `{line}`"));
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

fn example_parses(line: &str) -> Result<(), String> {
    let words = split_command_line(line)?;
    Cli::try_parse_from(words)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn has_unit_word(help: &str) -> bool {
    let lower = help.to_lowercase();
    lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| UNIT_WORDS.contains(&w))
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.parse::<f64>().is_ok()
}

/// Text with backtick spans and `Example:` lines removed.
fn prose_only(text: &str) -> String {
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with(EXAMPLE_PREFIX))
        .collect();
    kept.join("\n")
        .split('`')
        .enumerate()
        .filter(|(i, _)| i % 2 == 0)
        .map(|(_, s)| s)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Banned texts found as whole words (H12).
fn banned_hits(text: &str) -> Vec<&'static str> {
    let prose = prose_only(text);
    let lower = prose.to_lowercase();
    BANNED
        .iter()
        .filter(|b| {
            let (haystack, needle) = if b.exact_case {
                (prose.as_str(), b.text.to_string())
            } else {
                (lower.as_str(), b.text.to_lowercase())
            };
            haystack.match_indices(&needle).any(|(at, m)| {
                let before = haystack[..at].chars().next_back();
                let after = haystack[at + m.len()..].chars().next();
                !before.is_some_and(char::is_alphanumeric)
                    && !after.is_some_and(char::is_alphanumeric)
            })
        })
        .map(|b| b.text)
        .collect()
}

// ---- walker ---------------------------------------------------------------

struct ArgNode {
    label: String,
    help: String,
    long_help: String,
    defaults: Vec<String>,
    takes_value: bool,
    required: bool,
    hidden: bool,
}

struct Node {
    path: String,
    depth: usize,
    /// Hidden itself or under a hidden command.
    hidden: bool,
    leaf: bool,
    about: String,
    long_about: String,
    args: Vec<ArgNode>,
}

impl Node {
    fn text(&self) -> &str {
        if self.long_about.is_empty() {
            &self.about
        } else {
            &self.long_about
        }
    }
}

fn arg_node(arg: &Arg) -> ArgNode {
    let label = match (arg.get_long(), arg.get_short()) {
        (Some(long), _) => format!("--{long}"),
        (None, Some(short)) => format!("-{short}"),
        (None, None) => format!("<{}>", arg.get_id()),
    };
    ArgNode {
        label,
        help: arg.get_help().map(ToString::to_string).unwrap_or_default(),
        long_help: arg
            .get_long_help()
            .map(ToString::to_string)
            .unwrap_or_default(),
        defaults: arg
            .get_default_values()
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect(),
        takes_value: arg.get_action().takes_values(),
        required: arg.is_required_set(),
        hidden: arg.is_hide_set(),
    }
}

/// Arguments clap adds to every command itself.
const AUTO_ARGS: &[&str] = &["help", "version"];

struct Walk {
    nodes: Vec<Node>,
    /// Ids of global arguments already reported, so a copy clap propagates to
    /// a subcommand is not reported again.
    globals: BTreeSet<String>,
}

fn visit(cmd: &Command, path: &str, depth: usize, parent_hidden: bool, walk: &mut Walk) {
    let subs: Vec<&Command> = cmd
        .get_subcommands()
        .filter(|s| s.get_name() != "help")
        .collect();
    let hidden = parent_hidden || cmd.is_hide_set();
    let args = cmd
        .get_arguments()
        .filter(|a| !AUTO_ARGS.contains(&a.get_id().as_str()))
        .filter(|a| !a.is_global_set() || walk.globals.insert(a.get_id().to_string()))
        .map(arg_node)
        .collect();
    walk.nodes.push(Node {
        path: path.to_string(),
        depth,
        hidden,
        leaf: subs.is_empty(),
        about: cmd.get_about().map(ToString::to_string).unwrap_or_default(),
        long_about: cmd
            .get_long_about()
            .map(ToString::to_string)
            .unwrap_or_default(),
        args,
    });
    for sub in subs {
        let sub_path = if depth == 0 {
            sub.get_name().to_string()
        } else {
            format!("{path} {}", sub.get_name())
        };
        visit(sub, &sub_path, depth + 1, hidden, walk);
    }
}

/// Every command of the built tree, root first, parents before children, so a
/// global argument is reported once, on the command that declares it.
fn commands(root: &Command) -> Vec<Node> {
    let mut walk = Walk {
        nodes: Vec::new(),
        globals: BTreeSet::new(),
    };
    visit(root, root.get_name(), 0, false, &mut walk);
    walk.nodes
}

/// The tree with defaults and propagated arguments filled in.
fn live_tree() -> Command {
    let mut cmd = Cli::command();
    cmd.build();
    cmd
}

// ---- violations -----------------------------------------------------------

fn node_violations(node: &Node, out: &mut BTreeSet<String>) {
    let mut add = |rule: &str, label: Option<&str>| {
        out.insert(match label {
            Some(l) => format!("{rule} {} {l}", node.path),
            None => format!("{rule} {}", node.path),
        });
    };
    if node.about.trim().is_empty() {
        add("H1", None);
    }
    for arg in &node.args {
        if arg.help.trim().is_empty() && arg.long_help.trim().is_empty() {
            add("H2", Some(&arg.label));
        }
    }
    for line in example_commands(node.text()) {
        if example_parses(&line).is_err() {
            add("H10", None);
        }
    }
    if node.hidden {
        return;
    }
    if node.depth > 0 {
        let summary = first_sentence(&node.about);
        for rule in summary_violations(&summary, node.depth == 1) {
            add(rule, None);
        }
        if !node.about.trim().is_empty()
            && !node.about.contains('\n')
            && node.about.trim_end().ends_with('.')
        {
            add("H13", None);
        }
        let shown = node.args.iter().filter(|a| !a.hidden).count();
        let needs =
            shown >= ARGS_NEEDING_EXAMPLE || node.args.iter().any(|a| a.required && !a.hidden);
        if node.leaf && needs && !has_example(node.text()) {
            add("H9", None);
        }
    }
    if node.depth > 0 && !banned_hits(&first_paragraph(&node.about)).is_empty() {
        add("H12", None);
    }
    for arg in node.args.iter().filter(|a| !a.hidden) {
        let help = if arg.help.is_empty() {
            &arg.long_help
        } else {
            &arg.help
        };
        if help.trim().is_empty() {
            continue;
        }
        for rule in arg_help_violations(help) {
            add(rule, Some(&arg.label));
        }
        if !arg.defaults.is_empty()
            && (repeats_default(&arg.help) || repeats_default(&arg.long_help))
        {
            add("H8", Some(&arg.label));
        }
        let numeric = arg.takes_value && arg.defaults.iter().any(|d| is_number(d));
        if numeric && !has_unit_word(help) {
            add("H11", Some(&arg.label));
        }
        if !arg.help.contains('\n') && arg.help.trim_end().ends_with('.') {
            add("H13", Some(&arg.label));
        }
        if !banned_hits(&first_paragraph(help)).is_empty() {
            add("H12", Some(&arg.label));
        }
    }
}

fn violations(root: &Command) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for node in commands(root) {
        node_violations(&node, &mut out);
    }
    out
}

fn rule_of(entry: &str) -> &str {
    first_word(entry)
}

fn rule_ids(entries: &BTreeSet<String>) -> BTreeSet<&str> {
    entries.iter().map(|e| rule_of(e)).collect()
}

// ---- allowlists -----------------------------------------------------------

fn top_level_of(entry: &str) -> Option<&str> {
    entry.split_whitespace().nth(1)
}

fn area_file_for(entry: &str) -> Option<&'static str> {
    let top = top_level_of(entry)?;
    AREAS
        .iter()
        .find(|(_, tops)| tops.contains(&top))
        .map(|(file, _)| *file)
}

fn parse_allowlist(contents: &str) -> Vec<String> {
    contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// The violations grouped by rule, then by the file that would hold them.
fn report(live: &BTreeSet<String>) -> String {
    let mut by_file: BTreeMap<&str, Vec<&String>> = BTreeMap::new();
    for entry in live {
        by_file
            .entry(area_file_for(entry).unwrap_or("(no area)"))
            .or_default()
            .push(entry);
    }
    let mut out = String::new();
    for rule in rule_ids(live) {
        out.push_str(&format!("{rule}\n"));
        for entry in live.iter().filter(|e| rule_of(e) == rule) {
            out.push_str(&format!("  {entry}\n"));
        }
    }
    for (file, entries) in by_file {
        out.push_str(&format!("=== {file}\n"));
        for entry in entries {
            out.push_str(&format!("{entry}\n"));
        }
    }
    out
}

/// Checks the allowlist files against the live violations. `files` is
/// (file name, contents).
fn check_allowlists(live: &BTreeSet<String>, files: &[(String, String)]) -> Result<(), String> {
    let mut listed = BTreeSet::new();
    let mut problems = Vec::new();
    for (name, contents) in files {
        for entry in parse_allowlist(contents) {
            if area_file_for(&entry) != Some(name.as_str()) {
                problems.push(format!("{name}: `{entry}` belongs to another area file"));
            }
            if !live.contains(&entry) {
                problems.push(format!(
                    "{name}: stale entry `{entry}` no longer violates its rule, delete it"
                ));
            }
            if !listed.insert(entry.clone()) {
                problems.push(format!("{name}: `{entry}` is listed twice"));
            }
        }
    }
    let unlisted: BTreeSet<String> = live.difference(&listed).cloned().collect();
    if !unlisted.is_empty() {
        problems.push(format!(
            "violations not in any allowlist, grouped by rule:\n{}",
            report(&unlisted)
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

fn check_file_set(found: &[String]) -> Result<(), String> {
    let expected: BTreeSet<&str> = AREAS.iter().map(|(file, _)| *file).collect();
    let found: BTreeSet<&str> = found.iter().map(String::as_str).collect();
    if expected == found {
        Ok(())
    } else {
        Err(format!(
            "allowlist files must be exactly {expected:?}, found {found:?}"
        ))
    }
}

fn allowlist_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(ALLOWLIST_DIR)
}

fn read_allowlists() -> Vec<(String, String)> {
    AREAS
        .iter()
        .map(|(file, _)| {
            let text = std::fs::read_to_string(allowlist_dir().join(file)).unwrap_or_default();
            ((*file).to_string(), text)
        })
        .collect()
}

// ---- tests on the live tree ----------------------------------------------

#[test]
fn the_violations_equal_the_allowlists() {
    let live = violations(&live_tree());
    if let Err(message) = check_allowlists(&live, &read_allowlists()) {
        panic!("{message}");
    }
}

#[test]
fn the_allowlist_files_are_the_fixed_set() {
    let found: Vec<String> = std::fs::read_dir(allowlist_dir())
        .map(|dir| {
            dir.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    if let Err(message) = check_file_set(&found) {
        panic!("{message}");
    }
}

#[test]
fn a_new_command_gets_no_allowlist() {
    let cmd = live_tree().subcommand(Command::new("brand-new").about("Helpers for the thing"));
    let live = violations(&cmd);
    assert!(live.contains("H5 brand-new"));
    assert_eq!(area_file_for("H5 brand-new"), None);
    assert!(check_allowlists(&live, &read_allowlists()).is_err());
}

#[test]
fn walker_visits_every_subcommand() {
    fn count(cmd: &Command) -> usize {
        1 + cmd
            .get_subcommands()
            .filter(|s| s.get_name() != "help")
            .map(count)
            .sum::<usize>()
    }
    let root = live_tree();
    let nodes = commands(&root);
    assert_eq!(nodes.len(), count(&root));
    let json = playbook::command_index::render_json(&playbook::command_index::tree(), &[]).unwrap();
    let rows = serde_json::from_str::<Vec<serde_json::Value>>(&json)
        .unwrap()
        .len();
    let shown = nodes.iter().filter(|n| n.depth > 0 && !n.hidden).count();
    assert_eq!(
        shown, rows,
        "the walker and `playbook commands --json` disagree on visible commands"
    );
}

#[test]
fn walker_visits_every_argument() {
    fn collect(cmd: &Command, ids: &mut BTreeSet<String>, owned: &mut usize) {
        for arg in cmd
            .get_arguments()
            .filter(|a| !AUTO_ARGS.contains(&a.get_id().as_str()))
        {
            if !arg.is_global_set() || ids.insert(arg.get_id().to_string()) {
                *owned += 1;
            }
        }
        for sub in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
            collect(sub, ids, owned);
        }
    }
    let root = live_tree();
    let nodes = commands(&root);
    let mut owned = 0;
    collect(&root, &mut BTreeSet::new(), &mut owned);
    assert_eq!(nodes.iter().map(|n| n.args.len()).sum::<usize>(), owned);
    let update = nodes.iter().find(|n| n.path == "upgrade").unwrap();
    assert!(update.args.iter().any(|a| a.label == "--check"));
}

// ---- rule tests -----------------------------------------------------------

fn node_with(about: &str, args: Vec<ArgNode>, leaf: bool) -> Node {
    Node {
        path: "x".into(),
        depth: 2,
        hidden: false,
        leaf,
        about: about.into(),
        long_about: String::new(),
        args,
    }
}

fn arg_with(label: &str, help: &str) -> ArgNode {
    ArgNode {
        label: label.into(),
        help: help.into(),
        long_help: String::new(),
        defaults: vec![],
        takes_value: true,
        required: false,
        hidden: false,
    }
}

fn rules_of(node: &Node) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    node_violations(node, &mut out);
    out
}

#[test]
fn every_command_has_an_about() {
    let node = node_with("", vec![], true);
    assert!(rules_of(&node).contains("H1 x"));
}

#[test]
fn every_argument_has_help() {
    let node = node_with("Print the value", vec![arg_with("--n", "")], true);
    assert!(rules_of(&node).contains("H2 x --n"));
}

#[test]
fn a_summary_has_the_purpose_word_count() {
    let twelve = "a b c d e f g h i j k l";
    let fourteen = format!("{twelve} m n");
    assert!(summary_violations("Print it", false).contains(&"H3"));
    assert!(summary_violations(&fourteen, false).is_empty());
    assert!(summary_violations(&format!("{fourteen} o"), false).contains(&"H3"));
    assert!(summary_violations(twelve, true).is_empty());
    assert!(summary_violations(&fourteen, true).contains(&"H3"));
}

#[test]
fn a_summary_is_at_most_70_characters() {
    let seventy = format!("Print {}", "x".repeat(MAX_SUMMARY_CHARS - "Print ".len()));
    assert_eq!(seventy.chars().count(), MAX_SUMMARY_CHARS);
    assert!(!summary_violations(&seventy, false).contains(&"H4"));
    assert!(summary_violations(&format!("{seventy}z"), false).contains(&"H4"));
    let twelve_long = "aaaaa bbbbb ccccc ddddd eeeee fffff ggggg hhhhh iiiii jjjjj kkkkk lllll";
    assert_eq!(twelve_long.chars().count(), MAX_SUMMARY_CHARS + 1);
    assert_eq!(words(twelve_long), TOP_LEVEL_WORDS.1);
    assert_eq!(summary_violations(twelve_long, true), vec!["H4"]);
}

#[test]
fn a_summary_does_not_open_with_filler() {
    for bad in [
        "Helpers for doctor",
        "Checks for the plan",
        "Mode of the cc launcher",
        "The git steps",
    ] {
        assert!(summary_violations(bad, false).contains(&"H5"), "{bad}");
    }
    assert!(summary_violations("Check the plan", false).is_empty());
    assert!(summary_violations("Print the value", false).is_empty());
    // A real imperative check is a review item, so this passes.
    assert!(!summary_violations("Settings for doctor", false).contains(&"H5"));
}

#[test]
fn argument_help_is_two_to_100_characters() {
    let hundred = "x".repeat(ARG_SENTENCE_CHARS.1).replacen('x', "X", 1);
    assert!(!arg_help_violations(&hundred).contains(&"H6"));
    assert!(arg_help_violations(&format!("{hundred}x")).contains(&"H6"));
    assert!(arg_help_violations("X").contains(&"H6"));
    assert!(!arg_help_violations("Xy").contains(&"H6"));
    // Only the first sentence counts.
    assert!(!arg_help_violations(&format!("Short one. {hundred}{hundred}")).contains(&"H6"));
}

#[test]
fn argument_help_starts_with_a_capital_and_no_article() {
    assert!(arg_help_violations("PR number").is_empty());
    assert!(arg_help_violations("Seconds between reads").is_empty());
    assert!(arg_help_violations("`--x` or the default").is_empty());
    assert!(arg_help_violations("The plan slug").contains(&"H7"));
    assert!(arg_help_violations("auto, low or high").contains(&"H7"));
    assert!(arg_help_violations(&"y".repeat(101)).contains(&"H6"));
}

#[test]
fn help_does_not_repeat_a_printed_default() {
    assert!(repeats_default("Most results (default 20)"));
    assert!(repeats_default("Most results, default: 20"));
    assert!(!repeats_default("Most results to show"));
    let mut arg = arg_with("--n", "Most results (default 20)");
    arg.defaults = vec!["20".into()];
    let node = node_with("Print the value", vec![arg], true);
    assert!(rules_of(&node).contains("H8 x --n"));
    let mut plain = arg_with("--n", "Most results to show");
    plain.defaults = vec!["20".into()];
    assert!(!rules_of(&node_with("Print the value", vec![plain], true)).contains("H8 x --n"));
}

#[test]
fn a_non_obvious_command_has_an_example() {
    let three = (0..ARGS_NEEDING_EXAMPLE)
        .map(|i| arg_with(&format!("--a{i}"), "Some value"))
        .collect();
    assert!(rules_of(&node_with("Print the value", three, true)).contains("H9 x"));
    let one = vec![arg_with("--a", "Some value")];
    assert!(!rules_of(&node_with("Print the value", one, true)).contains("H9 x"));
    let mut required = arg_with("<id>", "Some value");
    required.required = true;
    assert!(rules_of(&node_with("Print the value", vec![required], true)).contains("H9 x"));
    let with_example = "Print the value\n\nExample: `playbook version`";
    let three = (0..ARGS_NEEDING_EXAMPLE)
        .map(|i| arg_with(&format!("--a{i}"), "Some value"))
        .collect();
    assert!(!rules_of(&node_with(with_example, three, true)).contains("H9 x"));
}

#[test]
fn every_example_parses() {
    assert!(example_parses("playbook config export > out.json").is_ok());
    assert!(example_parses("playbook nope").is_err());
    assert_eq!(
        example_commands("Text\nExample: `playbook version` now"),
        vec!["playbook version"]
    );
    let node = node_with("Print the value\n\nExample: `playbook nope`", vec![], true);
    assert!(rules_of(&node).contains("H10 x"));
}

#[test]
fn a_numeric_flag_names_its_unit() {
    let mut bare = arg_with("--n", "Number to show");
    bare.defaults = vec!["20".into()];
    assert!(rules_of(&node_with("Print the value", vec![bare], true)).contains("H11 x --n"));
    let mut unit = arg_with("--n", "Most results to show");
    unit.defaults = vec!["20".into()];
    assert!(!rules_of(&node_with("Print the value", vec![unit], true)).contains("H11 x --n"));
}

#[test]
fn no_banned_variant_word_appears() {
    assert!(banned_hits("Directory to scan").is_empty());
    assert_eq!(banned_hits("Folder to scan"), vec!["folder"]);
    assert!(banned_hits("Name of the `segment` command").is_empty());
    assert!(banned_hits("Segment number").is_empty());
    assert_eq!(banned_hits("the segment number"), vec!["segment"]);
    assert!(banned_hits("Print the thing\nExample: `playbook segment dir`").is_empty());
    assert_eq!(banned_hits("Open a pull request"), vec!["pull request"]);
    assert_eq!(banned_hits("Split each WU"), vec!["WU"]);
    assert!(banned_hits("Use the wu key").is_empty());
}

#[test]
fn a_one_line_help_has_no_final_period() {
    let node = node_with(
        "Print the value",
        vec![arg_with("--n", "Print the value.")],
        true,
    );
    assert!(rules_of(&node).contains("H13 x --n"));
    let multi = node_with(
        "Print the value",
        vec![arg_with("--n", "Print the value.\nMore.")],
        true,
    );
    assert!(!rules_of(&multi).contains("H13 x --n"));
}

// ---- splitter -------------------------------------------------------------

fn split(line: &str) -> Vec<String> {
    split_command_line(line).unwrap()
}

#[test]
fn a_double_quoted_run_is_one_word() {
    assert_eq!(
        split(r#"playbook code context "add retry support""#),
        ["playbook", "code", "context", "add retry support"]
    );
}

#[test]
fn a_single_quoted_run_is_one_word() {
    assert_eq!(
        split("playbook x --verify 'cargo test {tests}'"),
        ["playbook", "x", "--verify", "cargo test {tests}"]
    );
}

#[test]
fn a_quoted_flag_stays_inside_its_word() {
    assert_eq!(
        split(r##"playbook review prepare deep "#4265 --self""##),
        ["playbook", "review", "prepare", "deep", "#4265 --self"]
    );
}

#[test]
fn an_unquoted_redirect_cuts_the_line() {
    assert_eq!(
        split("playbook config export > out.json"),
        ["playbook", "config", "export"]
    );
    assert_eq!(split("playbook x 2>&1"), ["playbook", "x", "2"]);
}

#[test]
fn a_quoted_redirect_is_kept() {
    assert_eq!(split(r#"playbook x "a > b""#), ["playbook", "x", "a > b"]);
}

#[test]
fn an_unterminated_quote_is_an_error() {
    assert!(split_command_line(r#"playbook x "oops"#).is_err());
}

#[test]
fn a_backslash_and_a_pipe_are_plain() {
    assert_eq!(split(r"playbook a\b"), ["playbook", r"a\b"]);
    assert_eq!(split("playbook x | head"), ["playbook", "x", "|", "head"]);
}

// ---- injected fixtures ----------------------------------------------------

fn fixture_violations(cmd: Command) -> BTreeSet<String> {
    violations(&Command::new("playbook").subcommand(cmd))
}

#[test]
fn the_lint_fails_on_an_injected_bad_command() {
    let a = Command::new("x")
        .about("Helpers for the thing")
        .arg(Arg::new("quiet").long("quiet"));
    let found = fixture_violations(a);
    for rule in ["H2", "H3", "H5"] {
        assert!(
            rule_ids(&found).contains(rule),
            "A: {rule} missing in {found:?}"
        );
    }

    let long = "Print a very long summary that goes on and on past the limit of seventy chars";
    let b = Command::new("y")
        .about(long)
        .arg(Arg::new("a").long("a").help("x"))
        .arg(Arg::new("b").long("b").help("the thing"))
        .arg(
            Arg::new("c")
                .long("c")
                .default_value("20")
                .help("Most results (default 20)"),
        )
        .arg(Arg::new("d").long("d").help("Folder to scan"));
    let found = fixture_violations(b);
    for rule in ["H4", "H6", "H7", "H8", "H9", "H12"] {
        assert!(
            rule_ids(&found).contains(rule),
            "B: {rule} missing in {found:?}"
        );
    }

    let c = Command::new("z")
        .about("Print the value\n\nExample: `playbook nope`")
        .arg(
            Arg::new("n")
                .long("n")
                .default_value("20")
                .help("Number to show"),
        )
        .arg(Arg::new("p").long("p").help("Print the value."));
    let found = fixture_violations(c);
    for rule in ["H10", "H11", "H13"] {
        assert!(
            rule_ids(&found).contains(rule),
            "C: {rule} missing in {found:?}"
        );
    }
}

// ---- allowlist checks on literal text -------------------------------------

fn set(entries: &[&str]) -> BTreeSet<String> {
    entries.iter().map(ToString::to_string).collect()
}

fn file(name: &str, text: &str) -> (String, String) {
    (name.into(), text.into())
}

#[test]
fn a_stale_allowlist_entry_fails() {
    let live = set(&["H3 pr"]);
    let err = check_allowlists(&live, &[file("delivery.txt", "H3 pr\nH4 pr\n")]).unwrap_err();
    assert!(err.contains("stale entry `H4 pr`"), "{err}");
    assert!(check_allowlists(&live, &[file("delivery.txt", "H3 pr\n")]).is_ok());
}

#[test]
fn an_unlisted_violation_fails_grouped_by_rule() {
    let live = set(&["H3 pr", "H4 pr", "H4 plan"]);
    let err = check_allowlists(&live, &[]).unwrap_err();
    assert!(err.contains("H4\n  H4 plan\n  H4 pr\n"), "{err}");
    assert!(err.contains("=== planning.txt\nH4 plan\n"), "{err}");
}

#[test]
fn an_entry_in_the_wrong_area_file_fails() {
    let err = check_allowlists(&set(&["H3 pr"]), &[file("planning.txt", "H3 pr\n")]).unwrap_err();
    assert!(err.contains("belongs to another area file"), "{err}");
}

#[test]
fn a_file_outside_the_fixed_set_fails() {
    let mut names: Vec<String> = AREAS.iter().map(|(f, _)| (*f).to_string()).collect();
    assert!(check_file_set(&names).is_ok());
    names.push("extra.txt".into());
    assert!(check_file_set(&names).is_err());
    names.pop();
    names.pop();
    assert!(check_file_set(&names).is_err());
}

// ---- style guide ----------------------------------------------------------

const STYLE_GUIDE: &str = "docs/reference/cli-help-style.md";

/// Banned texts the page does not list in backticks (H12).
fn unlisted_variants(page: &str) -> Vec<&'static str> {
    BANNED
        .iter()
        .filter(|b| !page.contains(&format!("`{}`", b.text)))
        .map(|b| b.text)
        .collect()
}

#[test]
fn the_style_guide_lists_every_banned_variant() {
    // Arrange
    let path = PathBuf::from(env!("PLAYBOOK_REPO_ROOT")).join(STYLE_GUIDE);
    let page = std::fs::read_to_string(&path).unwrap_or_default();
    // Act
    let missing = unlisted_variants(&page);
    // Assert
    assert!(
        missing.is_empty(),
        "{STYLE_GUIDE} must list these banned variants in backticks: {missing:?}"
    );
}

#[test]
fn a_variant_missing_from_the_page_is_reported() {
    // Arrange
    let page = BANNED
        .iter()
        .filter(|b| b.text != "WUs")
        .map(|b| format!("`{}`", b.text))
        .collect::<Vec<_>>()
        .join(" ");
    // Act
    let missing = unlisted_variants(&page);
    // Assert
    assert_eq!(missing, vec!["WUs"]);
}
