// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Watch mode for GitHub events (ADR 0055): `playbook watch once`, `dry-run`,
//! `approve` and `reject`. The side effects (GitHub reads, the `gh` writes
//! and the clock) arrive through [`Ctx`], so tests run the same code with
//! fakes. Only [`run`] builds the real ones.

use std::path::Path;

use serde_json::Value;

use crate::common::time::parse_iso8601_utc_epoch;
use crate::config::{self, store as config_store};
use crate::WatchCommand;

pub mod action;
pub mod address;
pub mod audit;
pub mod backoff;
pub mod child;
pub mod claude;
pub mod clock;
pub mod code;
pub mod control;
pub mod cycle;
pub mod engine;
pub mod event;
pub mod gh;
pub mod github;
pub mod http;
pub mod journal;
pub mod limits;
pub mod lock;
pub mod merge;
pub mod review;
pub mod rule;
pub mod service;
pub mod settings;
pub mod source;
pub mod store;
pub mod trust;

use audit::AuditLog;
use backoff::StdJitter;
use clock::{Clock, SystemClock};
use engine::{matches, ApproveOutcome, Engine, GithubWrite, NamedRule};
use github::{Endpoint, GithubPoller, Http};
use journal::RecoverOps;
use limits::GlobalCaps;
use lock::{acquire, Acquire};
use rule::Autonomy;
use settings::{ceiling, effective, resolve_rule};
use source::{EventSource, SourceError};
use store::{Cursor, Store};

const ENDPOINTS: [Endpoint; 3] = [
    Endpoint::Issues,
    Endpoint::IssueComments,
    Endpoint::PullComments,
];
const DEFAULT_TOKEN_ENV: &str = "GITHUB_TOKEN";
const OFF_MESSAGE: &str = "watch is off: run `playbook config set watch.enabled true`\n";

/// What a command needs from outside the process.
pub struct Ctx<'a, W, C> {
    pub home: &'a Path,
    pub data: &'a Path,
    pub write: &'a W,
    pub clock: &'a C,
    /// The login of the authenticated user.
    pub me: &'a str,
    pub model: &'a dyn claude::ModelRunner,
    pub code: &'a dyn code::CodeWork,
}

impl<'a, W, C> Ctx<'a, W, C> {
    /// An engine over `store` with this context's seams and `settings`.
    pub fn engine<'e>(
        &'e self,
        store: &'e mut Store,
        audit: &'e AuditLog,
        settings: &'e Settings,
    ) -> Engine<'e, W, C>
    where
        'a: 'e,
    {
        Engine {
            store,
            write: self.write,
            clock: self.clock,
            audit,
            jitter: &StdJitter,
            model: self.model,
            code: self.code,
            allowed: &settings.allowed_authors,
            fix: settings.fix,
            auto_merge: settings.auto_merge,
            caps: settings.caps,
            me: self.me,
        }
    }
}

pub struct Settings {
    pub enabled: bool,
    pub repos: Vec<String>,
    pub poll_seconds: i64,
    pub max_catchup_seconds: i64,
    pub token_env: String,
    pub caps: GlobalCaps,
    /// `watch.allowedAuthors`: logins that count as trusted.
    pub allowed_authors: Vec<String>,
    /// `fix.maxFiles` and `fix.maxLines`, the most a change may touch.
    pub fix: action::DiffCaps,
    /// `pr.merge.enabled`: whether watch may merge a pull request.
    pub auto_merge: bool,
}

impl Settings {
    pub fn load(home: &Path) -> Result<Self, String> {
        let get = |key: &str| -> Result<Value, String> {
            config::resolve_valid(key, home, None)
                .map(|(value, _, _)| value)
                .map_err(|e| e.to_string())
        };
        let strings = |key: &str| -> Result<Vec<String>, String> {
            Ok(get(key)?
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default())
        };
        let repos = strings("watch.repos")?;
        Ok(Self {
            enabled: get("watch.enabled")?.as_bool().unwrap_or(false),
            repos,
            poll_seconds: get("watch.pollSeconds")?.as_i64().unwrap_or(60),
            max_catchup_seconds: get("watch.maxCatchupHours")?.as_i64().unwrap_or(72) * 3600,
            token_env: get("watch.tokenEnv")?
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_TOKEN_ENV)
                .to_string(),
            allowed_authors: strings("watch.allowedAuthors")?,
            auto_merge: get("pr.merge.enabled")?.as_bool().unwrap_or(false),
            fix: action::DiffCaps {
                files: get("fix.maxFiles")?.as_i64().unwrap_or(3),
                lines: get("fix.maxLines")?.as_i64().unwrap_or(500),
            },
            caps: GlobalCaps {
                actions_per_day: get("watch.limits.actionsPerDay")?.as_i64().unwrap_or(50),
                usd_per_day: get("watch.limits.usdPerDay")?.as_f64().unwrap_or(5.0),
            },
        })
    }
}

