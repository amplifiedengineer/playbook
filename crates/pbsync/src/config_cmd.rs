// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `pbsync config push` and `pbsync config pull`, and the config step of `pbsync run`.

use crate::cli::{Context, EXIT_FAILED, EXIT_OK, EXIT_USAGE};
use crate::config_sync::{pull_config, push_config};
use crate::remote::ensure_schema;
use crate::session::{connect, machine_id, open_local, NO_MACHINE, NO_USAGE_DB};
use crate::settings::{require_enabled, Settings};
use crate::tables::SHARED_TABLES;
use crate::transport::Transport;

const CONFIG_USAGE: &str = "usage: playbook sync config <push | pull> [--help]";
const PUSH_USAGE: &str = "usage: playbook sync config push\n\
  Sends the shareable config values listed in sync-keys.json.";
const PULL_USAGE: &str = "usage: playbook sync config pull\n\
  Writes config-proposal.json with the values other machines hold. Changes no config.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Push,
    Pull,
}

pub fn run(args: &[String], ctx: &mut Context) -> u8 {
    let (step, usage) = match args.first().map(String::as_str) {
        Some("push") => (Step::Push, PUSH_USAGE),
        Some("pull") => (Step::Pull, PULL_USAGE),
        Some("--help") | None if args.len() < 2 => {
            ctx.say(CONFIG_USAGE);
            return if args.is_empty() { EXIT_USAGE } else { EXIT_OK };
        }
        _ => return ctx.fail(EXIT_USAGE, &format!("unknown argument\n{CONFIG_USAGE}")),
    };
    match &args[1..] {
        [] => {}
        [flag] if flag == "--help" => {
            ctx.say(usage);
            return EXIT_OK;
        }
        [other, ..] => return ctx.fail(EXIT_USAGE, &format!("unknown argument {other}\n{usage}")),
    }
    match execute(step, ctx) {
        Ok(line) => {
            ctx.say(&line);
            EXIT_OK
        }
        Err(message) => ctx.fail(EXIT_FAILED, &message),
    }
}

fn execute(step: Step, ctx: &mut Context) -> Result<String, String> {
    let settings = Settings::load(&ctx.data_dir).map_err(|e| e.to_string())?;
    require_enabled(&settings).map_err(|e| e.to_string())?;
    let transport = connect(&settings, ctx.env)?;
    let machine = {
        let local = open_local(&ctx.data_dir)?.ok_or(NO_USAGE_DB)?;
        machine_id(&local)?.ok_or(NO_MACHINE)?
    };
    ensure_schema(&transport, &SHARED_TABLES, (ctx.now)()).map_err(|e| e.to_string())?;
    run_step(step, &transport, &machine, ctx).map(|(_, line)| line)
}

fn run_step(
    step: Step,
    transport: &dyn Transport,
    machine: &str,
    ctx: &Context,
) -> Result<(usize, String), String> {
    let now = (ctx.now)();
    match step {
        Step::Push => push_config(transport, &ctx.data_dir, machine, now)
            .map(|changed| (changed, format!("config: pushed {changed} changed values")))
            .map_err(|e| e.to_string()),
        Step::Pull => pull_config(transport, &ctx.data_dir, machine, now)
            .map(|stats| {
                let line = format!(
                    "config: proposed {} values, rejected {} keys",
                    stats.proposed, stats.rejected
                );
                (stats.proposed + stats.rejected, line)
            })
            .map_err(|e| e.to_string()),
    }
}

/// The config step of `pbsync run`, silent when nothing happened. A failure is a warning: the row sync stands.
pub(crate) fn sync_config(transport: &dyn Transport, machine: &str, ctx: &mut Context) {
    for step in [Step::Push, Step::Pull] {
        match run_step(step, transport, machine, ctx) {
            Ok((0, _)) => {}
            Ok((_, line)) => ctx.say(&line),
            Err(message) => {
                let _ = writeln!(ctx.err, "warning: config sync failed: {message}");
            }
        }
    }
}
