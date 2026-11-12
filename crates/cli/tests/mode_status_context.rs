// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook mode status --json --command NAME --args ARGS` prints what
//! `playbook run-context` printed, byte for byte, and exits 2 on a conflict.
//! Every run pins `$HOME` to a scratch directory and removes `PLAYBOOK_MODE`.

use std::path::Path;
use std::process::{Command, Output};

const CONFLICT_EXIT: i32 = 2;
const CONFLICT_MESSAGE: &str = "--auto and --ask conflict; pass one.";

/// The old spelling and the new one, for the same inputs.
const INPUTS: &[(&str, &str)] = &[
    ("implement", "--auto"),
    ("plan", "12 --ask"),
    ("fix", ""),
    ("plan", "idea --auto-design"),
    ("review", "--automatic feat/auto"),
];

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env_remove("PLAYBOOK_MODE")
        .output()
        .expect("playbook binary should spawn")
}

fn scratch_home(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "playbook-mode-status-context-{}-{tag}",
        playbook::testing::run_id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir should be creatable");
    dir
}

#[test]
fn mode_status_with_a_command_prints_the_run_context_bytes() {
    // Arrange
    let home = scratch_home("identity");

    for (command, args) in INPUTS {
        // Act
        let old = run(
            &home,
            &["run-context", "--command", command, "--args", args],
        );
        let new = run(
            &home,
            &[
                "mode",
                "status",
                "--json",
                "--command",
                command,
                "--args",
                args,
            ],
        );

        // Assert
        assert!(
            new.status.success(),
            "{}",
            String::from_utf8_lossy(&new.stderr)
        );
        assert_eq!(old.stdout, new.stdout, "{command} {args}");
        assert!(String::from_utf8_lossy(&new.stdout).contains("\"ceiling\""));
    }
}

#[test]
fn mode_status_without_a_command_leaves_the_ceiling_out() {
    // Arrange
    let home = scratch_home("bare");

    // Act
    let out = run(&home, &["mode", "status", "--json", "--args", "--ask"]);

    // Assert
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("\"mode\":\"ask\""), "{stdout}");
    assert!(!stdout.contains("\"ceiling\""), "{stdout}");
}

#[test]
fn mode_status_exits_2_when_the_arguments_hold_both_flags() {
    // Arrange
    let home = scratch_home("conflict");

    // Act
    let out = run(
        &home,
        &[
            "mode",
            "status",
            "--json",
            "--command",
            "adr",
            "--args",
            "--auto --ask",
        ],
    );

    // Assert
    assert_eq!(out.status.code(), Some(CONFLICT_EXIT));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains(CONFLICT_MESSAGE));
}
