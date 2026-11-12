// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

// The fake server is shared; this file only needs its request count.
#[allow(dead_code)]
mod support;

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use pbsync::cli::{run, Context, EXIT_FAILED, EXIT_OK, EXIT_USAGE};
use pbsync::settings::{
    require_enabled, settings_path, Settings, SettingsError, DEFAULT_INTERVAL_SECONDS,
};
use pbsync::transport::{HttpTransport, Statement, Transport};
use playbook_store::Connection;
use support::{FakeServer, Reply};

const NOW: i64 = 1_800_000_000;
const SENTINEL: &str = "sentinel-token-9f3a";
const URL: &str = "https://secret-db-name.turso.io/path?authToken=nope";
static NEXT: AtomicU32 = AtomicU32::new(0);

struct Outcome {
    code: u8,
    stdout: String,
    stderr: String,
}

fn temp_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "settings-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn pb(dir: &std::path::Path, args: &[&str], stdin: &str, token: Option<&str>) -> Outcome {
    let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    let env = |name: &str| {
        (name == "PBSYNC_TOKEN")
            .then(|| token.map(str::to_string))
            .flatten()
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut input = Cursor::new(stdin.as_bytes().to_vec());
    let mut ctx = Context {
        data_dir: dir.to_path_buf(),
        env: &env,
        now: &|| NOW,
        is_terminal: &|| false,
        input: &mut input,
        out: &mut out,
        err: &mut err,
    };
    let code = run(&args, &mut ctx);
    Outcome {
        code,
        stdout: String::from_utf8(out).unwrap(),
        stderr: String::from_utf8(err).unwrap(),
    }
}

fn enable_yes(dir: &std::path::Path, accounts: &[&str]) -> Outcome {
    let mut args = vec!["enable", "--url", URL, "--yes"];
    for account in accounts {
        args.extend(["--account", account]);
    }
    pb(dir, &args, "", None)
}

#[test]
fn a_fresh_folder_has_everything_off() {
    // Arrange
    let dir = temp_dir();

    // Act
    let settings = Settings::load(&dir).unwrap();

    // Assert
    assert_eq!(settings, Settings::default());
    assert!(!settings.enabled);
    assert_eq!(settings.interval_seconds, DEFAULT_INTERVAL_SECONDS);
    assert!(!settings_path(&dir).exists());
}

#[test]
fn enable_without_url_fails_with_the_fix() {
    // Arrange
    let dir = temp_dir();

    // Act
    let outcome = pb(&dir, &["enable", "--yes"], "", None);

    // Assert
    assert_eq!(outcome.code, EXIT_USAGE);
    assert!(
        outcome.stderr.contains("playbook sync enable --url"),
        "{}",
        outcome.stderr
    );
}

#[test]
fn unknown_flags_are_errors_and_help_works() {
    // Arrange
    let dir = temp_dir();

    // Act
    let unknown = pb(&dir, &["status", "--bogus"], "", None);
    let helps: Vec<Outcome> = ["enable", "disable", "status"]
        .iter()
        .map(|c| pb(&dir, &[c, "--help"], "", None))
        .collect();

    // Assert
    assert_eq!(unknown.code, EXIT_USAGE);
    assert!(helps
        .iter()
        .all(|h| h.code == EXIT_OK && h.stdout.contains("usage: playbook sync")));
}

#[test]
fn answering_no_leaves_sync_off_and_writes_nothing() {
    // Arrange
    let dir = temp_dir();

    // Act
    let outcome = pb(
        &dir,
        &["enable", "--url", URL, "--account", "a"],
        "no\n",
        None,
    );

    // Assert
    assert_eq!(outcome.code, EXIT_FAILED);
    assert!(outcome
        .stdout
        .contains("Nothing leaves this machine until you answer yes"));
    assert!(!settings_path(&dir).exists());
}

#[test]
fn the_consent_text_lists_what_leaves_and_what_does_not() {
    // Arrange
    let dir = temp_dir();

    // Act
    let outcome = pb(
        &dir,
        &["enable", "--url", URL, "--account", "a@x.io"],
        "no\n",
        None,
    );

    // Assert
    for expected in [
        "secret-db-name.turso.io",
        "event_id, timestamp, account, repo, model",
        "salted 128-bit hashes",
        "Never sent: session id, agent, branch, path or raw email",
        "Accounts that will sync: a@x.io",
    ] {
        assert!(
            outcome.stdout.contains(expected),
            "{expected}: {}",
            outcome.stdout
        );
    }
    assert!(!outcome.stdout.contains("authToken"));
}

