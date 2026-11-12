// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A hidden legacy command keeps its own code and output until 1.0.0. It
//! stays out of every list a user or an agent reads.

use std::ffi::OsString;

use clap::{Command, CommandFactory, Parser};
use playbook::Cli;

/// Hidden legacy commands, each removed in 1.0.0. Add a row only when a
/// command with a changed contract moves to a new name.
const LEGACY_COMMANDS: &[&str] = &[
    "commit all",
    "pr land",
    "pr ci-wait",
    "pr land-wait",
    "review suggest",
];

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

/// The triage rows this table must carry, as (old, new).
const TRIAGE_ROWS: [(&str, &str); 3] = [
    ("pr review-triage", "pr review triage"),
    ("review triage-merge", "review triage merge"),
    ("eval review-triage", "eval triage"),
];

/// Arguments a row needs to run offline. Most rows need none.
fn sample_tail(old: &str) -> &'static [&'static str] {
    match old {
        "review triage-merge" => &["--lenses", "a"],
        _ => &[],
    }
}

fn argv(path: &str, tail: &[&str]) -> Vec<OsString> {
    ["playbook"]
        .into_iter()
        .chain(path.split(' '))
        .chain(tail.iter().copied())
        .map(OsString::from)
        .collect()
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "playbook-command-renames-{}-{tag}",
        playbook::testing::run_id()
    ));
    std::fs::create_dir_all(dir.join("bin")).expect("scratch dir should be creatable");
    dir
}

/// Runs the binary on a scratch home with an empty PATH, so no network or
/// `gh` is reachable and stderr is a pipe, not a terminal.
fn run(args: &[OsString], tag: &str) -> std::process::Output {
    let root = scratch(tag);
    std::process::Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(&args[1..])
        .current_dir(&root)
        .env_clear()
        .env("PATH", root.join("bin"))
        .env("HOME", &root)
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("TMPDIR", &root)
        .output()
        .expect("playbook binary should spawn")
}

#[test]
fn the_triage_commands_are_alias_rows() {
    // Arrange
    let table = playbook::renames::COMMAND_RENAMES;

    for (old, new) in TRIAGE_ROWS {
        // Act
        let row = table.iter().find(|row| row.old == old);

        // Assert
        let row = row.unwrap_or_else(|| panic!("`{old}` has no alias row"));
        assert_eq!(row.new, new);
    }
}

#[test]
fn every_row_parses_the_old_name_as_the_new_one() {
    for row in playbook::renames::COMMAND_RENAMES {
        // Arrange
        let tail = sample_tail(row.old);
        let (rewritten, _) =
            playbook::renames::rewrite(&argv(row.old, tail), playbook::renames::COMMAND_RENAMES);
        let direct: Vec<_> = argv(row.new, row.extra)
            .into_iter()
            .chain(tail.iter().map(OsString::from))
            .collect();

        // Act
        let via_old = Cli::try_parse_from(&rewritten);
        let via_new = Cli::try_parse_from(&direct);

        // Assert
        let via_new = via_new.unwrap_or_else(|e| panic!("`{}` does not parse: {e}", row.new));
        let via_old = via_old.unwrap_or_else(|e| panic!("`{}` does not parse: {e}", row.old));
        assert_eq!(
            format!("{via_old:?}"),
            format!("{via_new:?}"),
            "{}",
            row.old
        );
    }
}

#[test]
fn every_row_gives_the_same_stdout_and_exit_code_through_the_binary() {
    for row in playbook::renames::COMMAND_RENAMES {
        // Arrange
        let tail = sample_tail(row.old);

        // Act
        let old = run(&argv(row.old, tail), "old");
        let new = run(&argv(row.new, tail), "new");

        // Assert
        assert_eq!(old.status.code(), new.status.code(), "{}", row.old);
        assert_eq!(old.stdout, new.stdout, "{}", row.old);
    }
}

#[test]
fn no_row_prints_a_deprecation_line_to_a_piped_stderr() {
    for row in playbook::renames::COMMAND_RENAMES {
        // Arrange
        let args = argv(row.old, sample_tail(row.old));
        let line = playbook::renames::deprecation_line(row.old, row.new);

        // Act
        let output = run(&args, "piped");

        // Assert
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains(&line), "{}: {stderr}", row.old);
    }
}

#[test]
fn no_row_prints_a_deprecation_line_with_json() {
    for row in playbook::renames::COMMAND_RENAMES {
        // Arrange
        let mut tail = sample_tail(row.old).to_vec();
        tail.push("--json");
        let args = argv(row.old, &tail);
        let (_, line) = playbook::renames::rewrite(&args, playbook::renames::COMMAND_RENAMES);

        // Act
        let printed = playbook::renames::printable_notice(&args, line, true);

        // Assert
        assert_eq!(printed, None, "{}", row.old);
    }
}
