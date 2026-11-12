// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `playbook config get/set/list`, spawning the real
//! compiled binary so the CLI-layer parsing and exit codes (not just the
//! `config::resolve`/`config::write::set` library calls) are proven. Every
//! test uses a scratch directory standing in for `$HOME`, and either a real
//! scratch git repo with a configured `origin` remote or a plain non-git
//! scratch directory, matching whichever the scenario needs; none touch the
//! developer's real `~/.config/playbook` or `~/.claude`.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh scratch directory under the OS temp dir, unique per call so
/// parallel tests never collide.
fn scratch_dir(tag: &str) -> PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "playbook-config-cli-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("scratch dir should be creatable");
    dir
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git command should spawn")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A real git repo with a configured `origin` remote, so `repo_slug()`
/// resolves to `test-owner/test-repo`. No commit is needed: `repo_slug`
/// only shells out to `git remote get-url origin`.
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

/// Run the real compiled binary with `cwd` and `$HOME` pinned to scratch
/// locations, so it never resolves against the developer's real config.
fn run_playbook(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
    // A run inside Claude Code inherits its telemetry variables, which
    // would count as the user's own setup.
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name == "CLAUDE_CODE_ENABLE_TELEMETRY" || name.starts_with("OTEL_") {
            cmd.env_remove(&*name);
        }
    }
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .output()
        .expect("playbook binary should spawn")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Everything stored, as `config export` prints it.
fn stored(home: &Path) -> Value {
    playbook::config::store::export(&home.join(".config").join("playbook"))
        .expect("the store should be readable")
}

/// Whether `field` (`global`, `orgs` or `repos`) holds nothing yet.
fn empty(home: &Path, field: &str) -> bool {
    stored(home)[field]
        .as_object()
        .is_none_or(serde_json::Map::is_empty)
}

fn global_config_path(home: &Path) -> PathBuf {
    home.join(".config").join("playbook").join("config.json")
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

#[test]
fn get_with_nothing_configured_prints_default_labeled_as_default() {
    // Arrange
    let repo = seeded_repo("get-default");
    let home = scratch_dir("get-default-home");

    // Act
    let out = run_playbook(&repo, &home, &["config", "get", "pr.review.enabled"]);

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("pr.review.enabled"), "{stdout}");
    assert!(stdout.contains("true"), "{stdout}");
    assert!(stdout.contains("(source: default)"), "{stdout}");
}

#[test]
fn set_without_flags_writes_repo_tier_and_get_reflects_it() {
    // Arrange: seed the repo tier via a real `set` call before observing it.
    let repo = seeded_repo("set-repo-tier");
    let home = scratch_dir("set-repo-tier-home");
    let seed = run_playbook(
        &repo,
        &home,
        &["config", "set", "pr.review.enabled", "false"],
    );
    assert!(seed.status.success(), "stderr: {}", stderr_of(&seed));

    // Act
    let out = run_playbook(&repo, &home, &["config", "get", "pr.review.enabled"]);

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("false"), "{stdout}");
    assert!(stdout.contains("(source: repo)"), "{stdout}");
}

#[test]
fn update_keys_go_to_the_global_tier_without_the_flag_and_refuse_org() {
    // Arrange
    let repo = seeded_repo("set-update-key");
    let home = scratch_dir("set-update-key-home");

    // Act: a piped debug build never starts the background release check,
    // so neither run reaches the network.
    let org = run_playbook(
        &repo,
        &home,
        &["config", "set", "upgrade.mode", "off", "--org"],
    );
    let plain = run_playbook(&repo, &home, &["config", "set", "upgrade.mode", "auto"]);

    // Assert
    assert!(plain.status.success(), "stderr: {}", stderr_of(&plain));
    assert_eq!(
        stored(&home)["global"]["upgrade.mode"],
        Value::String("auto".into())
    );
    assert!(empty(&home, "repos") && empty(&home, "orgs"));
    assert!(!org.status.success());
    assert!(stderr_of(&org).contains("--global"), "{}", stderr_of(&org));
}

