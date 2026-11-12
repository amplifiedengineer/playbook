// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! SQLite storage for playbook's tiered config: one `playbook.db` under the
//! playbook root replaces the three JSON file shapes (global, org, repo).
//!
//! Rows are `(tier, scope, key) -> value_json`, where `scope` is empty for the
//! global tier, the owner for an org and the whole slug for a repo. The first
//! open imports any legacy `config.json` files in one transaction and renames
//! them to `config.json.migrated`. WAL mode and a busy timeout replace the
//! directory lock the JSON files needed.

use super::write::Tier;
use super::{keys, ConfigError};
use playbook_store::schema::{self, SchemaError};
use playbook_store::{params, Connection, OptionalExtension};
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use playbook_data::config::SCHEMA_SQL;

/// The `config.db` layout this build writes. A database without the row counts
/// as version 0 and is stamped on open.
pub const SCHEMA_VERSION: i64 = 1;

/// The name `config.db` carries in version errors.
const DB_NAME: &str = "config.db";

/// The meta row that records the legacy JSON import ran.
const IMPORTED_FLAG: &str = "legacy_imported";

/// Where the database lives under `root`: `data/config.db`.
pub fn db_path(root: &Path) -> PathBuf {
    playbook_data::config::db_in(&root.join("data"))
}

/// Where earlier versions kept the same tables, `playbook.db` under `root`.
fn legacy_db_path(root: &Path) -> PathBuf {
    playbook_data::config::legacy_db_in(root)
}

/// How the move from `playbook.db` merges when both files exist (an older
/// playbook wrote the old path again): the row with the later `updated_at`
/// wins, so a setting changed by an old binary is not lost and one changed
/// since is not undone.
const MOVE_RULES: &[(&str, playbook_data::migrate::Rule)] = &[
    ("config", playbook_data::migrate::Rule::Newer("updated_at")),
    ("state", playbook_data::migrate::Rule::Newer("updated_at")),
    ("meta", playbook_data::migrate::Rule::Ignore),
];

/// Move `playbook.db` into the data folder as `config.db`. A deferred or
/// failed move is not an error: the next open merges what is left.
fn move_legacy_db(root: &Path) {
    let step = playbook_data::migrate::Move {
        old: legacy_db_path(root),
        new: db_path(root),
        policy: playbook_data::migrate::Policy::Merge(MOVE_RULES),
    };
    if let Ok(playbook_data::migrate::Outcome::Corrupt(_)) =
        playbook_data::migrate::migrate_with_notice(&step, "settings")
    {
        // Park it, so later opens do not repeat the check, and say so once.
        if playbook_data::migrate::set_aside(&step.old).is_ok() {
            playbook_data::migrate::notice(
                "playbook: the old settings database is unreadable and was renamed to \
                 playbook.db.migrated; settings start from their defaults",
            );
        }
    }
}

/// Whether there is anything to read: a database, or legacy JSON files that
/// the first open would import. When false, every key resolves to its default
/// and nothing needs creating.
fn has_data(root: &Path) -> bool {
    db_path(root).exists()
        || legacy_db_path(root).exists()
        || super::global_config_path(root).exists()
        || super::any_scoped_config(root)
}

fn tier_str(tier: Tier) -> &'static str {
    match tier {
        Tier::Global => "global",
        Tier::Org => "org",
        Tier::Repo => "repo",
    }
}

/// A database error is reported as the database path, the same shape a
/// malformed JSON file had, so callers keep one error case for "config store
/// unreadable".
fn broken(root: &Path) -> impl Fn(playbook_store::Error) -> ConfigError + '_ {
    move |_| ConfigError::Malformed(db_path(root))
}

/// Open (creating if needed) the database under `root`, apply the schema and
/// run the one-time legacy import.
pub fn open(root: &Path) -> Result<Connection, ConfigError> {
    fs::create_dir_all(root).map_err(|_| ConfigError::DirectoryUnwritable(root.to_path_buf()))?;
    let data = root.join("data");
    crate::common::atomic::ensure_private_dir(&data)
        .map_err(|_| ConfigError::DirectoryUnwritable(data.clone()))?;
    move_legacy_db(root);
    let conn = init_connection(root).map_err(|e| match e {
        // Securing or opening the file failed (permissions, read-only
        // volume): say so, rather than calling the store malformed.
        SchemaError::Store(playbook_store::Error::SqliteFailure(f, _))
            if f.code == playbook_store::ErrorCode::CannotOpen =>
        {
            ConfigError::DirectoryUnwritable(data.clone())
        }
        SchemaError::Store(other) => broken(root)(other),
        refused => ConfigError::SchemaRefused(refused.to_string()),
    })?;
    import_legacy_once(&conn, root)?;
    Ok(conn)
}

