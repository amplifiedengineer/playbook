// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The CLI and config reference page is generated from the real clap
//! definitions and the config key table, and committed. This test rebuilds it
//! and fails when the committed copy differs, so the page cannot drift.
//!
//! Regenerate with: `UPDATE_REFERENCE=1 cargo test -p playbook --test reference_docs`

use clap::{Command, CommandFactory};
use playbook::command_index::{visible, visible_args};
use playbook::config::keys::{
    allowed_enum_values, default_value, EFFORT_COMPONENT_KINDS, KNOWN_KEYS,
};
use playbook::Cli;
use serde_json::Value;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

const PAGE: &str = "docs/reference/cli-and-config.md";
const REGEN: &str = "UPDATE_REFERENCE=1 cargo test -p playbook --test reference_docs";
const CONFIG_KEYS: &str = "docs/reference/config-keys.md";

fn root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT"))
}

fn arg_line(arg: &clap::Arg) -> String {
    let help = arg
        .get_help()
        .map(|h| h.to_string().replace('\n', " "))
        .unwrap_or_default();
    let name = if arg.is_positional() {
        let value = arg
            .get_value_names()
            .and_then(|v| v.first().map(|n| n.to_string()))
            .unwrap_or_else(|| arg.get_id().to_string().to_uppercase());
        format!("<{value}>")
    } else {
        let mut parts = Vec::new();
        if let Some(short) = arg.get_short() {
            parts.push(format!("-{short}"));
        }
        if let Some(long) = arg.get_long() {
            parts.push(format!("--{long}"));
        }
        let mut label = parts.join(", ");
        if arg.get_action().takes_values() {
            let value = arg
                .get_value_names()
                .and_then(|v| v.first().map(|n| n.to_string()))
                .unwrap_or_else(|| arg.get_id().to_string().to_uppercase());
            label.push_str(&format!(" <{value}>"));
        }
        label
    };
    let possible: Vec<String> = arg
        .get_possible_values()
        .iter()
        .filter(|v| !v.is_hide_set())
        .map(|v| format!("`{}`", v.get_name()))
        .collect();
    let mut line = format!("- `{name}`");
    if !help.is_empty() {
        line.push_str(&format!(": {help}"));
    }
    if !possible.is_empty() {
        line.push_str(&format!(" (one of {})", possible.join(", ")));
    }
    line
}

fn walk(cmd: &Command, path: &str, out: &mut String) {
    let _ = writeln!(out, "### `{path}`\n");
    if let Some(about) = cmd.get_long_about().or_else(|| cmd.get_about()) {
        let text = about.to_string();
        let _ = writeln!(out, "{}\n", text.trim());
    }
    let args: Vec<&clap::Arg> = visible_args(cmd).collect();
    for arg in args {
        let _ = writeln!(out, "{}", arg_line(arg));
    }
    let mut subs: Vec<&Command> = visible(cmd).collect();
    subs.sort_by_key(|s| s.get_name().to_string());
    if visible_args(cmd).next().is_some() {
        out.push('\n');
    }
    if !subs.is_empty() {
        let names: Vec<String> = subs.iter().map(|s| format!("`{}`", s.get_name())).collect();
        let _ = writeln!(out, "Subcommands: {}\n", names.join(", "));
    }
    for sub in subs {
        walk(sub, &format!("{path} {}", sub.get_name()), out);
    }
}

fn show(value: &Value) -> String {
    match value {
        Value::String(s) if s.is_empty() => "empty".to_string(),
        Value::String(s) => format!("`{s}`"),
        other => format!("`{other}`"),
    }
}