#[test]
fn a_stale_repo_tier_update_key_is_not_reported_as_the_value() {
    // Arrange: a repo row an older binary could have written.
    let repo = seeded_repo("get-update-key");
    let home = scratch_dir("get-update-key-home");
    playbook::config::store::put(
        &home.join(".config").join("playbook"),
        playbook::config::write::Tier::Repo,
        Some("test-owner/test-repo"),
        "upgrade.mode",
        &Value::String("off".into()),
    )
    .unwrap();

    // Act
    let out = run_playbook(&repo, &home, &["config", "get", "upgrade.mode"]);

    // Assert
    let stdout = stdout_of(&out);
    assert!(stdout.contains("notify"), "{stdout}");
    assert!(stdout.contains("(source: default)"), "{stdout}");
}

#[test]
fn set_with_global_flag_writes_the_global_tier_file() {
    // Arrange
    let repo = seeded_repo("set-global-tier");
    let home = scratch_dir("set-global-tier-home");

    // Act
    let out = run_playbook(
        &repo,
        &home,
        &["config", "set", "pr.review.type", "quick", "--global"],
    );

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stored(&home)["global"]["pr.review.type"],
        Value::String("quick".into())
    );
    assert!(
        empty(&home, "orgs"),
        "org tier should not have been written"
    );
    assert!(
        empty(&home, "repos"),
        "repo tier should not have been written"
    );
}

#[test]
fn get_prints_a_string_value_unquoted_not_as_raw_json() {
    // Arrange: prose consumers (the finish-pull-request skill, doctor.md's
    // effective-config line) match this output as a bare `auto`/`quick`
    // token, not a JSON-quoted `"auto"`.
    let repo = seeded_repo("get-string-unquoted");
    let home = scratch_dir("get-string-unquoted-home");

    // Act
    let out = run_playbook(&repo, &home, &["config", "get", "pr.review.type"]);

    // Assert
    let stdout = stdout_of(&out);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert!(
        stdout.contains("pr.review.type: auto (source: default)"),
        "{stdout}"
    );
    assert!(!stdout.contains("\"auto\""), "{stdout}");
}

#[test]
fn set_with_both_org_and_global_flags_exits_non_zero_and_writes_nothing() {
    // Arrange
    let repo = seeded_repo("set-org-and-global");
    let home = scratch_dir("set-org-and-global-home");

    // Act
    let out = run_playbook(
        &repo,
        &home,
        &[
            "config",
            "set",
            "pr.review.enabled",
            "false",
            "--org",
            "--global",
        ],
    );

    // Assert
    assert!(!out.status.success());
    assert!(!stderr_of(&out).is_empty());
    assert!(empty(&home, "global"));
    assert!(empty(&home, "orgs"));
}

#[test]
fn get_with_a_typo_d_key_exits_non_zero_naming_the_valid_keys() {
    // Arrange
    let repo = seeded_repo("get-typo-key");
    let home = scratch_dir("get-typo-key-home");

    // Act
    let out = run_playbook(&repo, &home, &["config", "get", "autoReiw.enabled"]);

    // Assert
    assert!(!out.status.success());
    let stderr = stderr_of(&out);
    assert!(stderr.contains("unknown config key"), "{stderr}");
    assert!(stderr.contains("pr.review.enabled"), "{stderr}");
    assert!(stderr.contains("pr.review.type"), "{stderr}");
}

#[test]
fn set_with_an_unparseable_bool_value_exits_non_zero_and_writes_nothing() {
    // Arrange
    let repo = seeded_repo("set-notabool");
    let home = scratch_dir("set-notabool-home");

    // Act
    let out = run_playbook(
        &repo,
        &home,
        &["config", "set", "pr.review.enabled", "notabool"],
    );

    // Assert
    assert!(!out.status.success());
    assert!(!stderr_of(&out).is_empty());
    assert!(
        empty(&home, "repos"),
        "repo tier should not have been written"
    );
}

#[test]
fn set_with_an_out_of_enum_value_exits_non_zero_and_writes_nothing() {
    // Arrange
    let repo = seeded_repo("set-out-of-enum");
    let home = scratch_dir("set-out-of-enum-home");

    // Act
    let out = run_playbook(&repo, &home, &["config", "set", "pr.review.type", "medium"]);

    // Assert
    assert!(!out.status.success());
    assert!(!stderr_of(&out).is_empty());
    assert!(
        empty(&home, "repos"),
        "repo tier should not have been written"
    );
}