#[test]
fn yes_enables_with_consent_and_a_private_file_without_the_token() {
    // Arrange
    let dir = temp_dir();

    // Act
    let outcome = pb(
        &dir,
        &["enable", "--url", URL, "--account", "a"],
        "yes\n",
        Some(SENTINEL),
    );

    // Assert
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.stderr);
    let settings = Settings::load(&dir).unwrap();
    assert!(settings.enabled && settings.consented);
    assert_eq!(settings.enabled_at, Some(NOW));
    assert_eq!(settings.accounts, ["a"]);
    let text = std::fs::read_to_string(settings_path(&dir)).unwrap();
    for seen in [&text, &outcome.stdout, &outcome.stderr] {
        assert!(!seen.contains(SENTINEL));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(settings_path(&dir))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn enabling_with_no_account_syncs_nothing_and_says_so() {
    // Arrange
    let dir = temp_dir();

    // Act
    let outcome = enable_yes(&dir, &[]);

    // Assert
    assert_eq!(outcome.code, EXIT_OK);
    assert!(outcome.stdout.contains("Nothing will sync until"));
    assert!(Settings::load(&dir).unwrap().enabled);
}

#[test]
fn bad_urls_are_rejected_before_anything_is_asked_or_written() {
    for url in ["ftp://db.example.com", "http://db.example.com", "no-scheme"] {
        // Arrange
        let dir = temp_dir();

        // Act
        let outcome = pb(&dir, &["enable", "--url", url, "--yes"], "", None);

        // Assert
        assert_eq!(outcome.code, EXIT_USAGE, "{url}");
        assert!(!settings_path(&dir).exists(), "{url}");
    }
}

#[test]
fn plain_http_to_localhost_is_accepted() {
    // Arrange
    let dir = temp_dir();

    // Act
    let outcome = pb(
        &dir,
        &["enable", "--url", "http://127.0.0.1:8080", "--yes"],
        "",
        None,
    );

    // Assert
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.stderr);
}

#[test]
fn a_corrupt_file_is_a_named_error_and_stays_unchanged() {
    // Arrange
    let dir = temp_dir();
    let bytes = b"{ not json";
    std::fs::write(settings_path(&dir), bytes).unwrap();

    // Act
    let outcomes: Vec<Outcome> = [
        vec!["status"],
        vec!["disable"],
        vec!["enable", "--url", URL, "--yes"],
    ]
    .iter()
    .map(|args| pb(&dir, args, "", None))
    .collect();

    // Assert
    for outcome in &outcomes {
        assert_eq!(outcome.code, EXIT_FAILED);
        assert!(
            outcome.stderr.contains("not valid sync settings"),
            "{}",
            outcome.stderr
        );
    }
    assert_eq!(std::fs::read(settings_path(&dir)).unwrap(), bytes);
}