fn config_table() -> String {
    let mut out = String::from("| Key | Default | Allowed values |\n| :- | :- | :- |\n");
    for key in KNOWN_KEYS {
        let default = default_value(key).map_or_else(|| "none".to_string(), |v| show(&v));
        let allowed = allowed_enum_values(key).map_or_else(
            || match default_value(key) {
                Some(Value::Bool(_)) => "`true`, `false`".to_string(),
                Some(Value::Number(_)) => {
                    "a number, see the config keys page for the range".to_string()
                }
                _ if key.starts_with("models.") => {
                    "empty, or `claude-<tier>-<major>[-<minor>]` for that tier".to_string()
                }
                _ => "see the config keys page".to_string(),
            },
            |vals| {
                vals.iter()
                    .map(|v| format!("`{v}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        );
        let _ = writeln!(out, "| `{key}` | {default} | {allowed} |");
    }
    let sample = |kind: &str| format!("effort.{kind}.<name>");
    let family = EFFORT_COMPONENT_KINDS
        .iter()
        .map(|k| format!("`{}`", sample(k)))
        .collect::<Vec<_>>()
        .join(", ");
    let levels = allowed_enum_values("effort.agents.x")
        .map(|v| {
            v.iter()
                .map(|l| format!("`{l}`"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let _ = writeln!(out, "| {family} | `auto` | {levels} |");
    let _ = writeln!(out, "| `watch.rule.<id>` | empty | one rule as JSON text |");
    out
}

pub fn generate() -> String {
    let mut cmd = Cli::command();
    cmd.build();
    let mut out = String::new();
    out.push_str(
        "# CLI and config reference\n\n\
         <!-- Generated by crates/cli/tests/reference_docs.rs. Do not edit by hand.\n\
         Regenerate with: UPDATE_REFERENCE=1 cargo test -p playbook --test reference_docs -->\n\n\
         This page lists every `playbook` command, flag and config key. It is generated from the\n\
         clap definitions and the config key table, and a test fails when it is out of date.\n\
         For what each key means and when to change it, read the [config keys page](config-keys.md).\n\
         For one line per command and who may run it, run `playbook commands`.\n\n\
         ## Config keys\n\n\
         Set a key with `playbook config set <key> <value>` and read it with `playbook config get <key>`.\n\n",
    );
    out.push_str(&config_table());
    out.push_str("\n## Commands\n\n");
    walk(&cmd, "playbook", &mut out);
    out.trim_end().to_string() + "\n"
}

#[test]
fn the_reference_page_matches_the_generated_output() {
    let path = root().join(PAGE);
    let fresh = generate();
    if std::env::var_os("UPDATE_REFERENCE").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &fresh).unwrap();
        return;
    }
    let committed = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == fresh,
        "{PAGE} is out of date with the CLI definitions or the config keys.\nRegenerate it with: {REGEN}"
    );
}

#[test]
fn the_generated_output_is_deterministic() {
    assert_eq!(generate(), generate());
}

/// Splits a markdown table row into trimmed cells, keeping `\|` inside one.
fn split_cells(line: &str) -> Vec<String> {
    let inner = line.trim();
    let inner = inner.strip_prefix('|').unwrap_or(inner);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut cells = vec![String::new()];
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cells.last_mut().unwrap().push_str("\\|");
                chars.next();
            }
            '|' => cells.push(String::new()),
            _ => cells.last_mut().unwrap().push(c),
        }
    }
    cells.iter().map(|c| c.trim().to_string()).collect()
}

/// The data rows (those whose first cell starts with a code span) of the
/// table under the `## heading` section, as cells.
fn table_rows(page: &str, heading: &str) -> Vec<Vec<String>> {
    let mut in_section = false;
    let mut rows = Vec::new();
    for line in page.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            in_section = title.trim() == heading;
        } else if in_section && line.starts_with("| `") {
            rows.push(split_cells(line));
        }
    }
    rows
}

/// The code spans of a cell.
fn code_spans(cell: &str) -> Vec<&str> {
    cell.split('`').skip(1).step_by(2).collect()
}

/// A key name the page may show in the key column besides the known keys: a
/// family placeholder such as `watch.rule.<id>`.
fn is_family_placeholder(name: &str) -> bool {
    name == "watch.rule.<id>"
        || EFFORT_COMPONENT_KINDS
            .iter()
            .any(|kind| name == format!("effort.{kind}.<name>"))
}

