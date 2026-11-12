// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `usage.*` settings: the keys, their defaults and the values they accept.
//! This module is the source of truth for them. playbook-telemetry reads them
//! from playbook's own config store, `~/.config/playbook/data/config.db` (the
//! global tier, overridden by this tool's own `playbook-telemetry` app tier),
//! so the standalone tool and playbook always see the same settings. Before
//! playbook has moved that store, `~/.config/playbook/playbook.db` is read.
//! A playbook that predates it keeps them in
//! `~/.config/playbook/config.json` as a nested JSON object, which is read
//! when the store does not exist. This module reads; the one write is
//! `save_app`, for pbtop's own `usage.ui.*` keys. Other settings are changed
//! with `playbook config set`.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Every `usage.*` key.
#[cfg(test)]
pub const KEYS: &[&str] = &[
    "usage.theme",
    "usage.maskAccount",
    "usage.weekStart",
    "usage.timeZone",
    "usage.telemetry",
    "usage.budget.monthlyUsd",
    "usage.budget.weeklyUsd",
    "usage.ui.layout",
    "usage.ui.state",
];

/// The built-in default of `key`, `None` for a key that is not a usage key.
pub fn default_value(key: &str) -> Option<Value> {
    Some(match key {
        "usage.theme" => Value::from("default"),
        "usage.maskAccount" => Value::from(false),
        "usage.weekStart" => Value::from("monday"),
        "usage.timeZone" => Value::from("local"),
        "usage.telemetry" => Value::from("auto"),
        "usage.budget.monthlyUsd" | "usage.budget.weeklyUsd" => Value::from(0),
        "usage.ui.layout" | "usage.ui.state" => Value::from(""),
        _ => return None,
    })
}

/// The accepted strings of an enum key.
pub fn allowed_values(key: &str) -> Option<&'static [&'static str]> {
    match key {
        "usage.theme" => Some(&["default", "spectrum", "dark", "light", "mono"]),
        "usage.weekStart" => Some(&["monday", "sunday", "saturday"]),
        "usage.timeZone" => Some(&["local", "utc"]),
        "usage.telemetry" => Some(&["auto", "off"]),
        _ => None,
    }
}

/// The longest `usage.ui.state` value.
pub const MAX_STATE_BYTES: usize = 4096;

/// `value` when it is valid for `key`, else the reason it is not.
pub fn validate(key: &str, value: &Value) -> Result<(), String> {
    if let Some(allowed) = allowed_values(key) {
        return match value.as_str() {
            Some(s) if allowed.contains(&s) => Ok(()),
            _ => Err(format!("valid values are: {}", allowed.join(", "))),
        };
    }
    match key {
        "usage.ui.layout" => match value.as_str() {
            Some("") => Ok(()),
            Some(text) => crate::tui::layout::Layout::parse(text).map(|_| ()),
            None => Err("expected text like Charts=spend,tokens|burnup;Numbers=models".into()),
        },
        "usage.ui.state" => match value.as_str() {
            Some(text) if text.len() <= MAX_STATE_BYTES => Ok(()),
            _ => Err(format!("expected text of at most {MAX_STATE_BYTES} bytes")),
        },
        "usage.maskAccount" if !value.is_boolean() => Err("expected true or false".into()),
        "usage.maskAccount" => Ok(()),
        _ => match value.as_f64() {
            Some(n) if n >= 0.0 => Ok(()),
            _ => Err("expected a number of zero or more".into()),
        },
    }
}

fn warn_once(flag: &AtomicBool, message: &str) {
    if !flag.swap(true, Ordering::Relaxed) {
        crate::say::notice(message);
    }
}

static WARNED_STORE: AtomicBool = AtomicBool::new(false);
static WARNED_VALUE: AtomicBool = AtomicBool::new(false);
static WARNED_OLD_FILE: AtomicBool = AtomicBool::new(false);

/// `~/.config/playbook`, where playbook keeps its settings and this tool its data.
pub fn root(home: &Path) -> PathBuf {
    home.join(".config").join("playbook")
}

