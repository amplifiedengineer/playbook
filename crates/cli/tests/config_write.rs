// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `playbook::config::write::set`: creating and
//! merging tier files, rejecting an invalid key, value, or missing repo
//! context before any write happens, and concurrent-writer safety.

use playbook::config::write::{set, Tier};
use playbook::config::ConfigError;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh scratch directory standing in for `$HOME`, unique per call so
/// parallel tests never collide.
fn scratch_home(tag: &str) -> PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "playbook-config-write-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("scratch home should be creatable");
    dir
}

fn repo_config_path(home: &Path, owner: &str, repo: &str) -> PathBuf {
    home.join(".config")
        .join("playbook")
        .join("repos")
        .join(owner)
        .join(repo)
        .join(".config")
        .join("config.json")
}

fn root(home: &Path) -> PathBuf {
    home.join(".config").join("playbook")
}

fn db(home: &Path) -> PathBuf {
    playbook::config::store::db_path(&root(home))
}

/// Everything stored, as `config export` prints it.
fn stored(home: &Path) -> Value {
    playbook::config::store::export(&root(home)).expect("the store should be readable")
}

#[test]
fn writing_to_an_absent_tier_creates_the_database_and_the_row() {
    // Arrange
    let home = scratch_home("fresh");

    // Act
    let result = set(
        Tier::Repo,
        "autoReview.enabled",
        &json!(false),
        &home,
        Some("owner/repo"),
    );

    // Assert
    assert!(result.is_ok());
    assert_eq!(
        stored(&home)["repos"]["owner/repo"],
        json!({"autoReview.enabled": false})
    );

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn set_keeps_the_other_keys_of_the_same_tier() {
    // Arrange
    let home = scratch_home("merge");
    set(
        Tier::Repo,
        "autoReview.type",
        &json!("quick"),
        &home,
        Some("owner/repo"),
    )
    .unwrap();

    // Act
    let result = set(
        Tier::Repo,
        "autoReview.enabled",
        &json!(true),
        &home,
        Some("owner/repo"),
    );

    // Assert
    assert!(result.is_ok());
    assert_eq!(
        stored(&home)["repos"]["owner/repo"],
        json!({"autoReview.type": "quick", "autoReview.enabled": true})
    );

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn an_unknown_key_is_rejected_and_writes_nothing() {
    // Arrange
    let home = scratch_home("unknown-key");

    // Act
    let result = set(
        Tier::Repo,
        "notAKnownKey",
        &json!(true),
        &home,
        Some("owner/repo"),
    );

    // Assert
    let err = result.unwrap_err();
    assert!(matches!(&err, ConfigError::UnknownKey(k) if k == "notAKnownKey"));
    assert!(err.to_string().contains("autoReview.enabled"));
    assert!(!db(&home).exists());
}

#[test]
fn a_wrong_typed_value_is_rejected_and_writes_nothing() {
    // Arrange
    let home = scratch_home("wrong-type");

    // Act
    let result = set(
        Tier::Repo,
        "autoReview.enabled",
        &json!("not-a-bool"),
        &home,
        Some("owner/repo"),
    );

    // Assert
    assert!(matches!(result.unwrap_err(), ConfigError::WrongType { .. }));
    assert!(!db(&home).exists());
}

#[test]
fn an_out_of_enum_value_is_rejected_and_writes_nothing() {
    // Arrange
    let home = scratch_home("bad-enum");

    // Act
    let result = set(
        Tier::Repo,
        "autoReview.type",
        &json!("medium"),
        &home,
        Some("owner/repo"),
    );

    // Assert
    assert!(matches!(
        result.unwrap_err(),
        ConfigError::InvalidEnumValue { .. }
    ));
    assert!(!db(&home).exists());
}

#[test]
fn repo_tier_with_no_repo_slug_is_rejected_and_writes_nothing() {
    // Arrange
    let home = scratch_home("no-repo-slug");

    // Act
    let result = set(Tier::Repo, "autoReview.enabled", &json!(true), &home, None);

    // Assert
    assert!(matches!(
        result.unwrap_err(),
        ConfigError::MissingRepoContext
    ));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn an_uncreatable_directory_is_rejected_with_a_path_naming_error() {
    // Arrange: a regular file occupies the playbook root, so the database
    // directory cannot be created.
    let home = scratch_home("uncreatable-dir");
    let blocking_path = root(&home);
    fs::create_dir_all(blocking_path.parent().unwrap()).unwrap();
    fs::write(&blocking_path, "not a directory").unwrap();

    // Act
    let result = set(
        Tier::Repo,
        "autoReview.enabled",
        &json!(true),
        &home,
        Some("owner/repo"),
    );

    // Assert
    match result.unwrap_err() {
        ConfigError::DirectoryUnwritable(path) => {
            assert!(path.starts_with(&blocking_path));
        }
        other => panic!("expected DirectoryUnwritable, got {other:?}"),
    }

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_non_object_existing_tier_file_is_rejected_rather_than_overwritten() {
    // Arrange
    let home = scratch_home("non-object");
    let path = repo_config_path(&home, "owner", "repo");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "[1,2,3]").unwrap();

    // Act
    let result = set(
        Tier::Repo,
        "autoReview.enabled",
        &json!(true),
        &home,
        Some("owner/repo"),
    );

    // Assert
    let err = result.unwrap_err();
    assert!(matches!(err, ConfigError::Malformed(_)));
    assert!(err.to_string().contains(&path.display().to_string()));
    assert_eq!(fs::read_to_string(&path).unwrap(), "[1,2,3]");

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn twenty_concurrent_writers_keep_every_key() {
    // Arrange
    let home = scratch_home("concurrent");
    let thread_count = 20;

    // Act
    let handles: Vec<_> = (0..thread_count)
        .map(|i| {
            let home = home.clone();
            std::thread::spawn(move || {
                if i % 2 == 0 {
                    let enabled = i % 4 == 0;
                    set(
                        Tier::Repo,
                        "autoReview.enabled",
                        &json!(enabled),
                        &home,
                        Some("owner/repo"),
                    )
                } else {
                    let kind = if i % 4 == 1 { "quick" } else { "deep" };
                    set(
                        Tier::Repo,
                        "autoReview.type",
                        &json!(kind),
                        &home,
                        Some("owner/repo"),
                    )
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().expect("each write should succeed");
    }

    // Assert: SQLite serializes the writers, so both keys are present and
    // hold a value some thread actually wrote.
    let content = stored(&home)["repos"]["owner/repo"].clone();
    content["autoReview.enabled"]
        .as_bool()
        .expect("enabled should be a bool");
    let kind = content["autoReview.type"]
        .as_str()
        .expect("type should be a string");
    assert!(kind == "quick" || kind == "deep");

    let _ = fs::remove_dir_all(&home);
}

/// Writes `value` to the global tier and asserts the write is refused with a
/// message that names `key` and carries `constraint`, leaving no file behind.
fn assert_rejected(key: &str, value: &Value, constraint: &str) {
    // Arrange
    let home = scratch_home("range-rejected");

    // Act
    let result = set(Tier::Global, key, &value.clone(), &home, None);

    // Assert
    let err = result.expect_err(&format!("{key} = {value} must be rejected"));
    let message = err.to_string();
    assert!(message.contains(key), "{key} = {value}: {message}");
    assert!(
        message.contains(constraint),
        "{key} = {value} must explain '{constraint}': {message}"
    );
    assert!(
        !db(&home).exists(),
        "{key} = {value}: a rejected write must not create the database"
    );

    let _ = fs::remove_dir_all(&home);
}

/// Writes `value` to the global tier and asserts it lands in the file.
fn assert_written(key: &str, value: &Value) {
    // Arrange
    let home = scratch_home("range-accepted");

    // Act
    let result = set(Tier::Global, key, &value.clone(), &home, None);

    // Assert
    result.unwrap_or_else(|e| panic!("{key} = {value} must be accepted: {e}"));
    assert_eq!(&stored(&home)["global"][key], value, "{key}");

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_budget_under_one_cent_is_rejected_with_a_clear_message() {
    for value in [
        json!(0),
        json!(0.0),
        json!(-3),
        json!(-0.5),
        json!(0.004),
        json!(0.009),
    ] {
        assert_rejected("auto.budgetUsd", &value, "at least 0.01");
    }
}

#[test]
fn a_usage_budget_takes_zero_or_more_dollars_and_nothing_else() {
    for key in ["usage.budget.monthlyUsd", "usage.budget.weeklyUsd"] {
        for value in [json!(0), json!(200), json!(12.5)] {
            assert_written(key, &value);
        }
        for value in [json!(-1), json!(-0.5), json!(2_000_000_000)] {
            assert_rejected(key, &value, "a number of dollars");
        }
    }
}

#[test]
fn a_budget_of_a_cent_or_more_including_a_fractional_one_is_written() {
    for value in [json!(5), json!(1000), json!(0.5), json!(2.5), json!(0.01)] {
        assert_written("auto.budgetUsd", &value);
    }
}

#[test]
fn a_usage_refresh_interval_from_one_to_3600_seconds_is_written_and_others_rejected() {
    for value in [json!(1), json!(5), json!(3600)] {
        assert_written("usage.refreshSeconds", &value);
    }
    for value in [json!(0), json!(3601), json!(-1), json!(2.5)] {
        assert_rejected("usage.refreshSeconds", &value, "between 1 and 3600");
    }
}

#[test]
fn a_gate_round_cap_from_one_to_twenty_is_written_and_others_rejected() {
    for value in [json!(1), json!(5), json!(20)] {
        assert_written("gate.maxRounds", &value);
    }
    for value in [json!(0), json!(21), json!(-1), json!(2.5)] {
        assert_rejected("gate.maxRounds", &value, "from 1 to 20");
    }
}

#[test]
fn a_fix_threshold_that_is_not_a_positive_integer_is_rejected() {
    for key in ["fix.maxFiles", "fix.maxLines"] {
        for value in [json!(0), json!(-1), json!(2.5)] {
            assert_rejected(key, &value, "positive integer");
        }
    }
}

#[test]
fn a_positive_integer_fix_threshold_is_written() {
    for key in ["fix.maxFiles", "fix.maxLines"] {
        for value in [json!(1), json!(99)] {
            assert_written(key, &value);
        }
    }
}

#[test]
fn a_non_number_for_a_numeric_auto_or_fix_key_is_rejected_and_writes_nothing() {
    for key in ["auto.budgetUsd", "fix.maxFiles", "fix.maxLines"] {
        // Arrange
        let home = scratch_home("non-number");

        // Act
        let result = set(Tier::Global, key, &json!("banana"), &home, None);

        // Assert
        let err = result.expect_err(&format!("{key} = banana must be rejected"));
        assert!(err.to_string().contains(key), "{err}");
        assert!(!db(&home).exists(), "{key}");

        let _ = fs::remove_dir_all(&home);
    }
}

#[test]
fn zero_is_still_accepted_for_the_other_numeric_keys() {
    assert_written("usage.telemetryRetentionDays", &json!(0));
}

const BOOL_SETTINGS: [&str; 4] = [
    "autoReview.fix",
    "autoMerge.enabled",
    "commit.signOff",
    "pr.draft",
];

#[test]
fn a_non_bool_for_the_pr_and_commit_settings_is_rejected_and_writes_nothing() {
    for key in BOOL_SETTINGS {
        for value in [json!("yes"), json!(1)] {
            // Arrange
            let home = scratch_home("non-bool");

            // Act
            let result = set(Tier::Global, key, &value.clone(), &home, None);

            // Assert
            let err = result.expect_err(&format!("{key} = {value} must be rejected"));
            assert!(matches!(err, ConfigError::WrongType { .. }), "{key}: {err}");
            assert!(err.to_string().contains(key), "{err}");
            assert!(!db(&home).exists(), "{key}");

            let _ = fs::remove_dir_all(&home);
        }
    }
}

#[test]
fn a_bool_for_the_pr_and_commit_settings_is_written() {
    for key in BOOL_SETTINGS {
        assert_written(key, &json!(true));
        assert_written(key, &json!(false));
    }
}

#[test]
fn a_ui_layout_takes_tabs_of_known_panels_and_nothing_else() {
    assert_rejected("usage.ui.layout", &json!("A=bogus"), "tabs like");
    assert_written("usage.ui.layout", &json!(""));
    assert_written("usage.ui.layout", &json!("Money=budget,models"));
}

#[test]
fn a_bad_ui_layout_error_lists_the_panels_and_the_dotted_view_words() {
    // Arrange
    let home = scratch_home("layout-message");

    // Act
    let result = set(
        Tier::Global,
        "usage.ui.layout",
        &json!("A=bogus"),
        &home,
        None,
    );

    // Assert
    let err = result.expect_err("a bogus panel must be rejected");
    assert_eq!(
        err.to_string(),
        "config key usage.ui.layout does not accept '\"A=bogus\"', must be tabs like \
         Charts=spend,tokens;Numbers=models (1 to 12 tabs; panels: spend, tokens, models, \
         breakdown, live, sessions, burnup, hourly; views pinned with dotted words: \
         spend.total, spend.model, spend.daily, breakdown.projects, breakdown.agents, \
         breakdown.skills, breakdown.tools, breakdown.health; | starts a row of up to 2 \
         panels, with at most 4 rows, and 8 panels in a tab; a name is 1 to 16 characters)"
    );
    for panel in playbook_telemetry::tui::layout::ALL
        .into_iter()
        .filter(|p| !p.is_retired())
    {
        assert!(
            err.to_string().contains(panel.word()),
            "the error does not mention {}",
            panel.word()
        );
    }

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn code_brief_facts_tokens_defaults_to_off_and_takes_a_whole_number() {
    // Arrange
    let home = std::env::temp_dir().join(format!("playbook-brief-facts-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();

    // Act
    let off = set(
        Tier::Global,
        "code.briefFactsTokens",
        &Value::from(0),
        &home,
        None,
    );
    let on = set(
        Tier::Global,
        "code.briefFactsTokens",
        &Value::from(1500),
        &home,
        None,
    );
    let negative = set(
        Tier::Global,
        "code.briefFactsTokens",
        &Value::from(-1),
        &home,
        None,
    );

    // Assert
    std::fs::remove_dir_all(&home).unwrap();
    assert!(off.is_ok() && on.is_ok());
    assert!(negative.is_err());
    assert_eq!(
        playbook::config::keys::default_value("code.briefFactsTokens"),
        Some(Value::from(0))
    );
}
