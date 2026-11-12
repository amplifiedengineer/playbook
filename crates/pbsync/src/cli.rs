// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `pbsync` command line. `enable`, `disable` and `status` manage the
//! settings and make no network call; the others live in their own modules.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use playbook_store::schema::stored_version;
use playbook_store::types::Value as Sql;
use playbook_store::{params_from_iter, Connection, OpenFlags, OptionalExtension};

use crate::config_cmd;
use crate::purge;
use crate::remote::REMOTE_SCHEMA_VERSION;
use crate::settings::Settings;
use crate::sync_cmd::{self, Kind};
use crate::tables::usage_events;
use crate::token::{resolve_token, DEFAULT_TOKEN_ENV};
use crate::transport::validate_url;

pub const EXIT_OK: u8 = 0;
pub const EXIT_FAILED: u8 = 1;
pub const EXIT_USAGE: u8 = 2;

const USAGE_DB_FILE: &str = "usage.db";
const USAGE_DB_NAME: &str = "usage.db";
const NOT_STARTED: &str = "not started";
const KEY_LAST_SUCCESS: &str = "last_success";
const KEY_LAST_ERROR: &str = "last_error";
const KEY_FAILURES: &str = "failures";
const TABLE_EXISTS_SQL: &str =
    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)";
const READ_STATE_SQL: &str = "SELECT value FROM sync_state WHERE key = ?1";
const READ_MARK_SQL: &str = "SELECT rowid_mark FROM sync_watermarks WHERE table_name = ?1";

const TOP_USAGE: &str =
    "usage: playbook sync <enable | disable | status | push | pull | run | config | purge> [--help]
       pbsync --version";
const ENABLE_USAGE: &str =
    "usage: playbook sync enable --url URL [--account KEY]... [--token-env NAME] [--yes]\n\
  --url URL         the Turso database URL (https or libsql; http only for localhost)\n\
  --account KEY     a local account key to sync, repeatable; with none, nothing syncs\n\
  --token-env NAME  the environment variable that holds the token (default PBSYNC_TOKEN)\n\
  --yes             skip the confirmation question";
const DISABLE_USAGE: &str =
    "usage: playbook sync disable\nTurns sync off. Local data and the account list are kept.";
const STATUS_USAGE: &str =
    "usage: playbook sync status\nShows whether sync is on, and its local state. Makes no network call.";
const OLD_SYNC_NOTICE: &str =
    "pbsync sync is renamed, use: playbook sync run (the old name is removed in 1.0.0)";
const MISSING_URL: &str =
    "missing --url: run `playbook sync enable --url https://YOUR-DB.turso.io`";

/// Everything a command touches, injected so tests need no real home or clock.
pub struct Context<'a> {
    pub data_dir: PathBuf,
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub now: &'a dyn Fn() -> i64,
    /// Whether stderr is a terminal, which decides if a renamed verb says so.
    pub is_terminal: &'a dyn Fn() -> bool,
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

impl Context<'_> {
    pub(crate) fn say(&mut self, line: &str) {
        let _ = writeln!(self.out, "{line}");
    }

    pub(crate) fn fail(&mut self, code: u8, line: &str) -> u8 {
        let _ = writeln!(self.err, "error: {line}");
        code
    }
}

pub fn run(args: &[String], ctx: &mut Context) -> u8 {
    let Some((command, rest)) = args.split_first() else {
        return ctx.fail(EXIT_USAGE, TOP_USAGE);
    };
    match command.as_str() {
        "--version" if rest.is_empty() => {
            ctx.say(&format!("pbsync {}", env!("CARGO_PKG_VERSION")));
            EXIT_OK
        }
        "--help" | "-h" if rest.is_empty() => {
            ctx.say(TOP_USAGE);
            EXIT_OK
        }
        "enable" => enable(rest, ctx),
        "disable" => no_flags(rest, DISABLE_USAGE, ctx, disable),
        "status" => no_flags(rest, STATUS_USAGE, ctx, status),
        "push" => sync_cmd::run(Kind::Push, rest, ctx),
        "pull" => sync_cmd::run(Kind::Pull, rest, ctx),
        "run" => sync_cmd::run(Kind::Both, rest, ctx),
        "sync" => {
            if (ctx.is_terminal)() {
                let _ = writeln!(ctx.err, "{OLD_SYNC_NOTICE}");
            }
            sync_cmd::run(Kind::Both, rest, ctx)
        }
        "config" => config_cmd::run(rest, ctx),
        "purge" => purge::run(rest, ctx),
        other => ctx.fail(EXIT_USAGE, &format!("unknown command {other}\n{TOP_USAGE}")),
    }
}

