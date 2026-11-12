// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `playbook sync config apply`, spawning the real
//! binary against a scratch `$HOME` so the prompt, the exit codes and the
//! config store are proven end to end.

use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);
const NO_PROPOSAL: &str = "No sync proposal to apply.";
const PROPOSAL_FILE: &str = "config-proposal.json";

fn scratch_dir(tag: &str) -> PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "playbook-apply-proposal-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("scratch dir should be creatable");
    dir
}

fn proposal_path(home: &Path) -> PathBuf {
    home.join(".config/playbook/data").join(PROPOSAL_FILE)
}

fn write_proposal(home: &Path, proposed: &Value) {
    let from: serde_json::Map<String, Value> = proposed
        .as_object()
        .unwrap()
        .keys()
        .map(|key| (key.clone(), json!("a1b2c3d4")))
        .collect();
    let doc = json!({ "created_at": 1_700_000_000, "proposed": proposed, "from": from });
    let path = proposal_path(home);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, doc.to_string()).unwrap();
}

fn apply(home: &Path, args: &[&str], answer: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["sync", "config", "apply"])
        .args(args)
        .current_dir(scratch_dir("cwd"))
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("playbook binary should spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(answer.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn set_global(home: &Path, key: &str, value: &str) {
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["config", "set", "--global", key, value])
        .current_dir(scratch_dir("cwd"))
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(out.status.success());
}

fn stored(home: &Path) -> Value {
    playbook::config::store::export(&home.join(".config/playbook")).unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_yes_applies_two_valid_keys_after_the_diff_and_deletes_the_file() {
    // Arrange
    let home = scratch_dir("yes");
    set_global(&home, "usage.theme", "light");
    write_proposal(
        &home,
        &json!({"usage.theme": "dark", "usage.budget.monthlyUsd": "250"}),
    );

    // Act
    let out = apply(&home, &[], "yes\n");

    // Assert
    let shown = text(&out.stdout);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(shown.contains("usage.theme: light -> dark (from a1b2c3d4)"));
    assert!(shown.contains("usage.budget.monthlyUsd: (unset) -> 250 (from a1b2c3d4)"));
    assert_eq!(stored(&home)["global"]["usage.theme"], "dark");
    assert_eq!(stored(&home)["global"]["usage.budget.monthlyUsd"], 250);
    assert!(!proposal_path(&home).exists());
}

#[test]
fn a_no_changes_nothing_and_keeps_the_file() {
    // Arrange
    let home = scratch_dir("no");
    set_global(&home, "usage.theme", "light");
    write_proposal(&home, &json!({"usage.theme": "dark"}));
    let before = stored(&home);

    // Act
    let out = apply(&home, &[], "n\n");

    // Assert
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stored(&home), before);
    assert!(proposal_path(&home).exists());
}

#[test]
fn an_empty_answer_counts_as_no() {
    // Arrange
    let home = scratch_dir("eof");
    write_proposal(&home, &json!({"usage.theme": "dark"}));

    // Act
    let out = apply(&home, &[], "");

    // Assert
    assert_eq!(out.status.code(), Some(1));
    assert!(proposal_path(&home).exists());
}

#[test]
fn a_sync_key_and_a_local_only_key_are_rejected_and_listed_never_written() {
    // Arrange
    let home = scratch_dir("rejected");
    write_proposal(
        &home,
        &json!({"sync.url": "https://evil.example", "docs.anydocPath": "/x", "usage.theme": "dark"}),
    );

    // Act
    let out = apply(&home, &["--yes"], "");

    // Assert
    let shown = text(&out.stdout) + &text(&out.stderr);
    assert!(out.status.success(), "{shown}");
    assert!(shown.contains("rejected sync.url"));
    assert!(shown.contains("rejected docs.anydocPath"));
    let global = &stored(&home)["global"];
    assert_eq!(global["usage.theme"], "dark");
    assert!(global.get("sync.url").is_none());
    assert!(global.get("docs.anydocPath").is_none());
}

#[test]
fn an_invalid_value_is_rejected_while_a_valid_sibling_applies() {
    // Arrange
    let home = scratch_dir("invalid");
    write_proposal(
        &home,
        &json!({"usage.theme": "neon", "usage.weekStart": "sunday"}),
    );

    // Act
    let out = apply(&home, &[], "y\n");

    // Assert
    let shown = text(&out.stdout) + &text(&out.stderr);
    assert!(out.status.success(), "{shown}");
    assert!(shown.contains("rejected usage.theme"));
    let global = &stored(&home)["global"];
    assert_eq!(global["usage.weekStart"], "sunday");
    assert!(global.get("usage.theme").is_none());
}

#[test]
fn only_rejected_keys_exit_without_a_prompt() {
    // Arrange
    let home = scratch_dir("none-valid");
    write_proposal(&home, &json!({"sync.url": "https://x.example"}));

    // Act
    let out = apply(&home, &[], "");

    // Assert
    assert!(!text(&out.stdout).contains("Apply"));
    assert!(text(&out.stdout).contains("rejected sync.url"));
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn a_missing_file_prints_the_no_proposal_line_and_succeeds() {
    // Arrange
    let home = scratch_dir("missing");

    // Act
    let out = apply(&home, &[], "");

    // Assert
    assert!(out.status.success());
    assert!(text(&out.stdout).contains(NO_PROPOSAL));
}

#[test]
fn a_corrupt_file_names_itself_fails_and_is_left_alone() {
    // Arrange
    let home = scratch_dir("corrupt");
    let path = proposal_path(&home);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{not json").unwrap();

    // Act
    let out = apply(&home, &["--yes"], "");

    // Assert
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains(PROPOSAL_FILE));
    assert_eq!(fs::read_to_string(&path).unwrap(), "{not json");
}

#[test]
fn yes_skips_the_prompt_and_a_second_run_is_a_no_op() {
    // Arrange
    let home = scratch_dir("idempotent");
    write_proposal(&home, &json!({"usage.theme": "dark"}));

    // Act
    let first = apply(&home, &["--yes"], "");
    let after_first = stored(&home);
    let second = apply(&home, &["--yes"], "");

    // Assert
    assert!(first.status.success());
    assert!(!text(&first.stdout).contains("[y/N]"));
    assert!(second.status.success());
    assert!(text(&second.stdout).contains(NO_PROPOSAL));
    assert_eq!(stored(&home), after_first);
}

#[test]
fn a_project_value_that_shadows_an_applied_key_gets_one_note() {
    // Arrange
    let home = scratch_dir("shadow");
    let repo = scratch_dir("shadow-repo");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success());
    };
    git(&["init", "-q", "-b", "main"]);
    git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/test-owner/test-repo.git",
    ]);
    let set = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["config", "set", "usage.theme", "mono"])
        .current_dir(&repo)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(set.status.success(), "{}", text(&set.stderr));
    write_proposal(&home, &json!({"usage.theme": "dark"}));

    // Act
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["sync", "config", "apply", "--yes"])
        .current_dir(&repo)
        .env("HOME", &home)
        .output()
        .unwrap();

    // Assert
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("project settings override the applied value for usage.theme")
    );
    assert_eq!(stored(&home)["global"]["usage.theme"], "dark");
}
