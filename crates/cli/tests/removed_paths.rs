// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Subcommands that were removed are unknown to clap: exit 2, no output on stdout.

#[path = "support/isolated.rs"]
mod isolated;

#[test]
fn removed_subcommands_are_unknown_and_exit_2() {
    let removed: [&[&str]; 7] = [
        &["doctor", "plugin-version", "none.json"],
        &["doctor", "statusline-command", "none.json"],
        &["doctor", "hook-commands", "none.json"],
        &[
            "doctor",
            "hook-commands-for-event",
            "none.json",
            "PreToolUse",
        ],
        &["doctor", "hook-commands-matching", "none.json", "playbook"],
        &["hooks", "explain-rm", "ls"],
        &["usage", "ingest"],
    ];
    for args in removed {
        // Arrange
        let mut cmd = isolated::playbook();

        // Act
        let out = cmd
            .args(args)
            .output()
            .expect("playbook binary should spawn");

        // Assert
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {err}");
        assert!(err.contains("unrecognized subcommand"), "{args:?}: {err}");
    }
}
