// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook usage` and its subcommands: picks the terminal view or the text or
//! JSON output.

use super::command::UsageCommand;
use super::daemon::{self, Stopped};
use super::period::Period;
use super::query::{self, Range, WeekStart};
use super::run::{ingest_new, run_summary, Paths};
use super::{api, db, tui};
use playbook_store::Connection;
use std::path::Path;
use std::time::Duration;

/// How long `usage stop` waits for the collector before it may kill it.
const STOP_BUDGET: Duration = Duration::from_secs(3);

/// The flags of `playbook usage` itself.
pub struct Opts {
    pub json: bool,
    pub summary: bool,
    /// The `--range` value, or `None` when the flag was not given.
    pub range: Option<String>,
    /// The `--account` value: an account key or its name.
    pub account: Option<String>,
}

/// Whether the terminal view can run: it needs a terminal on both ends.
pub fn interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// The `--json` document: the totals and groups, the limits and burn rate
/// under `limits`, plus
/// the live view under `live`, and for today, week and month the comparison
/// with the previous period under `comparison`.
pub fn json_report(
    conn: &Connection,
    paths: &Paths,
    range: Range,
    now: i64,
    account: Option<&str>,
) -> Result<String, String> {
    let key = super::run::resolve_account(conn, account)?;
    let key = key.as_deref();
    let mut usage = query::load_window(conn, range, now)?;
    tui::account::keep(&mut usage, key);
    let mut tools = db::load_tool_events(conn)?;
    tui::account::keep_tools(&mut tools, key);
    let mut data = api::data_json(&usage, &tools, now, range);
    let scope = crate::telemetry_db::RowScope {
        machine: None,
        account: key,
    };
    let i = query::load_live_inputs(conn, now, scope)?;
    let live = api::live_json(&i.window, &i.sessions, &i.latest, now);
    let since = super::limits_source::events_since(now, configured_week_start());
    let mut limit_events = db::load_usage_events_since(conn, since)?;
    tui::account::keep(&mut limit_events, key);
    let limits = super::run::limits_json(paths, conn, &limit_events, now);
    if let Some(map) = data.as_object_mut() {
        map.insert("live".to_string(), live);
        map.insert("limits".to_string(), limits);
        if let Some(key) = key {
            map.insert("account".to_string(), key.into());
        }
        if let Some(period) = Period::of_range(range) {
            let mut events = db::load_usage_events_since(conn, period.windows(now).previous.start)?;
            tui::account::keep(&mut events, key);
            map.insert(
                "comparison".to_string(),
                api::comparison_json(&events, now, period),
            );
        }
    }
    serde_json::to_string_pretty(&data).map_err(|e| format!("failed to encode usage data: {e}"))
}

/// True when both paths reach one file: the same path, a symlink to it, or a
/// hard link to it.
fn is_same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return x.dev() == y.dev() && x.ino() == y.ino();
        }
    }
    matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// Runs the command. The returned text, when not empty, is printed on stdout.