/// The rules that apply to `repo` and the autonomy ceiling across its tiers.
fn repo_rules(home: &Path, repo: &str) -> Result<(Vec<NamedRule>, Autonomy), String> {
    let root = crate::common::paths::playbook_root_from(home);
    let text = |v: Option<Value>| v.and_then(|v| v.as_str().map(String::from));
    let mut rules = Vec::new();
    for key in config_store::keys_with_prefix(&root, "watch.rule.").map_err(|e| e.to_string())? {
        let tiers = config_store::lookup(&root, &key, Some(repo)).map_err(|e| e.to_string())?;
        let texts = [text(tiers.repo), text(tiers.org), text(tiers.global)];
        let texts: Vec<Option<&str>> = texts
            .iter()
            .map(|t| t.as_deref().filter(|s| !s.is_empty()))
            .collect();
        let id = key.trim_start_matches("watch.rule.").to_string();
        if let Some(rule) = resolve_rule(&texts).map_err(|e| format!("rule {id}: {e}"))? {
            rules.push((id, rule));
        }
    }
    let tiers =
        config_store::lookup(&root, "watch.autonomy", Some(repo)).map_err(|e| e.to_string())?;
    let levels: Vec<Autonomy> = [tiers.repo, tiers.org, tiers.global]
        .into_iter()
        .flatten()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();
    Ok((rules, ceiling(&levels)))
}

fn parse_since(since: Option<&str>) -> Result<Option<i64>, String> {
    since
        .map(|s| {
            parse_iso8601_utc_epoch(s)
                .ok_or_else(|| format!("--since must look like 2026-11-01T00:00:00Z, got {s}"))
        })
        .transpose()
}

/// Where a source starts. With no stored cursor and no `--since`, a source
/// starts now so that turning watch mode on never replays old history.
fn start_cursor(stored: Option<Cursor>, since: Option<i64>, now: i64) -> (Cursor, bool) {
    match (since, stored) {
        (Some(t), _) => (
            Cursor {
                since: t,
                ..Cursor::default()
            },
            true,
        ),
        (None, Some(c)) => (c, true),
        (None, None) => (
            Cursor {
                since: now,
                ..Cursor::default()
            },
            false,
        ),
    }
}

/// Notes a failed poll in `out`. A refused token is an error: nothing a
/// later try could fix.
fn poll_error(source: &str, err: &SourceError, out: &mut String) -> Result<(), String> {
    match err {
        SourceError::Unauthorized => Err(err.to_string()),
        other => {
            out.push_str(&format!("{source}: {other}\n"));
            Ok(())
        }
    }
}

/// `playbook watch once`: one poll of every repo, then the approved items.
pub fn once<W: GithubWrite + RecoverOps, C: Clock, H: Http>(
    ctx: &Ctx<'_, W, C>,
    since: Option<&str>,
    mut make_http: impl FnMut() -> H,
) -> Result<String, String> {
    let settings = Settings::load(ctx.home)?;
    if !settings.enabled {
        return Ok(OFF_MESSAGE.to_string());
    }
    let since = parse_since(since)?;
    let Acquire::Held(_lock) = acquire(&ctx.data.join("watch.lock")).map_err(|e| e.to_string())?
    else {
        return Ok("watch already running\n".to_string());
    };
    let mut store = Store::open(&ctx.data.join("watch.db")).map_err(|e| e.to_string())?;
    let audit = AuditLog::new(&ctx.data.join("watch"));
    let pass = cycle::cycle(ctx, &settings, since, &mut store, &audit, &mut make_http)?;
    Ok(pass.out)
}

