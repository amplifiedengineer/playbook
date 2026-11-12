// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook mode auto|ask|status`, spawning the real compiled binary so the
//! CLI parsing, exit codes, and repo-tier config write are proven end to end.
//! Every run has `PLAYBOOK_MODE` removed unless a case sets it, and `$HOME`
//! pinned to a scratch directory, so a developer's exported mode or real
//! config can never flip a result.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_dir(tag: &str) -> PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "playbook-mode-cli-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("scratch dir should be creatable");
    dir
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git command should spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A real git repo whose `origin` remote makes the repo slug
/// `test-owner/test-repo`.
fn seeded_repo(tag: &str) -> PathBuf {
    let dir = scratch_dir(tag);
    git_ok(&dir, &["init", "-q", "-b", "main"]);
    git_ok(
        &dir,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/test-owner/test-repo.git",
        ],
    );
    dir
}

fn repo_config_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("playbook")
        .join("repos")
        .join("test-owner")
        .join("test-repo")
        .join(".config")
        .join("config.json")
}

/// The repo tier as stored, `{key: value}`, for the test repo.
fn stored_repo(home: &Path) -> Value {
    playbook::config::store::export(&home.join(".config").join("playbook"))
        .expect("the store should be readable")["repos"]["test-owner/test-repo"]
        .clone()
}

/// Plant `contents` verbatim as the repo tier config under `home`.
fn write_repo_config(home: &Path, contents: &str) {
    let path = repo_config_path(home);
    fs::create_dir_all(path.parent().unwrap()).expect("config dir should be creatable");
    fs::write(path, contents).expect("config file should be writable");
}

/// Run the real binary with the mode env removed, then `env` applied, and
/// `$HOME` pinned.
fn run_playbook(cwd: &Path, home: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_playbook"));
    command
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("PLAYBOOK_MODE");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("playbook binary should spawn")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn status_json(out: &Output) -> Value {
    serde_json::from_str(&stdout_of(out))
        .unwrap_or_else(|e| panic!("status --json should print JSON ({e}): {}", stdout_of(out)))
}

#[test]
fn mode_auto_writes_the_repo_config_and_status_reports_source_config() {
    // Arrange
    let repo = seeded_repo("auto-writes");
    let home = scratch_dir("auto-writes-home");

    // Act
    let set = run_playbook(&repo, &home, &[], &["mode", "auto"]);
    let status = run_playbook(&repo, &home, &[], &["mode", "status"]);

    // Assert
    assert!(set.status.success(), "stderr: {}", stderr_of(&set));
    assert_eq!(stored_repo(&home)["mode"], "auto");
    assert!(status.status.success(), "stderr: {}", stderr_of(&status));
    assert!(
        stdout_of(&status).contains("mode: auto (source: config)"),
        "{}",
        stdout_of(&status)
    );
}

#[test]
fn playbook_mode_ask_in_the_env_beats_config_auto() {
    // Arrange
    let repo = seeded_repo("env-beats-config");
    let home = scratch_dir("env-beats-config-home");
    write_repo_config(&home, r#"{"mode":"auto"}"#);

    // Act
    let out = run_playbook(
        &repo,
        &home,
        &[("PLAYBOOK_MODE", "ask")],
        &["mode", "status"],
    );

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("mode: ask (source: env)"),
        "{}",
        stdout_of(&out)
    );
}

#[test]
fn flag_auto_with_hook_mode_ask_reports_a_non_empty_warning() {
    // Arrange
    let repo = seeded_repo("flag-auto-hook-ask");
    let home = scratch_dir("flag-auto-hook-ask-home");

    // Act
    let out = run_playbook(
        &repo,
        &home,
        &[],
        &["mode", "status", "--json", "--flag", "auto"],
    );

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = status_json(&out);
    assert_eq!(json["mode"], "auto");
    assert_eq!(json["source"], "flag");
    assert_eq!(json["hook_mode"], "ask");
    let warning = json["warning"].as_str().unwrap_or_default();
    assert!(
        warning.starts_with("--auto only changes this command."),
        "warning: {warning:?}"
    );
    assert!(
        warning.contains("the spend cap is off"),
        "warning: {warning:?}"
    );
    assert!(
        warning.contains("Run `playbook mode auto` to turn them on."),
        "warning: {warning:?}"
    );
}

#[test]
fn flag_ask_with_env_auto_warns_that_the_question_tool_is_still_blocked() {
    // Arrange
    let repo = seeded_repo("flag-ask-env-auto");
    let home = scratch_dir("flag-ask-env-auto-home");

    // Act
    let out = run_playbook(
        &repo,
        &home,
        &[("PLAYBOOK_MODE", "auto")],
        &["mode", "status", "--json", "--flag", "ask"],
    );

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = status_json(&out);
    assert_eq!(json["mode"], "ask");
    assert_eq!(json["source"], "flag");
    assert_eq!(json["hook_mode"], "auto");
    let warning = json["warning"].as_str().unwrap_or_default();
    assert!(
        warning.starts_with("--ask only changes this command."),
        "warning: {warning:?}"
    );
    assert!(
        warning.contains("the question tool is still blocked"),
        "warning: {warning:?}"
    );
}

#[test]
fn status_json_has_exactly_the_keys_mode_source_hook_mode_and_warning() {
    // Arrange
    let repo = seeded_repo("json-keys");
    let home = scratch_dir("json-keys-home");
    write_repo_config(&home, r#"{"mode":"auto"}"#);

    // Act
    let out = run_playbook(&repo, &home, &[], &["mode", "status", "--json"]);

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = status_json(&out);
    let mut keys: Vec<&str> = json
        .as_object()
        .expect("status --json should be an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["hook_mode", "mode", "source", "warning"]);
    assert_eq!(json["mode"], "auto");
    assert_eq!(json["source"], "config");
    assert_eq!(json["hook_mode"], "auto");
    assert!(
        json["warning"].is_null(),
        "no disagreement, no warning: {json}"
    );
}

#[test]
fn outside_a_git_repo_status_exits_0_with_source_default() {
    // Arrange
    let cwd = scratch_dir("no-repo-status-cwd");
    let home = scratch_dir("no-repo-status-home");

    // Act
    let out = run_playbook(&cwd, &home, &[], &["mode", "status"]);

    // Assert
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("mode: ask (source: default)"),
        "{}",
        stdout_of(&out)
    );
}

#[test]
fn outside_a_git_repo_mode_auto_exits_1_naming_the_missing_repo() {
    // Arrange
    let cwd = scratch_dir("no-repo-set-cwd");
    let home = scratch_dir("no-repo-set-home");

    // Act
    let out = run_playbook(&cwd, &home, &[], &["mode", "auto"]);

    // Assert
    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout_of(&out));
    let stderr = stderr_of(&out);
    assert!(stderr.starts_with("mode:"), "{stderr}");
    assert!(stderr.contains("repo_slug is unresolved"), "{stderr}");
    assert!(
        !home.join(".config").join("playbook").exists(),
        "nothing should have been written"
    );
}

#[test]
fn flag_banana_is_a_usage_error_with_exit_2() {
    // Arrange
    let repo = seeded_repo("flag-banana");
    let home = scratch_dir("flag-banana-home");

    // Act
    let out = run_playbook(&repo, &home, &[], &["mode", "status", "--flag", "banana"]);

    // Assert
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_of(&out));
}