/// The global-tier value of `key` from playbook's config store, or from the
/// legacy nested `config.json`. `None` when unset or unreadable (an unreadable
/// source prints a warning once).
fn stored(home: &Path, key: &str) -> Option<Value> {
    let dir = root(home);
    // The shared store: `data/config.db`, with this tool's own `app` tier
    // overriding the global one.
    let shared = playbook_data::config::db_in(&dir.join("data"));
    if shared.is_file() {
        return match playbook_data::config::get_in(
            &shared,
            key,
            None,
            Some(playbook_data::config::APP_TELEMETRY),
        ) {
            Ok(raw) => raw.and_then(|text| serde_json::from_str(&text).ok()),
            Err(e) => {
                warn_once(
                    &WARNED_STORE,
                    &format!(
                        "cannot read {}: {e}; using default settings",
                        shared.display()
                    ),
                );
                None
            }
        };
    }
    // Before playbook has moved its store: `playbook.db` next to the data folder.
    let db = playbook_data::config::legacy_db_in(&dir);
    if db.is_file() {
        return match read_store(&db, key) {
            Ok(v) => v,
            Err(e) => {
                warn_once(
                    &WARNED_STORE,
                    &format!("cannot read {}: {e}; using default settings", db.display()),
                );
                None
            }
        };
    }
    let json = dir.join("config.json");
    let text = std::fs::read_to_string(&json).ok()?;
    match serde_json::from_str::<Value>(&text) {
        Ok(doc) => key
            .split('.')
            .try_fold(&doc, |node, part| node.get(part))
            .cloned(),
        Err(e) => {
            warn_once(
                &WARNED_STORE,
                &format!(
                    "cannot read {}: {e}; using default settings",
                    json.display()
                ),
            );
            None
        }
    }
}

fn read_store(db: &Path, key: &str) -> Result<Option<Value>, String> {
    use playbook_store::{Connection, OpenFlags, OptionalExtension};
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_millis(2000))
        .map_err(|e| e.to_string())?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT value_json FROM config WHERE tier = 'global' AND scope = '' AND key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    raw.map(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
        .transpose()
}

/// Names the pre-0.1.5 standalone config file when it still exists, once.
fn old_file_notice(home: &Path) {
    for dir in ["playbook-telemetry", "playbook-usage"] {
        let old = home.join(".config").join(dir).join("config.json");
        if old.is_file() {
            warn_once(
                &WARNED_OLD_FILE,
                &format!(
                    "{} is no longer read; settings now come from {} (set them with `playbook config set usage.<key> <value>`)",
                    old.display(),
                    root(home).display()
                ),
            );
            return;
        }
    }
}

/// The stored value of `key` without validating it, so a caller can say why
/// an invalid value is ignored. `None` when nothing is stored.
pub fn stored_raw(home: &Path, key: &str) -> Option<Value> {
    stored(home, key)
}

/// The value of a usage key for the user at `home`: the stored value when it
/// is valid, else the default. `None` for a key that is not a usage key.
pub fn resolve(home: &Path, key: &str) -> Option<Value> {
    let default = default_value(key)?;
    old_file_notice(home);
    let Some(value) = stored(home, key) else {
        return Some(default);
    };
    match validate(key, &value) {
        Ok(()) => Some(value),
        Err(why) => {
            warn_once(
                &WARNED_VALUE,
                &format!("ignoring invalid value {value} for {key}: {why}"),
            );
            Some(default)
        }
    }
}

