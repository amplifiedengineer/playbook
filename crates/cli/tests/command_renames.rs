// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A hidden legacy command keeps its own code and output until 1.0.0. It
//! stays out of every list a user or an agent reads.

use clap::{Command, CommandFactory};
use playbook::Cli;

/// Hidden legacy commands, each removed in 1.0.0. Add a row only when a
/// command with a changed contract moves to a new name.
const LEGACY_COMMANDS: &[&str] = &["commit all", "pr land", "pr ci-wait", "pr land-wait"];

fn find<'a>(root: &'a Command, path: &str) -> Option<&'a Command> {
    path.split(' ')
        .try_fold(root, |node, name| node.find_subcommand(name))
}

#[test]
fn every_legacy_command_exists_and_is_hidden() {
    // Arrange
    let root = Cli::command();

    for path in LEGACY_COMMANDS {
        // Act
        let found = find(&root, path);

        // Assert
        let cmd = found.unwrap_or_else(|| panic!("`{path}` is gone; delete its row"));
        assert!(cmd.is_hide_set(), "`{path}` must be hidden");
    }
}

#[test]
fn no_legacy_command_is_listed_by_the_command_index() {
    // Arrange
    let root = playbook::command_index::tree();
    let group = |path: &str| vec![path.split(' ').next().unwrap().to_string()];

    for path in LEGACY_COMMANDS {
        // Act
        let text = playbook::command_index::render(&root, &group(path)).unwrap();
        let json = playbook::command_index::render_json(&root, &[]).unwrap();

        // Assert
        assert!(
            !text.lines().any(|l| l.starts_with(path)),
            "`{path}` is listed:\n{text}"
        );
        assert!(
            !json.contains(&format!("\"command\": \"{path}\"")),
            "`{path}` is in the json list"
        );
    }
}

const DELIVER_OLD: &str = "deliver";
const DELIVER_NEW: &str = "pr create";

#[test]
fn deliver_is_an_alias_row_for_pr_create() {
    // Arrange
    let args: Vec<std::ffi::OsString> = ["playbook", DELIVER_OLD, "--ready", "--json"]
        .iter()
        .map(std::ffi::OsString::from)
        .collect();

    // Act
    let (out, line) = playbook::renames::rewrite(&args, playbook::renames::COMMAND_RENAMES);

    // Assert
    let out: Vec<String> = out
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(out, ["playbook", "pr", "create", "--ready", "--json"]);
    assert_eq!(
        line,
        Some(playbook::renames::deprecation_line(
            DELIVER_OLD,
            DELIVER_NEW
        ))
    );
}

#[test]
fn the_wait_commands_print_a_deprecation_line_naming_pr_merge() {
    for old in ["pr ci-wait", "pr land-wait"] {
        // Arrange
        let mut words = vec!["playbook"];
        words.extend(old.split(' '));
        words.push("7");
        let args: Vec<std::ffi::OsString> = words.iter().map(std::ffi::OsString::from).collect();

        // Act
        let line = playbook::renames::notice_for(&args, playbook::renames::LEGACY_NOTICES);

        // Assert
        assert_eq!(
            line,
            Some(playbook::renames::deprecation_line(old, "pr merge"))
        );
    }
}

#[test]
fn the_wait_commands_notice_is_silent_with_json_or_off_a_terminal() {
    // Arrange
    let line = Some(playbook::renames::deprecation_line(
        "pr ci-wait",
        "pr merge",
    ));
    let plain: Vec<std::ffi::OsString> = vec!["playbook".into(), "pr".into()];
    let json: Vec<std::ffi::OsString> = vec!["playbook".into(), "--json".into()];

    // Act
    let on_terminal = playbook::renames::printable_notice(&plain, line.clone(), true);
    let with_json = playbook::renames::printable_notice(&json, line.clone(), true);
    let piped = playbook::renames::printable_notice(&plain, line.clone(), false);

    // Assert
    assert_eq!(on_terminal, line);
    assert_eq!((with_json, piped), (None, None));
}