#[test]
fn list_shows_known_keys_with_value_and_source_tier() {
    // Arrange
    let repo = seeded_repo("list-keys");
    let home = scratch_dir("list-keys-home");

    // Act
    let out = run_playbook(&repo, &home, &["config", "list"]);

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("pr.review.enabled"), "{stdout}");
    assert!(stdout.contains("pr.review.type"), "{stdout}");
    assert!(stdout.contains("(source: default)"), "{stdout}");
    assert!(stdout.contains("worktree.cleanup.enabled"), "{stdout}");
    assert!(
        stdout.contains("worktree.cleanup.enabled: true"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("worktreeCleanup.staleAfterDays"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("worktreeCleanup.conflictGracePeriodDays"),
        "{stdout}"
    );
}

#[test]
fn set_with_a_numeric_value_writes_it_and_get_reflects_it() {
    // Arrange: regression pin for the CLI's string-to-Value parser rejecting
    // a valid numeric string for a numeric key such as fix.maxFiles.
    let repo = seeded_repo("set-numeric");
    let home = scratch_dir("set-numeric-home");
    let seed = run_playbook(&repo, &home, &["config", "set", "fix.maxFiles", "14"]);
    assert!(seed.status.success(), "stderr: {}", stderr_of(&seed));

    // Act
    let out = run_playbook(&repo, &home, &["config", "get", "fix.maxFiles"]);

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("14"), "{stdout}");
    assert!(stdout.contains("(source: repo)"), "{stdout}");
}

#[test]
fn get_outside_any_git_repo_still_works_falling_through_to_default() {
    // Arrange: a scratch dir with no `.git` at all.
    let cwd = scratch_dir("get-no-git");
    let home = scratch_dir("get-no-git-home");

    // Act
    let out = run_playbook(&cwd, &home, &["config", "get", "pr.review.enabled"]);

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert!(!stderr_of(&out).contains("panicked"));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("(source: default)"), "{stdout}");
}

#[test]
fn list_outside_any_git_repo_still_works_falling_through_to_default() {
    // Arrange: a scratch dir with no `.git` at all.
    let cwd = scratch_dir("list-no-git");
    let home = scratch_dir("list-no-git-home");

    // Act
    let out = run_playbook(&cwd, &home, &["config", "list"]);

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert!(!stderr_of(&out).contains("panicked"));
}

#[test]
fn set_outside_any_git_repo_with_default_repo_tier_fails_clearly_not_a_crash() {
    // Arrange: a scratch dir with no `.git` at all, so the repo tier `set`
    // defaults to has no repo context to build a path from.
    let cwd = scratch_dir("set-no-git");
    let home = scratch_dir("set-no-git-home");

    // Act
    let out = run_playbook(
        &cwd,
        &home,
        &["config", "set", "pr.review.enabled", "false"],
    );

    // Assert
    assert!(!out.status.success());
    let stderr = stderr_of(&out);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(!stderr.is_empty());
}

/// Runs `config set --global -- <key> <value>` and returns the output. The
/// `--` lets a negative value reach the validator instead of the flag parser.
fn set_global(home: &Path, key: &str, value: &str) -> Output {
    let cwd = scratch_dir("set-validated-cwd");
    run_playbook(&cwd, home, &["config", "set", "--global", "--", key, value])
}

