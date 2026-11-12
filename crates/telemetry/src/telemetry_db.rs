// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Storage for OpenTelemetry usage rows. These tables sit next to the
//! transcript tables and never touch them: the transcript ingest, its tables
//! and its totals are unchanged. Reverting the feature means dropping the
//! tables (`migrate_down`), which loses no transcript data.
//!
//! Rows hold only numbers and short names (session id, request id, model,
//! effort, agent name). Prompts, tool content, paths, emails and user ids are
//! never stored; the receiver strips them before a row reaches this module.

use crate::machine_rows::LOCAL_MACHINE_SQL;
use playbook_store::types::ToSql;
use playbook_store::{params, params_from_iter, Connection, OptionalExtension};

/// The one account clause. Telemetry rows have no account of their own, so a
/// row belongs to every account that has a usage event in its session.
const ACCOUNT_SESSIONS_SQL: &str =
    "AND {col} IN (SELECT session_id FROM usage_events WHERE account = ?{n})";

/// Which rows a read covers: one machine and one account, each optional.
#[derive(Default, Clone, Copy)]
pub struct RowScope<'a> {
    pub machine: Option<&'a str>,
    pub account: Option<&'a str>,
}

impl<'a> RowScope<'a> {
    pub fn machine(machine: Option<&'a str>) -> Self {
        RowScope {
            machine,
            account: None,
        }
    }

    /// An account pick needs `usage_events` to name the account's sessions.
    fn without_events(&self, conn: &Connection) -> Result<bool, String> {
        if self.account.is_none() {
            return Ok(false);
        }
        conn.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'usage_events')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("failed to read telemetry rows: {e}"))
    }

    /// The account clause on `column` bound at `?n`, empty without an account.
    fn sessions_clause(&self, column: &str, n: usize) -> String {
        match self.account {
            Some(_) => ACCOUNT_SESSIONS_SQL
                .replace("{col}", column)
                .replace("{n}", &n.to_string()),
            None => String::new(),
        }
    }

    /// The account value to bind after the other arguments, if one is set.
    fn arg(&self) -> Option<&dyn ToSql> {
        self.account.as_ref().map(|a| a as &dyn ToSql)
    }
}

pub(super) const UP_SQL: &str = "
CREATE TABLE IF NOT EXISTS telemetry_requests (
    session_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    model TEXT NOT NULL,
    effort TEXT NOT NULL DEFAULT '',
    agent_name TEXT NOT NULL DEFAULT '',
    skill_name TEXT NOT NULL DEFAULT '',
    duration_ms INTEGER NOT NULL DEFAULT 0,
    input_tokens INTEGER NOT NULL CHECK(input_tokens >= 0),
    output_tokens INTEGER NOT NULL CHECK(output_tokens >= 0),
    cache_creation_tokens INTEGER NOT NULL CHECK(cache_creation_tokens >= 0),
    cache_read_tokens INTEGER NOT NULL CHECK(cache_read_tokens >= 0),
    cost_usd REAL NOT NULL CHECK(cost_usd >= 0),
    PRIMARY KEY (session_id, request_id)
);
CREATE INDEX IF NOT EXISTS telemetry_requests_by_time
    ON telemetry_requests (timestamp, session_id, request_id);
CREATE TABLE IF NOT EXISTS session_sources (
    session_id TEXT PRIMARY KEY,
    source TEXT NOT NULL CHECK(source IN ('telemetry','transcript')),
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS session_reconcile (
    session_id TEXT PRIMARY KEY,
    state TEXT NOT NULL CHECK(state IN ('complete','partial','disagree')),
    telemetry_tokens INTEGER NOT NULL,
    transcript_tokens INTEGER NOT NULL,
    telemetry_cost REAL NOT NULL,
    transcript_cost REAL NOT NULL,
    note TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS telemetry_tools (
    session_id TEXT NOT NULL,
    tool_use_id TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    tool_name TEXT NOT NULL,
    success INTEGER NOT NULL CHECK(success IN (0, 1)),
    duration_ms INTEGER NOT NULL CHECK(duration_ms >= 0),
    PRIMARY KEY (session_id, tool_use_id)
);
CREATE INDEX IF NOT EXISTS telemetry_tools_by_time ON telemetry_tools (timestamp);
CREATE TABLE IF NOT EXISTS telemetry_errors (
    session_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    timestamp INTEGER NOT NULL,
    model TEXT NOT NULL,
    status_code INTEGER NOT NULL CHECK(status_code >= 0),
    attempt INTEGER NOT NULL CHECK(attempt >= 0),
    duration_ms INTEGER NOT NULL CHECK(duration_ms >= 0),
    PRIMARY KEY (session_id, sequence)
);
CREATE INDEX IF NOT EXISTS telemetry_errors_by_time ON telemetry_errors (timestamp);
CREATE TABLE IF NOT EXISTS telemetry_metrics (
    session_id TEXT NOT NULL,
    metric TEXT NOT NULL,
    kind TEXT NOT NULL,
    start_ns INTEGER NOT NULL,
    end_ns INTEGER NOT NULL,
    timestamp INTEGER NOT NULL,
    value REAL NOT NULL CHECK(value >= 0),
    PRIMARY KEY (session_id, metric, kind, start_ns, end_ns)
);
CREATE INDEX IF NOT EXISTS telemetry_metrics_by_time ON telemetry_metrics (timestamp);";

const DOWN_SQL: &str = "
DROP INDEX IF EXISTS telemetry_requests_by_time;
DROP TABLE IF EXISTS telemetry_requests;
DROP TABLE IF EXISTS session_sources;
DROP TABLE IF EXISTS session_reconcile;
DROP INDEX IF EXISTS telemetry_tools_by_time;
DROP TABLE IF EXISTS telemetry_tools;
DROP INDEX IF EXISTS telemetry_errors_by_time;
DROP TABLE IF EXISTS telemetry_errors;
DROP INDEX IF EXISTS telemetry_metrics_by_time;
DROP TABLE IF EXISTS telemetry_metrics;";

/// Where a session's numbers come from. A session with no recorded source is
/// read from transcripts, so history before telemetry needs no backfill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Telemetry,
    Transcript,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Telemetry => "telemetry",
            Source::Transcript => "transcript",
        }
    }

    fn parse(text: &str) -> Source {
        if text == "telemetry" {
            Source::Telemetry
        } else {
            Source::Transcript
        }
    }
}

/// One API request as reported by telemetry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TelemetryRequest {
    pub session_id: String,
    pub request_id: String,
    pub timestamp: i64,
    pub model: String,
    pub effort: String,
    pub agent_name: String,
    /// The skill active for the request, empty when none.
    pub skill_name: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
    /// How long the API call took, in milliseconds.
    pub duration_ms: u64,
}

/// One tool call as reported by telemetry. Only the tool name, the outcome
/// and the time are kept, never the input or the result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TelemetryTool {
    pub session_id: String,
    pub tool_use_id: String,
    pub timestamp: i64,
    pub tool_name: String,
    pub success: bool,
    pub duration_ms: u64,
}

/// One failed API attempt. The error text is never read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TelemetryError {
    pub session_id: String,
    pub sequence: u64,
    pub timestamp: i64,
    pub model: String,
    pub status_code: u64,
    /// 1 for the first try; 2 or more means a retry was already made.
    pub attempt: u64,
    pub duration_ms: u64,
}

