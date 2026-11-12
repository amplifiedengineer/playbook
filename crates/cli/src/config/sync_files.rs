// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The two files that tell `pbsync` which config keys may leave the machine
//! and what their global values are. `pbsync` cannot depend on this crate, so
//! it reads these files instead of the config database.

use super::{keys, store, ConfigError};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The key list: `{"include": [...], "exclude": ["sync."]}`.
pub const KEY_LIST_FILE: &str = "sync-keys.json";
/// The current global values of the shared keys: `{"values": {key: text}}`.
pub const VALUES_FILE: &str = "config-values.json";

/// Why the sync files could not be refreshed.
#[derive(Debug)]
pub enum SyncFilesError {
    Config(ConfigError),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for SyncFilesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(err) => write!(f, "cannot read the config store: {err}"),
            Self::Io { path, source } => write!(f, "cannot write {}: {source}", path.display()),
        }
    }
}

/// Refresh both sync files in the data folder under `home`. A failure leaves
/// the previous files as they were.
pub fn write_sync_files(home: &Path) -> Result<(), SyncFilesError> {
    let dir = crate::usage::data::data_dir(home);
    let root = crate::common::paths::playbook_root_from(home);
    let values = shared_global_values(&root).map_err(SyncFilesError::Config)?;
    std::fs::create_dir_all(&dir).map_err(|source| SyncFilesError::Io {
        path: dir.clone(),
        source,
    })?;
    write_private(&dir.join(KEY_LIST_FILE), &key_list_json())?;
    write_private(&dir.join(VALUES_FILE), &json!({ "values": values }))
}

fn key_list_json() -> Value {
    let mut include: Vec<&str> = keys::SHARED_KEYS.to_vec();
    include.sort_unstable();
    json!({ "include": include, "exclude": [keys::SYNC_EXCLUDED_PREFIX] })
}

/// The stored global-tier value of each shared key that is set, as text.
/// Defaults and org or repo values never appear.
fn shared_global_values(root: &Path) -> Result<BTreeMap<String, String>, ConfigError> {
    let doc = store::export(root)?;
    let global = doc["global"].as_object().cloned().unwrap_or_default();
    Ok(global
        .into_iter()
        .filter(|(key, _)| keys::is_shared(key))
        .map(|(key, value)| (key, value_text(&value)))
        .collect())
}

