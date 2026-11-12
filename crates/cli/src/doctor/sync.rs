// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The doctor row for usage sync. Sync is off by default, so there is no row
//! until `sync.json` in the data folder turns it on. The cli does not depend on
//! `pbsync`: it reads the settings file into a private struct and `usage.db`
//! read-only. The row never holds the token or the URL.

use super::check::{Env, Level, Row};
use playbook_store::OptionalExtension;
use serde::Deserialize;
use std::path::Path;

const SETTINGS_FILE: &str = "sync.json";
const DEFAULT_TOKEN_ENV: &str = "PBSYNC_TOKEN";
const DEFAULT_INTERVAL_SECONDS: i64 = 900;
const ERROR_LIMIT: usize = 120;
const FIX_PUSH: &str = "run: playbook sync push";
const FIX_STATUS: &str = "run: playbook sync status";
const FIX_UPDATE: &str = "run: playbook update";
const KEY_LAST_SUCCESS: &str = "last_success";
const KEY_LAST_ERROR: &str = "last_error";
const KEY_FAILURES: &str = "failures";
const MARK_TABLE: &str = "usage_events";
const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

/// The fields of `sync.json` doctor reads; any other field is ignored.
#[derive(Deserialize)]
#[serde(default)]
struct Settings {
    enabled: bool,
    accounts: Vec<String>,
    token_env: String,
    interval_seconds: i64,
    enabled_at: Option<i64>,
    reset_watermark: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            accounts: Vec::new(),
            token_env: DEFAULT_TOKEN_ENV.to_string(),
            interval_seconds: DEFAULT_INTERVAL_SECONDS,
            enabled_at: None,
            reset_watermark: false,
        }
    }
}

/// What the local database says about the last pushes.
struct Local {
    last_success: Option<i64>,
    last_error: Option<String>,
    failures: i64,
    pending: i64,
}

/// What a probe reads from the machine, so tests can fix each part.
struct Inputs<'a> {
    now: i64,
    pbsync_found: bool,
    token_set: &'a dyn Fn(&str) -> bool,
}

pub(super) fn rows(env: &Env) -> Vec<Row> {
    let pbsync_found = crate::sync::helper_path(Some(&env.path_var)).is_some();
    let token_set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    let inputs = Inputs {
        now: crate::common::time::now_secs(),
        pbsync_found,
        token_set: &token_set,
    };
    row_for(&crate::usage::data::data_dir(&env.home), &inputs)
        .into_iter()
        .collect()
}

fn row_for(data_dir: &Path, inputs: &Inputs) -> Option<Row> {
    let path = data_dir.join(SETTINGS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(warn(format!(
                "sync: cannot read {SETTINGS_FILE} ({})",
                e.kind()
            )))
        }
    };
    let Ok(settings) = serde_json::from_str::<Settings>(&text) else {
        return Some(
            warn(format!(
                "sync: {SETTINGS_FILE} is not valid, left unchanged"
            ))
            .hint(format!("fix or delete {}", path.display())),
        );
    };
    if !settings.enabled {
        return None;
    }
    Some(enabled_row(data_dir, &settings, inputs))
}

fn warn(message: String) -> Row {
    Row::new("-", Level::Warn, message)
}

fn enabled_row(data_dir: &Path, settings: &Settings, inputs: &Inputs) -> Row {
    if !inputs.pbsync_found {
        return warn("sync: pbsync is missing".to_string()).hint(FIX_UPDATE);
    }
    let local = match read_local(&data_dir.join(crate::usage::data::USAGE_DB_FILE), settings) {
        Ok(Some(local)) => local,
        Ok(None) => {
            return warn("sync: local tables missing, run any playbook command".to_string())
        }
        Err(()) => return warn("sync: usage.db is unreadable".to_string()),
    };
    let age = local
        .last_success
        .map(|at| inputs.now.saturating_sub(at))
        .unwrap_or_else(|| {
            inputs
                .now
                .saturating_sub(settings.enabled_at.unwrap_or(inputs.now))
        });
    let since = local.last_success.map_or_else(
        || "never succeeded".to_string(),
        |_| format!("last success {} ago", short_age(age)),
    );
    let message = format!("sync: on, {since}, {} rows pending", local.pending);
    if !(inputs.token_set)(&settings.token_env) {
        return Row::new("-", Level::Fail, message).hint(format!(
            "set {}, then run: playbook sync status",
            settings.token_env
        ));
    }
    if let Some(error) = local.last_error.filter(|_| local.failures > 0) {
        let message = format!("{message}, last error: {}", one_line(&error));
        return warn(message).hint(FIX_STATUS);
    }
    if age > settings.interval_seconds {
        return warn(message).hint(FIX_PUSH);
    }
    Row::new("-", Level::Pass, message)
}