/// One delta data point of a counter metric. The key is the session, the
/// metric, its kind and the exact interval, so a redelivered point is a no-op.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TelemetryMetric {
    pub session_id: String,
    /// `active_time` (seconds), `lines_of_code`, `commits` or `pull_requests`.
    pub metric: String,
    /// The `type` attribute (`user`, `cli`, `added`, `removed`), or empty.
    pub kind: String,
    pub start_ns: i64,
    pub end_ns: i64,
    pub timestamp: i64,
    pub value: f64,
}

/// Creates the telemetry tables. Safe to run any number of times.
pub fn migrate_up(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(UP_SQL)
        .map_err(|e| format!("failed to create telemetry tables: {e}"))?;
    ensure_columns(conn)?;
    crate::machine_rows::ensure_machine_column(conn)
}

/// Tables made before a column existed lack it.
pub(super) fn ensure_columns(conn: &Connection) -> Result<(), String> {
    ensure_column(conn, "skill_name", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "duration_ms", "INTEGER NOT NULL DEFAULT 0")
}

fn ensure_column(conn: &Connection, column: &str, ddl: &str) -> Result<(), String> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(telemetry_requests)")
        .map_err(|e| format!("failed to inspect telemetry tables: {e}"))?;
    let has = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| format!("failed to inspect telemetry tables: {e}"))?
        .any(|c| c.is_ok_and(|n| n == column));
    if has {
        return Ok(());
    }
    conn.execute_batch(&format!(
        "ALTER TABLE telemetry_requests ADD COLUMN {column} {ddl}"
    ))
    .map_err(|e| format!("failed to add the {column} column: {e}"))
}

/// Drops the telemetry tables and nothing else.
pub fn migrate_down(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(DOWN_SQL)
        .map_err(|e| format!("failed to drop telemetry tables: {e}"))
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Stores a request. The pair (session id, request id) is the key, so a
/// redelivered batch is a no-op. Returns 1 when a row was written, else 0.
pub fn insert_request(conn: &Connection, r: &TelemetryRequest) -> Result<usize, String> {
    let cost = if r.cost_usd.is_finite() && r.cost_usd >= 0.0 {
        r.cost_usd
    } else {
        0.0
    };
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO telemetry_requests (session_id, request_id, timestamp, model,
            effort, agent_name, skill_name, input_tokens, output_tokens,
            cache_creation_tokens, cache_read_tokens, cost_usd, duration_ms, machine)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, {LOCAL_MACHINE_SQL})"
        ),
        params![
            r.session_id,
            r.request_id,
            r.timestamp,
            r.model,
            r.effort,
            r.agent_name,
            r.skill_name,
            to_i64(r.input_tokens),
            to_i64(r.output_tokens),
            to_i64(r.cache_creation_tokens),
            to_i64(r.cache_read_tokens),
            cost,
            to_i64(r.duration_ms)
        ],
    )
    .map_err(|e| format!("failed to insert telemetry request {}: {e}", r.request_id))
}

/// Stores a tool call. Keyed by session and tool use id, so a redelivered
/// batch is a no-op.
pub fn insert_tool(conn: &Connection, t: &TelemetryTool) -> Result<usize, String> {
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO telemetry_tools
            (session_id, tool_use_id, timestamp, tool_name, success, duration_ms, machine)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, {LOCAL_MACHINE_SQL})"
        ),
        params![
            t.session_id,
            t.tool_use_id,
            t.timestamp,
            t.tool_name,
            i64::from(t.success),
            to_i64(t.duration_ms)
        ],
    )
    .map_err(|e| format!("failed to insert telemetry tool {}: {e}", t.tool_use_id))
}

/// Stores a failed API attempt. Keyed by session and event sequence.
pub fn insert_metric(conn: &Connection, m: &TelemetryMetric) -> Result<usize, String> {
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO telemetry_metrics
         (session_id, metric, kind, start_ns, end_ns, timestamp, value, machine)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, {LOCAL_MACHINE_SQL})"
        ),
        params![
            m.session_id,
            m.metric,
            m.kind,
            m.start_ns,
            m.end_ns,
            m.timestamp,
            if m.value.is_finite() && m.value >= 0.0 {
                m.value
            } else {
                0.0
            }
        ],
    )
    .map_err(|e| format!("failed to store a telemetry metric: {e}"))
}

pub fn insert_error(conn: &Connection, e: &TelemetryError) -> Result<usize, String> {
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO telemetry_errors
            (session_id, sequence, timestamp, model, status_code, attempt, duration_ms, machine)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, {LOCAL_MACHINE_SQL})"
        ),
        params![
            e.session_id,
            to_i64(e.sequence),
            e.timestamp,
            e.model,
            to_i64(e.status_code),
            to_i64(e.attempt),
            to_i64(e.duration_ms)
        ],
    )
    .map_err(|err| format!("failed to insert telemetry error: {err}"))
}

/// When Claude Code logged an HTTP 429 in `[since, until)`, oldest first. One
/// entry per `(session, sequence)`, the key of the table, so one failed attempt
/// counts once however often its row is sent again.
pub fn rate_limited_at(conn: &Connection, since: i64, until: i64) -> Result<Vec<i64>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT timestamp FROM telemetry_errors
             WHERE status_code = 429 AND timestamp >= ?1 AND timestamp < ?2
             ORDER BY timestamp",
        )
        .map_err(|e| format!("failed to read rate limit errors: {e}"))?;
    let rows = stmt
        .query_map(params![since, until], |r| r.get::<_, i64>(0))
        .map_err(|e| format!("failed to read rate limit errors: {e}"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| format!("failed to read rate limit errors: {e}"))
}

/// Tool calls of one tool in a window. `calls` counts every call, live or
/// from a transcript. A transcript call has no outcome and no duration, so
/// `failures` and `total_ms` cover only calls that have one (`outcomes` and
/// `timed` count them) and nothing is guessed for the rest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolStat {
    pub name: String,
    pub calls: u64,
    /// Calls known only from a transcript: counted, but no outcome or time.
    pub transcript_calls: u64,
    pub failures: u64,
    /// Calls with a known outcome.
    pub outcomes: u64,
    /// Calls with a known duration.
    pub timed: u64,
    pub total_ms: u64,
    pub max_ms: u64,
}

impl ToolStat {
    /// Failures over calls with a known outcome, 0 to 1.
    pub fn error_rate(&self) -> f64 {
        ratio(self.failures, self.outcomes)
    }

    /// Average over calls with a known duration, 0 when there is none.
    pub fn avg_ms(&self) -> f64 {
        if self.timed == 0 {
            0.0
        } else {
            self.total_ms as f64 / self.timed as f64
        }
    }

    pub fn has_outcome(&self) -> bool {
        self.outcomes > 0
    }

    pub fn has_duration(&self) -> bool {
        self.timed > 0
    }
}

