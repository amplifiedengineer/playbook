// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

#[path = "support/harness.rs"]
mod harness;
#[path = "support/local.rs"]
mod local;
#[path = "support/sqlite_fake.rs"]
mod sqlite_fake;
#[allow(dead_code)]
mod support;

use harness::{Machine, Remote, NOW};
use pbsync::cli::{EXIT_FAILED, EXIT_OK};
use pbsync::config_sync::{pull_config, push_config, ConfigSyncError};
use pbsync::remote::ensure_schema;
use pbsync::transport::Value;
use serde_json::{json, Value as Json};
use sqlite_fake::SqliteFake;

const MACHINE_A: &str = "aaaaaaaa-1111-2222-3333-444444444444";
const MACHINE_B: &str = "bbbbbbbb-1111-2222-3333-444444444444";
const ROWS: &str =
    "SELECT machine, key, value, pushed_at FROM shared_config_snapshots ORDER BY machine, key";

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn keys_of(fake: &SqliteFake) -> Vec<Value> {
    fake.query("SELECT key FROM shared_config_snapshots ORDER BY key")
        .into_iter()
        .map(|row| row[0].clone())
        .collect()
}

fn write(machine: &Machine, include: &[&str], values: &[(&str, &str)]) {
    let map: serde_json::Map<String, Json> = values
        .iter()
        .map(|(k, v)| (k.to_string(), json!(v)))
        .collect();
    std::fs::write(
        machine.dir.join("sync-keys.json"),
        json!({"include": include, "exclude": ["sync."]}).to_string(),
    )
    .unwrap();
    std::fs::write(
        machine.dir.join("config-values.json"),
        json!({"values": map}).to_string(),
    )
    .unwrap();
}

fn fresh(test: &str) -> (Machine, SqliteFake) {
    let fake = SqliteFake::new();
    ensure_schema(&fake, &[], NOW).unwrap();
    (Machine::new(test, MACHINE_A), fake)
}

fn proposal(machine: &Machine) -> Json {
    serde_json::from_str(
        &std::fs::read_to_string(machine.dir.join("config-proposal.json")).unwrap(),
    )
    .unwrap()
}

#[test]
fn only_included_keys_leave_and_the_include_list_is_what_is_pushed() {
    // Arrange
    let (machine, fake) = fresh("only-included");
    write(
        &machine,
        &["ui.theme", "sync.url"],
        &[
            ("ui.theme", "dark"),
            ("sync.url", "https://secret.example"),
            ("repo.path", "/home/me/secret"),
            ("other.key", "x"),
        ],
    );
    // Act
    let changed = push_config(&fake, &machine.dir, MACHINE_A, NOW).unwrap();
    // Assert
    assert_eq!(changed, 1);
    assert_eq!(keys_of(&fake), [text("ui.theme")]);
}

#[test]
fn a_removed_local_key_disappears_from_this_machines_remote_rows() {
    // Arrange
    let (machine, fake) = fresh("removed-key");
    write(&machine, &["a", "b"], &[("a", "1"), ("b", "2")]);
    push_config(&fake, &machine.dir, MACHINE_A, NOW).unwrap();
    fake.seed("INSERT INTO shared_config_snapshots VALUES ('other', 'b', '9', 1, 1)");
    write(&machine, &["a", "b"], &[("a", "1")]);
    // Act
    let changed = push_config(&fake, &machine.dir, MACHINE_A, NOW + 5).unwrap();
    // Assert
    assert_eq!(changed, 1);
    assert_eq!(
        fake.query(ROWS),
        [
            [
                text("aaaaaaaa-1111-2222-3333-444444444444"),
                text("a"),
                text("1"),
                Value::Integer(NOW)
            ],
            [text("other"), text("b"), text("9"), Value::Integer(1)],
        ]
    );
}

