// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Binary-level tests for `playbook usage`, run
//! against a scratch `$HOME` seeded from the committed fixtures. The real
//! `~/.claude` is never read.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn fixtures() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT")).join("crates/telemetry/tests/fixtures/usage")
}

struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "playbook-usage-{}-{tag}-{n}",
            playbook::testing::run_id()
        ));
        let project = dir.join(".claude/projects/proj-one");
        fs::create_dir_all(&project).unwrap();
        fs::copy(
            fixtures().join("usage/proj-one/s1.jsonl"),
            project.join("s1.jsonl"),
        )
        .unwrap();
        fs::copy(
            fixtures().join("account/claude.json"),
            dir.join(".claude.json"),
        )
        .unwrap();
        Home(dir.canonicalize().unwrap())
    }

    /// The fixture session of the work login, plus a second session that a
    /// `session-accounts.log` line assigns to a token account.
    fn with_two_accounts() -> Self {
        let home = Self::new("two-accounts");
        let project = home.0.join(".claude/projects/proj-one");
        fs::write(
            project.join("s2.jsonl"),
            concat!(
                r#"{"type":"assistant","uuid":"v1","sessionId":"s2","timestamp":"2026-09-01T09:00:00.000Z","cwd":"/home/u/Work/proj-one","gitBranch":"main","effort":"high","message":{"id":"msg_S2","model":"claude-sonnet-5","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":100,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":200}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let data = home.0.join(".config/playbook/data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("session-accounts.log"), "s2\ttoken:abcd1234\n").unwrap();
        home
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .env("HOME", &self.0)
            .output()
            .unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(Path::new(&self.0));
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn bare_usage_ingests_and_prints_a_summary_with_fixture_values() {
    let home = Home::new("summary");

    let out = home.run(&["usage"]);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(
        text.contains("3 messages, $0.0973 estimated cost"),
        "{text}"
    );
    let row = text
        .lines()
        .find(|l| l.contains("claude-sonnet-5"))
        .unwrap();
    assert!(row.contains("770") && row.contains("0.0863"), "{row}");
    assert!(text.contains("dev@example.com"), "{text}");
}

#[test]
fn a_second_summary_run_reports_the_same_totals_and_adds_nothing() {
    let home = Home::new("summary-twice");

    let first = home.run(&["usage", "--summary"]);
    let second = home.run(&["usage", "--summary"]);

    assert!(first.status.success() && second.status.success());
    assert_eq!(stdout(&first), stdout(&second));
}

#[test]
fn a_home_with_no_claude_data_prints_the_empty_message_and_exits_zero() {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "playbook-usage-{}-empty-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .arg("usage")
        .env("HOME", &dir)
        .output()
        .unwrap();

    assert!(out.status.success());
    assert!(stdout(&out).contains("No usage recorded yet"));
    let _ = fs::remove_dir_all(dir);
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn the_summary_reads_codex_rollouts_when_the_sessions_dir_exists() {
    let home = Home::new("codex");
    copy_dir(&fixtures().join("codex"), &home.0.join(".codex/sessions"));

    let first = home.run(&["usage", "--summary"]);
    let second = home.run(&["usage", "--summary"]);

    assert!(first.status.success() && second.status.success());
    assert!(
        !stdout(&first).contains("No usage recorded yet"),
        "{}",
        stdout(&first)
    );
    assert_eq!(stdout(&first), stdout(&second));
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn usage_json_prints_totals_and_the_live_view_without_charts() {
    let home = Home::new("json");

    let out = home.run(&["usage", "--json", "--range", "all"]);

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["totals"]["messages"], 3);
    assert!((doc["totals"]["cost_usd"].as_f64().unwrap() - 0.0973221).abs() < 1e-9);
    assert!(doc["live"]["active"].is_array());
    assert!(doc.get("charts").is_none());
    assert!(doc["live"].get("burn_chart").is_none());
}

#[test]
fn usage_json_adds_a_comparison_for_a_period_range_only() {
    let home = Home::new("json-comparison");

    let week = home.run(&["usage", "--json", "--range", "week"]);
    let all = home.run(&["usage", "--json", "--range", "all"]);

    assert!(week.status.success(), "stderr: {}", stderr(&week));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&week)).unwrap();
    assert_eq!(doc["comparison"]["period"], "week");
    assert!(doc["comparison"]["change"]["cost_usd"]["amount"].is_number());
    let doc: serde_json::Value = serde_json::from_str(&stdout(&all)).unwrap();
    assert!(doc.get("comparison").is_none());
}

#[test]
fn usage_summary_prints_the_text_report() {
    let home = Home::new("summary-flag");

    let out = home.run(&["usage", "--summary"]);

    assert!(out.status.success());
    assert!(stdout(&out).contains("3 messages, $0.0973 estimated cost"));
}

#[test]
fn an_unknown_range_is_refused_with_the_valid_values() {
    let home = Home::new("bad-range");

    let out = home.run(&["usage", "--json", "--range", "7d"]);

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("unknown range `7d`"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn conflicting_flags_are_refused() {
    let home = Home::new("conflict");

    assert!(!home.run(&["usage", "--json", "--summary"]).status.success());
}

#[test]
fn the_web_dashboard_flag_and_subcommand_are_gone() {
    let home = Home::new("no-web");

    for args in [["usage", "--web"], ["usage", "dashboard"]] {
        assert!(!home.run(&args).status.success(), "{args:?}");
    }
}

fn json_range(home: &Home, range: &str) -> serde_json::Value {
    let out = home.run(&["usage", "--json", "--range", range]);
    assert!(out.status.success(), "{range}: {}", stderr(&out));
    serde_json::from_str(&stdout(&out)).unwrap()
}

#[test]
fn json_accepts_every_calendar_range_and_names_its_dates() {
    let home = Home::new("json-ranges");

    for (key, days) in [
        ("today", 1),
        ("week", 7),
        ("last-week", 7),
        ("last-month", 28),
    ] {
        let doc = json_range(&home, key);
        assert_eq!(doc["range"]["key"], key);
        let start = doc["range"]["start"].as_str().unwrap();
        let end = doc["range"]["end"].as_str().unwrap();
        assert!(start <= end, "{key}: {start} {end}");
        // Compare day numbers, not strings: a month has 28 to 31 days.
        let n = day_number(end) - day_number(start) + 1;
        if key == "last-month" {
            assert!((28..=31).contains(&n), "{key}: {n}");
        } else {
            assert_eq!(n, days, "{key}");
        }
    }
    // The fixture is old, so none of the short ranges hold it.
    assert_eq!(json_range(&home, "today")["totals"]["messages"], 0);
    assert_eq!(json_range(&home, "all")["totals"]["messages"], 3);
}

/// Days since 0000-03-01, enough to subtract two `YYYY-MM-DD` dates.
fn day_number(date: &str) -> i64 {
    let part = |i: usize| date.split('-').nth(i).unwrap().parse::<i64>().unwrap();
    let (mut y, m, d) = (part(0), part(1), part(2));
    let m = if m <= 2 {
        y -= 1;
        m + 9
    } else {
        m - 3
    };
    365 * y + y / 4 - y / 100 + y / 400 + (153 * m + 2) / 5 + d
}

#[test]
fn summary_accepts_a_range_and_says_which_dates_it_covers() {
    let home = Home::new("summary-range");

    let all = home.run(&["usage", "--summary", "--range", "all"]);
    let today = home.run(&["usage", "--summary", "--range", "today"]);
    let week = home.run(&["usage", "--range", "week"]);

    assert!(all.status.success() && today.status.success() && week.status.success());
    assert!(stdout(&all).contains("3 messages, $0.0973 estimated cost across all"));
    let text = stdout(&today);
    assert!(text.starts_with("No usage in today ("), "{text}");
    assert!(text.contains("local"), "{text}");
    // A range with no flag prints the summary when stdout is not a terminal.
    assert!(
        stdout(&week).starts_with("No usage in this week ("),
        "{}",
        stdout(&week)
    );
}

#[test]
fn an_unknown_range_lists_the_new_names() {
    let home = Home::new("range-names");

    let out = home.run(&["usage", "--summary", "--range", "yesterday"]);

    assert!(!out.status.success());
    let err = stderr(&out);
    for name in ["today", "week", "last-week", "last-month"] {
        assert!(err.contains(name), "{err}");
    }
}

#[test]
fn the_week_start_key_moves_the_week_in_the_json_range() {
    let home = Home::new("week-start");
    let start_of = |home: &Home| {
        let doc = json_range(home, "week");
        doc["range"]["start"].as_str().unwrap().to_string()
    };
    let monday = start_of(&home);
    let set = home.run(&["config", "set", "--global", "usage.weekStart", "sunday"]);
    assert!(set.status.success(), "{}", stderr(&set));

    let sunday = start_of(&home);

    // Sunday starts the day before Monday does, except on a Sunday itself,
    // when this week started today under the Sunday rule, six days after the
    // Monday rule's start.
    let gap = day_number(&monday) - day_number(&sunday);
    assert!(gap == 1 || gap == -6, "{monday} {sunday}");
    let bad = home.run(&["config", "set", "--global", "usage.weekStart", "friday"]);
    assert!(!bad.status.success());
}

#[test]
fn the_time_zone_setting_is_a_config_key_that_takes_local_or_utc() {
    let home = Home::new("time-zone-key");

    let set = home.run(&["config", "set", "--global", "usage.timeZone", "utc"]);
    let get = home.run(&["config", "get", "usage.timeZone"]);
    let bad = home.run(&["config", "set", "--global", "usage.timeZone", "mars"]);

    assert!(set.status.success(), "{}", stderr(&set));
    assert!(stdout(&get).contains("utc"));
    assert!(!bad.status.success());
}

#[test]
fn usage_status_says_not_running_and_prints_json_without_a_token() {
    let home = Home::new("status-none");

    let text = home.run(&["usage", "status"]);
    let json = home.run(&["usage", "status", "--json"]);

    assert!(text.status.success() && json.status.success());
    let text = stdout(&text);
    assert!(text.contains("collector: not running"), "{text}");
    assert!(text.contains("last event: none"), "{text}");
    assert!(!text.contains("background service"), "{text}");
    assert!(
        text.contains(&format!("fix: {}", playbook::usage::daemon::START_HINT)),
        "{text}"
    );
    assert!(!text.contains("collector-service"), "{text}");
    let doc: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
    assert_eq!(doc["running"], false);
    assert!(doc.get("service").is_none(), "{doc}");
    assert_eq!(doc["requests"], 0);
    assert!(doc.get("token").is_none());
}

#[test]
fn usage_status_says_nothing_of_a_leftover_service() {
    use playbook::usage::service::{log_path, render, Kind};
    let Some(kind) = Kind::current() else {
        return;
    };
    // Arrange: a service file an older setup wrote; doctor reports it.
    let home = Home::new("status-service");
    let file = kind.file(&home.0);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let program = std::path::Path::new(env!("CARGO_BIN_EXE_playbook"));
    std::fs::write(&file, render(kind, program, &log_path(&home.0))).unwrap();

    // Act
    let text = stdout(&home.run(&["usage", "status"]));

    // Assert
    assert!(!text.contains("background service"), "{text}");
    assert!(text.contains(playbook::usage::daemon::START_HINT), "{text}");
}

#[test]
fn a_collector_is_seen_by_status_and_idles_out_even_while_status_polls_it() {
    let home = Home::new("status-live");
    let mut child = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["usage", "collect", "--port", "0", "--idle-seconds", "2"])
        .env("HOME", &home.0)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    // Keep probing until the collector stops: probes must not keep it alive.
    // The loop follows the collector's own life instead of a fixed window, so
    // a slow start under load cannot make the probes miss it, and the limit
    // only guards against a collector that probing keeps alive.
    let mut seen = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the collector did not stop by itself while status polled it");
        }
        let json = home.run(&["usage", "status", "--json"]);
        let doc: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
        seen |= doc["running"] == true && doc["idle_seconds"] == 2;
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(seen, "status never saw the running collector");
    assert!(status.success());
    let after = stdout(&home.run(&["usage", "status"]));
    assert!(after.contains("not running"), "{after}");
}

#[test]
fn the_utc_flag_beats_a_local_environment_value() {
    // Arrange
    let home = Home::new("tz-utc");

    // Act
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["usage", "--summary", "--utc"])
        .env("HOME", &home.0)
        .env("PLAYBOOK_TELEMETRY_TZ", "local")
        .output()
        .unwrap();

    // Assert
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("Times and days are in UTC."),
        "{}",
        stdout(&out)
    );
}