/// The tools used in `[since, until)`, busiest first, then by name, from one
/// merged set of calls. Per tool use id the live row wins. A transcript call
/// (`tool_invocation_events`, shown as the `Agent` or `Skill` tool) counts only
/// when the call has no live row at all, so each call counts once whatever
/// the agent name says. The same rule written as a view:
/// `live UNION ALL (transcript events NOT IN live)`.
pub fn tool_stats(
    conn: &Connection,
    since: i64,
    until: i64,
    scope: RowScope<'_>,
) -> Result<Vec<ToolStat>, String> {
    if scope.without_events(conn)? {
        return Ok(Vec::new());
    }
    let machine = scope.machine;
    let transcript = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'tool_invocation_events')",
            [],
            |r| r.get::<_, bool>(0),
        )
        .map_err(|e| format!("failed to read telemetry tools: {e}"))?;
    let transcript_account = if scope.account.is_some() {
        "AND e.account = ?4"
    } else {
        ""
    };
    let live_account = scope.sessions_clause("session_id", 4);
    let from_transcripts = if transcript {
        format!(
            "UNION ALL
         SELECT CASE e.kind WHEN 'agent' THEN 'Agent' ELSE 'Skill' END, NULL, NULL, 1
         FROM tool_invocation_events e
         WHERE e.timestamp >= ?1 AND e.timestamp < ?2 AND (?3 IS NULL OR e.machine = ?3)
           {transcript_account}
           AND NOT EXISTS (SELECT 1 FROM telemetry_tools t
                           WHERE t.session_id = e.session_id AND t.tool_use_id = e.event_id)"
        )
    } else {
        String::new()
    };
    let sql = format!(
        "WITH calls(n, ok, ms, tr) AS (
            SELECT tool_name, success, duration_ms, 0 FROM telemetry_tools
            WHERE timestamp >= ?1 AND timestamp < ?2 AND (?3 IS NULL OR machine = ?3)
            {live_account}
            {from_transcripts})
         SELECT CASE WHEN n = '' THEN 'unknown' ELSE n END AS name,
                COUNT(*), SUM(tr), COALESCE(SUM(1 - ok), 0), COUNT(ok), COUNT(ms),
                COALESCE(SUM(ms), 0), COALESCE(MAX(ms), 0)
         FROM calls GROUP BY name ORDER BY COUNT(*) DESC, name"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("failed to read telemetry tools: {e}"))?;
    let mut args: Vec<&dyn ToSql> = vec![&since, &until, &machine];
    args.extend(scope.arg());
    let rows = stmt
        .query_map(params_from_iter(args), |r| {
            let n = |i: usize| -> playbook_store::Result<u64> {
                Ok(u64::try_from(r.get::<_, i64>(i)?).unwrap_or(0))
            };
            Ok(ToolStat {
                name: r.get(0)?,
                calls: n(1)?,
                transcript_calls: n(2)?,
                failures: n(3)?,
                outcomes: n(4)?,
                timed: n(5)?,
                total_ms: n(6)?,
                max_ms: n(7)?,
            })
        })
        .map_err(|e| format!("failed to read telemetry tools: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to read telemetry tools: {e}"))
}

/// The health figures of a window: API calls and their failures, tool calls,
/// and the time spent waiting on the API and on tools.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Health {
    pub requests: u64,
    pub api_ms: u64,
    pub errors: u64,
    /// Failed attempts that came after at least one earlier try.
    pub retried: u64,
    pub tool_calls: u64,
    pub tool_failures: u64,
    pub tool_ms: u64,
    /// Claude Code's own `active_time` metric, user and cli, in milliseconds.
    pub active_ms: u64,
    pub lines_changed: u64,
    pub commits: u64,
    pub pull_requests: u64,
}

impl Health {
    /// Whether the window has any telemetry row of any kind.
    pub fn is_empty(&self) -> bool {
        self.requests == 0
            && self.errors == 0
            && self.tool_calls == 0
            && self.active_ms == 0
            && self.lines_changed == 0
            && self.commits == 0
            && self.pull_requests == 0
    }

    /// Time waiting on the API or on tools. This is a floor for the time
    /// Claude Code was working, not its own active time metric.
    pub fn busy_ms(&self) -> u64 {
        self.api_ms + self.tool_ms
    }

    /// Failed attempts over all attempts (requests that finished plus
    /// failures), 0 to 1.
    pub fn api_error_rate(&self) -> f64 {
        ratio(self.errors, self.requests + self.errors)
    }

    pub fn tool_error_rate(&self) -> f64 {
        ratio(self.tool_failures, self.tool_calls)
    }
}

fn ratio(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// The figures of `[since, until)`.
pub fn health(
    conn: &Connection,
    since: i64,
    until: i64,
    scope: RowScope<'_>,
) -> Result<Health, String> {
    if scope.without_events(conn)? {
        return Ok(Health::default());
    }
    let machine = scope.machine;
    let one = |sql: &str| -> Result<Vec<u64>, String> {
        let mut args: Vec<&dyn ToSql> = vec![&since, &until, &machine];
        args.extend(scope.arg());
        conn.query_row(sql, params_from_iter(args), |r| {
            (0..r.as_ref().column_count())
                .map(|i| Ok(u64::try_from(r.get::<_, i64>(i)?).unwrap_or(0)))
                .collect()
        })
        .map_err(|e| format!("failed to read telemetry health: {e}"))
    };
    let account = scope.sessions_clause("session_id", 4);
    let req = one(&format!(
        "SELECT COUNT(*), COALESCE(SUM(duration_ms), 0) FROM telemetry_requests
                   WHERE timestamp >= ?1 AND timestamp < ?2 AND (?3 IS NULL OR machine = ?3)
                   {account}"
    ))?;
    let err = one(&format!(
        "SELECT COUNT(*), COALESCE(SUM(attempt > 1), 0) FROM telemetry_errors
                   WHERE timestamp >= ?1 AND timestamp < ?2 AND (?3 IS NULL OR machine = ?3)
                   {account}"
    ))?;
    let tool = one(&format!(
        "SELECT COUNT(*), COALESCE(SUM(1 - success), 0), COALESCE(SUM(duration_ms), 0)
                    FROM telemetry_tools
                    WHERE timestamp >= ?1 AND timestamp < ?2 AND (?3 IS NULL OR machine = ?3)
                    {account}"
    ))?;
    Ok(Health {
        requests: req[0],
        api_ms: req[1],
        errors: err[0],
        retried: err[1],
        tool_calls: tool[0],
        tool_failures: tool[1],
        tool_ms: tool[2],
        active_ms: metric_sum(conn, "active_time", since, until, scope)?.saturating_mul(1000),
        lines_changed: metric_sum(conn, "lines_of_code", since, until, scope)?,
        commits: metric_sum(conn, "commits", since, until, scope)?,
        pull_requests: metric_sum(conn, "pull_requests", since, until, scope)?,
    })
}

/// Sum of one metric in `[since, until)`, over all kinds.
fn metric_sum(
    conn: &Connection,
    metric: &str,
    since: i64,
    until: i64,
    scope: RowScope<'_>,
) -> Result<u64, String> {
    let machine = scope.machine;
    let sql = format!(
        "SELECT COALESCE(SUM(value), 0) FROM telemetry_metrics
         WHERE metric = ?1 AND timestamp >= ?2 AND timestamp < ?3
           AND (?4 IS NULL OR machine = ?4) {}",
        scope.sessions_clause("session_id", 5)
    );
    let mut args: Vec<&dyn ToSql> = vec![&metric, &since, &until, &machine];
    args.extend(scope.arg());
    let v: f64 = conn
        .query_row(&sql, params_from_iter(args), |r| r.get(0))
        .map_err(|e| format!("failed to read telemetry metrics: {e}"))?;
    Ok(v.round() as u64)
}

/// How many days of telemetry rows are kept, and the most rows kept per table.
pub const RETENTION_DAYS: u64 = 90;
pub const MAX_ROWS: u64 = 500_000;

const PRUNED_TABLES: [&str; 4] = [
    "telemetry_requests",
    "telemetry_tools",
    "telemetry_errors",
    "telemetry_metrics",
];

/// How long and how much telemetry is kept. 0 means no limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Retention {
    /// Rows older than this many days are deleted.
    pub days: u64,
    /// At most this many rows are kept per table, the newest ones.
    pub max_rows: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            days: RETENTION_DAYS,
            max_rows: MAX_ROWS,
        }
    }
}

