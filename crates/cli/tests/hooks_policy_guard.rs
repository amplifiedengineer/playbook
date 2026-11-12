// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook hook policy-guard` as a real process: what it denies, what it
//! lets through, that it fails safe on bad input, and that it is fast.

use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const HOME: &str = "/Users/tester";

fn run(stdin: &str, env: &[(&str, &str)]) -> (String, i32, Duration) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_playbook"));
    command
        .args(["hook", "policy-guard"])
        .env("HOME", HOME)
        .env("POLICY_GUARD", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    let started = Instant::now();
    let mut child = command.spawn().expect("playbook spawns");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
        started.elapsed(),
    )
}

fn bash(command: &str) -> String {
    json!({"tool_name": "Bash", "tool_input": {"command": command}}).to_string()
}

fn write_to(path: &str) -> String {
    json!({"tool_name": "Write", "tool_input": {"file_path": path, "content": "x"}}).to_string()
}

fn denial(stdout: &str) -> Option<String> {
    let v: Value = serde_json::from_str(stdout).ok()?;
    let inner = &v["hookSpecificOutput"];
    (inner["permissionDecision"] == "deny").then(|| {
        inner["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string()
    })
}

#[test]
fn no_verify_and_signing_off_are_denied_with_a_reason() {
    for command in [
        "git commit --no-verify -m x",
        "git commit -n -m x",
        "git push --no-verify",
        "git commit --no-gpg-sign -m x",
        "git -c commit.gpgsign=false commit -m x",
    ] {
        let (out, code, _) = run(&bash(command), &[]);
        assert_eq!(code, 0, "{command}");
        assert!(denial(&out).is_some(), "{command} should be denied: {out}");
    }
}

#[test]
fn a_hand_run_gh_pr_create_is_routed_to_the_skill() {
    let (out, _, _) = run(&bash("gh pr create --title t --body b"), &[]);

    let reason = denial(&out).expect("denied");
    assert!(reason.contains("/playbook:pr-create"), "{reason}");
}

#[test]
fn the_skills_own_pr_command_and_ordinary_git_pass() {
    for command in [
        "playbook pr create --title t --body-file f",
        "git commit -S -s -m 'feat: x'",
        "git push origin feature",
        "gh pr view 1",
        "gh pr merge 1 --auto",
        "ls",
    ] {
        let (out, code, _) = run(&bash(command), &[]);
        assert_eq!((out.as_str(), code), ("", 0), "{command}");
    }
}

#[test]
fn writes_to_claude_memory_are_denied_and_the_playbook_store_is_not() {
    let (out, _, _) = run(
        &write_to("/Users/tester/.claude/projects/-Users-tester-repo/memory/MEMORY.md"),
        &[],
    );
    assert!(denial(&out)
        .expect("denied")
        .contains("~/.config/playbook/memory"));

    let (out, _, _) = run(&bash("echo x >> ~/.claude/memory/a.md"), &[]);
    assert!(denial(&out).is_some(), "{out}");

    let (out, code, _) = run(&write_to("/Users/tester/.config/playbook/memory/a.md"), &[]);
    assert_eq!(code, 0);
    assert!(denial(&out).is_none(), "{out}");
    let (out, _, _) = run(&bash("cat ~/.claude/projects/p/memory/a.md"), &[]);
    assert_eq!(out, "", "reads are fine");
}

#[test]
fn bad_input_is_allowed_silently() {
    for input in [
        "",
        "{",
        "[1,2]",
        r#"{"tool_input":{}}"#,
        r#"{"tool_input":{"command":7}}"#,
    ] {
        let (out, code, _) = run(input, &[]);
        assert_eq!((out.as_str(), code), ("", 0), "{input}");
    }
}

#[test]
fn the_off_switch_allows_everything() {
    let (out, code, _) = run(&bash("git commit --no-verify"), &[("POLICY_GUARD", "0")]);

    assert_eq!((out.as_str(), code), ("", 0));
}

#[test]
fn a_long_command_is_still_fast() {
    // Arrange: a 200 KB command that mentions git.
    let command = format!("git status && echo {}", "word ".repeat(40_000));

    // Act
    let (out, code, took) = run(&bash(&command), &[]);

    // Assert: generous bound, since a CI runner can be slow.
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(took < Duration::from_secs(2), "{took:?}");
}

#[test]
fn writing_a_fact_file_shows_the_memory_format_and_does_not_deny() {
    // Arrange / Act
    let (out, code, _) = run(
        &write_to("/Users/tester/.config/playbook/memory/o/r/fact.md"),
        &[],
    );

    // Assert
    assert_eq!(code, 0);
    assert!(denial(&out).is_none(), "{out}");
    let v: Value = serde_json::from_str(&out).expect("json");
    let context = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        context.contains("supersedes") && context.contains("anchors:"),
        "{context}"
    );
}

