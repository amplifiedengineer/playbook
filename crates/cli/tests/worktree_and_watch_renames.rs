// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The worktree cleanup, watch and upgrade keys under their new names: a row
//! an older binary stored under the old name still applies through the
//! fallback, and the per-user list follows the new names. Each test uses a
//! scratch `$HOME`.

use playbook::config::keys;
use playbook::config::store;
use playbook::config::write::Tier;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

/// Old name, new name and a stored value that differs from the default.
const RENAMED: [(&str, &str, &str); 6] = [
    (
        "worktreeCleanup.enabled",
        "worktree.cleanup.enabled",
        "false",
    ),
    ("watch.maxCatchupHours", "watch.catchup.maxHours", "12"),
    (
        "watch.limits.actionsPerDay",
        "watch.limits.dailyActions",
        "7",
    ),
    ("watch.limits.usdPerDay", "watch.limits.dailyUsd", "2.5"),
    ("update.mode", "upgrade.mode", "off"),
    ("update.plugin", "upgrade.plugin", "ask"),
];
const USER_RENAMED: [&str; 5] = [
    "watch.catchup.maxHours",
    "watch.limits.dailyActions",
    "watch.limits.dailyUsd",
    "upgrade.mode",
    "upgrade.plugin",
];

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!(
        "pb-worktree-watch-{}-{tag}-{n}",
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
        .env_remove("PLAYBOOK_MODE")
        .env_remove("CI")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn root(home: &Path) -> PathBuf {
    playbook::common::paths::playbook_root_from(home)
}

#[test]
fn a_stored_old_row_reads_under_the_new_name() {
    for (old, new, shown) in RENAMED {
        // Arrange
        let home = scratch("old-row");
        let value: Value = serde_json::from_str(shown).unwrap_or_else(|_| json!(shown));
        store::put(&root(&home), Tier::Global, None, old, &value).unwrap();

        // Act
        let out = playbook(&home, &["config", "get", new]);

        // Assert
        assert!(out.status.success(), "{old}: {}", text(&out.stderr));
        let expected = format!("{new}: {shown} (source: global)");
        assert_eq!(text(&out.stdout).trim(), expected, "{old}");
    }
}

#[test]
fn the_per_user_list_follows_the_new_names() {
    for new in USER_RENAMED {
        // Assert
        assert!(keys::is_user_key(new), "{new}");
        assert!(keys::KNOWN_KEYS.contains(&new), "{new}");
    }
    for (old, _, _) in RENAMED {
        assert!(!keys::is_user_key(old), "{old}");
        assert!(!keys::KNOWN_KEYS.contains(&old), "{old}");
    }
}

#[test]
fn a_legacy_boolean_stored_under_the_old_plugin_name_reads_as_a_word() {
    for (stored, word) in [(true, "auto"), (false, "off")] {
        // Arrange
        let home = scratch("legacy-bool");
        store::put(
            &root(&home),
            Tier::Global,
            None,
            "update.plugin",
            &json!(stored),
        )
        .unwrap();

        // Act
        let out = playbook(&home, &["config", "get", "upgrade.plugin"]);

        // Assert
        assert_eq!(
            text(&out.stdout).trim(),
            format!("upgrade.plugin: {word} (source: global)")
        );
    }
}
