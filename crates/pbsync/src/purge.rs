// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `pbsync purge`: removes this machine's rows from the remote, then the
//! local copies of what sync wrote.

use playbook_store::Connection;

use crate::cli::{Context, EXIT_FAILED, EXIT_OK, EXIT_USAGE};
use crate::local_schema;
use crate::privacy::{random_salt, salted_hash, Domain};
use crate::push::read_state;
use crate::remote::{ensure_salt, ensure_schema, KEY_SALT};
use crate::session::{connect, machine_id, open_local};
use crate::settings::Settings;
use crate::tables::{TableDef, Transform, SHARED_TABLES};
use crate::transport::{Statement, Transport, Value};

const NOTHING: &str = "Nothing to purge.";
const CANCELLED: &str = "purge cancelled: nothing was changed";
const DONE: &str = "Purged. Sync is off; local usage data was kept.";
const REMOTE_FAILED: &str = "the remote purge failed";
const KEPT: &str = "local sync state was kept and sync is off";
const CLEAR_LOCAL_SQL: &str = "BEGIN;
    DELETE FROM usage_events_remote;
    DELETE FROM account_aliases;
    DELETE FROM sync_state;
    DELETE FROM sync_watermarks;
    DELETE FROM sync_cursors;
    COMMIT;";
const DELETE_SALT_SQL: &str = "DELETE FROM shared_meta WHERE key = ?";
const MACHINE_COLUMN: &str = "machine";

const USAGE: &str = "usage: playbook sync purge [--all] [--account KEY] [--yes]\n\
  Removes this machine's rows from the remote, clears the local copies and turns sync off.\n\
  --account KEY  remove only the rows of that account\n\
  --all          remove every machine's rows and the shared salt (asks first)\n\
  --yes          skip the question that --all asks";

struct Args {
    all: bool,
    account: Option<String>,
    yes: bool,
}

/// What a purge removes from the remote.
enum Scope {
    ThisMachine(String),
    ThisMachineAccount(String, String),
    Everything,
}

/// `Ok(None)` means `--help` was asked for.
fn parse(args: &[String]) -> Result<Option<Args>, String> {
    let mut parsed = Args {
        all: false,
        account: None,
        yes: false,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" => return Ok(None),
            "--all" => parsed.all = true,
            "--yes" => parsed.yes = true,
            "--account" => {
                let value = iter.next().filter(|v| !v.is_empty());
                parsed.account = Some(value.ok_or("--account needs a value")?.clone());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if parsed.all && parsed.account.is_some() {
        return Err("--all and --account cannot be combined".to_string());
    }
    Ok(Some(parsed))
}

pub fn run(args: &[String], ctx: &mut Context) -> u8 {
    let args = match parse(args) {
        Ok(Some(args)) => args,
        Ok(None) => {
            ctx.say(USAGE);
            return EXIT_OK;
        }
        Err(message) => return ctx.fail(EXIT_USAGE, &format!("{message}\n{USAGE}")),
    };
    match execute(&args, ctx) {
        Ok(line) => {
            ctx.say(line);
            EXIT_OK
        }
        Err(message) => ctx.fail(EXIT_FAILED, &message),
    }
}

/// The steps, in order: ask (for `--all`), turn sync off, delete remotely,
/// then clear locally. A remote failure leaves the local state as it was.
fn execute(args: &Args, ctx: &mut Context) -> Result<&'static str, String> {
    let mut settings = Settings::load(&ctx.data_dir).map_err(|e| e.to_string())?;
    let local = open_local(&ctx.data_dir)?;
    if let Some(local) = &local {
        local_schema::check(local).map_err(|e| e.to_string())?;
    }
    let partial = args.all || args.account.is_some();
    let synced = match &local {
        Some(local) => read_state(local, KEY_SALT)
            .map_err(|e| e.to_string())?
            .is_some(),
        None => false,
    };
    if !settings.enabled && !synced && !partial {
        return Ok(NOTHING);
    }
    if args.all && !args.yes && !confirmed(ctx) {
        return Err(CANCELLED.to_string());
    }
    if settings.enabled {
        settings.enabled = false;
        settings.save(&ctx.data_dir).map_err(|e| e.to_string())?;
    }
    let machine = match &local {
        Some(local) => machine_id(local)?,
        None => None,
    };
    let scope = match (args.all, &args.account, machine) {
        (true, _, _) => Scope::Everything,
        (false, _, None) => return Ok(NOTHING),
        (false, None, Some(machine)) => Scope::ThisMachine(machine),
        (false, Some(key), Some(machine)) => Scope::ThisMachineAccount(machine, key.clone()),
    };
    let transport = connect(&settings, ctx.env)?;
    delete_remote(&transport, &scope, (ctx.now)())
        .map_err(|error| format!("{REMOTE_FAILED}: {error}; {KEPT}"))?;
    if let Some(local) = &local {
        clear_local(local)?;
    }
    Ok(DONE)
}

fn confirmed(ctx: &mut Context) -> bool {
    let _ = write!(
        ctx.out,
        "This deletes every machine's rows and the shared salt. Type yes to continue: "
    );
    let _ = ctx.out.flush();
    let mut answer = String::new();
    ctx.input.read_line(&mut answer).is_ok() && answer.trim().eq_ignore_ascii_case("yes")
}

fn clear_local(local: &Connection) -> Result<(), String> {
    local
        .execute_batch(CLEAR_LOCAL_SQL)
        .map_err(|error| format!("the local database failed: {error}"))
}

fn delete_remote(transport: &dyn Transport, scope: &Scope, now: i64) -> Result<(), String> {
    ensure_schema(transport, &SHARED_TABLES, now).map_err(|e| e.to_string())?;
    let mut statements: Vec<Statement> = match scope {
        Scope::Everything => SHARED_TABLES.iter().map(delete_all).collect(),
        Scope::ThisMachine(machine) => SHARED_TABLES
            .iter()
            .map(|table| delete_machine(table, machine))
            .collect(),
        Scope::ThisMachineAccount(machine, key) => {
            let salt = ensure_salt(transport, &random_salt).map_err(|e| e.to_string())?;
            let hash = salted_hash(&salt, Domain::Account, key);
            SHARED_TABLES
                .iter()
                .filter_map(|table| delete_account(table, machine, &hash))
                .collect()
        }
    };
    if matches!(scope, Scope::Everything) {
        statements.push(Statement {
            sql: DELETE_SALT_SQL.to_string(),
            args: vec![Value::Text(KEY_SALT.to_string())],
        });
    }
    transport.execute(&statements).map_err(|e| e.to_string())?;
    Ok(())
}

fn delete_all(table: &TableDef) -> Statement {
    Statement {
        sql: format!("DELETE FROM {}", table.remote_table),
        args: vec![],
    }
}

fn delete_machine(table: &TableDef, machine: &str) -> Statement {
    Statement {
        sql: format!(
            "DELETE FROM {} WHERE {MACHINE_COLUMN} = ?",
            table.remote_table
        ),
        args: vec![Value::Text(machine.to_string())],
    }
}

/// None for a table that holds no account column.
fn delete_account(table: &TableDef, machine: &str, hash: &str) -> Option<Statement> {
    let column = table
        .columns
        .iter()
        .find(|c| c.transform == Transform::HashAccount)?;
    Some(Statement {
        sql: format!(
            "DELETE FROM {} WHERE {MACHINE_COLUMN} = ? AND {} = ?",
            table.remote_table, column.name
        ),
        args: vec![
            Value::Text(machine.to_string()),
            Value::Text(hash.to_string()),
        ],
    })
}