#[test]
fn only_a_write_of_a_fact_file_shows_the_format() {
    // Arrange: an Edit, a graph file, and a file outside the store.
    let edit = json!({"tool_name": "Edit", "tool_input": {"file_path": "/Users/tester/.config/playbook/memory/a.md"}}).to_string();

    // Act / Assert
    assert_eq!(run(&edit, &[]).0, "");
    assert_eq!(
        run(
            &write_to("/Users/tester/.config/playbook/memory/memory.graph.json"),
            &[]
        )
        .0,
        ""
    );
    assert_eq!(run(&write_to("/Users/tester/repo/notes.md"), &[]).0, "");
}

#[test]
fn denials_tell_an_unattended_run_what_to_do_when_the_sanctioned_path_fails() {
    let (out, _, _) = run(&bash("gh pr create --title t --body b"), &[]);
    let reason = denial(&out).expect("gh pr create is denied");
    assert!(reason.contains("playbook pr create --help"), "{reason}");
    assert!(reason.contains("Use `playbook pr create`"), "{reason}");
    assert!(!reason.contains("playbook deliver"), "{reason}");
    assert!(
        reason.contains("report the error text to the user, and stop"),
        "{reason}"
    );

    let (out, _, _) = run(&bash("git commit --no-verify -m x"), &[]);
    let reason = denial(&out).expect("--no-verify is denied");
    assert!(
        reason.contains("report the hook's error text and stop"),
        "{reason}"
    );

    let (out, _, _) = run(&bash("git commit --no-gpg-sign -m x"), &[]);
    let reason = denial(&out).expect("--no-gpg-sign is denied");
    assert!(
        reason.contains("report the error text and stop"),
        "{reason}"
    );

    for value in ["no", "0", "off", "False"] {
        let (out, _, _) = run(
            &bash(&format!("git -c commit.gpgsign={value} commit -m x")),
            &[],
        );
        assert!(denial(&out).is_some(), "gpgsign={value} must be denied");
    }
}

#[test]
fn protected_settings_cannot_be_changed_by_the_model() {
    let reason_of = |input: String| denial(&run(&input, &[]).0);
    for command in [
        "playbook config set --global pr.rules.prose off",
        "nohup playbook config import f.json",
        "WORKTREE_BASE_DIR=/ rm -rf /tmp/x",
        "printf x > /Users/tester/.config/playbook/config.json",
    ] {
        let reason = reason_of(bash(command)).unwrap_or_default();
        assert!(reason.contains("with `!`"), "{command}: {reason}");
    }
    for path in [
        "/Users/tester/.config/playbook/playbook.db",
        "/Users/tester/.config/playbook/data/config.db",
        "/Users/tester/.config/playbook/data/memory.db",
        "/Users/tester/.config/playbook/config.json",
    ] {
        assert!(reason_of(write_to(path)).is_some(), "{path}");
    }
    assert!(reason_of(write_to("/Users/tester/.config/playbook/memory/a.md")).is_none());
    let settings = json!({"tool_name": "Edit", "tool_input": {
        "file_path": "/Users/tester/.claude/settings.json",
        "new_string": "\"WORKTREE_BASE_DIR\": \"/\""
    }})
    .to_string();
    assert!(reason_of(settings).is_some());
    assert!(reason_of(bash("playbook config get pr.rules.prose")).is_none());
    assert!(reason_of(bash("git commit -m 'mention playbook.db'")).is_none());
}

/// Reads of playbook's state that blocked real sessions, sent through the
/// hook entry as JSON payloads.
#[test]
fn reading_playbook_state_is_never_denied() {
    let reason_of = |command: &str| denial(&run(&bash(command), &[]).0);
    for command in [
        "sqlite3 ~/.config/playbook/data/config.db \"select * from settings\"",
        "sqlite3 ~/.config/playbook/data/config.db .tables 2>/dev/null",
        "cd ~/.config/playbook/data && sqlite3 config.db .schema",
        "cat ~/.config/playbook/config.json 2>/dev/null",
        "jq . ~/.config/playbook/config.json",
        "ls -la ~/.config/playbook/memory ~/.config/playbook/data > /private/tmp/pb-ls",
        "grep -n hooks ~/.claude/settings.json 2>/dev/null",
        "playbook migrate --status 2>/dev/null",
    ] {
        assert_eq!(reason_of(command), None, "{command}");
    }
    let write = reason_of("sqlite3 ~/.config/playbook/data/config.db \"update settings set v=1\"")
        .unwrap_or_default();
    assert!(write.starts_with("Matched rule: a write"), "{write}");
}