#[test]
fn every_known_key_is_explained_in_the_config_keys_page() {
    // Arrange
    let guide = fs::read_to_string(root().join(CONFIG_KEYS)).unwrap();
    let rows = table_rows(&guide, "Reference");
    let columns = ["Key", "Default", "Values", "What it does", "Change it when"];

    // Act
    let row_for = |key: &str| -> Vec<&Vec<String>> {
        rows.iter()
            .filter(|row| code_spans(&row[0]).contains(&key))
            .collect()
    };

    // Assert
    for key in KNOWN_KEYS {
        let found = row_for(key);
        assert_eq!(
            found.len(),
            1,
            "{CONFIG_KEYS} needs exactly one row for the config key `{key}`, found {}",
            found.len()
        );
        let row = found[0];
        assert_eq!(
            row.len(),
            columns.len(),
            "the row of `{key}` has the wrong cell count"
        );
        for (name, cell) in columns.iter().zip(row.iter()) {
            assert!(!cell.is_empty(), "the `{name}` cell of `{key}` is empty");
        }
    }
    let families = EFFORT_COMPONENT_KINDS
        .iter()
        .map(|kind| format!("effort.{kind}.<name>"))
        .chain(["watch.rule.<id>".to_string()]);
    for family in families {
        let found = row_for(&family);
        assert_eq!(
            found.len(),
            1,
            "{CONFIG_KEYS} needs one row for the family `{family}`"
        );
        for (name, cell) in columns.iter().zip(found[0].iter()) {
            assert!(!cell.is_empty(), "the `{name}` cell of `{family}` is empty");
        }
    }
    for row in &rows {
        for name in code_spans(&row[0]) {
            assert!(
                KNOWN_KEYS.contains(&name) || is_family_placeholder(name),
                "{CONFIG_KEYS} has a row for `{name}`, which is not a current config key"
            );
        }
    }
}

/// Every `.rs` file under `dir`.
fn rust_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The production part of a source file: everything before the trailing
/// `#[cfg(test)] mod` block.
fn production_part(text: &str) -> &str {
    text.split("\n#[cfg(test)]\nmod ").next().unwrap_or(text)
}