#[test]
fn both_zone_flags_together_are_refused() {
    // Arrange
    let home = Home::new("tz-both");

    // Act
    let out = home.run(&["usage", "--summary", "--utc", "--local"]);

    // Assert
    assert!(!out.status.success());
}

#[test]
fn the_zone_flags_work_after_a_usage_subcommand() {
    // Arrange
    let home = Home::new("tz-sub");

    // Act
    let out = home.run(&["usage", "status", "--utc"]);

    // Assert
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn a_piped_summary_with_a_monthly_budget_prints_the_burn_up_table_in_ascii() {
    // Arrange
    let home = Home::new("burnup-table");
    let set = home.run(&[
        "config",
        "set",
        "--global",
        "usage.budget.monthlyUsd",
        "100",
    ]);
    assert!(
        set.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&set.stderr)
    );

    // Act: stdout is a pipe here, and the colors are off.
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["usage", "--summary"])
        .env("HOME", &home.0)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .output()
        .unwrap();

    // Assert
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    let table: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.contains("burn-up, cumulative spend this period"))
        .collect();
    assert!(!table.is_empty(), "{text}");
    assert!(
        table
            .iter()
            .any(|l| l.contains("date") && l.contains("cumulative")),
        "{text}"
    );
    assert!(table.iter().all(|l| l.is_ascii()), "{text}");
    assert!(!text.contains('\u{1b}'), "{text}");
}