#[test]
fn other_config_keys_survive_mode_auto_then_mode_ask() {
    // Arrange
    let repo = seeded_repo("keys-survive");
    let home = scratch_dir("keys-survive-home");
    write_repo_config(
        &home,
        r#"{"autoReview":{"enabled":false},"fix":{"maxFiles":7}}"#,
    );

    // Act
    let auto = run_playbook(&repo, &home, &[], &["mode", "auto"]);
    let ask = run_playbook(&repo, &home, &[], &["mode", "ask"]);

    // Assert
    assert!(auto.status.success(), "stderr: {}", stderr_of(&auto));
    assert!(ask.status.success(), "stderr: {}", stderr_of(&ask));
    let written = stored_repo(&home);
    assert_eq!(written["mode"], "ask");
    assert_eq!(written["pr.review.enabled"], false);
    assert_eq!(written["fix.maxFiles"], 7);
}

#[test]
fn mode_auto_twice_leaves_the_stored_values_identical() {
    // Arrange
    let repo = seeded_repo("idempotent");
    let home = scratch_dir("idempotent-home");
    let first = run_playbook(&repo, &home, &[], &["mode", "auto"]);
    assert!(first.status.success(), "stderr: {}", stderr_of(&first));
    let after_first = stored_repo(&home);

    // Act
    let second = run_playbook(&repo, &home, &[], &["mode", "auto"]);

    // Assert
    assert!(second.status.success(), "stderr: {}", stderr_of(&second));
    let after_second = stored_repo(&home);
    assert_eq!(after_first, after_second);
}

#[test]
fn run_context_reads_the_flag_from_the_arguments_and_adds_the_ceiling() {
    // Arrange
    let repo = seeded_repo("run-context");
    let home = scratch_dir("run-context-home");

    // Act
    let out = run_playbook(
        &repo,
        &home,
        &[],
        &["run-context", "--command", "plan", "--args", "12 --auto"],
    );

    // Assert
    assert!(out.status.success(), "{}", stderr_of(&out));
    let v = status_json(&out);
    assert_eq!(v["mode"], "auto");
    assert_eq!(v["source"], "flag");
    assert!(v.get("ceiling").is_some(), "{v}");
    assert!(v.get("warning").is_some() && v.get("hook_mode").is_some());
}

#[test]
fn run_context_without_a_command_leaves_the_ceiling_out() {
    let repo = seeded_repo("run-context-bare");
    let home = scratch_dir("run-context-bare-home");

    let out = run_playbook(&repo, &home, &[], &["run-context", "--args", "--ask"]);

    let v = status_json(&out);
    assert_eq!(v["mode"], "ask");
    assert!(v.get("ceiling").is_none(), "{v}");
}

#[test]
fn run_context_exits_2_when_both_flags_are_given() {
    let repo = seeded_repo("run-context-conflict");
    let home = scratch_dir("run-context-conflict-home");

    let out = run_playbook(
        &repo,
        &home,
        &[],
        &["run-context", "--command", "adr", "--args", "--auto --ask"],
    );

    assert_eq!(out.status.code(), Some(2));
    assert!(stdout_of(&out).is_empty());
    assert!(stderr_of(&out).contains("--auto and --ask conflict; pass one."));
}

#[test]
fn run_context_treats_auto_design_as_auto() {
    let repo = seeded_repo("run-context-design");
    let home = scratch_dir("run-context-design-home");

    let out = run_playbook(
        &repo,
        &home,
        &[],
        &[
            "run-context",
            "--command",
            "plan",
            "--args",
            "idea --auto-design",
        ],
    );

    assert_eq!(status_json(&out)["mode"], "auto");
}