fn read_local(path: &Path, settings: &Settings) -> Result<Option<Local>, ()> {
    if !path.is_file() {
        return Ok(None);
    }
    let conn = crate::state::open_read_only(path).map_err(|_| ())?;
    let has = |table: &str| -> Result<bool, ()> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |r| r.get(0),
        )
        .map_err(|_| ())
    };
    if !(has("sync_state")? && has("sync_watermarks")? && has("usage_events")?) {
        return Ok(None);
    }
    let state = |key: &str| -> Result<Option<String>, ()> {
        conn.query_row("SELECT value FROM sync_state WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|_| ())
    };
    let mark: i64 = if settings.reset_watermark {
        0
    } else {
        conn.query_row(
            "SELECT rowid_mark FROM sync_watermarks WHERE table_name = ?1",
            [MARK_TABLE],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| ())?
        .unwrap_or(0)
    };
    let mut pending = 0;
    for account in &settings.accounts {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM usage_events WHERE rowid > ?1 AND account = ?2",
                (mark, account),
                |r| r.get(0),
            )
            .map_err(|_| ())?;
        pending += count;
    }
    Ok(Some(Local {
        last_success: state(KEY_LAST_SUCCESS)?.and_then(|v| v.parse().ok()),
        last_error: state(KEY_LAST_ERROR)?.filter(|e| !e.trim().is_empty()),
        failures: state(KEY_FAILURES)?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        pending,
    }))
}

fn short_age(seconds: i64) -> String {
    match seconds {
        s if s < MINUTE => format!("{s}s"),
        s if s < HOUR => format!("{}m", s / MINUTE),
        s if s < DAY => format!("{}h", s / HOUR),
        s => format!("{}d", s / DAY),
    }
}