#[test]
fn a_summary_without_a_monthly_budget_has_no_burn_up_table() {
    // Arrange
    let home = Home::new("burnup-none");

    // Act
    let out = home.run(&["usage", "--summary"]);

    // Assert
    assert!(out.status.success());
    assert!(!stdout(&out).contains("burn-up"), "{}", stdout(&out));
}

#[test]
fn a_summary_for_the_work_account_has_its_exact_message_count_and_cost() {
    // Arrange
    let home = Home::with_two_accounts();

    // Act
    let out = home.run(&["usage", "--summary", "--account", "dev@example.com"]);

    // Assert
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    let mut lines = text.lines();
    assert!(
        lines
            .next()
            .unwrap()
            .starts_with("Usage: 3 messages, $0.0973 estimated cost"),
        "{text}"
    );
    assert_eq!(lines.next(), Some("Account: dev@example.com"));
    assert!(!text.contains("token:abcd1234"), "{text}");
}

#[test]
fn json_for_a_token_account_has_its_exact_token_totals() {
    // Arrange
    let home = Home::with_two_accounts();

    // Act
    let out = home.run(&[
        "usage",
        "--json",
        "--account",
        "token:abcd1234",
        "--range",
        "all",
    ]);

    // Assert
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["account"], "token:abcd1234");
    assert_eq!(v["totals"]["messages"], 1);
    assert_eq!(v["totals"]["input_tokens"], 100);
    assert_eq!(v["totals"]["output_tokens"], 200);
    assert_eq!(v["totals"]["cost_usd"], 0.0022);
}

#[test]
fn an_unknown_account_exits_one_and_points_to_the_account_list() {
    // Arrange
    let home = Home::with_two_accounts();

    // Act
    let out = home.run(&["usage", "--summary", "--account", "nobody"]);

    // Assert
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("playbook usage account list"), "{err}");
}