#[test]
fn set_rejects_an_out_of_range_auto_or_fix_value_with_a_clear_message_and_writes_nothing() {
    // Arrange: (key, value, what the message must say)
    let rows = [
        ("auto.budget.mainUsd", "0", "at least 0.01"),
        ("auto.budget.mainUsd", "-5", "at least 0.01"),
        ("auto.budget.mainUsd", "0.004", "at least 0.01"),
        ("auto.budget.mainUsd", "abc", "auto.budget.mainUsd"),
        ("auto.budget.agentUsd", "0", "at least 0.01"),
        ("auto.budget.agentUsd", "0.004", "at least 0.01"),
        ("auto.budget.ceilingUsd", "-1", "at least 0.01"),
        ("auto.budget.ceilingUsd", "0.004", "at least 0.01"),
        ("fix.maxFiles", "0", "positive integer"),
        ("fix.maxLines", "0", "positive integer"),
    ];
    for (key, value, message) in rows {
        let home = scratch_dir("set-validated-home");

        // Act
        let out = set_global(&home, key, value);

        // Assert
        assert!(!out.status.success(), "{key} {value} must exit non-zero");
        let stderr = stderr_of(&out);
        assert!(stderr.contains(key), "{key} {value}: {stderr}");
        assert!(stderr.contains(message), "{key} {value}: {stderr}");
        assert!(!stderr.contains("panicked"), "{key} {value}: {stderr}");
        assert!(
            empty(&home, "global"),
            "{key} {value}: nothing may be written"
        );
    }
}

#[test]
fn set_writes_valid_auto_and_fix_values_including_a_fractional_budget() {
    // Arrange
    let rows = [
        ("auto.budget.mainUsd", "2.5"),
        ("auto.budget.agentUsd", "0.5"),
        ("auto.budget.ceilingUsd", "0"),
        ("auto.budget.ceilingUsd", "200"),
        ("auto.budget.ceilingUsd", "4000"),
        ("watch.limits.dailyUsd", "0.5"),
        ("fix.maxFiles", "99"),
        ("fix.maxLines", "1"),
    ];
    for (key, value) in rows {
        let home = scratch_dir("set-valid-home");

        // Act
        let out = set_global(&home, key, value);

        // Assert
        assert!(out.status.success(), "{key} {value}: {}", stderr_of(&out));
        assert_eq!(stored(&home)["global"][key].to_string(), value, "{key}");
    }
}

#[test]
fn set_keeps_a_set_session_ceiling_at_or_above_both_budgets() {
    // Arrange: rows are (stored first, then the set, whether it is accepted,
    // what a refusal must say). The defaults are 200 and 50.
    type Row<'a> = (&'a [(&'a str, &'a str)], (&'a str, &'a str), bool, &'a str);
    let rows: [Row; 8] = [
        (
            &[],
            ("auto.budget.ceilingUsd", "199.99"),
            false,
            "below auto.budget.mainUsd (200)",
        ),
        (&[], ("auto.budget.ceilingUsd", "200"), true, ""),
        (
            &[("auto.budget.mainUsd", "10")],
            ("auto.budget.ceilingUsd", "40"),
            false,
            "below auto.budget.agentUsd (50)",
        ),
        (
            &[("auto.budget.mainUsd", "10"), ("auto.budget.agentUsd", "5")],
            ("auto.budget.ceilingUsd", "40"),
            true,
            "",
        ),
        (
            &[("auto.budget.ceilingUsd", "300")],
            ("auto.budget.mainUsd", "301"),
            false,
            "Raise the ceiling first",
        ),
        (
            &[("auto.budget.ceilingUsd", "300")],
            ("auto.budget.agentUsd", "300"),
            true,
            "",
        ),
        (
            &[("auto.budget.ceilingUsd", "300")],
            ("auto.budget.agentUsd", "300.01"),
            false,
            "above auto.budget.ceilingUsd (300)",
        ),
        (
            &[("auto.budget.ceilingUsd", "0")],
            ("auto.budget.mainUsd", "5000"),
            true,
            "",
        ),
    ];
    for (n, (before, (key, value), accepted, message)) in rows.into_iter().enumerate() {
        let home = scratch_dir(&format!("cap-order-{n}"));
        for (k, v) in before {
            assert!(
                set_global(&home, k, v).status.success(),
                "{n}: seed {k} {v}"
            );
        }

        // Act
        let out = set_global(&home, key, value);

        // Assert
        let stderr = stderr_of(&out);
        assert_eq!(
            out.status.success(),
            accepted,
            "{n}: {key} {value}: {stderr}"
        );
        assert!(stderr.contains(message), "{n}: {key} {value}: {stderr}");
        if !accepted {
            assert!(
                stored(&home)["global"].get(key).is_none(),
                "{n}: nothing may be written"
            );
        }
    }
}