#[test]
fn an_unknown_version_is_a_named_error_with_the_version() {
    // Arrange
    let dir = temp_dir();
    std::fs::write(settings_path(&dir), r#"{"version": 99}"#).unwrap();

    // Act
    let error = Settings::load(&dir).unwrap_err();

    // Assert
    assert!(matches!(
        error,
        SettingsError::UnsupportedVersion { version: 99, .. }
    ));
    assert!(error.to_string().contains("version 99"));
}

#[test]
fn disable_keeps_the_allowlist_and_is_idempotent() {
    // Arrange
    let dir = temp_dir();
    enable_yes(&dir, &["a", "b"]);

    // Act
    let first = pb(&dir, &["disable"], "", None);
    let second = pb(&dir, &["disable"], "", None);

    // Assert
    assert_eq!((first.code, second.code), (EXIT_OK, EXIT_OK));
    let settings = Settings::load(&dir).unwrap();
    assert!(!settings.enabled);
    assert_eq!(settings.accounts, ["a", "b"]);
}

#[test]
fn disable_on_a_fresh_folder_writes_nothing() {
    // Arrange
    let dir = temp_dir();

    // Act
    let outcome = pb(&dir, &["disable"], "", None);

    // Assert
    assert_eq!(outcome.code, EXIT_OK);
    assert!(!settings_path(&dir).exists());
}

#[test]
fn changing_the_allowlist_sets_reset_watermark_and_the_same_list_does_not() {
    // Arrange
    let dir = temp_dir();

    // Act
    enable_yes(&dir, &[]);
    let empty = Settings::load(&dir).unwrap().reset_watermark;
    enable_yes(&dir, &["a"]);
    let added = Settings::load(&dir).unwrap().reset_watermark;
    let mut settings = Settings::load(&dir).unwrap();
    settings.reset_watermark = false;
    settings.save(&dir).unwrap();
    enable_yes(&dir, &["a"]);
    let same = Settings::load(&dir).unwrap().reset_watermark;

    // Assert
    assert!(!empty);
    assert!(added);
    assert!(!same);
}

#[test]
fn status_prints_each_state_without_the_token() {
    // Arrange
    let off = temp_dir();
    let unset = temp_dir();
    enable_yes(&unset, &["a", "b"]);

    // Act
    let off_out = pb(&off, &["status"], "", Some(SENTINEL));
    let unset_out = pb(&unset, &["status"], "", None);
    let set_out = pb(&unset, &["status"], "", Some(SENTINEL));

    // Assert
    assert!(off_out.stdout.contains("sync: off"));
    assert!(unset_out.stdout.contains("sync: enabled"));
    assert!(unset_out
        .stdout
        .contains("url host: secret-db-name.turso.io\n"));
    assert!(unset_out.stdout.contains("accounts (2): a, b"));
    assert!(unset_out.stdout.contains("token: unset"));
    assert!(set_out.stdout.contains("token: set"));
    assert!(unset_out.stdout.contains("rows pending: not started"));
    for outcome in [&off_out, &unset_out, &set_out] {
        assert_eq!(outcome.code, EXIT_OK);
        assert!(!outcome.stdout.contains(SENTINEL) && !outcome.stderr.contains(SENTINEL));
        assert!(!outcome.stdout.contains("authToken") && !outcome.stdout.contains("/path"));
    }
}

#[test]
fn status_reads_the_local_sync_state_and_counts_pending_rows() {
    // Arrange
    let dir = temp_dir();
    enable_yes(&dir, &["dev@example.com"]);
    let db = Connection::open(dir.join("usage.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE usage_events (event_id TEXT PRIMARY KEY, account TEXT NOT NULL);
         CREATE TABLE sync_watermarks (table_name TEXT PRIMARY KEY, rowid_mark INTEGER NOT NULL);
         CREATE TABLE sync_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO usage_events VALUES ('1', 'dev@example.com'), ('2', 'dev@example.com'),
            ('3', 'dev@example.com'), ('4', 'other@example.com');
         INSERT INTO sync_watermarks VALUES ('usage_events', 1);
         INSERT INTO sync_state VALUES ('last_success', '1799999000'), ('failures', '2');",
    )
    .unwrap();
    drop(db);
    let mut settings = Settings::load(&dir).unwrap();
    assert!(settings.reset_watermark);
    let before_reset_consumed = pb(&dir, &["status"], "", None);
    settings.reset_watermark = false;
    settings.save(&dir).unwrap();

    // Act
    let outcome = pb(&dir, &["status"], "", None);

    // Assert
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.stderr);
    assert!(outcome.stdout.contains("last success: 1799999000"));
    assert!(outcome.stdout.contains("last error: not started"));
    assert!(outcome.stdout.contains("failures: 2"));
    assert!(
        outcome.stdout.contains("rows pending: 2"),
        "{}",
        outcome.stdout
    );
    assert!(outcome.stdout.contains("schema: not started"));
    assert!(before_reset_consumed.stdout.contains("rows pending: 3"));
}

#[test]
fn the_gate_refuses_when_off_and_makes_zero_requests() {
    // Arrange
    let server = FakeServer::start(vec![Reply::new(200, "{}")]);
    let dir = temp_dir();
    let settings = Settings::load(&dir).unwrap();

    // Act
    let gate = require_enabled(&settings);
    if gate.is_ok() {
        let token = pbsync::token::resolve_token(Some(SENTINEL), "X", &|_| None).unwrap();
        let _ = HttpTransport::new(&server.url, token)
            .unwrap()
            .execute(&[Statement {
                sql: "SELECT 1".into(),
                args: vec![],
            }]);
    }

    // Assert
    assert_eq!(gate, Err(SettingsError::Disabled));
    assert_eq!(server.requests().len(), 0);
}

#[test]
fn the_gate_passes_once_enabled() {
    // Arrange
    let dir = temp_dir();
    enable_yes(&dir, &[]);

    // Act
    let gate = require_enabled(&Settings::load(&dir).unwrap());

    // Assert
    assert_eq!(gate, Ok(()));
}