/// `playbook watch dry-run`: what would be proposed. Reads GitHub and the
/// stored cursors, and changes nothing.
pub fn dry_run<W: GithubWrite, C: Clock, H: Http>(
    ctx: &Ctx<'_, W, C>,
    since: Option<&str>,
    mut make_http: impl FnMut() -> H,
) -> Result<String, String> {
    let settings = Settings::load(ctx.home)?;
    let since = parse_since(since)?;
    let db = ctx.data.join("watch.db");
    let store = db
        .exists()
        .then(|| Store::open(&db))
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut out = String::new();
    let mut events = Vec::new();
    for repo in &settings.repos {
        let (rules, level) = repo_rules(ctx.home, repo)?;
        for (id, rule) in &rules {
            out.push_str(&format!(
                "{repo}: rule {id}: {:?} -> {:?} at {:?}\n",
                rule.on,
                rule.action,
                effective(rule.autonomy, level)
            ));
        }
        for endpoint in ENDPOINTS {
            let mut poller = GithubPoller::new(
                make_http(),
                ctx.clock,
                repo,
                endpoint,
                settings.poll_seconds,
            );
            let source = format!("{repo}/{}", poller.name());
            let stored = match &store {
                Some(s) => s.cursor(&source).map_err(|e| e.to_string())?,
                None => None,
            };
            let (cursor, started) = start_cursor(stored, since, ctx.clock.now());
            if !started {
                continue;
            }
            match poller.poll(&cursor, &mut events) {
                Ok(_) => {
                    for event in &events {
                        for (id, rule) in &rules {
                            if effective(rule.autonomy, level) != Autonomy::Off
                                && matches(rule, event, ctx.me)
                            {
                                out.push_str(&format!(
                                    "would propose: rule {id} for {} on {}#{}\n",
                                    event.key, event.repo, event.number
                                ));
                            }
                        }
                    }
                }
                Err(err) => poll_error(&source, &err, &mut out)?,
            }
        }
    }
    Ok(out)
}

/// `playbook watch approve <id>`. With another process running watch, the
/// item is only marked and that process carries it out.
pub fn approve<W: GithubWrite, C: Clock>(ctx: &Ctx<'_, W, C>, id: i64) -> Result<String, String> {
    let mut store = Store::open(&ctx.data.join("watch.db")).map_err(|e| e.to_string())?;
    let row = store
        .work(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("item {id} does not exist"))?;
    let (rules, _) = repo_rules(ctx.home, &row.repo)?;
    let settings = Settings::load(ctx.home)?;
    let lock = acquire(&ctx.data.join("watch.lock")).map_err(|e| e.to_string())?;
    let audit = AuditLog::new(&ctx.data.join("watch"));
    let mut engine = ctx.engine(&mut store, &audit, &settings);
    let outcome = engine
        .approve(id, &rules, matches!(lock, Acquire::Held(_)))
        .map_err(|e| e.to_string())?;
    Ok(match outcome {
        ApproveOutcome::Handed => {
            format!("item {id} approved: the running watch will carry it out\n")
        }
        ApproveOutcome::Done => format!("item {id} done\n"),
        ApproveOutcome::AlreadyDone => format!("item {id} already done\n"),
        ApproveOutcome::Skipped(why) => format!("item {id} skipped: {why}\n"),
        ApproveOutcome::Retry => format!("item {id} failed and will be tried again\n"),
        ApproveOutcome::Dead => {
            format!("item {id} failed too many times and is on the dead list\n")
        }
        ApproveOutcome::Stopped => {
            format!("item {id} stopped by the kill switch: it stays approved\n")
        }
        ApproveOutcome::Demoted(why) => format!("item {id} stays a proposal: {why}\n"),
    })
}

/// `playbook watch reject <id>`.
pub fn reject<W: GithubWrite, C: Clock>(ctx: &Ctx<'_, W, C>, id: i64) -> Result<String, String> {
    let settings = Settings::load(ctx.home)?;
    let mut store = Store::open(&ctx.data.join("watch.db")).map_err(|e| e.to_string())?;
    let audit = AuditLog::new(&ctx.data.join("watch"));
    let mut engine = ctx.engine(&mut store, &audit, &settings);
    engine.reject(id).map_err(|e| e.to_string())?;
    Ok(format!("item {id} rejected\n"))
}

/// The token of the `gh` login, when there is one.
fn gh_token() -> Option<String> {
    let mut command = std::process::Command::new("gh");
    command.args(["auth", "token"]);
    let output =
        crate::common::proc::run_with_timeout(&mut command, std::time::Duration::from_secs(30))?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !token.is_empty()).then_some(token)
}

/// `install-service` and `uninstall-service`, which need no token. This is the
/// only place that builds the real service manager.
fn service_command(
    cmd: Option<&WatchCommand>,
    home: &Path,
    data: &Path,
) -> Option<Result<String, String>> {
    let delete_log = match cmd {
        Some(WatchCommand::InstallService) => None,
        Some(WatchCommand::UninstallService { delete_log }) => Some(*delete_log),
        _ => return None,
    };
    Some(service_result(delete_log, home, data))
}