#[test]
fn a_second_push_writes_nothing_and_keeps_pushed_at() {
    // Arrange
    let (machine, fake) = fresh("second-push");
    write(&machine, &["a"], &[("a", "1")]);
    push_config(&fake, &machine.dir, MACHINE_A, NOW).unwrap();
    let before = fake.log().len();
    // Act
    let changed = push_config(&fake, &machine.dir, MACHINE_A, NOW + 60).unwrap();
    // Assert
    assert_eq!(changed, 0);
    assert_eq!(fake.log().len(), before + 1, "only the read ran");
    assert_eq!(fake.query(ROWS)[0][3], Value::Integer(NOW));
}

#[test]
fn a_missing_file_pushes_nothing_with_no_requests() {
    // Arrange
    let (machine, fake) = fresh("missing-file");
    let before = fake.log().len();
    // Act
    let changed = push_config(&fake, &machine.dir, MACHINE_A, NOW).unwrap();
    // Assert
    assert_eq!(changed, 0);
    assert_eq!(fake.log().len(), before);
}

#[test]
fn a_corrupt_include_file_is_a_named_error_with_zero_remote_writes() {
    // Arrange
    let (machine, fake) = fresh("corrupt-include");
    write(&machine, &["a"], &[("a", "1")]);
    std::fs::write(machine.dir.join("sync-keys.json"), "{not json").unwrap();
    let before = fake.log().len();
    // Act
    let result = push_config(&fake, &machine.dir, MACHINE_A, NOW);
    // Assert
    let error = result.unwrap_err();
    assert!(matches!(error, ConfigSyncError::Corrupt { .. }));
    assert!(error.to_string().contains("sync-keys.json"));
    assert_eq!(fake.log().len(), before);
}

#[test]
fn machine_b_gets_a_proposal_and_its_config_stays_byte_identical() {
    // Arrange
    let (a, fake) = fresh("propose-a");
    let b = Machine::new("propose-b", MACHINE_B);
    write(
        &a,
        &["x", "y", "z"],
        &[("x", "1"), ("y", "2"), ("z", "same")],
    );
    write(&b, &["x", "y", "z"], &[("y", "old"), ("z", "same")]);
    push_config(&fake, &a.dir, MACHINE_A, NOW).unwrap();
    let values_before = std::fs::read(b.dir.join("config-values.json")).unwrap();
    // Act
    let stats = pull_config(&fake, &b.dir, MACHINE_B, NOW + 1).unwrap();
    // Assert
    assert_eq!((stats.proposed, stats.rejected), (2, 0));
    assert_eq!(
        proposal(&b),
        json!({"created_at": NOW + 1, "proposed": {"x": "1", "y": "2"},
               "from": {"x": "aaaaaaaa", "y": "aaaaaaaa"}})
    );
    assert_eq!(
        std::fs::read(b.dir.join("config-values.json")).unwrap(),
        values_before
    );
}

#[test]
fn the_newest_value_wins_across_two_machines() {
    // Arrange
    let (c, fake) = fresh("newest");
    write(&c, &["k"], &[]);
    fake.seed(
        "INSERT INTO shared_config_snapshots VALUES ('m-old', 'k', 'older', 10, 1),
         ('m-new', 'k', 'newer', 20, 1)",
    );
    // Act
    pull_config(&fake, &c.dir, MACHINE_A, NOW).unwrap();
    // Assert
    assert_eq!(proposal(&c)["proposed"], json!({"k": "newer"}));
    assert_eq!(proposal(&c)["from"], json!({"k": "m-new"}));
}

#[test]
fn rows_outside_the_include_list_or_under_sync_are_rejected_and_counted() {
    // Arrange
    let (c, fake) = fresh("rejected");
    write(&c, &["ok", "sync.url"], &[]);
    fake.seed(
        "INSERT INTO shared_config_snapshots VALUES ('m', 'ok', 'v', 1, 1),
         ('m', 'unlisted', 'v', 1, 1), ('m', 'sync.url', 'https://x', 1, 1)",
    );
    // Act
    let stats = pull_config(&fake, &c.dir, MACHINE_A, NOW).unwrap();
    // Assert
    assert_eq!((stats.proposed, stats.rejected), (1, 2));
    assert_eq!(proposal(&c)["proposed"], json!({"ok": "v"}));
}