/// Rows removed (or, in a dry run, that would be) per table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Pruned {
    pub requests: u64,
    pub tools: u64,
    pub errors: u64,
    pub metrics: u64,
}

impl Pruned {
    pub fn total(&self) -> u64 {
        self.requests + self.tools + self.errors + self.metrics
    }
}

/// Deletes telemetry rows past the limits: first those older than
/// `retention.days`, then the oldest rows beyond `retention.max_rows` in each
/// table. Transcript tables are never touched, and totals come from them, so
/// no total changes. With `dry_run` nothing is deleted and the counts say what
/// would be.
pub fn prune(
    conn: &Connection,
    now: i64,
    retention: Retention,
    dry_run: bool,
) -> Result<Pruned, String> {
    let cutoff = (retention.days > 0).then(|| now - to_i64(retention.days).saturating_mul(86_400));
    let mut out = [0u64; 4];
    for (slot, table) in out.iter_mut().zip(PRUNED_TABLES) {
        let fail = |e: playbook_store::Error| format!("failed to prune {table}: {e}");
        let total: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .map_err(fail)?;
        let old: i64 = match cutoff {
            Some(c) => conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE timestamp < ?1"),
                    params![c],
                    |r| r.get(0),
                )
                .map_err(fail)?,
            None => 0,
        };
        let excess = if retention.max_rows > 0 {
            (total - old - to_i64(retention.max_rows)).max(0)
        } else {
            0
        };
        if !dry_run {
            if let Some(c) = cutoff {
                conn.execute(
                    &format!("DELETE FROM {table} WHERE timestamp < ?1"),
                    params![c],
                )
                .map_err(fail)?;
            }
            if excess > 0 {
                conn.execute(
                    &format!(
                        "DELETE FROM {table} WHERE rowid IN
                         (SELECT rowid FROM {table} ORDER BY timestamp, rowid LIMIT ?1)"
                    ),
                    params![excess],
                )
                .map_err(fail)?;
            }
        }
        *slot = u64::try_from(old + excess).unwrap_or(0);
    }
    Ok(Pruned {
        requests: out[0],
        tools: out[1],
        errors: out[2],
        metrics: out[3],
    })
}

pub fn count_requests(conn: &Connection) -> Result<i64, String> {
    conn.query_row("SELECT COUNT(*) FROM telemetry_requests", [], |r| r.get(0))
        .map_err(|e| format!("failed to count telemetry requests: {e}"))
}

/// Records which source a session uses.
pub fn set_session_source(
    conn: &Connection,
    session_id: &str,
    source: Source,
    now: i64,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO session_sources (session_id, source, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(session_id) DO UPDATE SET source = excluded.source,
            updated_at = excluded.updated_at",
        params![session_id, source.as_str(), now],
    )
    .map(|_| ())
    .map_err(|e| format!("failed to set the source of {session_id}: {e}"))
}

