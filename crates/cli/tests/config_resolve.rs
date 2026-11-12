// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `playbook::config::resolve`: precedence across the
//! repo/org/global/default tiers, and its error cases (a malformed file, a
//! valid-JSON-but-non-object file, an unknown key).

use playbook::config::{resolve, ConfigError, Source};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh scratch directory standing in for `$HOME`, unique per call so
/// parallel tests never collide.
fn scratch_home(tag: &str) -> PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "playbook-config-resolve-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("scratch home should be creatable");
    dir
}

fn write_json(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("path should have a parent")).expect("mkdir");
    fs::write(path, content).expect("scratch file should be writable");
}

fn global_config_path(home: &Path) -> PathBuf {
    home.join(".config").join("playbook").join("config.json")
}

fn org_config_path(home: &Path, owner: &str) -> PathBuf {
    home.join(".config")
        .join("playbook")
        .join("orgs")
        .join(owner)
        .join("config.json")
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

#[test]
fn no_config_files_anywhere_resolves_to_the_built_in_default() {
    // Arrange
    let home = scratch_home("no-files");

    // Act
    let got = resolve("pr.review.enabled", &home, None);

    // Assert
    assert_eq!(got.unwrap(), (Value::Bool(true), Source::Default));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_global_override_wins_over_the_default() {
    // Arrange
    let home = scratch_home("global-override");
    write_json(
        &global_config_path(&home),
        r#"{"autoReview": {"enabled": false}}"#,
    );

    // Act
    let got = resolve("pr.review.enabled", &home, None);

    // Assert
    assert_eq!(got.unwrap(), (Value::Bool(false), Source::Global));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_repo_override_wins_over_a_global_override() {
    // Arrange
    let home = scratch_home("repo-over-global");
    write_json(
        &global_config_path(&home),
        r#"{"autoReview": {"enabled": false}}"#,
    );
    write_json(
        &repo_config_path(&home, "owner", "repo"),
        r#"{"autoReview": {"enabled": true}}"#,
    );

    // Act
    let got = resolve("pr.review.enabled", &home, Some("owner/repo"));

    // Assert
    assert_eq!(got.unwrap(), (Value::Bool(true), Source::Repo));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn an_org_override_wins_over_global_when_no_repo_file_exists() {
    // Arrange
    let home = scratch_home("org-over-global");
    write_json(
        &global_config_path(&home),
        r#"{"autoReview": {"enabled": false}}"#,
    );
    write_json(
        &org_config_path(&home, "owner"),
        r#"{"autoReview": {"enabled": true}}"#,
    );

    // Act
    let got = resolve("pr.review.enabled", &home, Some("owner/repo"));

    // Assert
    let (value, source) = got.unwrap();
    assert_eq!(value, Value::Bool(true));
    assert_eq!(source, Source::Org);

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_malformed_file_fails_with_its_path_in_the_message() {
    // Arrange
    let home = scratch_home("malformed");
    let path = global_config_path(&home);
    write_json(&path, "not json at all {");

    // Act
    let got = resolve("pr.review.enabled", &home, None);

    // Assert
    let err = got.unwrap_err();
    assert!(matches!(err, ConfigError::Malformed(_)));
    assert!(err.to_string().contains(&path.display().to_string()));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_valid_json_non_object_file_fails_the_same_as_malformed() {
    // Arrange
    let home = scratch_home("non-object");
    let path = global_config_path(&home);
    write_json(&path, "[1,2,3]");

    // Act
    let got = resolve("pr.review.enabled", &home, None);

    // Assert
    let err = got.unwrap_err();
    assert!(matches!(err, ConfigError::Malformed(_)));
    assert!(err.to_string().contains(&path.display().to_string()));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn no_repo_slug_skips_the_repo_tier_entirely() {
    // Arrange: a repo-tier file exists at the path a real repo_slug would
    // resolve to, but resolve() is called with `None`, so it must never be
    // looked up.
    let home = scratch_home("no-repo-slug");
    write_json(
        &repo_config_path(&home, "owner", "repo"),
        r#"{"autoReview": {"enabled": false}}"#,
    );

    // Act
    let got = resolve("pr.review.enabled", &home, None);

    // Assert
    assert_eq!(got.unwrap(), (Value::Bool(true), Source::Default));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_tier_file_missing_the_looked_up_key_falls_through_to_the_next_tier() {
    // Arrange: the repo-tier file is a valid object, but only sets a
    // sibling key, not the one being resolved.
    let home = scratch_home("missing-key");
    write_json(
        &repo_config_path(&home, "owner", "repo"),
        r#"{"autoReview": {"type": "quick"}}"#,
    );

    // Act
    let got = resolve("pr.review.enabled", &home, Some("owner/repo"));

    // Assert
    assert_eq!(got.unwrap(), (Value::Bool(true), Source::Default));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn an_unknown_key_with_no_config_files_errors_instead_of_defaulting_to_null() {
    // Arrange
    let home = scratch_home("unknown-key");

    // Act
    let got = resolve("notAKnownKey", &home, None);

    // Assert
    assert!(matches!(got.unwrap_err(), ConfigError::UnknownKey(k) if k == "notAKnownKey"));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn an_unknown_key_present_in_a_tier_file_still_errors() {
    // Arrange: a tier file happens to contain a dotted path that is not on
    // KNOWN_KEYS (a stale or hand-edited key), which must not let it resolve
    // successfully just because some file has it.
    let home = scratch_home("unknown-key-present-in-file");
    write_json(&global_config_path(&home), r#"{"bogus": {"key": 1}}"#);

    // Act
    let got = resolve("bogus.key", &home, None);

    // Assert
    assert!(matches!(got.unwrap_err(), ConfigError::UnknownKey(k) if k == "bogus.key"));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn pr_draft_defaults_to_true() {
    // Arrange
    let home = scratch_home("pr-draft-default");

    // Act
    let got = resolve("pr.draft", &home, Some("owner/repo"));

    // Assert
    assert_eq!(got.unwrap(), (Value::Bool(true), Source::Default));
    assert!(playbook::pr::create::draft_setting(
        &home,
        Some("owner/repo")
    ));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn pr_draft_repo_beats_global_in_both_directions() {
    for (global, repo) in [(false, true), (true, false)] {
        // Arrange
        let home = scratch_home("pr-draft-tiers");
        write_json(
            &global_config_path(&home),
            &format!(r#"{{"pr": {{"draft": {global}}}}}"#),
        );
        write_json(
            &repo_config_path(&home, "owner", "repo"),
            &format!(r#"{{"pr": {{"draft": {repo}}}}}"#),
        );

        // Act
        let got = resolve("pr.draft", &home, Some("owner/repo"));

        // Assert
        assert_eq!(got.unwrap(), (Value::Bool(repo), Source::Repo));
        assert_eq!(
            playbook::pr::create::draft_setting(&home, Some("owner/repo")),
            repo
        );

        let _ = fs::remove_dir_all(&home);
    }
}

#[test]
fn pr_draft_false_opens_ready_and_a_broken_or_invalid_config_stays_draft() {
    // Arrange
    let home = scratch_home("pr-draft-global");
    let reset = |legacy: &str| {
        for name in ["playbook.db", "playbook.db-wal", "playbook.db-shm"] {
            let _ = fs::remove_file(global_config_path(&home).with_file_name(name));
        }
        for name in ["config.db", "config.db-wal", "config.db-shm"] {
            let _ = fs::remove_file(global_config_path(&home).with_file_name("data").join(name));
        }
        write_json(&global_config_path(&home), legacy);
    };
    reset(r#"{"pr": {"draft": false}}"#);

    // Act
    let auto = playbook::pr::create::draft_setting(&home, None);
    reset(r#"{"pr": {"draft": "banana"}}"#);
    let invalid = playbook::pr::create::draft_setting(&home, None);
    reset("not json");
    let broken = playbook::pr::create::draft_setting(&home, None);

    // Assert
    assert!(!auto);
    assert!(invalid, "an invalid value must not publish a ready PR");
    assert!(broken, "a broken config must not publish a ready PR");

    let _ = fs::remove_dir_all(&home);
}