pub fn run(
    sub: Option<UsageCommand>,
    opts: &Opts,
    paths: &Paths,
    interactive: bool,
) -> Result<String, String> {
    // Every command starts from a data file that exists (created owner-only,
    // with older data moved in first), so no command errors on a fresh home.
    // This runs before the terminal view takes over the screen.
    super::tz::ensure_init();
    // A rehearsal on a copy of the data file touches nothing else.
    if let Some(UsageCommand::Account {
        command,
        db: Some(db),
    }) = &sub
    {
        if !db.is_file() {
            return Err(format!("no usage database at {}", db.display()));
        }
        if is_same_file(db, &paths.db) {
            return Err(
                "--db names the live usage database; make a copy and name that".to_string(),
            );
        }
        return super::account_cmd::run_on(super::db::open_existing_db(db)?, command);
    }
    // The doctor only reads files, so it runs before the data file is created.
    if let Some(UsageCommand::Doctor { raw }) = sub {
        let usage_dir = paths
            .db
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let budgets = super::limits_source::manual_budgets(&super::host::home_dir());
        return Ok(super::plan::report(
            &paths.claude_json,
            usage_dir,
            &budgets,
            crate::util::time::now_secs(),
            raw,
        ));
    }
    // Stopping needs no data file, so it creates none.
    if let Some(UsageCommand::Stop) = sub {
        return stop_text(
            daemon::stop(paths, STOP_BUDGET, true),
            &daemon::lock_path(paths),
        );
    }
    super::db::open_db(&paths.db).map(drop)?;
    match sub {
        // Answered above, before the data file is created.
        Some(UsageCommand::Doctor { .. } | UsageCommand::Stop) => Ok(String::new()),
        Some(UsageCommand::Warm) => super::warm::run_warm(paths).map(|()| String::new()),
        Some(UsageCommand::Collect { port, idle_seconds }) => {
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            super::collector::run_collect(paths, port, idle_seconds, &stop)
        }
        Some(UsageCommand::Account { command, .. }) => super::account_cmd::run(paths, &command),
        Some(UsageCommand::Machine { name }) => super::machine::run(paths, name.as_deref()),
        Some(UsageCommand::Prune { dry_run }) => super::collector::run_prune(paths, dry_run),
        Some(UsageCommand::Status { json }) => {
            let status = super::collector::status(paths, super::collector::configured_retention())?;
            if json {
                serde_json::to_string_pretty(&status)
                    .map_err(|e| format!("failed to encode the collector status: {e}"))
            } else {
                Ok(status.text(crate::util::time::now_secs()))
            }
        }
        None if opts.summary => {
            let range = opts.range.as_deref().map(parse_range).transpose()?;
            run_summary(
                paths,
                range,
                crate::util::time::now_secs(),
                opts.account.as_deref(),
            )
        }
        None if opts.json => {
            let range = parse_range(opts.range.as_deref().unwrap_or(DEFAULT_RANGE))?;
            let (conn, _) = ingest_new(paths)?;
            json_report(
                &conn,
                paths,
                range,
                crate::util::time::now_secs(),
                opts.account.as_deref(),
            )
        }
        None if interactive => {
            let range = opts.range.as_deref().map(parse_range).transpose()?;
            let key = match opts.account.as_deref() {
                Some(account) => {
                    let (conn, _) = ingest_new(paths)?;
                    super::run::resolve_account(&conn, Some(account))?
                }
                None => None,
            };
            tui::run(paths, range, key)?;
            Ok(String::new())
        }
        None => {
            let range = opts.range.as_deref().map(parse_range).transpose()?;
            run_summary(
                paths,
                range,
                crate::util::time::now_secs(),
                opts.account.as_deref(),
            )
        }
    }
}

/// What `usage stop` prints for each outcome.
fn stop_text(stopped: Stopped, lock: &Path) -> Result<String, String> {
    match stopped {
        Stopped::Stopped | Stopped::Killed => Ok("stopped".to_string()),
        Stopped::NotRunning => Ok("not running".to_string()),
        Stopped::StillRunning => Err(format!(
            "the collector did not stop within {} seconds; see who holds {}",
            STOP_BUDGET.as_secs(),
            lock.display()
        )),
        Stopped::Unavailable => Err(format!("cannot read the lock {}", lock.display())),
    }
}

/// The range of `--json` when `--range` is not given.
const DEFAULT_RANGE: &str = "30d";

/// The first day of the week, from the `usage.weekStart` config key. Monday
/// when the key is unset or unreadable.
pub fn configured_week_start() -> WeekStart {
    super::host::config_value_now("usage.weekStart")
        .and_then(|value| value.as_str().and_then(WeekStart::parse))
        .unwrap_or_default()
}

/// Seconds between refreshes of the terminal view, from the
/// `usage.refreshSeconds` config key.
pub fn configured_refresh_seconds() -> u64 {
    super::host::config_value_now("usage.refreshSeconds")
        .and_then(|value| value.as_u64())
        .filter(|n| *n > 0)
        .unwrap_or(tui::worker::DEFAULT_REFRESH_SECONDS)
}

