// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook --help` groups the commands by audience, and `playbook commands`
//! keeps its output, from the real binary.

#[path = "support/isolated.rs"]
mod isolated;

use std::process::Output;

const USER: &[&str] = &[
    "setup",
    "hooks",
    "uninstall",
    "upgrade",
    "plugin",
    "version",
    "deps",
    "doctor",
    "gc",
    "migrate",
    "skills",
    "cc",
    "ccd",
    "usage",
    "sessions",
    "watch",
    "trust",
    "commands",
    "completions",
    "config",
    "effort",
    "mode",
    "memory",
    "sync",
];
const AGENT: &[&str] = &[
    "doc",
    "state",
    "route",
    "statusline",
    "settings",
    "manifest",
    "agents",
    "gate",
    "code",
    "sanitize",
    "ci",
    "path",
    "plan",
    "prose",
    "atlassian",
    "learn",
    "segment",
    "pr",
    "review",
    "commit",
    "plans",
    "worktree",
    "handoff",
    "adr",
    "glossary",
    "tools",
    "eval",
];

fn run(args: &[&str]) -> Output {
    isolated::playbook()
        .args(args)
        .env("HTTPS_PROXY", "127.0.0.1:9")
        .env("HTTP_PROXY", "127.0.0.1:9")
        .output()
        .expect("spawn playbook")
}

fn root_help() -> String {
    String::from_utf8_lossy(&run(&["--help"]).stdout).into_owned()
}

/// The command names listed under `heading`, without the `help` line clap adds.
fn block(help: &str, heading: &str) -> Vec<String> {
    help.lines()
        .skip_while(|l| *l != heading)
        .skip(1)
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.strip_prefix("  "))
        .filter(|l| !l.starts_with([' ', '-']))
        .filter_map(|l| l.split_whitespace().next())
        .filter(|n| *n != "help")
        .map(str::to_string)
        .collect()
}

#[test]
fn root_help_lists_user_commands_before_agent_commands() {
    // Arrange
    let help = root_help();

    // Act
    let mut user = block(&help, "Commands:");
    let mut agent = block(&help, "Agent commands:");

    // Assert
    let sorted = |names: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        v.sort_unstable();
        v
    };
    user.sort_unstable();
    agent.sort_unstable();
    assert_eq!(user, sorted(USER), "{help}");
    assert_eq!(agent, sorted(AGENT), "{help}");
    let at = |heading: &str| help.find(heading).unwrap_or_else(|| panic!("no {heading}"));
    assert!(at("\nCommands:\n") < at("\nAgent commands:\n"), "{help}");
}

#[test]
fn root_help_lists_each_visible_command_once_and_no_hidden_one() {
    // Arrange
    let help = root_help();

    // Act
    let mut listed = block(&help, "Commands:");
    listed.extend(block(&help, "Agent commands:"));

    // Assert
    assert!(help.contains("\nAgent commands:\n"), "{help}");
    let mut want: Vec<String> = USER.iter().chain(AGENT).map(|s| s.to_string()).collect();
    listed.sort_unstable();
    want.sort_unstable();
    assert_eq!(listed, want, "{help}");
    for hidden in ["init", "hook", "json"] {
        assert!(!listed.iter().any(|n| n == hidden), "{hidden} is listed");
    }
}

#[test]
fn root_help_keeps_usage_options_and_the_pb_alias_line() {
    // Arrange
    let help = root_help();

    // Act
    let has = |needle: &str| help.contains(needle);

    // Assert
    assert!(has("Usage: playbook <COMMAND>"), "{help}");
    assert!(has("Options:"), "{help}");
    assert!(has("-h, --help"), "{help}");
    assert!(has("-V, --version"), "{help}");
    assert!(has("`pb` is a short alias"), "{help}");
}

#[test]
fn playbook_commands_json_keeps_its_keys_and_lists_completions() {
    // Arrange
    let keys = [
        "command",
        "purpose",
        "access",
        "args",
        "subcommands",
        "intents",
    ];

    // Act
    let out = run(&["commands", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");

    // Assert
    assert!(out.status.success());
    let rows = rows.as_array().expect("array");
    assert!(rows.iter().any(|r| r["command"] == "completions"));
    for row in rows {
        let found: Vec<&str> = row
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(found, keys, "{row}");
    }
}