/// Open the file and apply pragmas and schema. Switching a fresh file to WAL
/// and creating tables from several processes at once can report "busy"
/// without waiting, so that one error is retried for a short while.
fn init_connection(root: &Path) -> Result<Connection, SchemaError> {
    let attempt = || -> Result<Connection, SchemaError> {
        // While the move is deferred (a lock is held, a writer is busy) the
        // old file is used as it is, instead of starting an empty new one.
        let path = if !db_path(root).exists() && legacy_db_path(root).is_file() {
            legacy_db_path(root)
        } else {
            db_path(root)
        };
        // Owner-only before SQLite can create it with the default umask; the
        // WAL and shm files copy this mode.
        crate::common::atomic::make_db_file_private(&path).map_err(|msg| {
            playbook_store::Error::SqliteFailure(
                playbook_store::ffi::Error::new(playbook_store::ffi::SQLITE_CANTOPEN),
                Some(msg),
            )
        })?;
        let conn = Connection::open(path)?;
        // Before any statement that can contend for the write lock: the
        // default busy timeout is 0.
        conn.busy_timeout(Duration::from_millis(5000))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // In WAL mode NORMAL cannot corrupt the file, and it skips an fsync
        // per commit. A power cut can lose the last write, never the store.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Before any table is created, so a newer database is left untouched.
        schema::check_and_stamp(&conn, DB_NAME, SCHEMA_VERSION)?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(conn)
    };
    let mut tries = 0;
    loop {
        match attempt() {
            Err(SchemaError::Store(playbook_store::Error::SqliteFailure(err, _)))
                if tries < 200
                    && matches!(
                        err.code,
                        playbook_store::ErrorCode::DatabaseBusy
                            | playbook_store::ErrorCode::DatabaseLocked
                    ) =>
            {
                tries += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

/// The global tier value of `key`, read through a read-only connection that
/// changes nothing on disk. `None` when the database, the table or the row is
/// missing or cannot be read.
pub fn global_read_only(root: &Path, key: &str) -> Option<Value> {
    let path = db_path(root);
    if !path.is_file() {
        return None;
    }
    let conn = crate::state::open_read_only(&path).ok()?;
    let raw: String = conn
        .query_row(
            "SELECT value_json FROM config WHERE tier = 'global' AND scope = '' AND key = ?1",
            params![key],
            |row| row.get(0),
        )
        .ok()?;
    serde_json::from_str(&raw).ok()
}

/// The value of `key` per tier, for the repo slug and its owner.
/// Precedence is the caller's: this only reports what each tier holds.
pub struct TierValues {
    pub repo: Option<Value>,
    pub org: Option<Value>,
    pub global: Option<Value>,
}

/// A connection for reading a store that is already set up: the file is
/// there and owner-only, no old `playbook.db` waits to be merged, and the
/// legacy import ran. Nothing then needs creating, moving, tightening or
/// importing, so the read skips the write-lock pragmas and the schema batch
/// of `open`. `None` sends the caller to `open`, which does all of that.
fn open_ready(root: &Path) -> Option<Connection> {
    let path = db_path(root);
    if legacy_db_path(root).exists() || !owner_only(&path) {
        return None;
    }
    let conn = crate::state::open_read_only(&path).ok()?;
    conn.query_row(
        "SELECT 1 FROM meta WHERE key = ?1",
        params![IMPORTED_FLAG],
        |_| Ok(()),
    )
    .optional()
    .ok()??;
    Some(conn)
}

/// Whether `path` is a file only its owner can touch, the mode `open` sets.
fn owner_only(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o077 == 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Look `key` up in the repo, org and global tiers with one query.
pub fn lookup(root: &Path, key: &str, repo_slug: Option<&str>) -> Result<TierValues, ConfigError> {
    if !has_data(root) {
        return Ok(TierValues {
            repo: None,
            org: None,
            global: None,
        });
    }
    if let Some(found) = open_ready(root).and_then(|c| tier_values(&c, root, key, repo_slug).ok()) {
        return Ok(found);
    }
    tier_values(&open(root)?, root, key, repo_slug)
}

/// The rows of `key` per tier, read from `conn`.
fn tier_values(
    conn: &Connection,
    root: &Path,
    key: &str,
    repo_slug: Option<&str>,
) -> Result<TierValues, ConfigError> {
    let mut out = TierValues {
        repo: None,
        org: None,
        global: None,
    };
    let (repo_scope, org_scope) = match repo_slug.and_then(|s| s.split_once('/')) {
        Some((owner, _)) => (repo_slug.unwrap_or_default(), owner),
        None => ("\u{0}", "\u{0}"),
    };
    // Rows written under an old org name still apply to a repo of the current
    // org. The current name wins; the old names follow, newest first.
    let (repo_name, owner) = repo_slug
        .and_then(|s| s.split_once('/'))
        .map_or(("", ""), |(o, r)| (r, o));
    let mut repo_scopes = vec![repo_scope.to_string()];
    let mut org_scopes = vec![org_scope.to_string()];
    for legacy in crate::common::org::legacy_owners_for(owner) {
        repo_scopes.push(format!("{legacy}/{repo_name}"));
        org_scopes.push((*legacy).to_string());
    }
    let mut stmt = conn
        .prepare(
            "SELECT tier, value_json FROM config WHERE key = ?1 AND \
             ((tier = 'repo' AND scope = ?2) OR (tier = 'org' AND scope = ?3) \
              OR (tier = 'global' AND scope = ''))",
        )
        .map_err(broken(root))?;
    for (repo_scope, org_scope) in repo_scopes.iter().zip(&org_scopes) {
        let rows = stmt
            .query_map(params![key, repo_scope, org_scope], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(broken(root))?;
        for row in rows {
            let (tier, raw) = row.map_err(broken(root))?;
            let value: Value =
                serde_json::from_str(&raw).map_err(|_| ConfigError::Malformed(db_path(root)))?;
            let slot = match tier.as_str() {
                "repo" => &mut out.repo,
                "org" => &mut out.org,
                _ => &mut out.global,
            };
            if slot.is_none() {
                *slot = Some(value);
            }
        }
    }
    Ok(out)
}

/// Whether any org or repo tier value exists. When none does, resolving with
/// or without a slug gives the same answer, so a caller can skip the slug
/// lookup (a `git` spawn).
pub fn any_scoped(root: &Path) -> bool {
    if db_path(root).exists() || legacy_db_path(root).exists() {
        let scoped = |conn: &Connection| {
            conn.query_row(
                "SELECT 1 FROM config WHERE tier != 'global' LIMIT 1",
                [],
                |_| Ok(()),
            )
            .optional()
        };
        if let Some(found) = open_ready(root).and_then(|c| scoped(&c).ok()) {
            return found.is_some();
        }
        let Ok(conn) = open(root) else { return false };
        return scoped(&conn).ok().flatten().is_some();
    }
    super::any_scoped_config(root)
}

/// The scope string a write to `tier` uses for `repo_slug`.
fn scope_for(tier: Tier, repo_slug: Option<&str>) -> Result<String, ConfigError> {
    match tier {
        Tier::Global => Ok(String::new()),
        Tier::Org => repo_slug
            .and_then(|s| s.split_once('/'))
            .map(|(owner, _)| owner.to_string())
            .ok_or(ConfigError::MissingRepoContext),
        Tier::Repo => repo_slug
            .filter(|s| s.split_once('/').is_some())
            .map(str::to_string)
            .ok_or(ConfigError::MissingRepoContext),
    }
}

/// Store `value` for `key` in `tier`. The caller has validated both.
pub fn put(
    root: &Path,
    tier: Tier,
    repo_slug: Option<&str>,
    key: &str,
    value: &Value,
) -> Result<(), ConfigError> {
    let scope = scope_for(tier, repo_slug)?;
    let conn = open(root)?;
    upsert(&conn, tier_str(tier), &scope, key, value).map_err(broken(root))
}

fn upsert(
    conn: &Connection,
    tier: &str,
    scope: &str,
    key: &str,
    value: &Value,
) -> playbook_store::Result<()> {
    upsert_at(
        conn,
        tier,
        scope,
        key,
        value,
        crate::common::time::now_secs(),
    )
}

/// [`upsert`] with an explicit `updated_at`. Values read from a leftover
/// `config.json` get 0, so the newer-write-wins merge of the old settings file
/// never ranks them above a real setting.
fn upsert_at(
    conn: &Connection,
    tier: &str,
    scope: &str,
    key: &str,
    value: &Value,
    updated_at: i64,
) -> playbook_store::Result<()> {
    conn.execute(
        "INSERT INTO config (tier, scope, key, value_json, updated_at) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (tier, scope, key) DO UPDATE SET \
         value_json = excluded.value_json, updated_at = excluded.updated_at",
        params![tier, scope, key, value.to_string(), updated_at],
    )
    .map(|_| ())
}

/// Every stored value as one JSON document, for `playbook config export`:
/// `{"version": 1, "global": {key: value}, "orgs": {owner: {...}}, "repos": {slug: {...}}}`.
pub fn export(root: &Path) -> Result<Value, ConfigError> {
    let mut global = Map::new();
    let mut orgs: Map<String, Value> = Map::new();
    let mut repos: Map<String, Value> = Map::new();
    if has_data(root) {
        let conn = open(root)?;
        let mut stmt = conn
            .prepare("SELECT tier, scope, key, value_json FROM config ORDER BY tier, scope, key")
            .map_err(broken(root))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(broken(root))?;
        for row in rows {
            let (tier, scope, key, raw) = row.map_err(broken(root))?;
            // A key a release retired stays in the database. Import rejects it
            // as unknown, so leaving it out keeps an export importable.
            if keys::default_value(&key).is_none() {
                continue;
            }
            let value: Value =
                serde_json::from_str(&raw).map_err(|_| ConfigError::Malformed(db_path(root)))?;
            let target = match tier.as_str() {
                "org" => orgs
                    .entry(scope)
                    .or_insert_with(|| Value::Object(Map::new())),
                "repo" => repos
                    .entry(scope)
                    .or_insert_with(|| Value::Object(Map::new())),
                // A tool's own app tier row is not a global setting.
                "app" => continue,
                _ => {
                    global.insert(key, value);
                    continue;
                }
            };
            if let Some(obj) = target.as_object_mut() {
                obj.insert(key, value);
            }
        }
    }
    Ok(serde_json::json!({
        "version": 1,
        "global": global,
        "orgs": orgs,
        "repos": repos,
    }))
}

/// Load an `export` document back in one transaction. Every key and value is
/// validated first, so a bad document changes nothing.
pub fn import(root: &Path, doc: &Value) -> Result<usize, ConfigError> {
    let mut rows: Vec<(&'static str, String, String, Value)> = Vec::new();
    let mut take = |tier: &'static str, scope: &str, map: &Value| -> Result<(), ConfigError> {
        let Some(obj) = map.as_object() else {
            return Ok(());
        };
        for (key, value) in obj {
            if super::keys::is_retired(key) {
                continue;
            }
            super::write::validate_key_and_value(key, value)?;
            super::write::require_global_for_user_key(key, tier == "global")?;
            rows.push((tier, scope.to_string(), key.clone(), value.clone()));
        }
        Ok(())
    };
    take("global", "", doc.get("global").unwrap_or(&Value::Null))?;
    for (tier, field) in [("org", "orgs"), ("repo", "repos")] {
        if let Some(scopes) = doc.get(field).and_then(Value::as_object) {
            for (scope, map) in scopes {
                if tier == "repo" && scope.split_once('/').is_none() {
                    return Err(ConfigError::MissingRepoContext);
                }
                take(tier, scope, map)?;
            }
        }
    }
    let mut conn = open(root)?;
    let tx = conn
        .transaction_with_behavior(playbook_store::TransactionBehavior::Immediate)
        .map_err(broken(root))?;
    for (tier, scope, key, value) in &rows {
        upsert(&tx, tier, scope, key, value).map_err(broken(root))?;
    }
    tx.commit().map_err(broken(root))?;
    Ok(rows.len())
}

/// One legacy file: the tier and scope it held, and where it is.
struct LegacyFile {
    tier: &'static str,
    scope: String,
    path: PathBuf,
}

/// Every legacy `config.json` under `root`: global, `orgs/<owner>`, and
/// `repos/<slug>/.config` (a slug can have more than two segments).
fn legacy_files(root: &Path) -> Vec<LegacyFile> {
    let mut out = Vec::new();
    let global = super::global_config_path(root);
    if global.exists() {
        out.push(LegacyFile {
            tier: "global",
            scope: String::new(),
            path: global,
        });
    }
    if let Ok(entries) = fs::read_dir(root.join("orgs")) {
        for entry in entries.flatten() {
            let path = entry.path().join("config.json");
            if path.exists() {
                out.push(LegacyFile {
                    tier: "org",
                    scope: entry.file_name().to_string_lossy().into_owned(),
                    path,
                });
            }
        }
    }
    let repos = root.join("repos");
    let mut stack: Vec<(PathBuf, usize)> = vec![(repos.clone(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if dir.file_name().is_some_and(|n| n == ".config") {
            let path = dir.join("config.json");
            if path.exists() {
                let slug = dir
                    .parent()
                    .and_then(|p| p.strip_prefix(&repos).ok())
                    .map(|p| {
                        p.components()
                            .map(|c| c.as_os_str().to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                            .join("/")
                    })
                    .unwrap_or_default();
                out.push(LegacyFile {
                    tier: "repo",
                    scope: slug,
                    path,
                });
            }
            continue;
        }
        if depth < 8 {
            if let Ok(rd) = fs::read_dir(&dir) {
                for e in rd.flatten().filter(|e| e.path().is_dir()) {
                    stack.push((e.path(), depth + 1));
                }
            }
        }
    }
    out
}

/// Every non-object value in `node` with its dotted path.
fn flatten_leaves(node: &Value, prefix: &str, out: &mut Vec<(String, Value)>) {
    match node.as_object() {
        Some(obj) => {
            for (k, v) in obj {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_leaves(v, &key, out);
            }
        }
        None if !prefix.is_empty() => out.push((prefix.to_string(), node.clone())),
        None => {}
    }
}

/// Rewrites every stored `key` whose value is the number `old` to `new`, in
/// every tier and scope, in one transaction. Any other value is left as it
/// is, and a store that holds nothing is not created. Returns how many values
/// changed.
pub fn replace_number(root: &Path, key: &str, old: f64, new: &Value) -> Result<usize, ConfigError> {
    replace_matching(root, key, |stored| stored.as_f64() == Some(old), new)
}

/// [`replace_number`] for any JSON value: rewrites every stored `key` that
/// equals `old` to `new`. A number compares by its JSON form, so use
/// [`replace_number`] for one.
pub fn replace_value(
    root: &Path,
    key: &str,
    old: &Value,
    new: &Value,
) -> Result<usize, ConfigError> {
    replace_matching(root, key, |stored| stored == old, new)
}

fn replace_matching(
    root: &Path,
    key: &str,
    is_old: impl Fn(&Value) -> bool,
    new: &Value,
) -> Result<usize, ConfigError> {
    if !has_data(root) {
        return Ok(0);
    }
    let mut conn = open(root)?;
    // Immediate, so a write by another process between the read and the
    // first update waits on the busy timeout instead of failing the upgrade.
    let tx = conn
        .transaction_with_behavior(playbook_store::TransactionBehavior::Immediate)
        .map_err(broken(root))?;
    let rows = {
        let mut stmt = tx
            .prepare("SELECT tier, scope, value_json FROM config WHERE key = ?1")
            .map_err(broken(root))?;
        let rows = stmt
            .query_map([key], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(broken(root))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(broken(root))?
    };
    let mut changed = 0;
    for (tier, scope, raw) in rows {
        let stored = serde_json::from_str::<Value>(&raw).ok();
        if stored.as_ref().is_some_and(&is_old) {
            upsert(&tx, &tier, &scope, key, new).map_err(broken(root))?;
            changed += 1;
        }
    }
    tx.commit().map_err(broken(root))?;
    Ok(changed)
}

/// The keys the global tier holds, sorted. Empty when there is no database.
pub fn global_keys(root: &Path) -> Result<Vec<String>, ConfigError> {
    if !has_data(root) {
        return Ok(Vec::new());
    }
    let conn = open(root)?;
    let mut stmt = conn
        .prepare("SELECT key FROM config WHERE tier = 'global' AND scope = '' ORDER BY key")
        .map_err(broken(root))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(broken(root))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(broken(root))
}

/// Every value stored for `key` in any tier, in tier then scope order. Empty
/// when there is no database.
pub fn stored_values(root: &Path, key: &str) -> Result<Vec<Value>, ConfigError> {
    if !has_data(root) {
        return Ok(Vec::new());
    }
    let conn = open(root)?;
    let mut stmt = conn
        .prepare("SELECT value_json FROM config WHERE key = ?1 ORDER BY tier, scope")
        .map_err(broken(root))?;
    let rows = stmt
        .query_map([key], |r| r.get::<_, String>(0))
        .map_err(broken(root))?;
    rows.map(|row| {
        let raw = row.map_err(broken(root))?;
        serde_json::from_str(&raw).map_err(|_| ConfigError::Malformed(db_path(root)))
    })
    .collect()
}

/// The distinct keys of every tier that start with `prefix`, sorted. Empty
/// when there is no database.
pub fn keys_with_prefix(root: &Path, prefix: &str) -> Result<Vec<String>, ConfigError> {
    if !has_data(root) {
        return Ok(Vec::new());
    }
    let conn = open(root)?;
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT key FROM config WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key",
        )
        .map_err(broken(root))?;
    let rows = stmt
        .query_map([prefix], |r| r.get::<_, String>(0))
        .map_err(broken(root))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(broken(root))
}

/// What `adopt_late_files` did with `config.json` files that appeared after
/// the one-time import (an older binary, or an old doc, wrote them).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LateReport {
    /// Files whose values were all new or equal: imported and renamed.
    pub imported: Vec<PathBuf>,
    /// Files left in place: malformed, or a key differs from the stored value.
    pub conflicts: Vec<PathBuf>,
}

/// Adopt `config.json` files that appeared after the import. A file whose
/// known keys are all absent from the store, or equal to it, is imported and
/// renamed to `config.json.migrated`. A file that disagrees with a stored
/// value, or does not parse, is left as it is and reported, so nothing is
/// overwritten silently. Does nothing before the first import.
pub fn adopt_late_files(root: &Path) -> Result<LateReport, ConfigError> {
    let mut report = LateReport::default();
    let files = legacy_files(root);
    if files.is_empty() || !db_path(root).exists() {
        return Ok(report);
    }
    let conn = open(root)?;
    // `open` imports on a first run; whatever is still on disk now is late.
    for file in legacy_files(root) {
        let parsed = fs::read_to_string(&file.path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .filter(Value::is_object);
        let Some(parsed) = parsed else {
            report.conflicts.push(file.path);
            continue;
        };
        let mut leaves = Vec::new();
        flatten_leaves(&parsed, "", &mut leaves);
        leaves.retain(|(key, _)| keys::default_value(key).is_some());
        let mut differs = false;
        for (key, value) in &leaves {
            let stored: Option<String> = conn
                .query_row(
                    "SELECT value_json FROM config WHERE tier = ?1 AND scope = ?2 AND key = ?3",
                    params![file.tier, file.scope, key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(broken(root))?;
            if let Some(stored) = stored {
                if serde_json::from_str::<Value>(&stored).ok().as_ref() != Some(value) {
                    differs = true;
                    break;
                }
            }
        }
        if differs {
            report.conflicts.push(file.path);
            continue;
        }
        for (key, value) in &leaves {
            upsert_at(&conn, file.tier, &file.scope, key, value, 0).map_err(broken(root))?;
        }
        let _ = fs::rename(&file.path, file.path.with_extension("json.migrated"));
        report.imported.push(file.path);
    }
    Ok(report)
}

/// Import the legacy JSON files once. The flag is checked again inside the
/// write transaction, so two processes opening a fresh database at the same
/// time import exactly once. A file that is not a JSON object fails the whole
/// import with its path and is left untouched, as it failed reads before.
fn import_legacy_once(conn: &Connection, root: &Path) -> Result<(), ConfigError> {
    let flagged = |c: &Connection| -> Result<bool, ConfigError> {
        c.query_row(
            "SELECT 1 FROM meta WHERE key = ?1",
            params![IMPORTED_FLAG],
            |_| Ok(()),
        )
        .optional()
        .map(|r| r.is_some())
        .map_err(broken(root))
    };
    if flagged(conn)? {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(broken(root))?;
    let outcome = (|| -> Result<Vec<PathBuf>, ConfigError> {
        if flagged(conn)? {
            return Ok(Vec::new());
        }
        let mut done = Vec::new();
        for file in legacy_files(root) {
            let raw = fs::read_to_string(&file.path)
                .map_err(|_| ConfigError::Malformed(file.path.clone()))?;
            let parsed: Value = serde_json::from_str(&raw)
                .map_err(|_| ConfigError::Malformed(file.path.clone()))?;
            if !parsed.is_object() {
                return Err(ConfigError::Malformed(file.path));
            }
            let mut leaves = Vec::new();
            flatten_leaves(&parsed, "", &mut leaves);
            for (key, value) in leaves {
                // Known keys and `effort.<kind>.<name>` keys; stale or future
                // keys stay in the `.migrated` file.
                if keys::default_value(&key).is_some() {
                    upsert_at(conn, file.tier, &file.scope, &key, &value, 0)
                        .map_err(broken(root))?;
                }
            }
            done.push(file.path);
        }
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, '1')",
            params![IMPORTED_FLAG],
        )
        .map_err(broken(root))?;
        Ok(done)
    })();
    match outcome {
        Ok(done) => {
            conn.execute_batch("COMMIT").map_err(broken(root))?;
            for path in done {
                let _ = fs::rename(&path, path.with_extension("json.migrated"));
            }
            Ok(())
        }
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::scratch_root as root;
    use serde_json::json;

    fn write_json(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn nothing_is_created_when_there_is_nothing_to_read() {
        let r = root("store-none");
        let got = lookup(&r, "mode", None).unwrap();
        assert!(got.repo.is_none() && got.org.is_none() && got.global.is_none());
        assert!(!db_path(&r).exists());
    }

    #[test]
    fn a_value_round_trips_per_tier_and_scope() {
        let r = root("store-rt");
        put(&r, Tier::Global, None, "mode", &json!("auto")).unwrap();
        put(&r, Tier::Org, Some("acme/widgets"), "mode", &json!("ask")).unwrap();
        put(&r, Tier::Repo, Some("acme/widgets"), "mode", &json!("auto")).unwrap();
        let got = lookup(&r, "mode", Some("acme/widgets")).unwrap();
        assert_eq!(got.global, Some(json!("auto")));
        assert_eq!(got.org, Some(json!("ask")));
        assert_eq!(got.repo, Some(json!("auto")));
        let other = lookup(&r, "mode", Some("other/thing")).unwrap();
        assert!(other.org.is_none() && other.repo.is_none());
        let none = lookup(&r, "mode", None).unwrap();
        assert!(none.org.is_none() && none.repo.is_none());
    }

    #[test]
    fn rows_written_under_an_old_org_name_still_apply_to_the_current_org() {
        let r = root("store-legacy-org");
        put(
            &r,
            Tier::Org,
            Some("pragmatic-engineer/playbook"),
            "mode",
            &json!("ask"),
        )
        .unwrap();
        put(
            &r,
            Tier::Repo,
            Some("amplified-engineer/playbook"),
            "mode",
            &json!("auto"),
        )
        .unwrap();

        let got = lookup(&r, "mode", Some("amplifiedengineer/playbook")).unwrap();
        assert_eq!(got.org, Some(json!("ask")));
        assert_eq!(got.repo, Some(json!("auto")));

        // A row under the current name wins over the old ones.
        put(
            &r,
            Tier::Org,
            Some("amplifiedengineer/playbook"),
            "mode",
            &json!("auto"),
        )
        .unwrap();
        let got = lookup(&r, "mode", Some("amplifiedengineer/playbook")).unwrap();
        assert_eq!(got.org, Some(json!("auto")));

        // A repo of another org never reads the old org rows.
        let other = lookup(&r, "mode", Some("acme/playbook")).unwrap();
        assert!(other.org.is_none() && other.repo.is_none());
    }

    #[test]
    fn replace_value_without_a_data_folder_creates_nothing() {
        // Arrange
        let r = root("store-replace-none");

        // Act
        let changed = replace_value(&r, "update.plugin", &json!(false), &json!("off")).unwrap();

        // Assert
        assert_eq!(changed, 0);
        assert!(!db_path(&r).exists());
    }

    #[test]
    fn replace_value_rewrites_only_the_matching_values_in_every_tier() {
        // Arrange
        let r = root("store-replace-value");
        put(&r, Tier::Global, None, "update.plugin", &json!(false)).unwrap();
        put(
            &r,
            Tier::Repo,
            Some("acme/app"),
            "update.plugin",
            &json!(false),
        )
        .unwrap();
        put(
            &r,
            Tier::Org,
            Some("acme/app"),
            "update.plugin",
            &json!("ask"),
        )
        .unwrap();

        // Act
        let changed = replace_value(&r, "update.plugin", &json!(false), &json!("off")).unwrap();

        // Assert
        assert_eq!(changed, 2);
        let got = lookup(&r, "update.plugin", Some("acme/app")).unwrap();
        assert_eq!(got.global, Some(json!("off")));
        assert_eq!(got.repo, Some(json!("off")));
        assert_eq!(got.org, Some(json!("ask")));
    }

    #[test]
    fn a_second_put_replaces_the_value() {
        let r = root("store-replace");
        put(&r, Tier::Global, None, "pr.draft", &json!(true)).unwrap();
        put(&r, Tier::Global, None, "pr.draft", &json!(false)).unwrap();
        assert_eq!(
            lookup(&r, "pr.draft", None).unwrap().global,
            Some(json!(false))
        );
    }

    #[test]
    fn org_and_repo_writes_need_a_slug() {
        let r = root("store-slug");
        assert!(matches!(
            put(&r, Tier::Org, None, "mode", &json!("ask")),
            Err(ConfigError::MissingRepoContext)
        ));
        assert!(matches!(
            put(&r, Tier::Repo, Some("noslash"), "mode", &json!("ask")),
            Err(ConfigError::MissingRepoContext)
        ));
    }

    #[test]
    fn legacy_files_are_imported_once_and_renamed() {
        let r = root("store-import");
        write_json(
            &r.join("config.json"),
            r#"{"mode":"auto","autoReview":{"type":"deep"}}"#,
        );
        write_json(
            &r.join("orgs/acme/config.json"),
            r#"{"pr":{"draft":false}}"#,
        );
        write_json(
            &r.join("repos/acme/widgets/.config/config.json"),
            r#"{"commit":{"signOff":false},"stale.unknown":1}"#,
        );
        write_json(
            &r.join("repos/group/sub/repo/.config/config.json"),
            r#"{"mode":"ask"}"#,
        );

        let got = lookup(&r, "autoReview.type", None).unwrap();
        assert_eq!(got.global, Some(json!("deep")));
        assert_eq!(
            lookup(&r, "pr.draft", Some("acme/x")).unwrap().org,
            Some(json!(false))
        );
        assert_eq!(
            lookup(&r, "commit.signOff", Some("acme/widgets"))
                .unwrap()
                .repo,
            Some(json!(false))
        );
        assert_eq!(
            lookup(&r, "mode", Some("group/sub/repo")).unwrap().repo,
            Some(json!("ask"))
        );
        assert!(!r.join("config.json").exists());
        assert!(r.join("config.json.migrated").exists());
        assert!(r.join("orgs/acme/config.json.migrated").exists());
        assert!(r
            .join("repos/acme/widgets/.config/config.json.migrated")
            .exists());
    }

    #[test]
    fn effort_component_keys_are_imported_and_listed() {
        let r = root("store-effort-keys");
        write_json(
            &r.join("config.json"),
            r#"{"effort":{"agents":{"reviewer":"low"},"skills":{"x":"high"},"bogus":{"a":"low"}}}"#,
        );
        assert_eq!(
            lookup(&r, "effort.agents.reviewer", None).unwrap().global,
            Some(json!("low"))
        );
        assert_eq!(
            global_keys(&r).unwrap(),
            vec!["effort.agents.reviewer", "effort.skills.x"]
        );
    }

    #[test]
    fn keys_with_a_prefix_are_listed_from_every_tier_once() {
        // Arrange
        let r = root("store-prefix-keys");
        put(&r, Tier::Global, None, "watch.rule.a", &json!("{}")).unwrap();
        put(&r, Tier::Repo, Some("o/r"), "watch.rule.a", &json!("{}")).unwrap();
        put(&r, Tier::Repo, Some("o/r"), "watch.rule.b", &json!("{}")).unwrap();
        put(&r, Tier::Global, None, "mode", &json!("ask")).unwrap();

        // Act
        let keys = keys_with_prefix(&r, "watch.rule.").unwrap();

        // Assert
        assert_eq!(keys, vec!["watch.rule.a", "watch.rule.b"]);
    }

    #[test]
    fn a_legacy_file_appearing_after_the_import_is_left_alone_by_reads() {
        let r = root("store-late");
        put(&r, Tier::Global, None, "mode", &json!("ask")).unwrap();
        write_json(&r.join("config.json"), r#"{"mode":"auto"}"#);
        assert_eq!(lookup(&r, "mode", None).unwrap().global, Some(json!("ask")));
        assert!(r.join("config.json").exists());
    }

    #[test]
    fn a_late_file_with_no_conflict_is_adopted_and_renamed() {
        let r = root("store-late-adopt");
        put(&r, Tier::Global, None, "mode", &json!("ask")).unwrap();
        write_json(
            &r.join("config.json"),
            r#"{"mode":"ask","pr":{"draft":false}}"#,
        );
        let report = adopt_late_files(&r).unwrap();
        assert_eq!(report.imported.len(), 1);
        assert!(report.conflicts.is_empty());
        assert!(r.join("config.json.migrated").exists());
        assert!(!r.join("config.json").exists());
        assert_eq!(
            lookup(&r, "pr.draft", None).unwrap().global,
            Some(json!(false))
        );
    }

    #[test]
    fn a_late_file_that_disagrees_is_left_in_place_and_reported() {
        let r = root("store-late-conflict");
        put(&r, Tier::Global, None, "mode", &json!("ask")).unwrap();
        write_json(&r.join("config.json"), r#"{"mode":"auto"}"#);
        let report = adopt_late_files(&r).unwrap();
        assert!(report.imported.is_empty());
        assert_eq!(report.conflicts, vec![r.join("config.json")]);
        assert!(r.join("config.json").exists());
        assert_eq!(lookup(&r, "mode", None).unwrap().global, Some(json!("ask")));
    }

    #[test]
    fn a_malformed_late_file_is_reported_not_fatal() {
        let r = root("store-late-bad");
        put(&r, Tier::Global, None, "mode", &json!("ask")).unwrap();
        write_json(&r.join("config.json"), "{not json");
        let report = adopt_late_files(&r).unwrap();
        assert_eq!(report.conflicts.len(), 1);
    }

    #[test]
    fn with_no_database_nothing_is_adopted() {
        let r = root("store-late-nodb");
        write_json(&r.join("config.json"), r#"{"mode":"auto"}"#);
        assert_eq!(adopt_late_files(&r).unwrap(), LateReport::default());
        assert!(r.join("config.json").exists());
    }

    #[test]
    fn a_malformed_legacy_file_fails_with_its_path_and_changes_nothing() {
        let r = root("store-bad");
        write_json(&r.join("config.json"), r#"{"mode":"auto"}"#);
        write_json(&r.join("orgs/acme/config.json"), "[1,2]");
        let err = lookup(&r, "mode", None).err().unwrap();
        match err {
            ConfigError::Malformed(p) => assert!(p.ends_with("orgs/acme/config.json"), "{p:?}"),
            other => panic!("{other:?}"),
        }
        assert!(r.join("config.json").exists(), "nothing renamed on failure");
    }

    #[test]
    fn a_corrupt_database_reports_its_path() {
        let r = root("store-corrupt");
        fs::create_dir_all(r.join("data")).unwrap();
        fs::write(
            db_path(&r),
            b"this is not a database file at all, not even close",
        )
        .unwrap();
        assert!(
            matches!(lookup(&r, "mode", None), Err(ConfigError::Malformed(p)) if p == db_path(&r))
        );
    }

    /// A `playbook.db` as an older playbook left it, with one global row.
    fn old_db(r: &Path, key: &str, value: &str, updated_at: i64) {
        let conn = Connection::open(legacy_db_path(r)).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO config VALUES ('global', '', ?1, ?2, ?3)",
            params![key, value, updated_at],
        )
        .unwrap();
    }

    fn global(r: &Path, key: &str) -> Option<Value> {
        lookup(r, key, None).unwrap().global
    }

    #[test]
    fn an_old_playbook_db_moves_into_config_db_on_first_open() {
        let r = root("store-move");
        old_db(&r, "mode", "\"auto\"", 10);

        assert_eq!(global(&r, "mode"), Some(json!("auto")));

        assert!(db_path(&r).is_file());
        assert!(!legacy_db_path(&r).exists());
        assert!(r.join("playbook.db.migrated").exists());
    }

    #[test]
    fn no_old_file_and_no_new_file_resolve_to_defaults_without_creating_anything() {
        let r = root("store-none");
        assert_eq!(global(&r, "mode"), None);
        assert!(!db_path(&r).exists());
        assert!(!r.join("data").exists());
    }

    #[test]
    fn both_files_merge_with_the_later_write_winning_and_no_double_counting() {
        let r = root("store-both");
        put(&r, Tier::Global, None, "mode", &json!("new")).unwrap();
        // An older playbook writes the old path: one newer, one older, one new key.
        old_db(&r, "mode", "\"older\"", 1);
        let conn = Connection::open(legacy_db_path(&r)).unwrap();
        conn.execute(
            "INSERT INTO config VALUES ('global', '', 'fix.maxFiles', '7', 99999999999)",
            [],
        )
        .unwrap();
        drop(conn);

        assert_eq!(global(&r, "mode"), Some(json!("new")), "newer row stays");
        assert_eq!(global(&r, "fix.maxFiles"), Some(json!(7)));
        assert!(!legacy_db_path(&r).exists());

        // Merging again changes nothing.
        let n: i64 = open(&r)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM config", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn a_later_value_in_the_old_file_replaces_the_stored_one() {
        let r = root("store-later");
        put(&r, Tier::Global, None, "mode", &json!("new")).unwrap();
        old_db(&r, "mode", "\"changed-by-old-binary\"", 99999999999);
        assert_eq!(global(&r, "mode"), Some(json!("changed-by-old-binary")));
    }

    #[test]
    fn settings_still_work_next_to_a_corrupt_old_file() {
        let r = root("store-old-corrupt");
        fs::write(legacy_db_path(&r), vec![b'x'; 4096]).unwrap();
        put(&r, Tier::Global, None, "mode", &json!("auto")).unwrap();
        assert_eq!(global(&r, "mode"), Some(json!("auto")));
        assert!(r.join("playbook.db.migrated").exists(), "kept, not deleted");
    }

    #[test]
    fn a_deferred_move_reads_the_old_file_instead_of_starting_an_empty_one() {
        let r = root("store-deferred");
        old_db(&r, "mode", "\"auto\"", 10);
        fs::create_dir_all(r.join("data").join("config.db.migrate.lock")).unwrap();

        let mode = playbook_data::migrate::with_test_lock_retries(2, || global(&r, "mode"));

        assert_eq!(mode, Some(json!("auto")));

        assert!(!db_path(&r).exists(), "no empty config.db was created");
        assert!(legacy_db_path(&r).exists());
    }

    #[test]
    fn a_corrupt_old_file_is_parked_once() {
        let r = root("store-park");
        fs::write(legacy_db_path(&r), vec![b'x'; 4096]).unwrap();
        assert_eq!(global(&r, "mode"), None);
        assert!(!legacy_db_path(&r).exists());
        assert!(r.join("playbook.db.migrated").exists());
    }

    #[test]
    fn a_leftover_config_json_never_outranks_a_real_setting_in_the_merge() {
        let r = root("store-stale-json");
        put(&r, Tier::Global, None, "mode", &json!("real")).unwrap();
        // An old binary imported a stale config.json into a recreated playbook.db.
        let conn = Connection::open(legacy_db_path(&r)).unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        upsert_at(&conn, "global", "", "mode", &json!("stale-json"), 0).unwrap();
        drop(conn);
        assert_eq!(global(&r, "mode"), Some(json!("real")));
    }

    #[test]
    fn any_scoped_sees_org_and_repo_rows_but_not_global() {
        let r = root("store-scoped");
        put(&r, Tier::Global, None, "mode", &json!("ask")).unwrap();
        assert!(!any_scoped(&r));
        put(&r, Tier::Org, Some("a/b"), "mode", &json!("ask")).unwrap();
        assert!(any_scoped(&r));
    }

    #[test]
    fn export_leaves_out_a_key_a_release_retired() {
        let r = root("store-retired");
        put(&r, Tier::Global, None, "mode", &json!("auto")).unwrap();
        put(&r, Tier::Global, None, "hints.pluginRemoved", &json!(false)).unwrap();

        let doc = export(&r).unwrap();

        assert_eq!(doc["global"]["mode"], json!("auto"));
        assert!(doc["global"].get("hints.pluginRemoved").is_none());
        assert!(import(&r, &doc).is_ok());
    }

    #[test]
    fn export_then_import_round_trips_and_validates() {
        let a = root("store-export-a");
        put(&a, Tier::Global, None, "mode", &json!("auto")).unwrap();
        put(&a, Tier::Org, Some("acme/w"), "pr.draft", &json!(false)).unwrap();
        put(&a, Tier::Repo, Some("acme/w"), "fix.maxFiles", &json!(5)).unwrap();
        let doc = export(&a).unwrap();
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["global"]["mode"], "auto");
        assert_eq!(doc["orgs"]["acme"]["pr.draft"], false);
        assert_eq!(doc["repos"]["acme/w"]["fix.maxFiles"], 5);

        let b = root("store-export-b");
        assert_eq!(import(&b, &doc).unwrap(), 3);
        assert_eq!(export(&b).unwrap(), doc);

        let bad = json!({"global": {"mode": "chaos"}});
        assert!(import(&b, &bad).is_err());
        let unknown = json!({"global": {"nope": 1}});
        assert!(matches!(
            import(&b, &unknown),
            Err(ConfigError::UnknownKey(_))
        ));
    }

    #[test]
    fn import_refuses_a_per_user_key_outside_the_global_section() {
        let r = root("store-import-user-key");
        let org = json!({"orgs": {"acme": {"update.mode": "off"}}});
        let repo = json!({"repos": {"acme/w": {"update.plugin": "off"}}});
        let global = json!({"global": {"update.mode": "off"}});
        assert!(matches!(import(&r, &org), Err(ConfigError::GlobalOnly(_))));
        assert!(matches!(import(&r, &repo), Err(ConfigError::GlobalOnly(_))));
        assert_eq!(import(&r, &global).unwrap(), 1);
    }

    #[test]
    fn a_read_only_global_lookup_changes_nothing_on_disk() {
        let r = root("store-global-read-only");
        assert_eq!(global_read_only(&r, "update.mode"), None);
        assert!(!db_path(&r).exists());
        put(&r, Tier::Global, None, "update.mode", &json!("auto")).unwrap();
        put(&r, Tier::Org, Some("acme/w"), "pr.draft", &json!(false)).unwrap();
        assert_eq!(global_read_only(&r, "update.mode"), Some(json!("auto")));
        assert_eq!(global_read_only(&r, "pr.draft"), None);
    }

    /// Every file under the data folder with its size and bytes.
    fn data_snapshot(r: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(r.join("data"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .map(|p| {
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    fs::read(&p).unwrap_or_default(),
                )
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn a_lookup_in_a_ready_store_changes_no_file() {
        let r = root("store-ready-read");
        put(&r, Tier::Global, None, "mode", &json!("auto")).unwrap();
        put(&r, Tier::Org, Some("acme/w"), "mode", &json!("ask")).unwrap();
        let before = data_snapshot(&r);

        let got = lookup(&r, "mode", Some("acme/w")).unwrap();
        let scoped = any_scoped(&r);

        assert_eq!(got.global, Some(json!("auto")));
        assert_eq!(got.org, Some(json!("ask")));
        assert!(scoped);
        assert_eq!(data_snapshot(&r), before);
    }

    #[cfg(unix)]
    #[test]
    fn a_lookup_still_tightens_a_database_left_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let r = root("store-wide-mode");
        put(&r, Tier::Global, None, "mode", &json!("auto")).unwrap();
        fs::set_permissions(db_path(&r), fs::Permissions::from_mode(0o644)).unwrap();

        let got = lookup(&r, "mode", None).unwrap();

        assert_eq!(got.global, Some(json!("auto")));
        let mode = fs::metadata(db_path(&r)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    fn stored_schema_version(r: &Path) -> Option<String> {
        let conn = Connection::open(db_path(r)).unwrap();
        conn.query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .ok()
    }

    #[test]
    fn a_new_store_is_stamped_and_one_without_the_row_opens_and_is_stamped() {
        // Arrange
        let r = root("store-schema-stamp");
        put(&r, Tier::Global, None, "mode", &json!("auto")).unwrap();
        Connection::open(db_path(&r))
            .unwrap()
            .execute("DELETE FROM meta WHERE key = 'schema_version'", [])
            .unwrap();

        // Act
        let got = lookup(&r, "mode", None).unwrap();
        drop(open(&r).unwrap());

        // Assert
        assert_eq!(got.global, Some(json!("auto")));
        assert_eq!(stored_schema_version(&r), Some(SCHEMA_VERSION.to_string()));
    }

    #[test]
    fn a_newer_store_is_refused_naming_both_versions_and_stays_as_it_was() {
        // Arrange
        let r = root("store-schema-newer");
        put(&r, Tier::Global, None, "mode", &json!("auto")).unwrap();
        let newer = (SCHEMA_VERSION + 1).to_string();
        Connection::open(db_path(&r))
            .unwrap()
            .execute(
                "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                [&newer],
            )
            .unwrap();

        // Act
        let err = open(&r).unwrap_err().to_string();

        // Assert
        assert!(err.contains(&format!("version {newer}")), "{err}");
        assert!(
            err.contains(&format!("understands {SCHEMA_VERSION}")),
            "{err}"
        );
        assert!(err.contains("playbook upgrade"), "{err}");
        assert_eq!(stored_schema_version(&r), Some(newer));
    }

    #[test]
    fn concurrent_first_opens_import_exactly_once() {
        let r = root("store-race");
        write_json(&r.join("config.json"), r#"{"mode":"auto"}"#);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let r = r.clone();
                std::thread::spawn(move || lookup(&r, "mode", None).unwrap().global)
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), Some(json!("auto")));
        }
        assert!(r.join("config.json.migrated").exists());
    }
}
