// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `auto-budget-defaults` migration (ADR 0028, 2026-10-17 amendment): a
//! stored auto budget that holds the old default of 5 moves to the new
//! default, through `playbook
//! migrate` and through the first run of any command. Any other value stays,
//! and a store that holds no budget gets no write.

use playbook::config::store;
use playbook::config::write::Tier;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SLUG: &str = "acme/app";
const READ_KEYS: [&str; 4] = [
    "fix.maxFiles",
    "auto.budgetUsd",
    "auto.agentBudgetUsd",
    "auto.sessionCeilingUsd",
];

fn scratch(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "pb-auto-budget-{tag}-{}",
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
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .env_remove("PLAYBOOK_PLUGIN_DIR")
        .env_remove("PLAYBOOK_MODE")
        .env_remove("HOOK_INPUT")
        .env_remove("CI")
        .env_remove("PLAYBOOK_HEADLESS")
        .output()
        .unwrap()
}

fn root(home: &Path) -> PathBuf {
    playbook::common::paths::playbook_root_from(home)
}

/// The stored rows, read under the old names the migration still works on:
/// the key rename waits for the next release, so export leaves them out.
fn stored(home: &Path) -> Value {
    let root = root(home);
    let mut global = serde_json::Map::new();
    let mut repo = serde_json::Map::new();
    let mut org = serde_json::Map::new();
    for key in READ_KEYS {
        let found = store::lookup(&root, key, Some(SLUG)).unwrap();
        for (map, value) in [
            (&mut global, found.global),
            (&mut repo, found.repo),
            (&mut org, found.org),
        ] {
            if let Some(value) = value {
                map.insert(key.to_string(), value);
            }
        }
    }
    json!({ "global": global, "repos": { SLUG: repo }, "orgs": { "acme": org } })
}

/// Seeds the store the way an older binary left it, without running this one.
/// The keys are stored as given, so a row can sit under an old name.
fn seed(home: &Path, rows: &[(Tier, &str, Value)]) {
    for (tier, key, value) in rows {
        store::put(&root(home), *tier, Some(SLUG), key, value).unwrap();
    }
}

fn old_defaults_and_choices(home: &Path) {
    seed(
        home,
        &[
            (Tier::Global, "auto.budgetUsd", json!(5)),
            (Tier::Global, "auto.agentBudgetUsd", json!(5)),
            (Tier::Repo, "auto.budgetUsd", json!(5.0)),
            (Tier::Org, "auto.budgetUsd", json!(7.5)),
            (Tier::Repo, "auto.agentBudgetUsd", json!(4)),
            (Tier::Global, "fix.maxFiles", json!(5)),
        ],
    );
}

fn assert_moved(home: &Path) {
    let doc = stored(home);
    assert_eq!(doc["global"]["auto.budgetUsd"], json!(200), "{doc}");
    assert_eq!(doc["global"]["auto.agentBudgetUsd"], json!(50), "{doc}");
    assert_eq!(doc["repos"][SLUG]["auto.budgetUsd"], json!(200), "{doc}");
    assert_eq!(doc["orgs"]["acme"]["auto.budgetUsd"], json!(7.5), "{doc}");
    assert_eq!(doc["repos"][SLUG]["auto.agentBudgetUsd"], json!(4), "{doc}");
    assert_eq!(doc["global"]["fix.maxFiles"], json!(5), "{doc}");
}

#[test]
fn migrate_moves_the_old_default_once_and_keeps_every_other_value() {
    // Arrange
    let home = scratch("migrate");
    old_defaults_and_choices(&home);

    // Act
    let first = playbook(&home, &["migrate", "--json"]);

    // Assert
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    let report: Value = serde_json::from_slice(&first.stdout).unwrap();
    let changed = report["changed"].to_string();
    assert!(changed.contains("auto.budgetUsd $5 to $200"), "{report}");
    assert!(
        changed.contains("auto.agentBudgetUsd $5 to $50"),
        "{report}"
    );
    assert_moved(&home);

    // Act: the user picks 5 on purpose after the move, then migrate runs again
    seed(&home, &[(Tier::Global, "auto.budgetUsd", json!(5))]);
    let second = playbook(&home, &["migrate", "--json"]);

    // Assert: the migration ran once, so the choice stays
    assert_eq!(second.status.code(), Some(0), "{second:?}");
    assert_eq!(stored(&home)["global"]["auto.budgetUsd"], json!(5));
}

#[test]
fn the_first_run_of_any_command_moves_the_old_default() {
    // Arrange
    let home = scratch("first-run");
    old_defaults_and_choices(&home);

    // Act
    let out = playbook(&home, &["mode", "status"]);

    // Assert
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_moved(&home);
}

#[test]
fn a_store_without_a_budget_is_not_written_and_resolves_to_the_new_default() {
    // Arrange: rows are a fresh home, and a store that holds one other key
    for (n, rows) in [vec![], vec![(Tier::Global, "fix.maxFiles", json!(5))]]
        .into_iter()
        .enumerate()
    {
        let home = scratch(&format!("untouched-{n}"));
        seed(&home, &rows);

        // Act
        let migrate = playbook(&home, &["migrate"]);
        let budget = playbook(&home, &["config", "get", "auto.budgetUsd"]);
        let agent = playbook(&home, &["config", "get", "auto.agentBudgetUsd"]);

        // Assert
        assert_eq!(migrate.status.code(), Some(0), "{n}: {migrate:?}");
        let doc = stored(&home);
        assert!(doc["global"].get("auto.budgetUsd").is_none(), "{n}: {doc}");
        assert!(
            doc["global"].get("auto.agentBudgetUsd").is_none(),
            "{n}: {doc}"
        );
        let text = |out: &Output| String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(text(&budget).contains("200"), "{n}: {}", text(&budget));
        assert!(text(&agent).contains("50"), "{n}: {}", text(&agent));
    }
}

#[test]
fn a_set_ceiling_below_the_new_default_holds_the_moved_budget_at_the_ceiling() {
    // Arrange: budgets of 5 under a ceiling of 40, valid before this change
    let home = scratch("ceiling");
    seed(
        &home,
        &[
            (Tier::Global, "auto.budgetUsd", json!(5)),
            (Tier::Global, "auto.agentBudgetUsd", json!(5)),
            (Tier::Global, "auto.sessionCeilingUsd", json!(40)),
        ],
    );

    // Act
    let out = playbook(&home, &["migrate", "--json"]);

    // Assert: both budgets rise to the ceiling, so it stays above them
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let doc = stored(&home);
    assert_eq!(
        doc["global"]["auto.budgetUsd"].as_f64(),
        Some(40.0),
        "{doc}"
    );
    assert_eq!(
        doc["global"]["auto.agentBudgetUsd"].as_f64(),
        Some(40.0),
        "{doc}"
    );
    assert_eq!(doc["global"]["auto.sessionCeilingUsd"], json!(40), "{doc}");
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("auto.budgetUsd $5 to $40"), "{report}");
    assert!(report.contains("auto.sessionCeilingUsd"), "{report}");
}
