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
use pbsync::push::backoff_seconds;
use playbook_common::sync_commands::FORWARDED;

const OTHER_ACCOUNT: &str = "other@example.com";
const NETWORK_COMMANDS: [&str; 4] = ["push", "pull", "run", "sync"];
const SHARED_ROWS: &str = "SELECT COUNT(*) FROM shared_usage_events";

fn machine_with_rows(test: &str, id: &str, rows: usize) -> Machine {
    let machine = Machine::new(test, id);
    let db = machine.db();
    for n in 0..rows {
        local::insert_for(&db, &format!("{id}-e{n}"), NOW - 10_000, 10, ACCOUNT, id);
    }
    local::insert_for(
        &db,
        &format!("{id}-other"),
        NOW - 10_000,
        10,
        OTHER_ACCOUNT,
        id,
    );
    machine
}

fn count(machine: &Machine, sql: &str) -> i64 {
    machine.db().query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn network_commands_make_no_request_while_off_or_never_enabled() {
    for command in NETWORK_COMMANDS {
        // Arrange
        let remote = Remote::start();
        let never = machine_with_rows("off-never", "m1", 1);
        let disabled = machine_with_rows("off-disabled", "m2", 1);
        disabled.enable(&remote.url, &[ACCOUNT]);
        assert_eq!(disabled.pb(&["disable"]).code, EXIT_OK);
        // Act
        let outcomes = [never.pb(&[command]), disabled.pb(&[command])];
        // Assert
        for outcome in outcomes {
            assert_eq!(outcome.code, EXIT_FAILED, "{command}");
            assert!(outcome.stderr.contains("sync is off"));
        }
        assert_eq!(remote.requests(), 0);
    }
}

#[test]
fn an_unset_token_is_a_named_error_without_a_request() {
    // Arrange
    let remote = Remote::start();
    let machine = machine_with_rows("no-token", "m1", 1);
    machine.enable(&remote.url, &[ACCOUNT]);
    // Act
    let outcome = machine.pb_at(&["push"], NOW, "", None);
    // Assert
    assert_eq!(outcome.code, EXIT_FAILED);
    assert!(outcome.stderr.contains("PBSYNC_TOKEN"));
    assert_eq!(remote.requests(), 0);
}

#[test]
fn push_sends_only_allowlisted_accounts_and_prints_a_one_line_summary() {
    // Arrange
    let remote = Remote::start();
    let machine = machine_with_rows("push-allow", "m1", 2);
    machine.enable(&remote.url, &[ACCOUNT]);
    // Act
    let outcome = machine.pb(&["push"]);
    // Assert
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.stderr);
    assert_eq!(outcome.stdout, "pushed 2 rows, pulled 0 rows, 0 overlaps\n");
    assert_eq!(remote.count(SHARED_ROWS), 2);
    assert_eq!(
        remote.count("SELECT COUNT(*) FROM shared_meta WHERE key = 'salt'"),
        1
    );
    assert!(!outcome.stdout.contains(&remote.url));
}

#[test]
fn reset_watermark_re_sends_every_row_and_is_cleared_only_after_success() {
    // Arrange
    let remote = Remote::start();
    let machine = machine_with_rows("reset", "m1", 2);
    machine.enable(&remote.url, &[ACCOUNT]);
    assert_eq!(machine.pb(&["push"]).code, EXIT_OK);
    let mut settings = machine.settings();
    settings.reset_watermark = true;
    settings.save(&machine.dir).unwrap();
    remote.refuse_requests_containing("INSERT INTO shared_usage_events");
    // Act
    let failed = machine.pb(&["push"]);
    // Assert
    assert_eq!(failed.code, EXIT_FAILED);
    assert!(machine.settings().reset_watermark);
    // Arrange
    remote.refuse_requests_containing("never matches this text");
    // Act
    let retried = machine.pb_at(&["push", "--force"], NOW + 1, "", Some(SENTINEL));
    // Assert
    assert_eq!(retried.code, EXIT_OK, "{}", retried.stderr);
    assert!(retried.stdout.starts_with("pushed 2 rows"));
    assert!(!machine.settings().reset_watermark);
}