fn no_flags(args: &[String], usage: &str, ctx: &mut Context, run: fn(&mut Context) -> u8) -> u8 {
    match args {
        [] => run(ctx),
        [flag] if flag == "--help" => {
            ctx.say(usage);
            EXIT_OK
        }
        [other, ..] => ctx.fail(EXIT_USAGE, &format!("unknown argument {other}\n{usage}")),
    }
}

struct EnableArgs {
    url: String,
    accounts: Vec<String>,
    token_env: String,
    yes: bool,
}

/// `Ok(None)` means `--help` was asked for.
fn parse_enable(args: &[String]) -> Result<Option<EnableArgs>, String> {
    let mut url = None;
    let mut accounts: Vec<String> = Vec::new();
    let mut token_env = DEFAULT_TOKEN_ENV.to_string();
    let mut yes = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| {
            iter.next()
                .cloned()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--help" => return Ok(None),
            "--yes" => yes = true,
            "--url" => url = Some(value("--url")?),
            "--token-env" => token_env = value("--token-env")?,
            "--account" => {
                let account = value("--account")?;
                if !accounts.contains(&account) {
                    accounts.push(account);
                }
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let url = url.ok_or_else(|| MISSING_URL.to_string())?;
    Ok(Some(EnableArgs {
        url,
        accounts,
        token_env,
        yes,
    }))
}

fn enable(args: &[String], ctx: &mut Context) -> u8 {
    let args = match parse_enable(args) {
        Ok(Some(args)) => args,
        Ok(None) => {
            ctx.say(ENABLE_USAGE);
            return EXIT_OK;
        }
        Err(message) => return ctx.fail(EXIT_USAGE, &format!("{message}\n{ENABLE_USAGE}")),
    };
    if let Err(error) = validate_url(&args.url) {
        return ctx.fail(EXIT_USAGE, &error.to_string());
    }
    let previous = match Settings::load(&ctx.data_dir) {
        Ok(settings) => settings,
        Err(error) => return ctx.fail(EXIT_FAILED, &error.to_string()),
    };
    show_consent(ctx, &args);
    if !args.yes && !confirmed(ctx) {
        ctx.say("Sync stays off.");
        return EXIT_FAILED;
    }
    let accounts_changed = sorted(&previous.accounts) != sorted(&args.accounts);
    let settings = Settings {
        enabled: true,
        url: Some(args.url),
        reset_watermark: previous.reset_watermark || accounts_changed,
        accounts: args.accounts,
        token_env: args.token_env,
        enabled_at: Some((ctx.now)()),
        consented: true,
        ..previous
    };
    if let Err(error) = settings.save(&ctx.data_dir) {
        return ctx.fail(EXIT_FAILED, &error.to_string());
    }
    ctx.say(&format!(
        "Sync is on. Set {} to your database token; pbsync never stores it.",
        settings.token_env
    ));
    EXIT_OK
}

fn sorted(accounts: &[String]) -> Vec<&String> {
    let mut sorted: Vec<&String> = accounts.iter().collect();
    sorted.sort();
    sorted
}

fn show_consent(ctx: &mut Context, args: &EnableArgs) {
    let columns: Vec<&str> = usage_events().columns.iter().map(|c| c.name).collect();
    ctx.say(&format!(
        "pbsync will send these usage_events columns to {}:\n  {}",
        host_of(&args.url),
        columns.join(", ")
    ));
    ctx.say("account and repo leave as salted 128-bit hashes.");
    ctx.say("Never sent: session id, agent, branch, path or raw email.");
    if args.accounts.is_empty() {
        ctx.say("Accounts that will sync: none. Nothing will sync until you run `playbook sync enable` again with --account KEY.");
    } else {
        ctx.say(&format!(
            "Accounts that will sync: {}",
            args.accounts.join(", ")
        ));
    }
    ctx.say("Nothing leaves this machine until you answer yes.");
}

fn confirmed(ctx: &mut Context) -> bool {
    let _ = write!(ctx.out, "Enable sync? Type yes to continue: ");
    let _ = ctx.out.flush();
    let mut answer = String::new();
    ctx.input.read_line(&mut answer).is_ok() && answer.trim().eq_ignore_ascii_case("yes")
}

fn disable(ctx: &mut Context) -> u8 {
    let mut settings = match Settings::load(&ctx.data_dir) {
        Ok(settings) => settings,
        Err(error) => return ctx.fail(EXIT_FAILED, &error.to_string()),
    };
    if settings.enabled {
        settings.enabled = false;
        if let Err(error) = settings.save(&ctx.data_dir) {
            return ctx.fail(EXIT_FAILED, &error.to_string());
        }
    }
    ctx.say("Sync is off. Local data and the account list are kept.");
    EXIT_OK
}

fn status(ctx: &mut Context) -> u8 {
    let settings = match Settings::load(&ctx.data_dir) {
        Ok(settings) => settings,
        Err(error) => return ctx.fail(EXIT_FAILED, &error.to_string()),
    };
    ctx.say(&format!(
        "sync: {}",
        if settings.enabled { "enabled" } else { "off" }
    ));
    ctx.say(&format!(
        "url host: {}",
        settings.url.as_deref().map_or("none", host_of)
    ));
    ctx.say(&format!(
        "accounts ({}): {}",
        settings.accounts.len(),
        settings.accounts.join(", ")
    ));
    let token_set = resolve_token(None, &settings.token_env, ctx.env).is_ok();
    ctx.say(&format!(
        "token: {} ({})",
        if token_set { "set" } else { "unset" },
        settings.token_env
    ));
    for line in local_lines(&ctx.data_dir, &settings) {
        ctx.say(&line);
    }
    EXIT_OK
}

/// The host of a URL: no scheme, credentials, port, path or query.
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    host_port.split(':').next().unwrap_or_default()
}

/// The status lines read from `usage.db`; they say `not started` when the
/// database or a table is missing.
fn local_lines(data_dir: &Path, settings: &Settings) -> Vec<String> {
    let path = data_dir.join(USAGE_DB_FILE);
    let not_started = || {
        [
            "last success",
            "last error",
            "failures",
            "rows pending",
            "schema",
        ]
        .map(|label| format!("{label}: {NOT_STARTED}"))
        .to_vec()
    };
    if !path.exists() {
        return not_started();
    }
    match read_local(&path, settings) {
        Ok(lines) => lines,
        Err(reason) => vec![format!("local state: unreadable ({reason})")],
    }
}

fn read_local(path: &Path, settings: &Settings) -> Result<Vec<String>, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let has = |table: &str| -> Result<bool, String> {
        conn.query_row(TABLE_EXISTS_SQL, [table], |r| r.get(0))
            .map_err(|e| e.to_string())
    };
    let mut lines = Vec::new();
    let state_ready = has("sync_state")?;
    for (label, key) in [
        ("last success", KEY_LAST_SUCCESS),
        ("last error", KEY_LAST_ERROR),
        ("failures", KEY_FAILURES),
    ] {
        let value = if state_ready {
            state_value(&conn, key)?
        } else {
            None
        };
        lines.push(format!(
            "{label}: {}",
            value.unwrap_or_else(|| NOT_STARTED.to_string())
        ));
    }
    let pending = if has("usage_events")? && has("sync_watermarks")? {
        pending_rows(&conn, settings)?.to_string()
    } else {
        NOT_STARTED.to_string()
    };
    lines.push(format!("rows pending: {pending}"));
    lines.push(schema_line(&conn)?);
    Ok(lines)
}