fn service_result(delete_log: Option<bool>, home: &Path, data: &Path) -> Result<String, String> {
    let kind = service::kind_here()?;
    if let Some(why) = crate::service_manager::refusal(home) {
        return Err(format!("the service was not changed, because {why}"));
    }
    let manager = crate::service_manager::manager_for(home);
    let uid = crate::common::account::uid();
    match delete_log {
        None => {
            let exe = std::env::current_exe().map_err(|e| format!("no path for playbook: {e}"))?;
            let path_env = std::env::var("PATH").unwrap_or_default();
            let launch = service::Launch {
                exe: &exe,
                path_env: &path_env,
            };
            service::install(kind, home, &launch, manager.as_ref(), uid)
        }
        Some(delete_log) => service::uninstall(
            kind,
            home,
            manager.as_ref(),
            uid,
            &data.join("watch"),
            delete_log,
        ),
    }
}

/// Builds the real GitHub clients and runs `cmd`, or the foreground loop when
/// there is none.
pub fn run(cmd: Option<&WatchCommand>) -> Result<String, String> {
    let home = crate::common::session::home_dir();
    let data = playbook_data::data_dir_from(&home);
    let settings = Settings::load(&home)?;
    if matches!(cmd, None | Some(WatchCommand::Once { .. })) && !settings.enabled {
        return Ok(OFF_MESSAGE.to_string());
    }
    if let Some(out) = service_command(cmd, &home, &data) {
        return out;
    }
    let needs_token = matches!(
        cmd,
        None | Some(
            WatchCommand::Once { .. } | WatchCommand::DryRun { .. } | WatchCommand::Approve { .. }
        )
    );
    let token = if !needs_token {
        String::new()
    } else {
        // A timer starts with no shell environment, so `gh` is the fallback.
        std::env::var(&settings.token_env)
            .ok()
            .or_else(gh_token)
            .ok_or_else(|| {
                format!(
                    "set the {} environment variable to a GitHub token, or run `gh auth login`",
                    settings.token_env
                )
            })?
    };
    claude::note_token_env(&settings.token_env);
    let write = gh::GhWrite::new(token.clone());
    let clock = SystemClock;
    let me = match cmd {
        None | Some(WatchCommand::Once { .. } | WatchCommand::DryRun { .. }) => write.login()?,
        _ => String::new(),
    };
    let clone = std::env::current_dir().map_err(|e| format!("no current directory: {e}"))?;
    let code = code::GitCode {
        clone: &clone,
        worktrees: data.join("watch").join("worktrees"),
        home: &home,
    };
    let ctx = Ctx {
        home: &home,
        data: &data,
        write: &write,
        clock: &clock,
        me: &me,
        model: &claude::ClaudeCli,
        code: &code,
    };
    let Some(cmd) = cmd else {
        let mut env = cycle::LoopEnv {
            sleep: &mut |seconds| {
                std::thread::sleep(std::time::Duration::from_secs(
                    seconds.max(0).unsigned_abs(),
                ));
            },
            stop: &|| false,
            print: &mut |text| print!("{text}"),
            jitter: &StdJitter,
        };
        cycle::run_loop(&ctx, || http::UreqHttp::new(&token), &mut env)?;
        return Ok(String::new());
    };
    match cmd {
        WatchCommand::Once { since } => {
            once(&ctx, since.as_deref(), || http::UreqHttp::new(&token))
        }
        WatchCommand::DryRun { since } => {
            dry_run(&ctx, since.as_deref(), || http::UreqHttp::new(&token))
        }
        WatchCommand::Approve { id } => approve(&ctx, *id),
        WatchCommand::Reject { id } => reject(&ctx, *id),
        WatchCommand::Status { json } => control::status(&ctx, *json),
        WatchCommand::Pause => control::set_paused(&ctx, true),
        WatchCommand::Resume => control::set_paused(&ctx, false),
        WatchCommand::InstallService | WatchCommand::UninstallService { .. } => {
            Err("service commands are handled before the token is read".to_string())
        }
        WatchCommand::Retry { id } => control::retry(&ctx, *id),
        WatchCommand::Log {
            rule,
            outcome,
            json,
        } => control::log(&ctx, rule.as_deref(), outcome.as_deref(), *json),
    }
}

#[cfg(test)]
mod tests;