/// The first line of an error, cut so one row stays one line.
fn one_line(error: &str) -> String {
    let line = error.lines().next().unwrap_or_default().trim();
    line.chars().take(ERROR_LIMIT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;

    const NOW: i64 = 1_800_000_000;
    const INTERVAL: i64 = 900;
    const SENTINEL: &str = "sentinel-token-value-123";
    const ACCOUNT: &str = "dev@example.com";

    fn data_dir(tag: &str) -> std::path::PathBuf {
        let dir = scratch_dir(tag).join("data");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_settings(dir: &Path, enabled: bool, extra: &str) {
        let body = format!(
            r#"{{"version":1,"enabled":{enabled},"url":"https://sync.example.com/secret/path?k=v",
            "accounts":["{ACCOUNT}"],"token_env":"PB_TEST_TOKEN","interval_seconds":{INTERVAL},
            "enabled_at":{},"future_field":true{extra}}}"#,
            NOW - 10_000
        );
        std::fs::write(dir.join(SETTINGS_FILE), body).unwrap();
    }

    /// A usage.db with the sync tables, one event per rowid, and `state` rows.
    fn make_db(dir: &Path, state: &[(&str, &str)], mark: i64, events: &[&str]) {
        let conn = playbook_store::Connection::open(dir.join("usage.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE usage_events (event_id TEXT PRIMARY KEY, account TEXT NOT NULL);
             CREATE TABLE sync_watermarks (table_name TEXT PRIMARY KEY, rowid_mark INTEGER NOT NULL);
             CREATE TABLE sync_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        for (i, account) in events.iter().enumerate() {
            conn.execute(
                "INSERT INTO usage_events VALUES (?1, ?2)",
                (i.to_string(), account),
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO sync_watermarks VALUES ('usage_events', ?1)",
            [mark],
        )
        .unwrap();
        for (key, value) in state {
            conn.execute("INSERT INTO sync_state VALUES (?1, ?2)", (key, value))
                .unwrap();
        }
    }

    fn probe(dir: &Path, token_set: bool, pbsync_found: bool) -> Option<Row> {
        let lookup = move |name: &str| token_set && name == "PB_TEST_TOKEN";
        row_for(
            dir,
            &Inputs {
                now: NOW,
                pbsync_found,
                token_set: &lookup,
            },
        )
    }

    fn last_success(age: i64) -> String {
        (NOW - age).to_string()
    }

    #[test]
    fn no_row_when_sync_is_off() {
        // Arrange
        let none = data_dir("sync-none");
        let disabled = data_dir("sync-disabled");
        write_settings(&disabled, false, "");

        // Act
        let rows = [probe(&none, true, true), probe(&disabled, true, true)];

        // Assert
        assert_eq!(rows, [None, None]);
    }

    #[test]
    fn the_row_follows_the_age_of_the_last_success() {
        for (age, level, hint) in [
            (INTERVAL - 1, Level::Pass, None),
            (INTERVAL, Level::Pass, None),
            (INTERVAL + 1, Level::Warn, Some(FIX_PUSH)),
        ] {
            // Arrange
            let dir = data_dir(&format!("sync-age-{age}"));
            write_settings(&dir, true, "");
            make_db(
                &dir,
                &[(KEY_LAST_SUCCESS, &last_success(age))],
                0,
                &[ACCOUNT],
            );

            // Act
            let row = probe(&dir, true, true).unwrap();

            // Assert
            assert_eq!(row.level, level, "{age}: {row:?}");
            assert_eq!(row.hint.as_deref(), hint, "{age}");
            assert!(
                row.message.starts_with("sync: on, last success "),
                "{row:?}"
            );
            assert!(row.message.ends_with("1 rows pending"), "{row:?}");
        }
    }

    #[test]
    fn a_healthy_row_says_how_long_ago_and_how_many_pending() {
        // Arrange
        let dir = data_dir("sync-healthy");
        write_settings(&dir, true, "");
        make_db(&dir, &[(KEY_LAST_SUCCESS, &last_success(125))], 0, &[]);

        // Act
        let row = probe(&dir, true, true).unwrap();

        // Assert
        assert_eq!(row.message, "sync: on, last success 2m ago, 0 rows pending");
        assert_eq!(row.level, Level::Pass);
    }

    #[test]
    fn pending_counts_rows_past_the_mark_for_the_synced_accounts() {
        // Arrange
        let dir = data_dir("sync-pending");
        write_settings(&dir, true, "");
        make_db(
            &dir,
            &[(KEY_LAST_SUCCESS, &last_success(1))],
            1,
            &[ACCOUNT, ACCOUNT, "other", ACCOUNT],
        );

        // Act
        let row = probe(&dir, true, true).unwrap();

        // Assert
        assert!(row.message.ends_with("2 rows pending"), "{row:?}");
    }

    #[test]
    fn never_succeeded_is_stale_only_after_the_interval_since_enabling() {
        // Arrange
        let dir = data_dir("sync-never");
        write_settings(&dir, true, "");
        make_db(&dir, &[], 0, &[]);

        // Act
        let row = probe(&dir, true, true).unwrap();

        // Assert
        assert_eq!(row.level, Level::Warn);
        assert!(row.message.contains("never succeeded"), "{row:?}");
        assert_eq!(row.hint.as_deref(), Some(FIX_PUSH));
    }

    #[test]
    fn never_succeeded_inside_the_interval_is_healthy() {
        // Arrange
        let dir = data_dir("sync-fresh");
        write_settings(&dir, true, "");
        let text = std::fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap();
        std::fs::write(
            dir.join(SETTINGS_FILE),
            text.replace(&(NOW - 10_000).to_string(), &(NOW - 5).to_string()),
        )
        .unwrap();
        make_db(&dir, &[], 0, &[]);

        // Act
        let row = probe(&dir, true, true).unwrap();

        // Assert
        assert_eq!(row.level, Level::Pass, "{row:?}");
    }

    #[test]
    fn a_recorded_error_with_failures_warns_with_one_line_and_the_status_fix() {
        // Arrange
        let dir = data_dir("sync-error");
        write_settings(&dir, true, "");
        make_db(
            &dir,
            &[
                (KEY_LAST_SUCCESS, &last_success(1)),
                (KEY_LAST_ERROR, "server said no\nsecond line"),
                (KEY_FAILURES, "3"),
            ],
            0,
            &[],
        );

        // Act
        let row = probe(&dir, true, true).unwrap();

        // Assert
        assert_eq!(row.level, Level::Warn);
        assert!(
            row.message.ends_with("last error: server said no"),
            "{row:?}"
        );
        assert_eq!(row.hint.as_deref(), Some(FIX_STATUS));
    }

    #[test]
    fn the_fix_text_names_playbook_sync() {
        // Arrange
        let dir = data_dir("sync-fix-text");
        write_settings(&dir, true, "");
        make_db(
            &dir,
            &[(KEY_FAILURES, "2"), (KEY_LAST_ERROR, "boom")],
            0,
            &[],
        );

        // Act
        let failing = probe(&dir, true, true).unwrap();
        let stale = {
            let dir = data_dir("sync-fix-stale");
            write_settings(&dir, true, "");
            make_db(
                &dir,
                &[(KEY_LAST_SUCCESS, &last_success(INTERVAL * 3))],
                0,
                &[],
            );
            probe(&dir, true, true).unwrap()
        };

        // Assert
        assert_eq!(failing.hint.as_deref(), Some("run: playbook sync status"));
        assert_eq!(stale.hint.as_deref(), Some("run: playbook sync push"));
    }

    #[test]
    fn a_missing_helper_hint_says_to_run_playbook_update() {
        // Arrange
        let dir = data_dir("sync-missing-hint");
        write_settings(&dir, true, "");

        // Act
        let row = probe(&dir, true, false).unwrap();

        // Assert
        assert_eq!(row.hint.as_deref(), Some("run: playbook update"));
    }

    #[test]
    fn an_unset_token_fails_naming_the_variable_only() {
        // Arrange
        let dir = data_dir("sync-token");
        write_settings(&dir, true, "");
        make_db(&dir, &[(KEY_LAST_SUCCESS, &last_success(1))], 0, &[]);

        // Act
        let unset = probe(&dir, false, true).unwrap();
        let set = probe(&dir, true, true).unwrap();

        // Assert
        assert_eq!(unset.level, Level::Fail);
        assert_eq!(
            unset.hint.as_deref(),
            Some("set PB_TEST_TOKEN, then run: playbook sync status")
        );
        assert_eq!(set.level, Level::Pass);
    }

    #[test]
    fn missing_sync_tables_say_to_run_any_command() {
        // Arrange
        let dir = data_dir("sync-tables");
        write_settings(&dir, true, "");
        let conn = playbook_store::Connection::open(dir.join("usage.db")).unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT)")
            .unwrap();
        drop(conn);

        // Act
        let row = probe(&dir, true, true).unwrap();

        // Assert
        assert_eq!(row.level, Level::Warn);
        assert_eq!(
            row.message,
            "sync: local tables missing, run any playbook command"
        );
    }

    #[test]
    fn a_missing_pbsync_binary_warns() {
        // Arrange
        let dir = data_dir("sync-nobin");
        write_settings(&dir, true, "");
        make_db(&dir, &[(KEY_LAST_SUCCESS, &last_success(1))], 0, &[]);

        // Act
        let row = probe(&dir, true, false).unwrap();

        // Assert
        assert_eq!(row.level, Level::Warn);
        assert_eq!(row.message, "sync: pbsync is missing");
    }

    #[test]
    fn a_corrupt_settings_file_warns_and_is_left_alone() {
        // Arrange
        let dir = data_dir("sync-corrupt");
        std::fs::write(dir.join(SETTINGS_FILE), "{not json").unwrap();

        // Act
        let row = probe(&dir, true, true).unwrap();

        // Assert
        assert_eq!(row.level, Level::Warn);
        assert!(row.message.contains(SETTINGS_FILE), "{row:?}");
        assert_eq!(
            std::fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap(),
            "{not json"
        );
    }

    #[test]
    fn the_row_never_shows_the_token_or_the_url() {
        // Arrange
        let dir = data_dir("sync-secret");
        write_settings(&dir, true, "");
        make_db(
            &dir,
            &[
                (KEY_LAST_SUCCESS, &last_success(1)),
                (KEY_LAST_ERROR, "boom"),
                (KEY_FAILURES, "1"),
            ],
            0,
            &[],
        );
        let lookup = |_: &str| true;

        // Act
        let row = row_for(
            &dir,
            &Inputs {
                now: NOW,
                pbsync_found: true,
                token_set: &lookup,
            },
        )
        .unwrap();
        let shown = format!("{row:?}");

        // Assert
        for secret in [SENTINEL, "secret/path", "k=v", "sync.example.com"] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }
}