fn state_value(conn: &Connection, key: &str) -> Result<Option<String>, String> {
    conn.query_row(READ_STATE_SQL, [key], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())
}

/// Rows the next push would send. A pending watermark reset counts every row.
fn pending_rows(conn: &Connection, settings: &Settings) -> Result<i64, String> {
    if settings.accounts.is_empty() {
        return Ok(0);
    }
    let table = usage_events();
    let mark: i64 = if settings.reset_watermark {
        0
    } else {
        conn.query_row(READ_MARK_SQL, [table.local_table], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?
            .unwrap_or(0)
    };
    let marks = vec!["?"; settings.accounts.len()].join(", ");
    let sql = format!(
        "SELECT COUNT(*) FROM {} WHERE rowid > ?1 AND account IN ({marks})",
        table.local_table
    );
    let args =
        std::iter::once(Sql::Integer(mark)).chain(settings.accounts.iter().cloned().map(Sql::Text));
    conn.query_row(&sql, params_from_iter(args), |r| r.get(0))
        .map_err(|e| e.to_string())
}

/// The remote version is only known after a push, so skew shows up there as a
/// named error; here the two versions are listed side by side.
fn schema_line(conn: &Connection) -> Result<String, String> {
    let local = stored_version(conn, USAGE_DB_NAME).map_err(|e| e.to_string())?;
    Ok(match local {
        None => format!("schema: {NOT_STARTED}"),
        Some(v) => format!(
            "schema: {USAGE_DB_NAME} version {v}, sync format version {REMOTE_SCHEMA_VERSION}"
        ),
    })
}
