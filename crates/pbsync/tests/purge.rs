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

use harness::{Machine, Remote, NOW, SENTINEL};
use local::ACCOUNT;
use pbsync::cli::{EXIT_FAILED, EXIT_OK, EXIT_USAGE};
use pbsync::privacy::{salted_hash, Domain};
use pbsync::transport::Value;

const OTHER_ACCOUNT: &str = "other@example.com";
const SHARED_ROWS: &str = "SELECT COUNT(*) FROM shared_usage_events";
const SALT_ROWS: &str = "SELECT COUNT(*) FROM shared_meta WHERE key = 'salt'";

/// Two machines that both synced once against one remote: A has two rows and
/// B has one, and each pulled the other's.
fn two_synced_machines(test: &str) -> (Remote, Machine, Machine) {
    let remote = Remote::start();
    let a = Machine::new(&format!("{test}-a"), "machine-a");
    let b = Machine::new(&format!("{test}-b"), "machine-b");
    for (machine, rows) in [(&a, 2), (&b, 1)] {
        for n in 0..rows {
            let event = format!("{}-e{n}", machine.id);
            local::insert_for(
                &machine.db(),
                &event,
                NOW - 10_000,
                10,
                ACCOUNT,
                &machine.id,
            );
        }
        machine.enable(&remote.url, &[ACCOUNT, OTHER_ACCOUNT]);
    }
    local::insert_for(
        &a.db(),
        "a-other",
        NOW - 10_000,
        10,
        OTHER_ACCOUNT,
        "machine-a",
    );
    for machine in [&a, &b, &a] {
        assert_eq!(machine.pb(&["sync"]).code, EXIT_OK);
    }
    (remote, a, b)
}

fn count(machine: &Machine, sql: &str) -> i64 {
    machine.db().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn rows_of(remote: &Remote, machine: &str) -> i64 {
    remote.count(&format!("{SHARED_ROWS} WHERE machine = '{machine}'"))
}

#[test]
fn purge_removes_only_this_machines_rows_keeps_the_salt_and_clears_local_copies() {
    // Arrange
    let (remote, a, _b) = two_synced_machines("purge-one");
    assert_eq!(count(&a, "SELECT COUNT(*) FROM usage_events_remote"), 1);
    // Act
    let outcome = a.pb(&["purge"]);
    // Assert
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.stderr);
    assert_eq!(rows_of(&remote, "machine-a"), 0);
    assert_eq!(rows_of(&remote, "machine-b"), 1);
    assert_eq!(remote.count(SALT_ROWS), 1);
    for table in [
        "usage_events_remote",
        "account_aliases",
        "sync_state",
        "sync_watermarks",
        "sync_cursors",
    ] {
        assert_eq!(
            count(&a, &format!("SELECT COUNT(*) FROM {table}")),
            0,
            "{table}"
        );
    }
    assert_eq!(count(&a, "SELECT COUNT(*) FROM usage_events"), 3);
    assert!(!a.settings().enabled);
}

#[test]
fn purge_all_asks_first_then_removes_every_row_and_the_salt() {
    // Arrange
    let (remote, a, b) = two_synced_machines("purge-all");
    // Act
    let declined = a.pb_at(&["purge", "--all"], NOW, "no\n", Some(SENTINEL));
    let rows_after_no = remote.count(SHARED_ROWS);
    let confirmed = a.pb_at(&["purge", "--all"], NOW, "yes\n", Some(SENTINEL));
    let (rows_after_yes, salts_after_yes) = (remote.count(SHARED_ROWS), remote.count(SALT_ROWS));
    let stale = b.pb(&["push"]);
    // Assert
    assert_eq!(declined.code, EXIT_FAILED);
    assert_eq!(rows_after_no, 4);
    assert_eq!(confirmed.code, EXIT_OK, "{}", confirmed.stderr);
    assert_eq!((rows_after_yes, salts_after_yes), (0, 0));
    assert_eq!(stale.code, EXIT_FAILED);
    assert!(stale.stderr.contains("playbook sync purge"));
}