/// Stores `value` for `key` in this tool's own `app` tier of the config store.
/// Returns `false` when the store does not exist yet; it is never created
/// here, because a new `config.db` would hide an older store that playbook
/// has not moved. A failed write is an error the caller may ignore.
pub fn save_app(home: &Path, key: &str, value: &Value) -> Result<bool, String> {
    default_value(key).ok_or_else(|| format!("{key} is not a usage key"))?;
    validate(key, value)?;
    let db = playbook_data::config::db_in(&root(home).join("data"));
    playbook_data::config::set_app_in(
        &db,
        key,
        playbook_data::config::APP_TELEMETRY,
        &value.to_string(),
        crate::util::time::now_secs(),
    )
    .map_err(|e| format!("cannot write {}: {e}", db.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::scratch_dir;

    fn home(tag: &str) -> PathBuf {
        let h = scratch_dir(tag);
        std::fs::create_dir_all(root(&h)).unwrap();
        h
    }

    fn put_store(home: &Path, key: &str, json: &str) {
        let conn = playbook_store::Connection::open(root(home).join("playbook.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS config (tier TEXT NOT NULL, scope TEXT NOT NULL, key TEXT NOT NULL, value_json TEXT NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY (tier, scope, key)) WITHOUT ROWID;",
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO config VALUES ('global', '', ?1, ?2, 0)",
            [key, json],
        )
        .unwrap();
    }

    #[test]
    fn defaults_apply_when_nothing_is_stored() {
        let h = home("cfg-none");
        assert_eq!(resolve(&h, "usage.theme"), Some(Value::from("default")));
        assert_eq!(resolve(&h, "usage.budget.weeklyUsd"), Some(Value::from(0)));
        assert_eq!(resolve(&h, "model.default"), None);
    }

    #[test]
    fn the_playbook_store_is_read_and_unrelated_keys_are_ignored() {
        let h = home("cfg-store");
        put_store(&h, "usage.theme", "\"dark\"");
        put_store(&h, "usage.budget.monthlyUsd", "200");
        put_store(&h, "models.default", "\"opus\"");
        assert_eq!(resolve(&h, "usage.theme"), Some(Value::from("dark")));
        assert_eq!(
            resolve(&h, "usage.budget.monthlyUsd"),
            Some(Value::from(200))
        );
        assert_eq!(resolve(&h, "usage.weekStart"), Some(Value::from("monday")));
    }

    #[test]
    fn config_db_wins_over_playbook_db_and_the_app_tier_wins_over_global() {
        let h = home("cfg-shared");
        put_store(&h, "usage.theme", "\"light\"");
        let data = root(&h).join("data");
        std::fs::create_dir_all(&data).unwrap();
        let conn = playbook_store::Connection::open(playbook_data::config::db_in(&data)).unwrap();
        conn.execute_batch(playbook_data::config::SCHEMA_SQL)
            .unwrap();
        conn.execute_batch(
            "INSERT INTO config VALUES ('global','','usage.theme','\"dark\"',1);
             INSERT INTO config VALUES ('global','','usage.weekStart','\"sunday\"',1);
             INSERT INTO config VALUES ('app','playbook-telemetry','usage.weekStart','\"saturday\"',1);
             INSERT INTO config VALUES ('app','pbtop','usage.theme','\"mono\"',1);",
        )
        .unwrap();
        assert_eq!(resolve(&h, "usage.theme"), Some(Value::from("dark")));
        assert_eq!(
            resolve(&h, "usage.weekStart"),
            Some(Value::from("saturday"))
        );
    }

    #[test]
    fn the_legacy_nested_file_is_read_when_there_is_no_store() {
        let h = home("cfg-legacy");
        std::fs::write(
            root(&h).join("config.json"),
            r#"{"usage":{"weekStart":"sunday","budget":{"weeklyUsd":50}},"other":1}"#,
        )
        .unwrap();
        assert_eq!(resolve(&h, "usage.weekStart"), Some(Value::from("sunday")));
        assert_eq!(resolve(&h, "usage.budget.weeklyUsd"), Some(Value::from(50)));
        assert_eq!(resolve(&h, "usage.theme"), Some(Value::from("default")));
    }

    #[test]
    fn an_invalid_value_falls_back_to_the_default() {
        let h = home("cfg-invalid");
        put_store(&h, "usage.theme", "\"neon\"");
        put_store(&h, "usage.budget.monthlyUsd", "-3");
        assert_eq!(resolve(&h, "usage.theme"), Some(Value::from("default")));
        assert_eq!(resolve(&h, "usage.budget.monthlyUsd"), Some(Value::from(0)));
    }

    #[test]
    fn a_corrupt_file_or_store_falls_back_to_defaults() {
        let h = home("cfg-corrupt-json");
        std::fs::write(root(&h).join("config.json"), "{ not json").unwrap();
        assert_eq!(resolve(&h, "usage.theme"), Some(Value::from("default")));
        let h2 = home("cfg-corrupt-db");
        std::fs::write(
            root(&h2).join("playbook.db"),
            b"this is not a database at all",
        )
        .unwrap();
        assert_eq!(resolve(&h2, "usage.theme"), Some(Value::from("default")));
    }

    #[test]
    fn every_key_has_a_valid_default() {
        for key in KEYS {
            let d = default_value(key).unwrap();
            assert!(validate(key, &d).is_ok(), "{key}");
        }
    }

    #[test]
    fn the_cut_settings_are_not_usage_keys_and_stored_values_are_ignored() {
        let h = home("cfg-cut");
        put_store(&h, "usage.refreshSeconds", "30");
        for key in [
            "usage.refreshSeconds",
            "usage.telemetryRetentionDays",
            "usage.telemetryMaxRows",
            "usage.autoIngest",
            "usage.autoIngestMinutes",
        ] {
            assert_eq!(default_value(key), None, "{key}");
            assert_eq!(resolve(&h, key), None, "{key}");
        }
    }

    #[test]
    fn the_ui_keys_accept_good_text_and_refuse_bad_values() {
        let long = "x".repeat(MAX_STATE_BYTES + 1);
        let fits = "x".repeat(MAX_STATE_BYTES);

        assert!(validate("usage.ui.layout", &Value::from("Money=budget,models")).is_ok());
        assert!(validate("usage.ui.layout", &Value::from("A=bogus")).is_err());
        assert!(validate("usage.ui.layout", &Value::from(5)).is_err());
        assert!(validate("usage.ui.state", &Value::from(fits)).is_ok());
        assert!(validate("usage.ui.state", &Value::from(long)).is_err());
        assert!(validate("usage.ui.state", &Value::from(5)).is_err());
        assert!(validate("usage.maskAccount", &Value::from(true)).is_ok());
        assert!(validate("usage.maskAccount", &Value::from("yes")).is_err());
    }

    #[test]
    fn save_app_refuses_a_key_that_is_not_a_usage_key_or_a_bad_value() {
        let h = home("cfg-save-guard");

        let other = save_app(&h, "models.default", &Value::from("opus"));
        let bad = save_app(&h, "usage.ui.layout", &Value::from("A=bogus"));

        assert!(other.is_err());
        assert!(bad.is_err());
    }
}
