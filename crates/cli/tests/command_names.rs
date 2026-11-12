// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! New command names are one lowercase word per segment and at most four
//! segments deep. Hyphenated paths that predate the rule are pinned in
//! `LEGACY_HYPHENATED`; an entry whose command is gone must be removed.

use clap::Command;

const MAX_DEPTH: usize = 4;

/// Hyphenated command paths that exist today. Do not add to this list.
const LEGACY_HYPHENATED: &[&str] = &[
    "pr ci-wait",
    "pr land-wait",
    "run-context",
    "memory import-claude",
    "cc bust-cache",
    "sanitize commit-msg",
    "sanitize pr-text",
    "doctor pending-migrations",
    "watch dry-run",
    "watch install-service",
    "watch uninstall-service",
    "config apply-proposal",
    "config sync-files",
];

/// Paths under `cmd` (root excluded) that end in a hyphenated segment outside
/// `legacy`, or sit deeper than `MAX_DEPTH`.
fn violations(cmd: &Command, legacy: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    walk(cmd, &mut Vec::new(), legacy, &mut found);
    found
}

fn walk(cmd: &Command, path: &mut Vec<String>, legacy: &[&str], found: &mut Vec<String>) {
    // clap injects a `help` subcommand at every level once the tree is built.
    for sub in cmd.get_subcommands().filter(|sub| sub.get_name() != "help") {
        path.push(sub.get_name().to_string());
        let joined = path.join(" ");
        let hyphenated = sub.get_name().contains('-') && !legacy.contains(&joined.as_str());
        if hyphenated || path.len() > MAX_DEPTH {
            found.push(joined);
        }
        walk(sub, path, legacy, found);
        path.pop();
    }
}

/// Entries of `legacy` that name no command under `cmd`.
fn stale_entries(cmd: &Command, legacy: &[&str]) -> Vec<String> {
    legacy
        .iter()
        .filter(|entry| {
            let mut node = cmd;
            for segment in entry.split(' ') {
                match node.find_subcommand(segment) {
                    Some(next) => node = next,
                    None => return true,
                }
            }
            false
        })
        .map(|entry| (*entry).to_string())
        .collect()
}

fn synthetic(path: &[&'static str]) -> Command {
    let mut leaf = Command::new(path[path.len() - 1]);
    for name in path[..path.len() - 1].iter().rev() {
        leaf = Command::new(*name).subcommand(leaf);
    }
    Command::new("playbook").subcommand(leaf)
}

#[test]
fn walker_reports_unlisted_hyphenated_path() {
    // Arrange
    let cmd = synthetic(&["pr", "foo-bar"]);

    // Act
    let found = violations(&cmd, &[]);

    // Assert
    assert_eq!(found, vec!["pr foo-bar".to_string()]);
}

#[test]
fn stale_check_reports_entry_naming_a_gone_command() {
    // Arrange
    let cmd = playbook::parse::command();

    // Act
    let found = stale_entries(&cmd, &["pr nope-gone"]);

    // Assert
    assert_eq!(found, vec!["pr nope-gone".to_string()]);
}

#[test]
fn walker_reports_depth_over_four() {
    // Arrange
    let cmd = synthetic(&["a", "b", "c", "d", "e"]);

    // Act
    let found = violations(&cmd, &[]);

    // Assert
    assert_eq!(found, vec!["a b c d e".to_string()]);
}

#[test]
fn hook_takes_its_name_as_a_value() {
    // Arrange
    let cmd = playbook::parse::command();

    // Act
    let hook = cmd.find_subcommand("hook").expect("hook command exists");

    // Assert
    assert_eq!(hook.get_subcommands().count(), 0);
}

#[test]
fn real_tree_has_no_violations() {
    // Arrange
    let cmd = playbook::parse::command();

    // Act
    let found = violations(&cmd, LEGACY_HYPHENATED);

    // Assert
    assert!(
        found.is_empty(),
        "unlisted hyphenated or too deep: {found:?}"
    );
}

#[test]
fn legacy_list_has_no_stale_entries() {
    // Arrange
    let cmd = playbook::parse::command();

    // Act
    let found = stale_entries(&cmd, LEGACY_HYPHENATED);

    // Assert
    assert!(found.is_empty(), "stale legacy entries: {found:?}");
    assert_eq!(LEGACY_HYPHENATED.len(), 13);
}