#[test]
fn purge_all_with_yes_skips_the_question() {
    // Arrange
    let (remote, a, _b) = two_synced_machines("purge-all-yes");
    // Act
    let outcome = a.pb(&["purge", "--all", "--yes"]);
    // Assert
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.stderr);
    assert_eq!(remote.count(SHARED_ROWS), 0);
}

#[test]
fn a_failing_remote_keeps_local_state_turns_sync_off_and_exits_non_zero() {
    // Arrange
    let (remote, a, _b) = two_synced_machines("purge-fail");
    remote.refuse_everything();
    // Act
    let outcome = a.pb(&["purge"]);
    // Assert
    assert_eq!(outcome.code, EXIT_FAILED);
    assert!(outcome.stderr.contains("local sync state was kept"));
    assert_eq!(
        count(&a, "SELECT COUNT(*) FROM sync_state WHERE key = 'salt'"),
        1
    );
    assert_eq!(count(&a, "SELECT COUNT(*) FROM usage_events_remote"), 1);
    assert!(!a.settings().enabled);
    assert_eq!(rows_of(&remote, "machine-a"), 3);
}

#[test]
fn a_second_purge_is_a_no_op_without_requests() {
    // Arrange
    let (remote, a, _b) = two_synced_machines("purge-twice");
    assert_eq!(a.pb(&["purge"]).code, EXIT_OK);
    let requests = remote.requests();
    // Act
    let second = a.pb(&["purge"]);
    // Assert
    assert_eq!(second.code, EXIT_OK);
    assert!(second.stdout.contains("Nothing to purge"));
    assert_eq!(remote.requests(), requests);
}

#[test]
fn re_enabling_after_purge_reuses_the_salt_and_re_sends_every_row() {
    // Arrange
    let (remote, a, _b) = two_synced_machines("purge-reenable");
    let before = remote.query("SELECT account, repo FROM shared_usage_events WHERE machine = 'machine-a' ORDER BY event_id");
    assert_eq!(a.pb(&["purge"]).code, EXIT_OK);
    a.enable(&remote.url, &[ACCOUNT, OTHER_ACCOUNT]);
    // Act
    let outcome = a.pb(&["push"]);
    // Assert
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.stderr);
    assert!(
        outcome.stdout.starts_with("pushed 3 rows"),
        "{}",
        outcome.stdout
    );
    let after = remote.query("SELECT account, repo FROM shared_usage_events WHERE machine = 'machine-a' ORDER BY event_id");
    assert_eq!(before, after);
    assert_eq!(remote.count(SALT_ROWS), 1);
}

#[test]
fn purge_account_removes_only_that_accounts_remote_rows_of_this_machine() {
    // Arrange
    let (remote, a, _b) = two_synced_machines("purge-account");
    let salt = match &remote.query("SELECT value FROM shared_meta WHERE key = 'salt'")[0][0] {
        Value::Text(salt) => salt.clone(),
        other => panic!("{other:?}"),
    };
    // Act
    let outcome = a.pb(&["purge", "--account", OTHER_ACCOUNT]);
    // Assert
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.stderr);
    let hash = salted_hash(&salt, Domain::Account, OTHER_ACCOUNT);
    assert_eq!(
        remote.count(&format!("{SHARED_ROWS} WHERE account = '{hash}'")),
        0
    );
    assert_eq!(rows_of(&remote, "machine-a"), 2);
    assert_eq!(rows_of(&remote, "machine-b"), 1);
}

#[test]
fn purge_all_cannot_be_combined_with_account() {
    // Arrange
    let machine = Machine::new("purge-conflict", "m1");
    // Act
    let outcome = machine.pb(&["purge", "--all", "--account", ACCOUNT]);
    // Assert
    assert_eq!(outcome.code, EXIT_USAGE);
}