#[test]
fn two_machines_converge_through_one_remote() {
    // Arrange
    let remote = Remote::start();
    let a = machine_with_rows("two-a", "machine-a", 2);
    let b = machine_with_rows("two-b", "machine-b", 1);
    a.enable(&remote.url, &[ACCOUNT]);
    b.enable(&remote.url, &[ACCOUNT]);
    // Act
    let a_first = a.pb(&["sync"]);
    let b_sync = b.pb(&["sync"]);
    let a_second = a.pb(&["pull"]);
    // Assert
    assert_eq!(a_first.stdout, "pushed 2 rows, pulled 0 rows, 0 overlaps\n");
    assert_eq!(b_sync.stdout, "pushed 1 rows, pulled 2 rows, 0 overlaps\n");
    assert_eq!(
        a_second.stdout,
        "pushed 0 rows, pulled 1 rows, 0 overlaps\n"
    );
    let copies = "SELECT COUNT(*) FROM usage_events_remote WHERE machine = ";
    assert_eq!(count(&b, &format!("{copies}'machine-a'")), 2);
    assert_eq!(count(&a, &format!("{copies}'machine-b'")), 1);
    assert_eq!(remote.count(SHARED_ROWS), 3);
}

#[test]
fn a_salt_that_differs_from_the_remote_stops_with_the_purge_message() {
    // Arrange
    let remote = Remote::start();
    let a = machine_with_rows("salt-a", "machine-a", 1);
    let b = machine_with_rows("salt-b", "machine-b", 1);
    a.enable(&remote.url, &[ACCOUNT]);
    b.enable(&remote.url, &[ACCOUNT]);
    assert_eq!(a.pb(&["push"]).code, EXIT_OK);
    b.db()
        .execute("INSERT INTO sync_state VALUES ('salt', 'an-old-salt')", [])
        .unwrap();
    // Act
    let outcome = b.pb(&["sync"]);
    // Assert
    assert_eq!(outcome.code, EXIT_FAILED);
    assert!(outcome.stderr.contains("playbook sync purge"));
    assert_eq!(remote.count(SHARED_ROWS), 1);
}

#[test]
fn failures_back_off_until_the_wait_passes_unless_forced() {
    // Arrange
    let remote = Remote::start();
    let machine = machine_with_rows("backoff", "m1", 1);
    machine.enable(&remote.url, &[ACCOUNT]);
    let wait = i64::try_from(backoff_seconds(2)).unwrap();
    machine
        .db()
        .execute_batch(&format!(
            "INSERT INTO sync_state VALUES ('failures', '2'), ('last_attempt', '{NOW}')"
        ))
        .unwrap();
    // Act
    let early = machine.pb_at(&["push"], NOW + wait - 1, "", Some(SENTINEL));
    let requests_when_skipped = remote.requests();
    let forced = machine.pb_at(&["push", "--force"], NOW + 1, "", Some(SENTINEL));
    let after = machine.pb_at(&["pull"], NOW + wait + 5, "", Some(SENTINEL));
    // Assert
    assert_eq!(early.code, EXIT_OK);
    assert!(early.stdout.contains("backing off"));
    assert_eq!(requests_when_skipped, 0);
    assert_eq!(forced.code, EXIT_OK, "{}", forced.stderr);
    assert_eq!(after.code, EXIT_OK);
    assert!(after.stdout.starts_with("pushed"));
}

#[test]
fn a_usage_db_newer_or_older_than_sync_understands_makes_no_remote_request() {
    for command in NETWORK_COMMANDS {
        for (version, expected) in [(6, "update pbsync"), (4, "run any playbook command")] {
            // Arrange
            let remote = Remote::start();
            let machine = machine_with_rows("version", "m1", 1);
            machine.enable(&remote.url, &[ACCOUNT]);
            local::stamp(&machine.db(), version);
            // Act
            let outcome = machine.pb(&[command]);
            // Assert
            assert_eq!(outcome.code, EXIT_FAILED);
            assert!(outcome.stderr.contains(expected), "{}", outcome.stderr);
            assert_eq!(remote.requests(), 0);
        }
    }
}

