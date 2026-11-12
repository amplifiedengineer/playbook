// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `pbsync` commands that talk to the remote: `push`, `pull` and `run`.

use playbook_store::{params, Connection};

use crate::cli::{Context, EXIT_FAILED, EXIT_OK, EXIT_USAGE};
use crate::config_cmd;
use crate::local_schema;
use crate::privacy::random_salt;
use crate::pull::{pull, PullSettings};
use crate::push::{
    backoff_seconds, push, read_state, record_failure, write_state, PushSettings, KEY_FAILURES,
};
use crate::remote::{ensure_salt, ensure_schema, KEY_SALT};
use crate::session::{connect, machine_id, open_local, NO_MACHINE, NO_USAGE_DB};
use crate::settings::{require_enabled, Settings};
use crate::tables::SHARED_TABLES;
use crate::transport::HttpTransport;

const KEY_LAST_ATTEMPT: &str = "last_attempt";
const BACKING_OFF: &str = "backing off after recent failures: use --force to try now";
const SALT_MISMATCH: &str =
    "the shared salt changed on the remote: run `playbook sync purge` on this machine, then enable sync again";
const RESET_WATERMARK_SQL: &str = "DELETE FROM sync_watermarks WHERE table_name = ?1";

const PUSH_USAGE: &str = "usage: playbook sync push [--force]\n\
  Sends new usage rows for the allowed accounts.\n\
  --force  run even while backing off after failures";
const PULL_USAGE: &str = "usage: playbook sync pull [--force]\n\
  Copies the usage rows the other machines sent.\n\
  --force  run even while backing off after failures";
const SYNC_USAGE: &str = "usage: playbook sync run [--force]\n\
  Runs push, then pull, then the shared config push and pull.\n\
  --force  run even while backing off after failures";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Push,
    Pull,
    Both,
}

impl Kind {
    const fn usage(self) -> &'static str {
        match self {
            Self::Push => PUSH_USAGE,
            Self::Pull => PULL_USAGE,
            Self::Both => SYNC_USAGE,
        }
    }
}

#[derive(Default)]
struct Totals {
    pushed: usize,
    pulled: usize,
    overlaps: usize,
}

pub fn run(kind: Kind, args: &[String], ctx: &mut Context) -> u8 {
    let force = match args {
        [] => false,
        [flag] if flag == "--force" => true,
        [flag] if flag == "--help" => {
            ctx.say(kind.usage());
            return EXIT_OK;
        }
        [other, ..] => {
            return ctx.fail(
                EXIT_USAGE,
                &format!("unknown argument {other}\n{}", kind.usage()),
            )
        }
    };
    match execute(kind, force, ctx) {
        Ok(line) => {
            ctx.say(&line);
            EXIT_OK
        }
        Err(message) => ctx.fail(EXIT_FAILED, &message),
    }
}

/// The steps, in order: gate, token, local database, backoff, machine id,
/// shared salt, then the push and the pull.
fn execute(kind: Kind, force: bool, ctx: &mut Context) -> Result<String, String> {
    let mut settings = Settings::load(&ctx.data_dir).map_err(|e| e.to_string())?;
    require_enabled(&settings).map_err(|e| e.to_string())?;
    let transport = connect(&settings, ctx.env)?;
    let local = open_local(&ctx.data_dir)?.ok_or(NO_USAGE_DB)?;
    local_schema::check(&local).map_err(|e| e.to_string())?;
    let now = (ctx.now)();
    if !force && backing_off(&local, now)? {
        return Ok(BACKING_OFF.to_string());
    }
    write_state(&local, KEY_LAST_ATTEMPT, &now.to_string()).map_err(|e| e.to_string())?;
    let machine = machine_id(&local)?.ok_or(NO_MACHINE)?;
    let salt = shared_salt(&local, &transport, now)?;
    let mut totals = Totals::default();
    if kind != Kind::Pull {
        totals.pushed = push_tables(&local, &transport, &mut settings, ctx, &machine, &salt)?;
    }
    if kind != Kind::Push {
        let pull_settings = PullSettings {
            machine: machine.clone(),
            salt,
        };
        for table in &SHARED_TABLES {
            let stats = pull(&local, &transport, table, &pull_settings, ctx.now)
                .map_err(|e| e.to_string())?;
            totals.pulled += stats.rows_written;
            totals.overlaps += stats.overlaps;
        }
    }
    if kind == Kind::Both {
        config_cmd::sync_config(&transport, &machine, ctx);
    }
    Ok(format!(
        "pushed {} rows, pulled {} rows, {} overlaps",
        totals.pushed, totals.pulled, totals.overlaps
    ))
}

fn backing_off(local: &Connection, now: i64) -> Result<bool, String> {
    let number = |key: &str| -> Result<Option<i64>, String> {
        let value = read_state(local, key).map_err(|e| e.to_string())?;
        Ok(value.and_then(|v| v.parse().ok()))
    };
    let failures = u32::try_from(number(KEY_FAILURES)?.unwrap_or(0)).unwrap_or(u32::MAX);
    let last_attempt = number(KEY_LAST_ATTEMPT)?.unwrap_or(0);
    let wait = i64::try_from(backoff_seconds(failures)).unwrap_or(i64::MAX);
    Ok(failures > 0 && now < last_attempt.saturating_add(wait))
}

/// Creates the remote tables and returns the shared salt. The local copy must
/// match it: a mismatch means the remote was purged and another salt took over.
fn shared_salt(local: &Connection, transport: &HttpTransport, now: i64) -> Result<String, String> {
    let failed = |error: &dyn std::fmt::Display| {
        let _ = record_failure(local, error);
        error.to_string()
    };
    ensure_schema(transport, &SHARED_TABLES, now).map_err(|e| failed(&e))?;
    let remote = ensure_salt(transport, &random_salt).map_err(|e| failed(&e))?;
    match read_state(local, KEY_SALT).map_err(|e| e.to_string())? {
        Some(stored) if stored != remote => Err(SALT_MISMATCH.to_string()),
        Some(_) => Ok(remote),
        None => {
            write_state(local, KEY_SALT, &remote).map_err(|e| e.to_string())?;
            Ok(remote)
        }
    }
}

/// Pushes every shared table. A pending watermark reset runs first and is
/// cleared only after the push succeeded.
fn push_tables(
    local: &Connection,
    transport: &HttpTransport,
    settings: &mut Settings,
    ctx: &Context,
    machine: &str,
    salt: &str,
) -> Result<usize, String> {
    if settings.reset_watermark {
        for table in &SHARED_TABLES {
            local
                .execute(RESET_WATERMARK_SQL, params![table.local_table])
                .map_err(|e| e.to_string())?;
        }
    }
    let push_settings = PushSettings {
        machine: machine.to_string(),
        allowed_accounts: settings.accounts.clone(),
        salt: salt.to_string(),
    };
    let mut sent = 0;
    for table in &SHARED_TABLES {
        sent += push(local, transport, table, &push_settings, ctx.now)
            .map_err(|e| e.to_string())?
            .rows_sent;
    }
    if settings.reset_watermark {
        settings.reset_watermark = false;
        settings.save(&ctx.data_dir).map_err(|e| e.to_string())?;
    }
    Ok(sent)
}
