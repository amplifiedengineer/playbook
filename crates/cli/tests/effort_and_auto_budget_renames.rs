// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The effort ceiling and the three auto budget keys under their new names:
//! rows an older binary stored move on migrate, and the ceiling-below-budget
//! refusal works under either name. Each test uses a scratch `$HOME`.

use playbook::config::write::Tier;
use playbook::config::{self, store};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const SLUG: &str = "acme/app";
const MAIN_OLD: &str = "auto.budgetUsd";
const MAIN_NEW: &str = "auto.budget.mainUsd";
const AGENT_OLD: &str = "auto.agentBudgetUsd";
const AGENT_NEW: &str = "auto.budget.agentUsd";
const CEILING_OLD: &str = "auto.sessionCeilingUsd";
const CEILING_NEW: &str = "auto.budget.ceilingUsd";
const EFFORT_OLD: &str = "maxEffortLevel";
const EFFORT_NEW: &str = "effort.max";
const RENAMED: [(&str, &str); 4] = [
    (MAIN_OLD, MAIN_NEW),
    (AGENT_OLD, AGENT_NEW),
    (CEILING_OLD, CEILING_NEW),
    (EFFORT_OLD, EFFORT_NEW),
];

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!(
        "pb-effort-budget-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).unwrap();
    home
}

fn playbook(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .env_remove("PLAYBOOK_PLUGIN_DIR")
        .env_remove("PLAYBOOK_MODE")
        .env_remove("HOOK_INPUT")
        .env_remove("CI")
        .env_remove("PLAYBOOK_HEADLESS")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn root(home: &Path) -> PathBuf {
    playbook::common::paths::playbook_root_from(home)
}

fn stored(home: &Path) -> Value {
    store::export(&root(home)).unwrap()
}

/// Stores a row under its old name, the way an older binary left it.
fn seed_old(home: &Path, tier: Tier, key: &str, value: Value) {
    store::put(&root(home), tier, Some(SLUG), key, &value).unwrap();
}

#[test]
fn a_stored_old_row_reads_under_the_new_name_and_migrate_leaves_it_alone() {
    for (old, new, global, repo) in [
        (MAIN_OLD, MAIN_NEW, json!(12), json!(15)),
        (AGENT_OLD, AGENT_NEW, json!(6), json!(8)),
        (CEILING_OLD, CEILING_NEW, json!(900), json!(950)),
    ] {
        // Arrange: the rename migration waits for the next release, so a
        // binary built before then reads the old row through its fallback
        let home = scratch("old-row");
        seed_old(&home, Tier::Global, old, global.clone());
        seed_old(&home, Tier::Repo, old, repo.clone());
        let before = store::stored_values(&root(&home), old).unwrap();

        // Act
        let first = playbook(&home, &["migrate"]);
        let second = playbook(&home, &["migrate"]);
        let (global_read, _) = config::resolve(new, &home, None).unwrap();
        let (repo_read, _) = config::resolve(new, &home, Some(SLUG)).unwrap();

        // Assert
        assert!(first.status.success(), "{old}: {}", text(&first.stderr));
        assert!(second.status.success(), "{old}: {}", text(&second.stderr));
        assert_eq!(global_read, global, "{old}");
        assert_eq!(repo_read, repo, "{old}");
        assert_eq!(
            store::stored_values(&root(&home), old).unwrap(),
            before,
            "{old}: migrate changed the rows"
        );
    }
}

#[test]
fn a_stored_effort_ceiling_under_the_old_name_still_applies() {
    // Arrange
    let home = scratch("effort");
    seed_old(&home, Tier::Global, EFFORT_OLD, json!("low"));

    // Act
    let status = playbook(&home, &["effort", "--json"]);

    // Assert
    let report: Value = serde_json::from_str(text(&status.stdout).trim()).unwrap();
    assert_eq!(report["playbook"], json!("low"), "{report}");
}

#[test]
fn set_stores_the_effort_ceiling_under_its_new_name() {
    for name in [EFFORT_OLD, EFFORT_NEW] {
        // Arrange
        let home = scratch("effort-set");

        // Act
        let out = playbook(&home, &["config", "set", "--global", name, "high"]);

        // Assert
        assert!(out.status.success(), "{name}: {}", text(&out.stderr));
        assert_eq!(stored(&home)["global"][EFFORT_NEW], json!("high"), "{name}");
    }
}

#[test]
fn a_ceiling_below_a_budget_is_refused_under_the_new_and_the_old_names() {
    for (budget, ceiling) in [(MAIN_NEW, CEILING_NEW), (MAIN_OLD, CEILING_OLD)] {
        // Arrange: a stored budget of 300, so a ceiling of 100 is below it
        let home = scratch("ceiling");
        let set_budget = playbook(&home, &["config", "set", "--global", budget, "300"]);
        assert!(set_budget.status.success(), "{}", text(&set_budget.stderr));

        // Act
        let refused = playbook(&home, &["config", "set", "--global", ceiling, "100"]);

        // Assert
        assert!(!refused.status.success(), "{ceiling} 100 was accepted");
        let message = text(&refused.stderr);
        assert!(message.contains(MAIN_NEW), "{message}");
        assert!(stored(&home)["global"].get(CEILING_NEW).is_none());
    }
}

#[test]
fn a_budget_above_a_set_ceiling_is_refused_under_the_new_and_the_old_names() {
    for (budget, ceiling) in [(MAIN_NEW, CEILING_NEW), (AGENT_OLD, CEILING_OLD)] {
        // Arrange
        let home = scratch("budget");
        let set_ceiling = playbook(&home, &["config", "set", "--global", ceiling, "300"]);
        assert!(
            set_ceiling.status.success(),
            "{}",
            text(&set_ceiling.stderr)
        );

        // Act
        let refused = playbook(&home, &["config", "set", "--global", budget, "301"]);

        // Assert
        assert!(!refused.status.success(), "{budget} 301 was accepted");
        let message = text(&refused.stderr);
        assert!(message.contains(CEILING_NEW), "{message}");
    }
}

#[test]
fn every_old_name_is_gone_from_the_known_keys() {
    for (old, new) in RENAMED {
        // Act
        let known_old = playbook::config::keys::KNOWN_KEYS.contains(&old);
        let known_new = playbook::config::keys::KNOWN_KEYS.contains(&new);

        // Assert
        assert!(!known_old, "{old} is still a known key");
        assert!(known_new, "{new} is not a known key");
    }
}