/// A config value as text: a string without its quotes, anything else as JSON.
pub fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn write_private(path: &Path, doc: &Value) -> Result<(), SyncFilesError> {
    let body = format!("{doc:#}\n");
    crate::common::atomic::write_atomic_private(path, body).map_err(|source| SyncFilesError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::write::{set, Tier};
    use std::fs;

    fn scratch_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "playbook-sync-files-{}-{tag}",
            crate::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch home should be creatable");
        dir
    }

    fn read(home: &Path, name: &str) -> Value {
        let path = crate::usage::data::data_dir(home).join(name);
        serde_json::from_str(&fs::read_to_string(path).expect("file should exist"))
            .expect("file should be JSON")
    }

    #[test]
    fn key_list_equals_shared_keys_and_excludes_the_sync_prefix() {
        // Arrange
        let home = scratch_home("keylist");

        // Act
        write_sync_files(&home).expect("write should succeed");

        // Assert
        let doc = read(&home, KEY_LIST_FILE);
        let include: Vec<&str> = doc["include"]
            .as_array()
            .expect("include is an array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let mut expected = keys::SHARED_KEYS.to_vec();
        expected.sort_unstable();
        assert_eq!(include, expected);
        assert_eq!(doc["exclude"], json!([keys::SYNC_EXCLUDED_PREFIX]));
    }

    #[test]
    fn values_hold_only_set_global_shared_keys() {
        // Arrange
        let home = scratch_home("values");
        set(Tier::Global, "usage.theme", &json!("dark"), &home, None).unwrap();
        set(Tier::Global, "auto.budgetUsd", &json!(150), &home, None).unwrap();
        set(
            Tier::Global,
            "docs.anydocPath",
            &json!("/opt/anydoc"),
            &home,
            None,
        )
        .unwrap();
        set(Tier::Repo, "pr.draft", &json!(false), &home, Some("o/r")).unwrap();

        // Act
        write_sync_files(&home).expect("write should succeed");

        // Assert
        let doc = read(&home, VALUES_FILE);
        assert_eq!(
            doc,
            json!({ "values": { "usage.theme": "dark", "auto.budgetUsd": "150" } })
        );
    }

    #[test]
    fn a_removed_value_leaves_the_values_file_on_refresh() {
        // Arrange
        let home = scratch_home("removed");
        set(Tier::Global, "usage.theme", &json!("dark"), &home, None).unwrap();
        write_sync_files(&home).unwrap();
        let root = crate::common::paths::playbook_root_from(&home);
        store::open(&root)
            .unwrap()
            .execute("DELETE FROM config WHERE key = 'usage.theme'", [])
            .unwrap();

        // Act
        write_sync_files(&home).expect("write should succeed");

        // Assert
        assert_eq!(read(&home, VALUES_FILE), json!({ "values": {} }));
    }

    #[test]
    fn no_sync_or_path_key_appears_in_either_file() {
        // Arrange
        let home = scratch_home("leak");
        set(
            Tier::Global,
            "launcher.shortName",
            &json!(true),
            &home,
            None,
        )
        .unwrap();
        set(Tier::Global, "usage.theme", &json!("dark"), &home, None).unwrap();

        // Act
        write_sync_files(&home).expect("write should succeed");

        // Assert
        let listed = read(&home, KEY_LIST_FILE)["include"].clone();
        let valued = read(&home, VALUES_FILE)["values"].clone();
        let names: Vec<String> = listed
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|k| k.as_str().map(str::to_string))
            .chain(valued.as_object().unwrap().keys().cloned())
            .collect();
        assert!(!names.is_empty());
        for name in names {
            assert!(!name.starts_with(keys::SYNC_EXCLUDED_PREFIX), "{name}");
            assert!(
                !name.contains("Path") && name != "launcher.shortName",
                "{name}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn both_files_are_owner_only() {
        // Arrange
        use std::os::unix::fs::PermissionsExt;
        let home = scratch_home("mode");

        // Act
        write_sync_files(&home).expect("write should succeed");

        // Assert
        let dir = crate::usage::data::data_dir(&home);
        for name in [KEY_LIST_FILE, VALUES_FILE] {
            let mode = fs::metadata(dir.join(name)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
    }

    #[test]
    fn a_data_folder_that_is_a_file_is_a_clear_error() {
        // Arrange
        let home = scratch_home("blocked");
        let dir = crate::usage::data::data_dir(&home);
        fs::create_dir_all(dir.parent().unwrap()).unwrap();
        fs::write(&dir, "not a folder").unwrap();

        // Act
        let err = write_sync_files(&home).expect_err("write should fail");

        // Assert
        assert!(err.to_string().starts_with("cannot "), "{err}");
    }

    #[test]
    fn the_sync_files_hold_the_new_name_of_a_renamed_shared_key() {
        for row in keys::renames().iter().filter(|r| keys::is_shared(r.new)) {
            // Arrange
            let home = scratch_home("renamed-shared");
            let default = keys::default_value(row.new).unwrap().as_bool().unwrap();
            set(Tier::Global, row.old, &json!(!default), &home, None).unwrap();

            // Act
            write_sync_files(&home).expect("write should succeed");

            // Assert
            let list = read(&home, KEY_LIST_FILE)["include"].to_string();
            let values = read(&home, VALUES_FILE)["values"].clone();
            assert!(list.contains(row.new), "{list}");
            assert!(!list.contains(row.old), "{list}");
            assert_eq!(values[row.new], json!((!default).to_string()));
            assert!(values.get(row.old).is_none(), "{values}");
        }
    }
}