#[test]
fn a_claude_settings_file_cannot_be_written_by_the_model() {
    let dir =
        std::env::temp_dir().join(format!("pb-guard-settings-{}", playbook::testing::run_id()));
    std::fs::create_dir_all(dir.join(".claude")).expect("scratch dir");
    let file = dir.join(".claude/settings.json");
    let kept = r#"{"model":"opus"}"#;
    std::fs::write(&file, kept).expect("settings");
    let path = file.to_string_lossy().into_owned();
    let local = dir
        .join(".claude/settings.local.json")
        .to_string_lossy()
        .into_owned();
    let reason_of = |input: String| denial(&run(&input, &[]).0);
    let tool =
        |name: &str, input: Value| json!({"tool_name": name, "tool_input": input}).to_string();

    let tool_reason = "not with the Edit or Write tool";
    let shell_reason = "not by writing the file";
    let cases = [
        (
            tool(
                "Write",
                json!({"file_path": &path, "content": kept.replace("opus", "sonnet")}),
            ),
            tool_reason,
        ),
        (
            tool("Write", json!({"file_path": &local, "content": "{}"})),
            tool_reason,
        ),
        (
            tool(
                "Edit",
                json!({"file_path": &path, "old_string": "opus", "new_string": "sonnet"}),
            ),
            tool_reason,
        ),
        (
            tool("MultiEdit", json!({"file_path": &path, "edits": []})),
            tool_reason,
        ),
        (bash(&format!("echo '{{}}' > {path}")), shell_reason),
        (bash(&format!("sed -i s/opus/sonnet/ {path}")), shell_reason),
        (
            bash("playbook uninstall --yes"),
            "The hook entries in settings.json",
        ),
    ];
    for (input, expected) in cases {
        let reason = reason_of(input.clone()).unwrap_or_default();
        assert!(reason.contains(expected), "{input}: {reason}");
    }

    // A read, a dry run, a copy out of the file and an edit of another file pass.
    let other = dir.join(".claude/agents.md").to_string_lossy().into_owned();
    for input in [
        bash(&format!("cat {path}")),
        bash(&format!("cp {path} /tmp/settings-backup.json")),
        bash("playbook uninstall --dry-run"),
        tool(
            "Edit",
            json!({"file_path": &other, "old_string": "a", "new_string": "b"}),
        ),
    ] {
        let (out, code, _) = run(&input, &[]);
        assert_eq!((out.as_str(), code), ("", 0), "{input}");
    }
}

fn run_in_home(stdin: &str, home: &std::path::Path, switch: Option<&str>) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_playbook"));
    command
        .args(["hook", "policy-guard"])
        .env("HOME", home)
        .env_remove("POLICY_GUARD")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(value) = switch {
        command.env("POLICY_GUARD", value);
    }
    let mut child = command.spawn().expect("playbook spawns");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into_owned()
}

fn scratch_home(tag: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!(
        "playbook-policy-guard-{}-{tag}",
        playbook::testing::run_id()
    ));
    std::fs::create_dir_all(&home).expect("scratch home should be creatable");
    home
}

fn enable_in(home: &std::path::Path, value: bool) {
    playbook::config::write::set(
        playbook::config::write::Tier::Global,
        "hooks.policyGuard.enabled",
        &json!(value),
        home,
        None,
    )
    .expect("the key should be settable");
}

const DENIED_COMMAND: &str = "git commit --no-verify -m x";

#[test]
fn a_fresh_install_allows_every_call_because_the_guard_is_off_by_default() {
    let home = scratch_home("default-off");

    for input in [
        bash(DENIED_COMMAND),
        bash("gh pr create --title t --body b"),
        write_to(&format!("{}/.claude/settings.json", home.display())),
        write_to(&format!("{}/.claude/memory/x.md", home.display())),
    ] {
        let out = run_in_home(&input, &home, None);

        assert_eq!(denial(&out), None, "{input}");
    }
}

#[test]
fn the_stored_key_turns_the_denials_on_and_false_turns_them_off_again() {
    let home = scratch_home("stored-key");
    enable_in(&home, true);

    let on = run_in_home(&bash(DENIED_COMMAND), &home, None);
    enable_in(&home, false);
    let off = run_in_home(&bash(DENIED_COMMAND), &home, None);

    assert!(denial(&on).is_some(), "{on}");
    assert_eq!(denial(&off), None, "{off}");
}

#[test]
fn the_environment_switch_wins_over_the_stored_key_in_both_directions() {
    let home = scratch_home("env-wins");
    enable_in(&home, true);
    let forced_off = run_in_home(&bash(DENIED_COMMAND), &home, Some("0"));
    enable_in(&home, false);
    let forced_on = run_in_home(&bash(DENIED_COMMAND), &home, Some("1"));

    assert_eq!(denial(&forced_off), None, "{forced_off}");
    assert!(denial(&forced_on).is_some(), "{forced_on}");
}

#[test]
fn the_fact_format_is_still_shown_while_the_guard_is_off() {
    let home = scratch_home("fact-off");
    let path = format!("{}/.config/playbook/memory/o/r/fact.md", home.display());

    let out = run_in_home(&write_to(&path), &home, None);

    assert_eq!(denial(&out), None, "{out}");
    assert!(out.contains("supersedes"), "{out}");
}

#[test]
fn an_unrecognized_environment_value_falls_back_to_the_stored_key() {
    let home = scratch_home("env-other");
    enable_in(&home, true);

    let out = run_in_home(&bash(DENIED_COMMAND), &home, Some("true"));

    assert!(denial(&out).is_some(), "{out}");
}

#[test]
fn the_model_cannot_turn_the_guard_off_with_config_set() {
    let (out, _, _) = run(
        &bash("playbook config set --global hooks.policyGuard.enabled false"),
        &[],
    );

    assert!(denial(&out).is_some(), "{out}");
}