/// The source of a session; transcripts when none was recorded.
pub fn session_source(conn: &Connection, session_id: &str) -> Result<Source, String> {
    conn.query_row(
        "SELECT source FROM session_sources WHERE session_id = ?1",
        [session_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map(|found| found.map_or(Source::Transcript, |s| Source::parse(&s)))
    .map_err(|e| format!("failed to read the source of {session_id}: {e}"))
}

/// Telemetry requests in `[since, until)` grouped by agent name, sorted by
/// name. The main session (no agent name) is `main`.
pub fn agent_groups(
    conn: &Connection,
    since: i64,
    until: i64,
    scope: RowScope<'_>,
) -> Result<Vec<super::aggregate::Group>, String> {
    groups(conn, "agent_name", "main", since, until, scope)
}

/// Telemetry requests in `[since, until)` grouped by the skill that was
/// active, sorted by name. Requests made with no skill active are left out.
pub fn skill_groups(
    conn: &Connection,
    since: i64,
    until: i64,
    scope: RowScope<'_>,
) -> Result<Vec<super::aggregate::Group>, String> {
    groups(conn, "skill_name", "", since, until, scope)
}

/// Agent or skill invocations in `[since, until)` counted from transcripts,
/// sorted by name. `messages` holds the count; tokens and cost stay zero.
pub fn invocation_groups(
    conn: &Connection,
    kind: crate::ToolKind,
    since: i64,
    until: i64,
    scope: RowScope<'_>,
) -> Result<Vec<super::aggregate::Group>, String> {
    if scope.without_events(conn)? {
        return Ok(Vec::new());
    }
    let machine = scope.machine;
    let account = if scope.account.is_some() {
        "AND account = ?5"
    } else {
        ""
    };
    let mut stmt = conn
        .prepare(&format!(
            "SELECT name, COUNT(*) FROM tool_invocation_events
             WHERE kind = ?1 AND timestamp >= ?2 AND timestamp < ?3
               AND (?4 IS NULL OR machine = ?4) {account}
             GROUP BY name ORDER BY name"
        ))
        .map_err(|e| e.to_string())?;
    let kind = kind.as_str();
    let mut args: Vec<&dyn ToSql> = vec![&kind, &since, &until, &machine];
    args.extend(scope.arg());
    let rows = stmt
        .query_map(params_from_iter(args), |r| {
            Ok(super::aggregate::Group {
                key: r.get(0)?,
                messages: r.get::<_, i64>(1)?.max(0) as u64,
                ..Default::default()
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// `column` is one of two fixed names, never user input.
fn groups(
    conn: &Connection,
    column: &str,
    empty_as: &str,
    since: i64,
    until: i64,
    scope: RowScope<'_>,
) -> Result<Vec<super::aggregate::Group>, String> {
    if scope.without_events(conn)? {
        return Ok(Vec::new());
    }
    let machine = scope.machine;
    let filter = if empty_as.is_empty() {
        format!("AND {column} <> ''")
    } else {
        String::new()
    };
    let account = scope.sessions_clause("session_id", 5);
    let sql = format!(
        "SELECT CASE WHEN {column} = '' THEN ?3 ELSE {column} END AS k,
                COUNT(*), SUM(input_tokens), SUM(output_tokens),
                SUM(cache_creation_tokens), SUM(cache_read_tokens), SUM(cost_usd)
         FROM telemetry_requests
         WHERE timestamp >= ?1 AND timestamp < ?2 AND (?4 IS NULL OR machine = ?4) {filter}
           {account}
         GROUP BY k ORDER BY k"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("failed to read telemetry groups: {e}"))?;
    let mut args: Vec<&dyn ToSql> = vec![&since, &until, &empty_as, &machine];
    args.extend(scope.arg());
    let rows = stmt
        .query_map(params_from_iter(args), |r| {
            let n = |i: usize| -> playbook_store::Result<u64> {
                Ok(u64::try_from(r.get::<_, i64>(i)?).unwrap_or(0))
            };
            Ok(super::aggregate::Group {
                key: r.get(0)?,
                messages: n(1)?,
                input_tokens: n(2)?,
                output_tokens: n(3)?,
                cache_creation_tokens: n(4)?,
                cache_read_tokens: n(5)?,
                cost_usd: r.get(6)?,
                unpriced: 0,
            })
        })
        .map_err(|e| format!("failed to read telemetry groups: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to read telemetry groups: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{count_usage_events, open_db, upsert_usage_event};
    use crate::UsageEvent;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate_up(&conn).unwrap();
        conn
    }

    fn request(id: &str) -> TelemetryRequest {
        TelemetryRequest {
            session_id: "s1".into(),
            request_id: id.into(),
            timestamp: 100,
            model: "opus".into(),
            agent_name: "reviewer".into(),
            input_tokens: 10,
            output_tokens: 5,
            cost_usd: 0.25,
            ..TelemetryRequest::default()
        }
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [name],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn inserting_the_same_request_twice_stores_one_row() {
        let conn = mem();
        assert_eq!(insert_request(&conn, &request("r1")).unwrap(), 1);
        assert_eq!(insert_request(&conn, &request("r1")).unwrap(), 0);
        assert_eq!(count_requests(&conn).unwrap(), 1);
    }

    #[test]
    fn the_same_request_id_in_two_sessions_is_two_rows() {
        let conn = mem();
        let mut other = request("r1");
        other.session_id = "s2".into();
        insert_request(&conn, &request("r1")).unwrap();
        insert_request(&conn, &other).unwrap();
        assert_eq!(count_requests(&conn).unwrap(), 2);
    }

    #[test]
    fn a_bad_cost_is_stored_as_zero() {
        let conn = mem();
        let mut r = request("r1");
        r.cost_usd = f64::NAN;
        insert_request(&conn, &r).unwrap();
        let cost: f64 = conn
            .query_row("SELECT cost_usd FROM telemetry_requests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn a_session_without_a_record_reads_from_transcripts() {
        let conn = mem();
        assert_eq!(session_source(&conn, "none").unwrap(), Source::Transcript);
    }

    #[test]
    fn the_session_source_can_change_and_is_kept_per_session() {
        let conn = mem();
        set_session_source(&conn, "s1", Source::Telemetry, 1).unwrap();
        set_session_source(&conn, "s2", Source::Transcript, 1).unwrap();
        assert_eq!(session_source(&conn, "s1").unwrap(), Source::Telemetry);
        set_session_source(&conn, "s1", Source::Transcript, 2).unwrap();
        assert_eq!(session_source(&conn, "s1").unwrap(), Source::Transcript);
        assert_eq!(session_source(&conn, "s2").unwrap(), Source::Transcript);
    }

    #[test]
    fn the_source_column_rejects_an_unknown_value() {
        let conn = mem();
        let err = conn.execute("INSERT INTO session_sources VALUES ('s', 'guess', 1)", []);
        assert!(err.is_err());
    }

    #[test]
    fn migrate_up_is_idempotent_and_down_removes_only_telemetry_tables() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE usage_events (event_id TEXT);")
            .unwrap();
        migrate_up(&conn).unwrap();
        migrate_up(&conn).unwrap();
        insert_request(&conn, &request("r1")).unwrap();
        assert!(table_exists(&conn, "telemetry_requests"));

        migrate_down(&conn).unwrap();
        migrate_down(&conn).unwrap();

        assert!(!table_exists(&conn, "telemetry_requests"));
        assert!(!table_exists(&conn, "session_sources"));
        assert!(!table_exists(&conn, "session_reconcile"));
        assert!(!table_exists(&conn, "telemetry_tools"));
        assert!(!table_exists(&conn, "telemetry_errors"));
        assert!(table_exists(&conn, "usage_events"));
        migrate_up(&conn).unwrap();
        assert_eq!(count_requests(&conn).unwrap(), 0);
    }

    #[test]
    fn opening_the_usage_db_creates_the_tables_and_leaves_transcript_ingest_alone() {
        let dir = crate::util::scratch_dir("telemetry-open");
        let conn = open_db(&dir.join("usage.db")).unwrap();
        assert!(table_exists(&conn, "telemetry_requests"));
        let event = UsageEvent {
            event_id: "m1".into(),
            session_id: "s1".into(),
            model: "opus".into(),
            input_tokens: 3,
            ..UsageEvent::default()
        };
        upsert_usage_event(&conn, &event).unwrap();
        upsert_usage_event(&conn, &event).unwrap();
        assert_eq!(count_usage_events(&conn).unwrap(), 1);
        assert_eq!(count_requests(&conn).unwrap(), 0);
        assert_eq!(session_source(&conn, "s1").unwrap(), Source::Transcript);
    }
    #[test]
    fn agent_groups_sum_cost_per_agent_inside_the_window_and_name_the_main_session() {
        let conn = mem();
        let mut main = request("r1");
        main.agent_name = String::new();
        main.cost_usd = 1.0;
        let mut sub = request("r2");
        sub.agent_name = "reviewer".into();
        sub.cost_usd = 0.5;
        let mut sub2 = request("r3");
        sub2.agent_name = "reviewer".into();
        sub2.cost_usd = 0.25;
        let mut outside = request("r4");
        outside.timestamp = 10_000;
        for r in [&main, &sub, &sub2, &outside] {
            insert_request(&conn, r).unwrap();
        }
        let groups = agent_groups(&conn, 0, 1_000, RowScope::default()).unwrap();
        let names: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(names, ["main", "reviewer"]);
        assert!((groups[1].cost_usd - 0.75).abs() < 1e-9);
        assert_eq!(groups[1].messages, 2);
        assert!(agent_groups(&conn, 5_000, 6_000, RowScope::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn skill_groups_sum_cost_per_skill_and_skip_requests_without_one() {
        let conn = mem();
        let mut a = request("r1");
        a.skill_name = "review".into();
        a.cost_usd = 0.5;
        let mut b = request("r2");
        b.skill_name = "review".into();
        b.cost_usd = 0.25;
        let mut c = request("r3");
        c.skill_name = "plan".into();
        let none = request("r4");
        for r in [&a, &b, &c, &none] {
            insert_request(&conn, r).unwrap();
        }
        let groups = skill_groups(&conn, 0, 1_000, RowScope::default()).unwrap();
        let names: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(names, ["plan", "review"]);
        assert!((groups[1].cost_usd - 0.75).abs() < 1e-9);
        assert_eq!(groups[1].messages, 2);
    }

    #[test]
    fn a_table_made_before_skills_gains_the_column_and_keeps_its_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE telemetry_requests (
                session_id TEXT NOT NULL, request_id TEXT NOT NULL, timestamp INTEGER NOT NULL,
                model TEXT NOT NULL, effort TEXT NOT NULL DEFAULT '',
                agent_name TEXT NOT NULL DEFAULT '', input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL, cache_creation_tokens INTEGER NOT NULL,
                cache_read_tokens INTEGER NOT NULL, cost_usd REAL NOT NULL,
                PRIMARY KEY (session_id, request_id));
             INSERT INTO telemetry_requests VALUES ('s','r',1,'m','','',1,1,0,0,0.5);",
        )
        .unwrap();
        migrate_up(&conn).unwrap();
        migrate_up(&conn).unwrap();
        assert_eq!(count_requests(&conn).unwrap(), 1);
        assert!(skill_groups(&conn, 0, 10, RowScope::default())
            .unwrap()
            .is_empty());
    }

    fn tool(id: &str, name: &str, ok: bool, ms: u64, ts: i64) -> TelemetryTool {
        TelemetryTool {
            session_id: "s1".into(),
            tool_use_id: id.into(),
            timestamp: ts,
            tool_name: name.into(),
            success: ok,
            duration_ms: ms,
        }
    }

    fn throttled(session: &str, seq: u64, attempt: u64, ts: i64, code: u64) -> TelemetryError {
        TelemetryError {
            session_id: session.into(),
            status_code: code,
            ..failure(seq, attempt, ts)
        }
    }

    #[test]
    fn only_a_429_inside_the_window_is_a_rate_limit_and_each_call_counts_once() {
        let conn = mem();
        let (start, end) = (1_000, 2_000);
        let rows = [
            throttled("s1", 1, 1, start - 1, 429),
            throttled("s1", 2, 1, start, 429),
            throttled("s1", 3, 1, end - 1, 429),
            throttled("s1", 4, 1, end, 429),
            // the same row sent again with another attempt is ignored, and a
            // 529 in the window is not a rate limit
            throttled("s1", 3, 2, end - 1, 429),
            // a later row of a retried call has its own sequence and time
            throttled("s1", 6, 2, start + 3, 429),
            throttled("s1", 5, 1, start + 5, 529),
            // another session may reuse a sequence number
            throttled("s2", 2, 1, start + 7, 429),
        ];
        for r in &rows {
            insert_error(&conn, r).unwrap();
        }

        let got = rate_limited_at(&conn, start, end).unwrap();

        assert_eq!(got, vec![start, start + 3, start + 7, end - 1]);
        assert!(rate_limited_at(&conn, 5_000, 6_000).unwrap().is_empty());
    }

    fn failure(seq: u64, attempt: u64, ts: i64) -> TelemetryError {
        TelemetryError {
            session_id: "s1".into(),
            sequence: seq,
            timestamp: ts,
            status_code: 529,
            attempt,
            duration_ms: 100,
            ..TelemetryError::default()
        }
    }

    #[test]
    fn tool_and_error_rows_dedupe_on_their_keys() {
        let conn = mem();
        assert_eq!(
            insert_tool(&conn, &tool("t1", "Bash", true, 5, 1)).unwrap(),
            1
        );
        assert_eq!(
            insert_tool(&conn, &tool("t1", "Bash", true, 5, 1)).unwrap(),
            0
        );
        assert_eq!(insert_error(&conn, &failure(1, 1, 1)).unwrap(), 1);
        assert_eq!(insert_error(&conn, &failure(1, 1, 1)).unwrap(), 0);
        assert_eq!(insert_error(&conn, &failure(2, 1, 1)).unwrap(), 1);
    }

    #[test]
    fn tool_stats_give_calls_failures_and_latency_busiest_first() {
        let conn = mem();
        for t in [
            tool("a", "Bash", true, 100, 10),
            tool("b", "Bash", false, 300, 20),
            tool("c", "Read", true, 10, 30),
            tool("d", "Read", true, 10, 5_000),
            tool("e", "", true, 1, 40),
        ] {
            insert_tool(&conn, &t).unwrap();
        }
        let stats = tool_stats(&conn, 0, 1_000, RowScope::default()).unwrap();
        let names: Vec<&str> = stats.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Bash", "Read", "unknown"]);
        let bash = &stats[0];
        assert_eq!((bash.calls, bash.failures, bash.max_ms), (2, 1, 300));
        assert_eq!(bash.avg_ms(), 200.0);
        assert_eq!(bash.error_rate(), 0.5);
        assert_eq!(
            stats[1].calls, 1,
            "the call outside the window is not counted"
        );
        assert!(tool_stats(&conn, 9_000, 9_100, RowScope::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn health_sums_requests_errors_retries_and_busy_time() {
        let conn = mem();
        let mut r = request("r1");
        r.duration_ms = 1_000;
        insert_request(&conn, &r).unwrap();
        insert_tool(&conn, &tool("a", "Bash", false, 500, 10)).unwrap();
        insert_error(&conn, &failure(1, 1, 10)).unwrap();
        insert_error(&conn, &failure(2, 2, 10)).unwrap();
        let h = health(&conn, 0, 1_000, RowScope::default()).unwrap();
        assert_eq!((h.requests, h.errors, h.retried), (1, 2, 1));
        assert_eq!((h.tool_calls, h.tool_failures), (1, 1));
        assert_eq!(h.busy_ms(), 1_500);
        assert!((h.api_error_rate() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(h.tool_error_rate(), 1.0);
        assert!(!h.is_empty());
        let none = health(&conn, 5_000, 6_000, RowScope::default()).unwrap();
        assert!(none.is_empty());
        assert_eq!((none.api_error_rate(), none.tool_error_rate()), (0.0, 0.0));
    }

    #[test]
    fn health_adds_active_time_lines_commits_and_pull_requests() {
        let conn = mem();
        let m = |metric: &str, kind: &str, end: i64, v: f64| TelemetryMetric {
            session_id: "s".into(),
            metric: metric.into(),
            kind: kind.into(),
            start_ns: end - 1,
            end_ns: end,
            timestamp: end,
            value: v,
        };
        for row in [
            m("active_time", "user", 10, 30.0),
            m("active_time", "cli", 10, 90.0),
            m("lines_of_code", "added", 10, 40.0),
            m("lines_of_code", "removed", 10, 10.0),
            m("commits", "", 10, 2.0),
            m("pull_requests", "", 10, 1.0),
            m("active_time", "cli", 10, 5.0),
        ] {
            insert_metric(&conn, &row).unwrap();
        }
        // The same interval, session, metric and kind is one point.
        let h = health(&conn, 0, 100, RowScope::default()).unwrap();
        assert_eq!(h.active_ms, 120_000);
        assert_eq!((h.lines_changed, h.commits, h.pull_requests), (50, 2, 1));
        assert!(!h.is_empty());
        assert!(health(&conn, 100, 200, RowScope::default())
            .unwrap()
            .is_empty());
    }

    fn fill(conn: &Connection, n: i64) {
        for i in 0..n {
            let mut r = request(&format!("r{i}"));
            r.timestamp = 1_000 + i;
            insert_request(conn, &r).unwrap();
            insert_tool(conn, &tool(&format!("t{i}"), "Bash", true, 1, 1_000 + i)).unwrap();
            insert_error(conn, &failure(i as u64, 1, 1_000 + i)).unwrap();
            let m = TelemetryMetric {
                session_id: "s".into(),
                metric: "commits".into(),
                kind: String::new(),
                start_ns: i,
                end_ns: i + 1,
                timestamp: 1_000 + i,
                value: 1.0,
            };
            insert_metric(conn, &m).unwrap();
        }
    }

    #[test]
    fn prune_drops_rows_older_than_the_retention_in_every_table() {
        let conn = mem();
        fill(&conn, 10);
        let day = 86_400;
        // Rows are at 1000..1009; keep rows newer than now - 1 day = 1005.
        let r = Retention {
            days: 1,
            max_rows: 0,
        };
        let dry = prune(&conn, 1_005 + day, r, true).unwrap();
        assert_eq!(dry.total(), 20, "{dry:?}");
        assert_eq!(
            count_requests(&conn).unwrap(),
            10,
            "a dry run deletes nothing"
        );
        let done = prune(&conn, 1_005 + day, r, false).unwrap();
        assert_eq!(done, dry);
        assert_eq!(count_requests(&conn).unwrap(), 5);
        let left: i64 = conn
            .query_row("SELECT MIN(timestamp) FROM telemetry_metrics", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(left, 1_005);
    }

    #[test]
    fn prune_keeps_the_newest_rows_up_to_the_row_limit_and_zero_means_no_limit() {
        let conn = mem();
        fill(&conn, 10);
        let r = Retention {
            days: 0,
            max_rows: 4,
        };
        let done = prune(&conn, 5_000_000, r, false).unwrap();
        assert_eq!(
            (done.requests, done.tools, done.errors, done.metrics),
            (6, 6, 6, 6)
        );
        let oldest: i64 = conn
            .query_row("SELECT MIN(timestamp) FROM telemetry_requests", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(oldest, 1_006);
        let none = Retention {
            days: 0,
            max_rows: 0,
        };
        assert_eq!(prune(&conn, 9_000_000, none, false).unwrap().total(), 0);
        assert_eq!(count_requests(&conn).unwrap(), 4);
    }

    #[test]
    fn prune_leaves_the_transcript_tables_alone() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE usage_events (event_id TEXT); INSERT INTO usage_events VALUES ('a');",
        )
        .unwrap();
        migrate_up(&conn).unwrap();
        prune(
            &conn,
            i64::MAX / 4,
            Retention {
                days: 1,
                max_rows: 1,
            },
            false,
        )
        .unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_table_from_before_slice_two_gains_the_duration_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE telemetry_requests (
                session_id TEXT NOT NULL, request_id TEXT NOT NULL, timestamp INTEGER NOT NULL,
                model TEXT NOT NULL, effort TEXT NOT NULL DEFAULT '',
                agent_name TEXT NOT NULL DEFAULT '', skill_name TEXT NOT NULL DEFAULT '',
                input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
                cache_creation_tokens INTEGER NOT NULL, cache_read_tokens INTEGER NOT NULL,
                cost_usd REAL NOT NULL, PRIMARY KEY (session_id, request_id));
             INSERT INTO telemetry_requests VALUES ('s','r',1,'m','','','',1,1,0,0,0.5);",
        )
        .unwrap();
        migrate_up(&conn).unwrap();
        assert_eq!(health(&conn, 0, 10, RowScope::default()).unwrap().api_ms, 0);
        insert_request(&conn, &request("r2")).unwrap();
    }

    #[test]
    fn invocation_groups_count_transcript_agents_inside_the_window_by_kind() {
        let dir = std::env::temp_dir().join(format!("pbtop-inv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = crate::db::open_db(&dir.join("usage.db")).unwrap();
        let tool =
            |id: &str, ts: i64, kind: crate::ToolKind, name: &str| crate::ToolInvocationEvent {
                event_id: id.into(),
                timestamp: ts,
                session_id: "s".into(),
                account: "a".into(),
                kind,
                name: name.into(),
            };
        for e in [
            tool("1", 10, crate::ToolKind::Agent, "Explore"),
            tool("2", 20, crate::ToolKind::Agent, "Explore"),
            tool("3", 30, crate::ToolKind::Agent, "reviewer"),
            tool("4", 40, crate::ToolKind::Skill, "plan"),
            tool("5", 5_000, crate::ToolKind::Agent, "late"),
        ] {
            crate::db::insert_tool_event(&conn, &e).unwrap();
        }

        let groups =
            invocation_groups(&conn, crate::ToolKind::Agent, 0, 1_000, RowScope::default())
                .unwrap();

        let got: Vec<(&str, u64)> = groups
            .iter()
            .map(|g| (g.key.as_str(), g.messages))
            .collect();
        assert_eq!(got, [("Explore", 2), ("reviewer", 1)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn with_events(events: &[(&str, &str, i64, &str)]) -> Connection {
        let conn = mem();
        conn.execute_batch(
            "CREATE TABLE tool_invocation_events (event_id TEXT PRIMARY KEY,
                timestamp INTEGER NOT NULL, session_id TEXT NOT NULL, account TEXT NOT NULL,
                kind TEXT NOT NULL, name TEXT NOT NULL,
                machine TEXT NOT NULL DEFAULT '');",
        )
        .unwrap();
        for (id, session, ts, kind) in events {
            let name = if *kind == "agent" {
                "playbook:reviewer"
            } else {
                "playbook:plan"
            };
            conn.execute(
                "INSERT INTO tool_invocation_events (event_id, timestamp, session_id, account, kind, name)
                 VALUES (?1, ?2, ?3, 'a', ?4, ?5)",
                params![id, ts, session, kind, name],
            )
            .unwrap();
        }
        conn
    }

    fn stat<'a>(stats: &'a [ToolStat], name: &str) -> &'a ToolStat {
        stats.iter().find(|s| s.name == name).unwrap()
    }

    #[test]
    fn transcript_counts_match_a_hand_counted_fixture_with_no_invented_numbers() {
        let conn = with_events(&[
            ("a1", "s1", 10, "agent"),
            ("a2", "s1", 20, "agent"),
            ("a3", "s1", 30, "agent"),
            ("k1", "s1", 40, "skill"),
            ("k2", "s1", 50, "skill"),
        ]);
        let stats = tool_stats(&conn, 0, 100, RowScope::default()).unwrap();
        let agent = stat(&stats, "Agent");
        assert_eq!((agent.calls, agent.transcript_calls), (3, 3));
        assert_eq!((agent.outcomes, agent.timed, agent.failures), (0, 0, 0));
        assert!(!agent.has_outcome() && !agent.has_duration());
        assert_eq!(stat(&stats, "Skill").calls, 2);
        assert!(tool_stats(&conn, 100, 200, RowScope::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_live_row_wins_per_tool_use_id() {
        // The transcript names the agent `playbook:reviewer`, the live data
        // may say `custom`; neither name is in the key, so one id is one call.
        let conn = with_events(&[("tu1", "s1", 10, "agent"), ("tu2", "s1", 20, "agent")]);
        insert_tool(&conn, &tool("tu1", "Agent", false, 300, 11)).unwrap();
        let stats = tool_stats(&conn, 0, 100, RowScope::default()).unwrap();
        let agent = stat(&stats, "Agent");
        assert_eq!((agent.calls, agent.transcript_calls), (2, 1));
        assert_eq!((agent.failures, agent.outcomes, agent.timed), (1, 1, 1));
        assert_eq!((agent.total_ms, agent.max_ms), (300, 300));
    }

    #[test]
    fn the_same_tool_use_id_in_another_session_is_a_different_call() {
        let conn = with_events(&[("tu1", "other", 10, "agent")]);
        insert_tool(&conn, &tool("tu1", "Agent", true, 5, 11)).unwrap();
        let stats = tool_stats(&conn, 0, 100, RowScope::default()).unwrap();
        let agent = stat(&stats, "Agent");
        assert_eq!((agent.calls, agent.transcript_calls), (2, 1));
    }

    #[test]
    fn a_live_row_outside_the_window_still_hides_its_transcript_twin() {
        let conn = with_events(&[("tu1", "s1", 10, "agent")]);
        insert_tool(&conn, &tool("tu1", "Agent", true, 5, 500)).unwrap();
        assert!(tool_stats(&conn, 0, 100, RowScope::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn duration_and_error_figures_use_only_calls_that_have_them() {
        let conn = with_events(&[("a1", "s1", 10, "agent"), ("a2", "s1", 11, "agent")]);
        insert_tool(&conn, &tool("live1", "Agent", true, 100, 12)).unwrap();
        insert_tool(&conn, &tool("live2", "Agent", false, 300, 13)).unwrap();
        let stats = tool_stats(&conn, 0, 100, RowScope::default()).unwrap();
        let agent = stat(&stats, "Agent");
        assert_eq!(agent.calls, 4);
        assert_eq!(
            agent.avg_ms(),
            200.0,
            "the two unknown durations are left out"
        );
        assert_eq!(
            agent.error_rate(),
            0.5,
            "the two unknown outcomes are left out"
        );
    }

    #[test]
    fn transcript_only_and_live_only_days_each_report_their_own_rows() {
        let day = 86_400;
        let conn = with_events(&[("old", "s1", 2 * day + 5, "skill")]);
        insert_tool(&conn, &tool("live", "Bash", true, 3, 5 * day + 5)).unwrap();
        let transcript_day = tool_stats(&conn, 2 * day, 3 * day, RowScope::default()).unwrap();
        assert_eq!(transcript_day.len(), 1);
        assert_eq!(transcript_day[0].transcript_calls, 1);
        let live_day = tool_stats(&conn, 5 * day, 6 * day, RowScope::default()).unwrap();
        assert_eq!(
            (live_day[0].name.as_str(), live_day[0].transcript_calls),
            ("Bash", 0)
        );
        let health = health(&conn, 2 * day, 3 * day, RowScope::default()).unwrap();
        assert!(health.is_empty(), "health stays live only");
    }

    fn scoped(account: Option<&str>) -> RowScope<'_> {
        RowScope {
            machine: None,
            account,
        }
    }

    /// Sessions `s-work` and `s-home` belong to one account each, `s-both`
    /// has events under both, and `s-orphan` has telemetry but no usage event.
    /// Every session has one agent request, one skill request, one live tool
    /// call, one retried error and one metric. Transcript agent calls sit in
    /// `s-work` (1), `s-both` (1 work, 2 home) and `s-orphan` (1 home).
    fn two_account_store(tag: &str) -> Connection {
        let dir = crate::util::scratch_dir(tag);
        let conn = crate::db::open_db(&dir.join("usage.db")).unwrap();
        let usage = [
            ("e1", "s-work", "work"),
            ("e2", "s-home", "home"),
            ("e3", "s-both", "work"),
            ("e4", "s-both", "home"),
        ];
        for (id, session, account) in usage {
            upsert_usage_event(
                &conn,
                &UsageEvent {
                    event_id: id.into(),
                    session_id: session.into(),
                    account: account.into(),
                    model: "opus".into(),
                    ..UsageEvent::default()
                },
            )
            .unwrap();
        }
        for session in ["s-work", "s-home", "s-both", "s-orphan"] {
            let request = TelemetryRequest {
                session_id: session.into(),
                request_id: format!("r-{session}"),
                agent_name: "reviewer".into(),
                skill_name: "plan".into(),
                ..request("x")
            };
            insert_request(&conn, &request).unwrap();
            let call = TelemetryTool {
                session_id: session.into(),
                ..tool(&format!("t-{session}"), "Bash", true, 5, 100)
            };
            insert_tool(&conn, &call).unwrap();
            let error = TelemetryError {
                session_id: session.into(),
                attempt: 2,
                ..failure(1, 2, 100)
            };
            insert_error(&conn, &error).unwrap();
            let metric = TelemetryMetric {
                session_id: session.into(),
                metric: "commits".into(),
                timestamp: 100,
                value: 1.0,
                ..TelemetryMetric::default()
            };
            insert_metric(&conn, &metric).unwrap();
        }
        for (id, session, account) in [
            ("i1", "s-work", "work"),
            ("i2", "s-both", "work"),
            ("i3", "s-both", "home"),
            ("i4", "s-both", "home"),
            ("i5", "s-orphan", "home"),
        ] {
            conn.execute(
                "INSERT INTO tool_invocation_events (event_id, timestamp, session_id, account, kind, name)
                 VALUES (?1, 100, ?2, ?3, 'agent', 'reviewer')",
                params![id, session, account],
            )
            .unwrap();
        }
        conn
    }

    /// What every scoped read returns, as comparable text.
    fn seen(conn: &Connection, scope: RowScope<'_>) -> [String; 5] {
        let keys = |g: Vec<crate::aggregate::Group>| {
            g.iter()
                .map(|g| format!("{}={}", g.key, g.messages))
                .collect::<Vec<_>>()
                .join(",")
        };
        let h = health(conn, 0, 1_000, scope).unwrap();
        let tools = tool_stats(conn, 0, 1_000, scope).unwrap();
        [
            keys(agent_groups(conn, 0, 1_000, scope).unwrap()),
            keys(skill_groups(conn, 0, 1_000, scope).unwrap()),
            // A store without usage events has no transcript table either.
            keys(
                invocation_groups(conn, crate::ToolKind::Agent, 0, 1_000, scope)
                    .unwrap_or_default(),
            ),
            format!(
                "req={} err={} tools={} commits={}",
                h.requests, h.errors, h.tool_calls, h.commits
            ),
            tools
                .iter()
                .map(|t| format!("{}={}/{}", t.name, t.calls, t.transcript_calls))
                .collect::<Vec<_>>()
                .join(","),
        ]
    }

    #[test]
    fn an_account_keeps_only_the_telemetry_of_its_sessions() {
        // Arrange
        let conn = two_account_store("scope-work");

        // Act
        let work = seen(&conn, scoped(Some("work")));

        // Assert: s-work and s-both stay, s-home and the orphan drop out.
        assert_eq!(work[0], "reviewer=2");
        assert_eq!(work[1], "plan=2");
        assert_eq!(work[3], "req=2 err=2 tools=2 commits=2");
    }

    #[test]
    fn no_account_keeps_every_row_including_a_session_without_usage_events() {
        // Arrange
        let conn = two_account_store("scope-all");

        // Act
        let all = seen(&conn, scoped(None));

        // Assert
        assert_eq!(all[0], "reviewer=4");
        assert_eq!(all[1], "plan=4");
        assert_eq!(all[2], "reviewer=5");
        assert_eq!(all[3], "req=4 err=4 tools=4 commits=4");
    }

    #[test]
    fn a_session_with_events_under_two_accounts_counts_under_each() {
        // Arrange
        let conn = two_account_store("scope-split");

        // Act
        let work = seen(&conn, scoped(Some("work")));
        let home = seen(&conn, scoped(Some("home")));

        // Assert: s-both shows in both accounts' telemetry rows.
        assert_eq!(work[0], "reviewer=2");
        assert_eq!(home[0], "reviewer=2");
        // Transcript calls follow their own row's account (work 2, home 3),
        // and live Bash calls follow the session join (s-both under each).
        assert_eq!(work[2], "reviewer=2");
        assert_eq!(home[2], "reviewer=3");
        assert_eq!(work[4], "Agent=2/2,Bash=2/0");
        assert_eq!(home[4], "Agent=3/3,Bash=2/0");
    }

    #[test]
    fn an_account_with_no_usage_events_table_returns_no_rows_and_no_account_returns_all() {
        // Arrange
        let conn = mem();
        insert_request(&conn, &request("r1")).unwrap();
        insert_tool(&conn, &tool("t1", "Bash", true, 5, 100)).unwrap();

        // Act
        let all = seen(&conn, scoped(None));
        let work = seen(&conn, scoped(Some("work")));

        // Assert
        assert_eq!(all[0], "reviewer=1");
        assert_eq!(all[3], "req=1 err=0 tools=1 commits=0");
        assert_eq!(work, ["", "", "", "req=0 err=0 tools=0 commits=0", ""]);
    }
}