#[test]
fn setting_a_budget_checks_only_that_budget_against_the_ceiling() {
    // Arrange: a ceiling of 30 stored before the check existed, below the
    // default main budget of 200
    let home = scratch_dir("cap-order-own-key");
    let root = home.join(".config").join("playbook");
    playbook::config::store::put(
        &root,
        playbook::config::write::Tier::Global,
        None,
        "auto.budget.ceilingUsd",
        &serde_json::json!(30),
    )
    .expect("seed");

    // Act
    let lower_agent = set_global(&home, "auto.budget.agentUsd", "10");
    let lower_main = set_global(&home, "auto.budget.mainUsd", "10");

    // Assert: lowering a budget under the ceiling always works
    assert!(lower_agent.status.success(), "{}", stderr_of(&lower_agent));
    assert!(lower_main.status.success(), "{}", stderr_of(&lower_main));
}

#[test]
fn a_global_write_is_checked_inside_the_repo_and_outside_any_repo() {
    // Arrange: this repo raises its own ceiling to 1000 and its budget to 300
    let repo = seeded_repo("cap-order-tiers");
    let home = scratch_dir("cap-order-tiers-home");
    for (key, value) in [
        ("auto.budget.ceilingUsd", "1000"),
        ("auto.budget.mainUsd", "300"),
    ] {
        let out = run_playbook(&repo, &home, &["config", "set", key, value]);
        assert!(out.status.success(), "{key}: {}", stderr_of(&out));
    }
    let global_ceiling = run_playbook(
        &repo,
        &home,
        &["config", "set", "--global", "auto.budget.ceilingUsd", "250"],
    );
    let outside = set_global(&home, "auto.budget.ceilingUsd", "250");

    // Act: a global budget above the global ceiling, written from the repo
    let global_budget = run_playbook(
        &repo,
        &home,
        &["config", "set", "--global", "auto.budget.mainUsd", "800"],
    );

    // Assert: the repo's own budget of 300 refuses a global ceiling of 250
    // from inside it; outside any repo the default budget of 200 allows it
    assert!(
        !global_ceiling.status.success(),
        "a repo budget of 300 is above 250"
    );
    assert!(stderr_of(&global_ceiling).contains("below auto.budget.mainUsd (300)"));
    assert!(outside.status.success(), "{}", stderr_of(&outside));
    // The repo ceiling of 1000 does not hide the global ceiling of 250
    assert!(
        !global_budget.status.success(),
        "800 is above the global 250"
    );
    assert!(stderr_of(&global_budget).contains("above auto.budget.ceilingUsd (250)"));
}

#[test]
fn the_pr_and_commit_settings_default_then_follow_a_global_set() {
    // Arrange
    let repo = seeded_repo("pr-settings");
    let home = scratch_dir("pr-settings-home");
    let rows = [
        ("pr.review.fix", "false", "true"),
        ("pr.merge.enabled", "false", "true"),
        ("commit.signOff", "true", "false"),
        ("pr.draft", "true", "false"),
    ];

    for (key, default, flipped) in rows {
        // Act
        let before = run_playbook(&repo, &home, &["config", "get", key]);
        let bad = run_playbook(&repo, &home, &["config", "set", "--global", key, "bad"]);
        let write = run_playbook(&repo, &home, &["config", "set", "--global", key, flipped]);
        let after = run_playbook(&repo, &home, &["config", "get", key]);

        // Assert
        assert_eq!(
            stdout_of(&before).trim(),
            format!("{key}: {default} (source: default)")
        );
        assert!(!bad.status.success(), "{key} = bad must be rejected");
        assert!(write.status.success(), "stderr: {}", stderr_of(&write));
        assert_eq!(
            stdout_of(&after).trim(),
            format!("{key}: {flipped} (source: global)")
        );
    }
}

/// Plants `contents` as the config file at `path`, creating its directory.
fn plant_config(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("config path has a parent")).expect("config dir");
    fs::write(path, contents).expect("config file should be writable");
}

