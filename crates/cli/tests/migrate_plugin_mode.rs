// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `plugin-update-mode` migration: a stored boolean under the old name
//! `update.plugin` (now `upgrade.plugin`) moves to `auto` or `off`, once. A word
//! stays, and a store without the key gets no write.
//!
//! The migration says `since: "next"`, so a dev build never runs it on its
//! own. These tests run the registry entry through the library with its
//! `since` set to a released one, then read the key with the real binary.

use playbook::config::store;
use playbook::config::write::Tier;
use playbook::init::migrate::{
    registry, run_with, Ctx, Migration, Outcome, Report, PLUGIN_UPDATE_MODE_MIGRATION,
};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const KEY: &str = "update.plugin";
const SLUG: &str = "acme/app";
const RELEASED: &str = "0.1.0";

fn scratch(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "pb-plugin-mode-{tag}-{}",
        playbook::testing::run_id()
    ));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).unwrap();
    home
}

fn root(home: &Path) -> PathBuf {
    playbook::common::paths::playbook_root_from(home)
}

/// Seeds the store the way an older binary left it, which `config set` now refuses.
fn seed(home: &Path, tier: Tier, value: Value) {
    store::put(&root(home), tier, Some(SLUG), KEY, &value).unwrap();
}

fn migration() -> Migration {
    let shipped = registry()
        .into_iter()
        .find(|m| m.id == PLUGIN_UPDATE_MODE_MIGRATION)
        .expect("the migration is registered");
    Migration {
        since: RELEASED,
        ..shipped
    }
}

fn run(home: &Path) -> Report {
    run_with(&[migration()], &Ctx::for_home(home))
}

fn get(home: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["config", "get", KEY])
        .current_dir(home)
        .env("HOME", home)
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .env_remove("PLAYBOOK_PLUGIN_DIR")
        .env_remove("PLAYBOOK_MODE")
        .env_remove("HOOK_INPUT")
        .env_remove("CI")
        .env_remove("PLAYBOOK_HEADLESS")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    // The line reads `update.plugin: off (source: global)`.
    let line = String::from_utf8_lossy(&out.stdout).into_owned();
    line.split(": ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_default()
        .to_string()
}

#[test]
fn a_stored_boolean_becomes_a_word_and_a_second_run_reports_nothing() {
    for (old, word) in [(json!(false), "off"), (json!(true), "auto")] {
        // Arrange
        let home = scratch(word);
        seed(&home, Tier::Global, old);

        // Act
        let first = run(&home);
        let after = get(&home);
        let second = run(&home);

        // Assert
        assert!(!first.failed, "{word}: {:?}", first.warnings);
        assert_eq!(first.steps.len(), 1, "{word}");
        assert_eq!(after, word);
        assert!(second.steps.is_empty(), "{word}");
        assert_eq!(get(&home), word);
    }
}

#[test]
fn every_tier_moves_and_a_word_stays() {
    // Arrange
    let home = scratch("tiers");
    seed(&home, Tier::Global, json!(true));
    seed(&home, Tier::Repo, json!(false));
    seed(&home, Tier::Org, json!("ask"));

    // Act
    let report = run(&home);

    // Assert
    assert!(!report.failed, "{:?}", report.warnings);
    // Export hides the old name, so read the rows of that name directly.
    let found = store::lookup(&root(&home), KEY, Some(SLUG)).unwrap();
    assert_eq!(found.global, Some(json!("auto")));
    assert_eq!(found.repo, Some(json!("off")));
    assert_eq!(found.org, Some(json!("ask")));
}

#[test]
fn a_stored_word_is_left_alone() {
    // Arrange
    let home = scratch("ask");
    seed(&home, Tier::Global, json!("ask"));

    // Act
    let report = run(&home);

    // Assert
    assert!(!report.failed, "{:?}", report.warnings);
    assert_eq!(get(&home), "ask");
}

#[test]
fn a_fresh_home_gets_no_store_and_resolves_to_the_default() {
    // Arrange
    let home = scratch("fresh");

    // Act
    // The runner's own record lives in the config db, so the step is run
    // alone to show it writes nothing.
    let step = (migration().run)(&Ctx::for_home(&home));
    let db_created = store::db_path(&root(&home)).exists();
    let report = run(&home);
    let resolved = get(&home);

    // Assert
    assert!(matches!(step, Outcome::Done(_)));
    assert!(!db_created, "the step must not create the config db");
    assert!(!report.failed, "{:?}", report.warnings);
    assert_eq!(resolved, "auto");
    assert!(store::export(&root(&home)).unwrap()["global"]
        .get(KEY)
        .is_none());
}