#[test]
fn no_proposal_when_equal_and_a_stale_one_is_removed() {
    // Arrange
    let (c, fake) = fresh("equal");
    write(&c, &["k"], &[("k", "v")]);
    std::fs::write(c.dir.join("config-proposal.json"), "{}").unwrap();
    fake.seed("INSERT INTO shared_config_snapshots VALUES ('m', 'k', 'v', 1, 1)");
    // Act
    let stats = pull_config(&fake, &c.dir, MACHINE_A, NOW).unwrap();
    // Assert
    assert_eq!(stats.proposed, 0);
    assert!(!c.dir.join("config-proposal.json").exists());
}

#[cfg(unix)]
#[test]
fn the_proposal_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    // Arrange
    let (c, fake) = fresh("mode");
    write(&c, &["k"], &[]);
    fake.seed("INSERT INTO shared_config_snapshots VALUES ('m', 'k', 'v', 1, 1)");
    // Act
    pull_config(&fake, &c.dir, MACHINE_A, NOW).unwrap();
    // Assert
    let mode = std::fs::metadata(c.dir.join("config-proposal.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn config_push_and_pull_over_http_and_sync_runs_both() {
    // Arrange
    let remote = Remote::start();
    let a = Machine::new("http-a", MACHINE_A);
    let b = Machine::new("http-b", MACHINE_B);
    a.enable(&remote.url, &[]);
    b.enable(&remote.url, &[]);
    write(&a, &["k"], &[("k", "v")]);
    write(&b, &["k"], &[]);
    // Act
    let pushed = a.pb(&["config", "push"]);
    let synced = b.pb(&["sync"]);
    // Assert
    assert_eq!(pushed.code, EXIT_OK, "{}", pushed.stderr);
    assert_eq!(synced.code, EXIT_OK, "{}", synced.stderr);
    assert_eq!(
        remote.count("SELECT COUNT(*) FROM shared_config_snapshots"),
        1
    );
    assert_eq!(proposal(&b)["proposed"], json!({"k": "v"}));
}

#[test]
fn a_config_failure_in_sync_warns_and_keeps_the_exit_code() {
    // Arrange
    let remote = Remote::start();
    let a = Machine::new("warn-a", MACHINE_A);
    a.enable(&remote.url, &[]);
    write(&a, &["k"], &[("k", "v")]);
    std::fs::write(a.dir.join("sync-keys.json"), "{bad").unwrap();
    // Act
    let outcome = a.pb(&["sync"]);
    // Assert
    assert_eq!(outcome.code, EXIT_OK);
    assert!(
        outcome.stderr.contains("warning: config sync failed"),
        "{}",
        outcome.stderr
    );
}

#[test]
fn config_commands_make_zero_requests_while_sync_is_off() {
    // Arrange
    let remote = Remote::start();
    let a = Machine::new("off", MACHINE_A);
    a.enable(&remote.url, &[]);
    a.pb(&["disable"]);
    // Act
    let outcomes = [a.pb(&["config", "push"]), a.pb(&["config", "pull"])];
    // Assert
    for outcome in outcomes {
        assert_eq!(outcome.code, EXIT_FAILED);
    }
    assert_eq!(remote.requests(), 0);
}

#[test]
fn each_config_command_has_help() {
    // Arrange
    let a = Machine::new("help", MACHINE_A);
    // Act
    let outcomes = [
        a.pb(&["config", "push", "--help"]),
        a.pb(&["config", "pull", "--help"]),
    ];
    // Assert
    assert!(outcomes[0].stdout.contains("playbook sync config push"));
    assert!(outcomes[1].stdout.contains("playbook sync config pull"));
}