/// Parses a range name with the configured week start.
pub fn parse_range(value: &str) -> Result<Range, String> {
    Range::parse_with(Some(value), configured_week_start()).ok_or_else(|| {
        format!(
            "unknown range `{value}`; use today, week, last-week, month, last-month, 30d, 60d, 90d or all"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::AccountCommand;
    use crate::{ToolInvocationEvent, ToolKind, UsageEvent};

    fn paths(name: &str) -> Paths {
        let dir = crate::util::scratch_dir(name);
        crate::host::set_test_home(&dir);
        let mut p = Paths::from_home(&dir);
        p.db = dir.join("usage").join("usage.db");
        p
    }

    fn no_opts() -> Opts {
        Opts {
            json: false,
            summary: false,
            range: None,
            account: None,
        }
    }

    #[test]
    fn stop_with_nothing_running_says_so_and_creates_no_data_file() {
        let p = paths("entry-stop-none");

        let out = run(Some(UsageCommand::Stop), &no_opts(), &p, false);

        assert_eq!(out, Ok("not running".to_string()));
        assert!(!p.db.exists());
    }

    #[cfg(unix)]
    #[test]
    fn stop_ends_a_running_collector_and_frees_the_lock() {
        let p = paths("entry-stop-running");
        let _collector = crate::collector::tests::Running::start(&p);

        let out = run(Some(UsageCommand::Stop), &no_opts(), &p, false);

        assert_eq!(out, Ok("stopped".to_string()));
        assert_eq!(
            crate::daemon::lock_state(&p),
            crate::daemon::LockState::Free
        );
    }

    #[test]
    fn a_collector_that_stays_is_an_error() {
        let err = stop_text(Stopped::StillRunning, Path::new("/x/collector.lock")).unwrap_err();
        assert!(err.contains("did not stop"), "{err}");
        let err = stop_text(Stopped::Unavailable, Path::new("/x/collector.lock")).unwrap_err();
        assert!(err.contains("/x/collector.lock"), "{err}");
        assert_eq!(
            stop_text(Stopped::Killed, Path::new("/x")),
            Ok("stopped".into())
        );
    }

    fn account(p: &Paths, args: AccountCommand) -> Result<String, String> {
        run(
            Some(UsageCommand::Account {
                command: args,
                db: None,
            }),
            &no_opts(),
            p,
            false,
        )
    }

    #[test]
    fn the_account_commands_name_a_key_and_list_it_through_the_entry_point() {
        let p = paths("entry-account");
        let conn = crate::db::open_db(&p.db).unwrap();
        conn.execute(
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                cache_read_tokens, cost_usd) VALUES ('e', 86400, 's', 'a@b.c', 'm', '', 'r', 'b', 1, 1, 0, 0, 2.5)",
            [],
        )
        .unwrap();
        let name = |name: &str| AccountCommand::Name {
            key: "a@b.c".into(),
            name: name.into(),
        };

        assert!(account(&p, AccountCommand::List)
            .unwrap()
            .starts_with("a@b.c\n  1 events, $2.50"));
        assert_eq!(account(&p, name("Home")).unwrap(), "a@b.c is shown as Home");
        assert!(account(&p, AccountCommand::List)
            .unwrap()
            .starts_with("Home (a@b.c)"));
        assert_eq!(account(&p, name("")).unwrap(), "cleared the name of a@b.c");
    }

    #[test]
    fn assign_dry_runs_then_applies_and_undoes_through_the_entry_point() {
        crate::tz::set(crate::tz::Zone::Utc);
        let p = paths("entry-assign");
        let conn = crate::db::open_db(&p.db).unwrap();
        conn.execute(
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                cache_read_tokens, cost_usd) VALUES ('e', 86400, 's', 'a@b.c', 'm', '', 'r', 'b', 1, 1, 0, 0, 2.5)",
            [],
        )
        .unwrap();
        let assign = |confirm| AccountCommand::Assign {
            from: Some("1970-01-02".into()),
            from_session: None,
            before_session: None,
            through_session: None,
            session: None,
            to: Some("1970-01-03".into()),
            target: "personal".into(),
            was: None,
            confirm,
        };

        let dry = account(&p, assign(false)).unwrap();
        assert!(
            dry.contains("1 events ($2.50)") && dry.contains("--confirm"),
            "{dry}"
        );
        assert!(account(&p, assign(true)).unwrap().contains("move 1:"));
        assert!(account(&p, assign(true))
            .unwrap()
            .starts_with("nothing to move"));
        let undone = account(&p, AccountCommand::Undo { id: None }).unwrap();
        assert!(undone.contains("restored 1"), "{undone}");
    }

    #[test]
    fn the_db_option_runs_an_account_command_on_a_copy_and_leaves_the_default_alone() {
        let p = paths("entry-account-db");
        let copy = crate::util::scratch_dir("entry-account-copy").join("copy.db");
        crate::db::open_db(&copy).unwrap();
        let on = |db: &Path| {
            run(
                Some(UsageCommand::Account {
                    command: AccountCommand::List,
                    db: Some(db.to_path_buf()),
                }),
                &no_opts(),
                &p,
                false,
            )
        };

        assert_eq!(on(&copy), Ok("No usage recorded yet.".to_string()));
        assert!(!p.db.exists(), "the default database was not created");
        let err = on(&copy.with_file_name("missing.db")).unwrap_err();
        assert!(err.contains("no usage database at"), "{err}");
        crate::db::open_db(&p.db).unwrap();
        let err = on(&p.db).unwrap_err();
        assert!(err.contains("names the live usage database"), "{err}");
        let link = copy.with_file_name("hard.db");
        std::fs::hard_link(&p.db, &link).unwrap();
        let err = on(&link).unwrap_err();
        assert!(err.contains("names the live usage database"), "{err}");
        let sym = copy.with_file_name("sym.db");
        std::os::unix::fs::symlink(&p.db, &sym).unwrap();
        assert!(on(&sym).is_err());
    }

    #[test]
    fn a_confirmed_move_on_a_copy_leaves_the_live_database_and_its_folder_alone() {
        use std::os::unix::fs::PermissionsExt;
        let p = paths("entry-account-live");
        let live = crate::db::open_db(&p.db).unwrap();
        let seed = |conn: &playbook_store::Connection| {
            conn.execute(
                "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                    repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                    cache_read_tokens, cost_usd) VALUES ('e', 100, 's', 'a@b.c', 'm', '', 'r', 'b', 1, 1, 0, 0, 1.0)",
                [],
            )
            .unwrap();
        };
        seed(&live);
        drop(live);
        let dir = crate::util::scratch_dir("entry-account-copy-dir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let copy = dir.join("copy.db");
        std::fs::copy(&p.db, &copy).unwrap();
        let out = run(
            Some(UsageCommand::Account {
                command: AccountCommand::Assign {
                    from: Some("1970-01-01".into()),
                    to: Some("1970-01-03".into()),
                    from_session: None,
                    before_session: None,
                    through_session: None,
                    session: None,
                    target: "token:abcd1234".into(),
                    was: None,
                    confirm: true,
                },
                db: Some(copy.clone()),
            }),
            &no_opts(),
            &p,
            false,
        )
        .unwrap();
        assert!(out.contains("now belong to token:abcd1234"), "{out}");
        let account = |db: &Path| -> String {
            playbook_store::Connection::open(db)
                .unwrap()
                .query_row("SELECT account FROM usage_events", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(account(&copy), "token:abcd1234");
        assert_eq!(account(&p.db), "a@b.c");
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "the folder of the copy keeps its mode");
    }

    const NOW: i64 = 1_788_300_000;

    /// Two accounts: `work@x.io` has two events, `token:abcd1234` one; the price comes from the model.
    fn two_account_store(name: &str) -> Paths {
        crate::tz::set(crate::tz::Zone::Utc);
        let p = paths(name);
        let conn = crate::db::open_db(&p.db).unwrap();
        let usage = |id: &str, session: &str, account: &str, ts: i64, cost: f64| UsageEvent {
            event_id: id.into(),
            timestamp: ts,
            session_id: session.into(),
            account: account.into(),
            model: "claude-sonnet-5".into(),
            repo: "proj".into(),
            branch: "main".into(),
            input_tokens: 10,
            output_tokens: 20,
            cost_usd: cost,
            ..UsageEvent::default()
        };
        for e in [
            usage("e1", "s1", "work@x.io", NOW - 3600, 1.5),
            usage("e2", "s1", "work@x.io", NOW - 1800, 0.25),
            usage("e3", "s2", "token:abcd1234", NOW - 900, 4.0),
        ] {
            crate::db::upsert_usage_event(&conn, &e).unwrap();
        }
        let tool = |id: &str, session: &str, account: &str, name: &str| ToolInvocationEvent {
            event_id: id.into(),
            timestamp: NOW - 1000,
            session_id: session.into(),
            account: account.into(),
            kind: ToolKind::Skill,
            name: name.into(),
        };
        crate::db::insert_tool_event(&conn, &tool("t1", "s1", "work@x.io", "plan")).unwrap();
        crate::db::insert_tool_event(&conn, &tool("t2", "s2", "token:abcd1234", "fix")).unwrap();
        p
    }

    fn summary_text(p: &Paths, account: Option<&str>) -> Result<String, String> {
        run_summary(p, None, NOW, account)
    }

    fn json_text(p: &Paths, account: Option<&str>) -> Result<String, String> {
        let (conn, _) = ingest_new(p).unwrap();
        json_report(&conn, p, parse_range("all").unwrap(), NOW, account)
    }

    #[test]
    fn without_an_account_the_summary_and_json_match_the_captured_output() {
        // Arrange
        let p = two_account_store("entry-golden");

        // Act
        let text = summary_text(&p, None).unwrap();
        let json = json_text(&p, None).unwrap();

        // Assert
        assert_eq!(text, include_str!("golden/usage_summary.txt"));
        assert_eq!(json, include_str!("golden/usage_json.json"));
    }

    #[test]
    fn one_account_gets_its_own_summary_total_and_names_itself() {
        // Arrange
        let p = two_account_store("entry-account-summary");
        let conn = crate::db::open_db(&p.db).unwrap();
        crate::account_store::set_name(&conn, "work@x.io", "Work").unwrap();
        drop(conn);

        // Act
        let text = summary_text(&p, Some("work")).unwrap();

        // Assert
        let mut lines = text.lines();
        assert!(lines
            .next()
            .unwrap()
            .starts_with("Usage: 2 messages, $0.0004"));
        assert_eq!(lines.next(), Some("Account: Work"));
        assert!(text.contains("  plan") && !text.contains("  fix"), "{text}");
        assert!(!text.contains("token:abcd1234"), "{text}");
    }

    #[test]
    fn the_mask_key_cuts_the_account_line_and_the_account_table_of_the_summary() {
        // Arrange
        let p = two_account_store("entry-account-mask");
        let data = crate::config::root(&crate::host::home_dir()).join("data");
        std::fs::create_dir_all(&data).unwrap();
        let conn = playbook_store::Connection::open(playbook_data::config::db_in(&data)).unwrap();
        conn.execute_batch(playbook_data::config::SCHEMA_SQL)
            .unwrap();
        conn.execute(
            "INSERT INTO config VALUES ('global','',?1,'true',1)",
            ["usage.maskAccount"],
        )
        .unwrap();

        // Act
        let text = summary_text(&p, Some("work@x.io")).unwrap();

        // Assert
        assert!(text.contains("Account: @x\n"), "{text}");
        assert!(!text.contains("work@x.io"), "{text}");
        assert!(text.matches("@x").count() >= 2, "{text}");
    }

    #[test]
    fn one_account_gets_its_own_json_totals_and_names_its_key() {
        // Arrange
        let p = two_account_store("entry-account-json");

        // Act
        let json = json_text(&p, Some("token:abcd1234")).unwrap();

        // Assert
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["account"], "token:abcd1234");
        assert_eq!(v["totals"]["messages"], 1);
        assert_eq!(v["totals"]["cost_usd"], 0.00022);
        assert_eq!(v["live"]["today"]["messages"], 1);
        assert_eq!(v["skills"][0]["name"], "fix");
    }

    #[test]
    fn an_unknown_account_is_an_error_that_points_to_the_account_list() {
        // Arrange
        let p = two_account_store("entry-account-unknown");

        // Act
        let summary = summary_text(&p, Some("nobody")).unwrap_err();
        let json = json_text(&p, Some("nobody")).unwrap_err();

        // Assert
        assert!(summary.contains("playbook usage account list"), "{summary}");
        assert_eq!(summary, json);
    }

    #[test]
    fn a_range_names_one_of_the_nine_values() {
        for ok in [
            "today",
            "week",
            "last-week",
            "month",
            "last-month",
            "30d",
            "60d",
            "90d",
            "all",
        ] {
            assert!(parse_range(ok).is_ok(), "{ok}");
        }
        let err = parse_range("7d").unwrap_err();
        assert!(err.contains("unknown range `7d`"), "{err}");
    }
}