#[test]
fn get_and_list_name_the_tier_of_an_invalid_value_they_ignore() {
    // Arrange: an invalid repo value over a valid global one. The invalid
    // value is dropped for the default, and the lower tier is not consulted.
    let repo = seeded_repo("ignored-tier");
    let home = scratch_dir("ignored-tier-home");
    plant_config(&repo_config_path(&home), r#"{"mode":"banana"}"#);
    plant_config(&global_config_path(&home), r#"{"mode":"auto"}"#);
    let expected = "mode: ask (source: default, repo value ignored)";

    for (command, args) in [
        ("config get", vec!["config", "get", "mode"]),
        ("config list", vec!["config", "list"]),
    ] {
        // Act
        let out = run_playbook(&repo, &home, &args);

        // Assert
        assert!(out.status.success(), "{command}: {}", stderr_of(&out));
        let stdout = stdout_of(&out);
        assert!(stdout.contains(expected), "{command}: {stdout}");
        let stderr = stderr_of(&out);
        assert!(
            stderr.contains(&format!("{command}: warning")) && stderr.contains("banana"),
            "{command}: {stderr}"
        );
    }
}

#[test]
fn set_accepts_auto_for_the_review_type() {
    // Arrange
    let repo = seeded_repo("set-review-auto");
    let home = scratch_dir("set-review-auto-home");

    // Act
    let out = run_playbook(&repo, &home, &["config", "set", "pr.review.type", "auto"]);

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
}

#[test]
fn export_then_import_round_trips_through_the_cli() {
    // Arrange
    let repo = seeded_repo("export-import");
    let home = scratch_dir("export-import-home");
    let other = scratch_dir("export-import-other");
    assert!(run_playbook(
        &repo,
        &home,
        &["config", "set", "pr.draft", "false", "--global"]
    )
    .status
    .success());
    assert!(
        run_playbook(&repo, &home, &["config", "set", "fix.maxFiles", "9"])
            .status
            .success()
    );

    // Act
    let exported = run_playbook(&repo, &home, &["config", "export"]);
    let file = home.join("saved.json");
    fs::write(&file, stdout_of(&exported)).unwrap();
    let imported = run_playbook(&repo, &other, &["config", "import", file.to_str().unwrap()]);
    let got = run_playbook(&repo, &other, &["config", "get", "fix.maxFiles"]);

    // Assert
    assert!(exported.status.success(), "{}", stderr_of(&exported));
    assert!(imported.status.success(), "{}", stderr_of(&imported));
    assert!(
        stdout_of(&imported).contains("2 setting(s)"),
        "{}",
        stdout_of(&imported)
    );
    assert!(stdout_of(&got).contains('9'), "{}", stdout_of(&got));
    assert!(
        stdout_of(&got).contains("(source: repo)"),
        "{}",
        stdout_of(&got)
    );
}

#[test]
fn import_of_an_invalid_document_stores_nothing_and_exits_1() {
    // Arrange
    let repo = seeded_repo("import-bad");
    let home = scratch_dir("import-bad-home");
    let file = home.join("bad.json");
    fs::write(&file, r#"{"global":{"mode":"chaos"}}"#).unwrap();

    // Act
    let out = run_playbook(&repo, &home, &["config", "import", file.to_str().unwrap()]);

    // Assert
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("mode"), "{}", stderr_of(&out));
    assert!(empty(&home, "global"));
}

#[test]
fn a_config_json_written_after_the_import_is_adopted_when_it_agrees() {
    // Arrange: the store exists, then a file shows up with a new key.
    let home = scratch_dir("late-adopt");
    let cwd = scratch_dir("late-adopt-cwd");
    assert!(set_global(&home, "mode", "ask").status.success());
    fs::write(
        global_config_path(&home),
        r#"{"mode":"ask","pr":{"draft":false}}"#,
    )
    .unwrap();

    // Act
    let out = run_playbook(&cwd, &home, &["config", "get", "pr.draft"]);

    // Assert: adopted without a warning, file renamed.
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("false"), "{}", stdout_of(&out));
    assert!(!stderr_of(&out).contains("warning"), "{}", stderr_of(&out));
    assert!(!global_config_path(&home).exists());
}

#[test]
fn a_config_json_that_disagrees_with_the_store_is_left_and_named_in_a_warning() {
    let home = scratch_dir("late-conflict");
    let cwd = scratch_dir("late-conflict-cwd");
    assert!(set_global(&home, "mode", "ask").status.success());
    fs::write(global_config_path(&home), r#"{"mode":"auto"}"#).unwrap();

    let out = run_playbook(&cwd, &home, &["config", "get", "mode"]);

    assert!(out.status.success());
    assert!(stdout_of(&out).contains("ask"), "{}", stdout_of(&out));
    let err = stderr_of(&out);
    assert!(
        err.contains("config.json") && err.contains("playbook config set"),
        "{err}"
    );
    assert!(
        global_config_path(&home).exists(),
        "the file is not touched"
    );
}

fn claude_settings(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

#[test]
fn setting_telemetry_to_auto_writes_the_claude_keys_and_says_how_to_undo() {
    // Arrange
    let repo = seeded_repo("tel-sync-on");
    let home = scratch_dir("tel-sync-on-home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(claude_settings(&home), r#"{"env":{"FOO":"1"}}"#).unwrap();

    // Act
    let out = run_playbook(&repo, &home, &["config", "set", "usage.telemetry", "auto"]);

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("+ CLAUDE_CODE_ENABLE_TELEMETRY=1"),
        "{stdout}"
    );
    assert!(stdout.contains("<token>"), "{stdout}");
    assert!(
        stdout.contains("OTEL_LOG_TOOL_DETAILS stays off"),
        "{stdout}"
    );
    assert!(
        stdout.contains("playbook config set usage.telemetry off"),
        "{stdout}"
    );
    let written = fs::read_to_string(claude_settings(&home)).unwrap();
    assert!(written.contains("\"FOO\""), "{written}");
    assert!(!written.contains("OTEL_LOG_TOOL_DETAILS"), "{written}");
}

#[test]
fn setting_telemetry_to_off_removes_only_the_owned_keys() {
    // Arrange
    let repo = seeded_repo("tel-sync-off");
    let home = scratch_dir("tel-sync-off-home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(claude_settings(&home), r#"{"env":{"FOO":"1"}}"#).unwrap();
    let on = run_playbook(&repo, &home, &["config", "set", "usage.telemetry", "auto"]);
    assert!(on.status.success(), "stderr: {}", stderr_of(&on));

    // Act
    let out = run_playbook(&repo, &home, &["config", "set", "usage.telemetry", "off"]);

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("  - OTEL_LOGS_EXPORTER=otlp"), "{stdout}");
    assert!(
        stdout.contains("playbook config set usage.telemetry auto"),
        "{stdout}"
    );
    let root: Value = serde_json::from_str(&fs::read_to_string(claude_settings(&home)).unwrap())
        .expect("settings stay valid JSON");
    assert_eq!(root["env"], serde_json::json!({"FOO": "1"}));
}

#[test]
fn setting_telemetry_with_a_malformed_claude_file_leaves_it_untouched() {
    // Arrange
    let repo = seeded_repo("tel-sync-bad");
    let home = scratch_dir("tel-sync-bad-home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(claude_settings(&home), "{ not json").unwrap();

    // Act
    let out = run_playbook(&repo, &home, &["config", "set", "usage.telemetry", "auto"]);

    // Assert
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("were not changed"),
        "{}",
        stderr_of(&out)
    );
    assert_eq!(
        fs::read_to_string(claude_settings(&home)).unwrap(),
        "{ not json"
    );
}

#[test]
fn set_of_a_shared_global_key_refreshes_the_sync_files() {
    // Arrange
    let cwd = scratch_dir("sync-set-cwd");
    let home = scratch_dir("sync-set-home");

    // Act
    let out = run_playbook(
        &cwd,
        &home,
        &["config", "set", "--global", "usage.theme", "dark"],
    );

    // Assert
    assert!(out.status.success(), "{}", stderr_of(&out));
    let data = home.join(".config").join("playbook").join("data");
    let values: Value =
        serde_json::from_str(&fs::read_to_string(data.join("config-values.json")).unwrap())
            .unwrap();
    assert_eq!(values["values"]["usage.theme"], "dark");
    assert!(data.join("sync-keys.json").is_file());
}

#[test]
fn set_of_a_local_key_writes_no_sync_files() {
    // Arrange
    let cwd = scratch_dir("sync-local-cwd");
    let home = scratch_dir("sync-local-home");

    // Act
    let out = run_playbook(&cwd, &home, &["config", "set", "--global", "mode", "auto"]);

    // Assert
    assert!(out.status.success(), "{}", stderr_of(&out));
    let data = home.join(".config").join("playbook").join("data");
    assert!(!data.join("config-values.json").exists());
}

#[test]
fn set_still_succeeds_with_a_warning_when_the_sync_files_cannot_be_written() {
    // Arrange
    let cwd = scratch_dir("sync-warn-cwd");
    let home = scratch_dir("sync-warn-home");
    let data = home.join(".config").join("playbook").join("data");
    fs::create_dir_all(data.join("sync-keys.json")).unwrap();

    // Act
    let out = run_playbook(
        &cwd,
        &home,
        &["config", "set", "--global", "usage.theme", "dark"],
    );

    // Assert
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("sync files not refreshed"));
    assert_eq!(stored(&home)["global"]["usage.theme"], "dark");
}

#[test]
fn sync_files_command_writes_both_files_and_fails_clearly_when_blocked() {
    // Arrange
    let cwd = scratch_dir("sync-cmd-cwd");
    let home = scratch_dir("sync-cmd-home");
    let data = home.join(".config").join("playbook").join("data");

    // Act
    let ok = run_playbook(&cwd, &home, &["config", "sync-files"]);
    fs::remove_file(data.join("sync-keys.json")).unwrap();
    fs::create_dir_all(data.join("sync-keys.json")).unwrap();
    let blocked = run_playbook(&cwd, &home, &["config", "sync-files"]);

    // Assert
    assert!(ok.status.success(), "{}", stderr_of(&ok));
    assert!(!blocked.status.success());
    assert!(stderr_of(&blocked).contains("config sync-files: cannot write"));
}

const RETIRED_DEAD_KEYS: [(&str, &str); 3] = [
    ("sessions.autoClose", "off"),
    ("launcher.claudeOverride", "false"),
    ("agents.variants", "auto"),
];

#[test]
fn a_retired_key_is_refused_by_set() {
    for (key, value) in RETIRED_DEAD_KEYS {
        // Arrange
        let repo = seeded_repo("retired-set");
        let home = scratch_dir("retired-set-home");

        // Act
        let out = run_playbook(&repo, &home, &["config", "set", "--global", key, value]);

        // Assert
        assert!(!out.status.success(), "{key}");
        assert!(stderr_of(&out).contains(key), "{key}: {}", stderr_of(&out));
        assert!(empty(&home, "global"), "{key}");
    }
}

#[test]
fn import_skips_a_retired_key() {
    for (key, _) in RETIRED_DEAD_KEYS {
        // Arrange
        let repo = seeded_repo("retired-import");
        let home = scratch_dir("retired-import-home");
        let file = home.join("doc.json");
        fs::write(
            &file,
            format!(r#"{{"global": {{"{key}": "off", "pr.draft": false}}}}"#),
        )
        .unwrap();

        // Act
        let out = run_playbook(&repo, &home, &["config", "import", file.to_str().unwrap()]);

        // Assert
        assert!(out.status.success(), "{key}: {}", stderr_of(&out));
        assert!(
            stdout_of(&out).contains("1 setting(s)"),
            "{key}: {}",
            stdout_of(&out)
        );
        assert!(stored(&home)["global"].get(key).is_none(), "{key}");
    }
}

#[test]
fn a_stored_retired_value_does_not_break_list() {
    for (key, _) in RETIRED_DEAD_KEYS {
        // Arrange
        let repo = seeded_repo("retired-list");
        let home = scratch_dir("retired-list-home");
        let root = home.join(".config").join("playbook");
        playbook::config::store::put(
            &root,
            playbook::config::write::Tier::Global,
            None,
            key,
            &serde_json::json!("off"),
        )
        .unwrap();

        // Act
        let out = run_playbook(&repo, &home, &["config", "list"]);

        // Assert
        assert!(out.status.success(), "{key}: {}", stderr_of(&out));
        assert!(!stdout_of(&out).contains(key), "{key}: {}", stdout_of(&out));
    }
}