#[test]
fn a_missing_machine_id_or_usage_db_is_a_named_error() {
    // Arrange
    let remote = Remote::start();
    let no_id = machine_with_rows("no-id", "m1", 1);
    no_id.enable(&remote.url, &[ACCOUNT]);
    no_id.db().execute("DELETE FROM machine", []).unwrap();
    let no_db = Machine::new("no-db", "m2");
    no_db.enable(&remote.url, &[ACCOUNT]);
    std::fs::remove_file(no_db.dir.join("usage.db")).unwrap();
    // Act
    let (without_id, without_db) = (no_id.pb(&["push"]), no_db.pb(&["push"]));
    // Assert
    assert!(without_id.stderr.contains("no machine id"));
    assert!(without_db.stderr.contains("usage.db does not exist"));
    assert_eq!(remote.requests(), 0);
}

#[test]
fn every_command_has_help_and_rejects_unknown_flags() {
    // Arrange
    let machine = Machine::new("help", "m1");
    for command in ["push", "pull", "run", "purge"] {
        // Act
        let help = machine.pb(&[command, "--help"]);
        let unknown = machine.pb(&[command, "--bogus"]);
        // Assert
        assert_eq!(help.code, EXIT_OK);
        assert!(help
            .stdout
            .contains(&format!("usage: playbook sync {command}")));
        assert_eq!(unknown.code, EXIT_USAGE);
    }
}

#[test]
fn every_forwarded_name_is_accepted_with_help_and_says_playbook_sync() {
    // Arrange
    assert!(!FORWARDED.is_empty(), "the forwarded list is empty");
    let machine = Machine::new("forwarded-help", "m1");
    for name in FORWARDED {
        let mut args: Vec<&str> = name.split(' ').collect();
        args.push("--help");

        // Act
        let help = machine.pb(&args);

        // Assert
        assert_eq!(help.code, EXIT_OK, "{name}: {}", help.stderr);
        assert!(
            help.stdout
                .contains(&format!("usage: playbook sync {name}")),
            "{name}: {}",
            help.stdout
        );
    }
}

#[test]
fn the_top_usage_lists_the_forwarded_verbs_and_leaves_sync_out() {
    // Arrange
    let machine = Machine::new("top-usage", "m1");
    let verbs: std::collections::BTreeSet<&str> = FORWARDED
        .iter()
        .filter_map(|name| name.split(' ').next())
        .collect();

    // Act
    let outcome = machine.pb(&[]);

    // Assert
    assert_eq!(outcome.code, EXIT_USAGE);
    let line = outcome
        .stderr
        .lines()
        .find(|l| l.contains("usage:"))
        .expect("a usage line");
    let listed = line
        .split_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .map(|(list, _)| list.split(" | ").collect::<std::collections::BTreeSet<_>>())
        .expect("a verb list");
    assert_eq!(listed, verbs, "{line}");
    assert!(line.contains("playbook sync"), "{line}");
}

#[test]
fn the_old_sync_verb_still_runs_and_names_its_new_spelling_on_a_terminal_only() {
    for (is_terminal, notice) in [(true, true), (false, false)] {
        // Arrange
        let machine = machine_with_rows("sync-notice", "m1", 1);

        // Act
        let outcome = machine.pb_terminal(&["sync"], is_terminal);

        // Assert
        assert_eq!(outcome.code, EXIT_FAILED, "{}", outcome.stderr);
        assert!(outcome.stderr.contains("sync is off"));
        assert_eq!(
            outcome.stderr.contains("playbook sync run"),
            notice,
            "terminal {is_terminal}: {}",
            outcome.stderr
        );
    }
}

#[test]
fn run_pushes_then_pulls_like_the_old_sync_verb() {
    // Arrange
    let remote = Remote::start();
    let machine = machine_with_rows("run-verb", "m1", 2);
    machine.enable(&remote.url, &[ACCOUNT]);

    // Act
    let outcome = machine.pb(&["run"]);

    // Assert
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.stderr);
    assert_eq!(remote.count(SHARED_ROWS), 2);
    assert!(!outcome.stderr.contains("playbook sync run"));
}