/// The `PLAYBOOK_*` variables the shipped binaries read with `env::var` or
/// `env::var_os`, in `crates/*/src`. Test code is skipped: the `tests/`
/// folders, `testing.rs` and the trailing test module of a file. So are the
/// test seams: a variable with `TEST` in its name, and a read within three
/// lines of a `debug_assertions` check, which a release build never makes.
fn production_env_vars() -> std::collections::BTreeSet<String> {
    let mut files = Vec::new();
    for krate in fs::read_dir(root().join("crates")).unwrap() {
        let src = krate.unwrap().path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    let texts: Vec<String> = files
        .iter()
        .filter(|f| f.file_name().is_some_and(|n| n != "testing.rs"))
        .map(|f| fs::read_to_string(f).unwrap())
        .collect();
    let consts =
        regex::Regex::new(r#"const\s+([A-Z_]+):\s*&str\s*=\s*"(PLAYBOOK_[A-Z_]+)""#).unwrap();
    let reads = regex::Regex::new(
        r#"env::var(?:_os)?\(\s*(?:"(PLAYBOOK_[A-Z_]+)"|(?:[a-z_]+::)*([A-Z_]+))\s*\)"#,
    )
    .unwrap();
    let production: Vec<&str> = texts.iter().map(|t| production_part(t)).collect();
    let named: std::collections::HashMap<&str, &str> = production
        .iter()
        .flat_map(|t| consts.captures_iter(t))
        .map(|c| (c.get(1).unwrap().as_str(), c.get(2).unwrap().as_str()))
        .collect();
    let mut found = std::collections::BTreeSet::new();
    for text in production {
        for read in reads.captures_iter(text) {
            let name = match (read.get(1), read.get(2)) {
                (Some(literal), _) => literal.as_str(),
                (None, Some(ident)) => match named.get(ident.as_str()) {
                    Some(value) => value,
                    None => continue,
                },
                _ => continue,
            };
            let line = text[..read.get(0).unwrap().start()].matches('\n').count();
            let guarded = text
                .lines()
                .skip(line.saturating_sub(3))
                .take(4)
                .any(|l| l.contains("debug_assertions"));
            if !guarded && !name.contains("TEST") {
                found.insert(name.to_string());
            }
        }
    }
    found
}

#[test]
fn every_production_env_var_has_a_row_in_the_config_keys_page() {
    // Arrange
    let guide = fs::read_to_string(root().join(CONFIG_KEYS)).unwrap();
    let rows = table_rows(&guide, "Environment variables");

    // Act
    let vars = production_env_vars();

    // Assert
    assert!(
        vars.contains("PLAYBOOK_MODE"),
        "the scan found no PLAYBOOK_MODE read, so it is broken: {vars:?}"
    );
    for var in &vars {
        let row = rows
            .iter()
            .find(|row| code_spans(&row[0]).contains(&var.as_str()))
            .unwrap_or_else(|| {
                panic!("{CONFIG_KEYS} has no row in `Environment variables` for `{var}`")
            });
        assert_eq!(row.len(), 3, "the row of `{var}` has the wrong cell count");
        assert!(
            row.iter().all(|cell| !cell.is_empty()),
            "the row of `{var}` has an empty cell"
        );
    }
}

#[test]
fn the_page_is_linked_from_the_index_and_the_readme() {
    for file in ["docs/README.md", "README.md"] {
        let text = fs::read_to_string(root().join(file)).unwrap();
        assert!(
            text.contains("reference/cli-and-config.md"),
            "{file} does not link to {PAGE}"
        );
    }
}

/// Lines that may name the helper by its short alias followed by a word.
const HELPER_SENTENCES: &[&str] = &[
    "The old `pbsync sync`.",
    "checks the pbsync and pbdoc size budgets",
];

/// Old spellings that no user instruction may use any more.
const STALE_PATTERNS: &[&str] = &[r"\bpbsync\s+[a-z]", r"config\s+(apply-proposal|sync-files)"];

/// Every `file:line` in `text` that tells a user to run an old spelling.
fn stale_instructions(file: &str, text: &str) -> Vec<String> {
    let patterns: Vec<regex::Regex> = STALE_PATTERNS
        .iter()
        .map(|p| regex::Regex::new(p).unwrap())
        .collect();
    text.lines()
        .enumerate()
        .filter(|(_, line)| !HELPER_SENTENCES.iter().any(|ok| line.contains(ok)))
        .filter(|(_, line)| patterns.iter().any(|p| p.is_match(line)))
        .map(|(i, line)| format!("{file}:{}: {}", i + 1, line.trim()))
        .collect()
}

/// Markdown files that carry user instructions: everything under `docs/`
/// except the ADRs, which are history, plus `CONTRIBUTING.md`.
fn instruction_pages() -> Vec<PathBuf> {
    fn walk_dir(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "adr") {
                    walk_dir(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    let mut pages = vec![root().join("CONTRIBUTING.md")];
    walk_dir(&root().join("docs"), &mut pages);
    pages.sort();
    pages
}

#[test]
fn the_stale_instruction_check_flags_old_spellings() {
    // Arrange
    let text = "fine\n```bash\npbsync push\n```\nrun `playbook config apply-proposal`\nThe old `pbsync sync`.\n`pbsync` is a short name";

    // Act
    let hits = stale_instructions("x.md", text);

    // Assert
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert!(hits[0].starts_with("x.md:3:"), "{hits:?}");
    assert!(hits[1].starts_with("x.md:5:"), "{hits:?}");
}

#[test]
fn no_doc_tells_a_user_to_run_an_old_sync_spelling() {
    // Arrange
    let pages = instruction_pages();

    // Act
    let hits: Vec<String> = pages
        .iter()
        .flat_map(|path| {
            let rel = path.strip_prefix(root()).unwrap().display().to_string();
            stale_instructions(&rel, &fs::read_to_string(path).unwrap())
        })
        .collect();

    // Assert
    assert!(
        hits.is_empty(),
        "user instructions must say `playbook sync ...`:\n{}",
        hits.join("\n")
    );
}
