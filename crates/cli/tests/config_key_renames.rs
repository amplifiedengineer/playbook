// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Every row of `KEY_RENAMES` through the real binary: `config set`, `get`,
//! `import` and `apply-proposal` accept the old name and act on the new one.
//! The tests iterate the table, so a rename added later is covered without a
//! new test. The cases that need a pseudo terminal or a seeded old row live
//! beside the code as unit tests. Each test uses a scratch `$HOME`.

use playbook::config::keys::{self, KeyRename, KEY_RENAMES};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);
const PROPOSAL_FILE: &str = "config-proposal.json";

fn scratch_dir(tag: &str) -> PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "playbook-key-renames-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("scratch dir should be creatable");
    dir
}

fn playbook(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(args)
        .current_dir(scratch_dir("cwd"))
        .env("HOME", home)
        .output()
        .expect("playbook binary should spawn")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A value of the new key's type that is not its default.
fn other_than_default(row: &KeyRename) -> Value {
    let default = keys::default_value(row.new).expect("the new name is known");
    match &default {
        Value::Bool(flag) => json!(!flag),
        Value::String(current) => keys::allowed_enum_values(row.new)
            .and_then(|allowed| allowed.iter().find(|word| *word != current))
            .map_or_else(|| json!(format!("{current}x")), |word| json!(word)),
        // At least the default main budget, so a session ceiling stays valid.
        other => json!((other.as_i64().unwrap_or(0) + 1).max(i64::from(keys::AUTO_BUDGET_USD))),
    }
}

fn arg_text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_string)
}

fn stored(home: &Path) -> Value {
    playbook::config::store::export(&home.join(".config/playbook")).unwrap()
}

#[test]
fn set_with_an_old_name_stores_the_new_name_and_prints_no_notice_when_piped() {
    for row in KEY_RENAMES {
        // Arrange
        let home = scratch_dir("set");
        let value = other_than_default(row);

        // Act
        let out = playbook(
            &home,
            &["config", "set", "--global", row.old, &arg_text(&value)],
        );

        // Assert
        assert!(out.status.success(), "{}: {}", row.old, text(&out.stderr));
        assert!(text(&out.stdout).contains(row.new), "{}", text(&out.stdout));
        assert_eq!(stored(&home)["global"][row.new], value, "{}", row.new);
        assert!(stored(&home)["global"].get(row.old).is_none());
        assert!(
            !text(&out.stderr).contains("renamed"),
            "{}",
            text(&out.stderr)
        );
    }
}

#[test]
fn get_with_an_old_name_prints_the_new_name_and_value() {
    for row in KEY_RENAMES {
        // Arrange
        let home = scratch_dir("get");
        let value = other_than_default(row);
        playbook(
            &home,
            &["config", "set", "--global", row.new, &arg_text(&value)],
        );

        // Act
        let out = playbook(&home, &["config", "get", row.old]);

        // Assert
        assert!(out.status.success(), "{}: {}", row.old, text(&out.stderr));
        let shown = text(&out.stdout);
        assert!(
            shown.starts_with(&format!("{}: {}", row.new, arg_text(&value))),
            "{shown}"
        );
        assert!(
            !text(&out.stderr).contains("renamed"),
            "{}",
            text(&out.stderr)
        );
    }
}

#[test]
fn import_with_an_old_name_stores_the_new_name() {
    for row in KEY_RENAMES {
        // Arrange
        let home = scratch_dir("import");
        let value = other_than_default(row);
        let file = home.join("in.json");
        fs::write(&file, json!({ "global": { row.old: value } }).to_string()).unwrap();

        // Act
        let out = playbook(&home, &["config", "import", file.to_str().unwrap()]);

        // Assert
        assert!(out.status.success(), "{}: {}", row.old, text(&out.stderr));
        assert_eq!(stored(&home)["global"][row.new], value, "{}", row.new);
    }
}

#[test]
fn apply_proposal_with_an_old_name_stores_the_new_name() {
    for row in KEY_RENAMES.iter().filter(|r| keys::is_shared(r.new)) {
        // Arrange
        let home = scratch_dir("proposal");
        let value = other_than_default(row);
        let path = home.join(".config/playbook/data").join(PROPOSAL_FILE);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let doc = json!({
            "created_at": 1_700_000_000,
            "proposed": { row.old: arg_text(&value) },
            "from": {},
        });
        fs::write(&path, doc.to_string()).unwrap();

        // Act
        let out = playbook(&home, &["config", "apply-proposal", "--yes"]);

        // Assert
        assert!(out.status.success(), "{}: {}", row.old, text(&out.stderr));
        assert_eq!(stored(&home)["global"][row.new], value, "{}", row.new);
    }
}
