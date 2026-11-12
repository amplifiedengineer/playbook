// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! In `auto`, tracks spend against a budget per agent on every tool call.
//!
//! Each transcript (the main one and every `subagents/agent-*.jsonl`) is
//! read from its own byte offset and priced, from a baseline taken the first
//! time the hook runs. Values are rounded to integer cents before they are
//! subtracted. The result is persisted as `<session dir>/auto-cost.json`.
//!
//! The caller is the main session, or a subagent named by the `agent_id` in
//! the hook input. The main session is held to `auto.budget.mainUsd` against its
//! own transcript, and each subagent to `auto.budget.agentUsd` against its own.
//! A caller that cannot be named counts as the main session. At the warn tier
//! the caller hears one note about its own budget. At its cap only that caller
//! is parked: new work is denied, while reading, the closing steps in the
//! `closing` module and a park note stay allowed. The whole tree's spend has a
//! ceiling, `auto.budget.ceilingUsd`, past which every caller is parked.
//! When the caller's spend cannot be read, including a transcript with
//! unpriced models and no telemetry, it fails closed the same way. Under the
//! cap it also denies the model changing its own guard: the `mode`, `auto.*`
//! and `fix.*` settings and the config files.

use crate::common::atomic::with_dir_lock;
use crate::common::mode::{hook_slug, resolve, resolve_for_hook_at, Mode, Source};
use crate::common::paths::playbook_root_from;
use crate::common::payload::Payload;
use crate::common::shell::{program_index, program_name, simple_commands};
use crate::common::{emit_pre_context, emit_pre_deny, home_dir, session_dir, session_id};
use crate::config;
use crate::usage::claude_code::{one_hour_cache_tokens, token};
use crate::usage::UsageEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

mod closing;

const INTERVAL_VAR: &str = "PLAYBOOK_AUTO_COST_INTERVAL_MS";
const DEFAULT_INTERVAL_MS: u64 = 2000;
const STATE_FILE: &str = "auto-cost.json";
/// Left in the session dir while the main session is denied work, so hooks
/// that ask for writes (the memory capture Stop hook) stand down.
const PARKED_MARKER: &str = "parked";
const TELEMETRY_TAIL_BYTES: u64 = 16 * 1024;

#[derive(Default, Serialize, Deserialize)]
struct State {
    effective_cents: i64,
    computed_at_ms: u64,
    transcript: Transcript,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    telemetry: Option<Telemetry>,
}

#[derive(Default, Serialize, Deserialize)]
struct Transcript {
    files: BTreeMap<String, FileCost>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct FileCost {
    baseline_cents: i64,
    usd: f64,
    offset: u64,
    inode: u64,
    /// The last message read and the cost already counted for it, so the
    /// rest of a message streamed across two reads is not counted twice.
    #[serde(default)]
    last_id: String,
    #[serde(default)]
    last_usd: f64,
    /// A message used a model missing from the price table, so `usd` is a
    /// lower bound.
    #[serde(default)]
    unpriced: bool,
}

impl FileCost {
    fn restart(&mut self) {
        self.usd = 0.0;
        self.offset = 0;
        self.last_id.clear();
        self.last_usd = 0.0;
        self.unpriced = false;
    }
}

#[derive(Serialize, Deserialize)]
struct Telemetry {
    baseline_cents: i64,
    cents: i64,
}

const DEFAULT_BUDGET_CENTS: i64 = config::keys::AUTO_BUDGET_USD as i64 * 100;
const DEFAULT_AGENT_BUDGET_CENTS: i64 = config::keys::AUTO_AGENT_BUDGET_USD as i64 * 100;
/// With `auto.budget.ceilingUsd` unset, the tree's ceiling is this many
/// times the main cap.
const CEILING_FACTOR: i64 = config::keys::AUTO_CEILING_FACTOR as i64;
/// The percent of a cap at which the spend hook warns.
const WARN_PCT: u64 = 70;
const SAFE_TOOLS: [&str; 3] = ["Read", "Grep", "Glob"];
const MODE_ESCAPE: &str = "playbook mode ask";

pub fn run(payload: &Payload) {
    let env = std::env::var("PLAYBOOK_MODE").ok();
    let from_env = resolve(None, env.as_deref(), None);
    if from_env.source == Source::Env && from_env.mode == Mode::Ask {
        return;
    }
    let home = home_dir();
    let slug = hook_slug(&home);
    let slug = slug.as_deref();
    let root = playbook_root_from(&home);
    let is_auto = resolve_for_hook_at(env.as_deref(), &home, slug).mode == Mode::Auto;
    if is_auto || config_broke_mid_session(payload, &root, &home, slug) {
        let mut limits = Limits::read(&home, slug);
        limits.mode_from_env = from_env.source == Source::Env;
        enforce(payload, &limits, &root);
    }
}

/// An unreadable config resolves to `ask`, which would switch the cap off for
/// a session it already tracks, so such a session stays guarded.
fn config_broke_mid_session(
    payload: &Payload,
    root: &Path,
    home: &Path,
    slug: Option<&str>,
) -> bool {
    let sid = session_id(payload);
    !sid.is_empty()
        && config::resolve("mode", home, slug).is_err()
        && root.join("runtime").join(sid).join(STATE_FILE).is_file()
}

/// The caps and the warn tier in integer cents.
struct Limits {
    /// The main session's cap.
    cap: i64,
    /// The cap of each subagent.
    agent_cap: i64,
    /// The whole tree's ceiling, past which every caller is parked.
    ceiling: i64,
    /// `auto.budget.ceilingUsd` is set, so `config set` refuses a budget
    /// above it.
    ceiling_is_set: bool,
    pct: u64,
    budget_is_valid: bool,
    agent_budget_is_valid: bool,
    /// `PLAYBOOK_MODE` is set in the environment, which beats the config, so
    /// `playbook mode ask` cannot lift a guard.
    mode_from_env: bool,
}

impl Limits {
    fn read(home: &Path, slug: Option<&str>) -> Self {
        let value = |key: &str| {
            config::resolve(key, home, slug)
                .ok()
                .map(|(value, _)| value)
        };
        let cents = |key: &str| value(key).and_then(|value| config::auto_cap_cents(&value));
        let budget = cents("auto.budget.mainUsd");
        let agent_budget = cents("auto.budget.agentUsd");
        let cap = budget.unwrap_or(DEFAULT_BUDGET_CENTS);
        let set_ceiling = cents("auto.budget.ceilingUsd");
        Limits {
            cap,
            agent_cap: agent_budget.unwrap_or(DEFAULT_AGENT_BUDGET_CENTS),
            ceiling: set_ceiling.unwrap_or(cap.saturating_mul(CEILING_FACTOR)),
            ceiling_is_set: set_ceiling.is_some(),
            pct: WARN_PCT,
            budget_is_valid: budget.is_some(),
            agent_budget_is_valid: agent_budget.is_some(),
            mode_from_env: false,
        }
    }

    fn cap_of(&self, caller: &Caller) -> i64 {
        match caller {
            Caller::Main => self.cap,
            Caller::Agent(_) => self.agent_cap,
        }
    }

    fn warn_of(&self, caller: &Caller) -> i64 {
        (self.cap_of(caller) as f64 * self.pct as f64 / 100.0).round() as i64
    }
}

/// Who made the tool call. Claude Code puts `agent_id` in the hook input of a
/// call made inside a subagent and leaves it out for the main session. The
/// transcript path is the main one for both, and a subagent's own transcript
/// sits at `<main minus .jsonl>/subagents/agent-<id>.jsonl`.
#[derive(Clone, PartialEq, Debug)]
enum Caller {
    Main,
    Agent(String),
}

/// The transcripts the hook reads for one call.
struct Transcripts {
    main: PathBuf,
    caller: Caller,
}

impl Transcripts {
    fn of(payload: &Payload) -> Self {
        let given = PathBuf::from(payload.field(".transcript_path"));
        let id = agent_id(&payload.field(".agent_id"));
        // A transcript path that already is a subagent's own names the caller
        // and leads back to the main one.
        let (main, from_path) = match own_agent_id(&given) {
            Some((main, id)) => (main, Some(id)),
            None => (given, None),
        };
        let caller = id.or(from_path).map_or(Caller::Main, Caller::Agent);
        Transcripts { main, caller }
    }

    /// The state key of the caller's own transcript.
    fn own_key(&self) -> Option<String> {
        let path = match &self.caller {
            Caller::Main => (!self.main.as_os_str().is_empty()).then(|| self.main.clone()),
            Caller::Agent(id) => {
                let stem = self.main.to_str()?.strip_suffix(".jsonl")?;
                Some(
                    Path::new(stem)
                        .join("subagents")
                        .join(format!("agent-{id}.jsonl")),
                )
            }
        };
        path.map(|path| path.to_string_lossy().into_owned())
    }
}

/// The id without the `agent-` prefix, kept to characters that are safe in a
/// file name. `None` means no usable id.
fn agent_id(raw: &str) -> Option<String> {
    let id: String = raw
        .strip_prefix("agent-")
        .unwrap_or(raw)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    (!id.is_empty()).then_some(id)
}

/// For `<stem>/subagents/agent-<id>.jsonl`, the main transcript and the id.
fn own_agent_id(path: &Path) -> Option<(PathBuf, String)> {
    if !is_agent_transcript(path) {
        return None;
    }
    let dir = path.parent()?;
    if dir.file_name()? != "subagents" {
        return None;
    }
    let stem = dir.parent()?;
    let name = path.file_name()?.to_str()?;
    let id = agent_id(name.strip_suffix(".jsonl")?)?;
    Some((stem.with_extension("jsonl"), id))
}

/// The caller's spend in cents, or `None` when it cannot be trusted: a
/// transcript with every message priced, or else the statusline telemetry,
/// which covers the whole tree and so is an upper bound. A model missing from
/// the price table counts as free, so without telemetry that cost is a guess.
/// A subagent whose transcript does not exist yet has spent nothing.
fn spend_cents(state: &State, transcripts: &Transcripts) -> Option<i64> {
    let telemetry = state
        .telemetry
        .as_ref()
        .map(|t| t.cents.saturating_sub(t.baseline_cents));
    let entry = transcripts
        .own_key()
        .and_then(|key| state.transcript.files.get(&key));
    // The agent's file is missing: it has not started writing yet, which is
    // only believed while the main transcript is readable. Otherwise the
    // path may simply be wrong, and the spend is unknown.
    let main_is_read = state
        .transcript
        .files
        .contains_key(&transcripts.main.to_string_lossy().into_owned());
    match (entry, &transcripts.caller) {
        (Some(file), _) if !file.unpriced => {
            Some(to_cents(file.usd).saturating_sub(file.baseline_cents))
        }
        (None, Caller::Agent(_)) if transcripts.own_key().is_some() && main_is_read => Some(0),
        _ => telemetry,
    }
}

fn enforce(payload: &Payload, limits: &Limits, root: &Path) {
    let transcripts = Transcripts::of(payload);
    let caller = &transcripts.caller;
    let dir = PathBuf::from(session_dir(payload));
    let park_note = (!dir.as_os_str().is_empty()).then(|| dir.join(park_note_name(caller)));
    let call = ToolCall::of(payload);
    let state = refresh(payload);
    let spend = state
        .as_ref()
        .and_then(|state| spend_cents(state, &transcripts));
    let guard = call.changes_the_guard(root);
    let (Some(state), Some(spend)) = (state, spend) else {
        let allowed = call.is_allowed_when_parked(park_note.as_deref());
        if allowed && !guard || call.is_mode_escape() {
            return;
        }
        if !allowed && *caller == Caller::Main {
            set_parked(&dir, true);
        }
        emit_pre_deny(&if allowed {
            SELF_PROTECTION_REASON.to_string()
        } else {
            unreadable_reason(limits.mode_from_env)
        });
        return;
    };
    let cap = limits.cap_of(caller);
    let over_ceiling = state.effective_cents >= limits.ceiling;
    if *caller == Caller::Main {
        set_parked(&dir, spend >= cap || over_ceiling);
    }
    if spend >= cap || over_ceiling {
        if !call.is_allowed_when_parked(park_note.as_deref()) {
            let note = park_note.unwrap_or_default();
            let reason = if spend >= cap {
                budget_reached_reason(caller, limits, &note)
            } else {
                ceiling_reason(limits.ceiling, &note, limits.mode_from_env)
            };
            emit_pre_deny(&reason);
        } else if guard {
            emit_pre_deny(SELF_PROTECTION_REASON);
        }
        return;
    }
    if guard {
        emit_pre_deny(SELF_PROTECTION_REASON);
        return;
    }
    let suffix = match caller {
        Caller::Main => String::new(),
        Caller::Agent(id) => format!("-{id}"),
    };
    let mut notes = Vec::new();
    if !limits.budget_is_valid && claim(&dir.join("budget-note")) {
        notes.push(invalid_budget_note(
            "auto.budget.mainUsd",
            "cap",
            DEFAULT_BUDGET_CENTS,
        ));
    }
    if !limits.agent_budget_is_valid && claim(&dir.join("agent-budget-note")) {
        notes.push(invalid_budget_note(
            "auto.budget.agentUsd",
            "agent cap",
            DEFAULT_AGENT_BUDGET_CENTS,
        ));
    }
    if spend >= limits.warn_of(caller) && claim(&dir.join(format!("warned{suffix}"))) {
        notes.push(warn_note(limits, caller));
    }
    if !notes.is_empty() {
        emit_pre_context("PreToolUse", &notes.join(" "));
    }
}

/// Creates or removes the marker. Best effort: a session dir that cannot be
/// written to has nothing to protect.
fn set_parked(dir: &Path, parked: bool) {
    if dir.as_os_str().is_empty() {
        return;
    }
    let marker = dir.join(PARKED_MARKER);
    if parked {
        let _ = File::create(marker);
    } else {
        let _ = fs::remove_file(marker);
    }
}

fn park_note_name(caller: &Caller) -> String {
    match caller {
        Caller::Main => "park-note.md".to_string(),
        Caller::Agent(id) => format!("park-note-{id}.md"),
    }
}

const UNREADABLE_REASON: &str = "cost unreadable: the spend can't be checked. Stop here and report to the user. Reading files, the closing steps (commit, push, pull request) and a park note still work, and `playbook mode ask` is allowed.";
const UNREADABLE_REASON_ENV: &str = "cost unreadable: the spend can't be checked. Stop here and report to the user. Reading files, the closing steps (commit, push, pull request) and a park note still work. PLAYBOOK_MODE is set in the environment, so `playbook mode ask` will not lift this: tell the user to unset PLAYBOOK_MODE and restart the session.";

fn unreadable_reason(mode_from_env: bool) -> String {
    if mode_from_env {
        UNREADABLE_REASON_ENV
    } else {
        UNREADABLE_REASON
    }
    .to_string()
}

/// How the user can lift a guard: `playbook mode ask` when auto comes from the
/// config, or unsetting the variable when it comes from the environment.
fn user_exit(mode_from_env: bool) -> &'static str {
    if mode_from_env {
        "unset PLAYBOOK_MODE in the environment and restart the session"
    } else {
        "run `playbook mode ask`"
    }
}
const SELF_PROTECTION_REASON: &str =
    "Auto mode settings are locked while auto is on. Ask the user to run this command themselves (in Claude Code, the `!` prefix runs it in the session).";

/// The one-time note for a budget key whose stored value is not a number of
/// at least one cent, so the built-in default applies.
fn invalid_budget_note(key: &str, what: &str, default_cents: i64) -> String {
    format!(
        "{key} must be a number of at least 0.01, so the default {what} of {} applies. Ask the user to run `playbook config set {key} <usd>`, since the model cannot change it.",
        dollars(default_cents)
    )
}

fn dollars(cents: i64) -> String {
    format!("${:.2}", cents as f64 / 100.0)
}

/// The command that doubles a cap, in whole dollars, for a denial to name.
/// When a set ceiling is below the doubled cap, `config set` would refuse
/// the budget, so the command raises the ceiling to the same value first.
fn raise_command(key: &str, cents: i64, set_ceiling: Option<i64>) -> String {
    let doubled = cents.saturating_mul(2).saturating_add(99) / 100;
    match set_ceiling {
        Some(ceiling) if ceiling < doubled.saturating_mul(100) => format!(
            "playbook config set auto.budget.ceilingUsd {doubled} && playbook config set {key} {doubled}"
        ),
        _ => format!("playbook config set {key} {doubled}"),
    }
}

fn budget_reached_reason(caller: &Caller, limits: &Limits, park_note: &Path) -> String {
    let cap = limits.cap_of(caller);
    let set_ceiling = limits.ceiling_is_set.then_some(limits.ceiling);
    let (who, key) = match caller {
        Caller::Main => ("the main session", "auto.budget.mainUsd"),
        Caller::Agent(_) => ("this agent", "auto.budget.agentUsd"),
    };
    format!(
        "budget reached: {who} has spent its {} cap, which is `{key}`. New work is denied. If there is finished work to deliver, the closing steps still work (commit, push and pull request through the playbook skills). Write a park note to {} describing where you stopped, then report to the user. Tell the user which cap was hit and that they can raise it by running `{}` themselves (any number of dollars works), or {}.",
        dollars(cap),
        park_note.display(),
        raise_command(key, cap, set_ceiling),
        user_exit(limits.mode_from_env)
    )
}

fn ceiling_reason(ceiling: i64, park_note: &Path, mode_from_env: bool) -> String {
    format!(
        "session ceiling reached: the agents together have spent the {} ceiling, which is `auto.budget.ceilingUsd` (0 means ten times auto.budget.mainUsd), so every agent is parked and new work is denied. The closing steps still work (commit, push and pull request through the playbook skills). Write a park note to {} describing where you stopped, then report to the user. Tell the user the session ceiling was hit and that they can raise it by running `{}` themselves (any number of dollars works), or {}.",
        dollars(ceiling),
        park_note.display(),
        raise_command("auto.budget.ceilingUsd", ceiling, None),
        user_exit(mode_from_env)
    )
}

fn warn_note(limits: &Limits, caller: &Caller) -> String {
    let who = match caller {
        Caller::Main => "the main session",
        Caller::Agent(_) => "this agent",
    };
    format!(
        "AUTO MODE budget: {}% of {who}'s {} cap is spent. Finish the current step, then report to the user before the cap parks {who}.",
        limits.pct,
        dollars(limits.cap_of(caller))
    )
}

/// Creates `marker` exclusively, so of concurrent hook processes only one wins.
fn claim(marker: &Path) -> bool {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)
        .is_ok()
}

struct ToolCall {
    tool: String,
    command: String,
    file_path: String,
    skill: String,
    cwd: String,
}

impl ToolCall {
    fn of(payload: &Payload) -> Self {
        ToolCall {
            tool: payload.field(".tool_name"),
            command: payload.field(".tool_input.command"),
            file_path: payload.field(".tool_input.file_path"),
            skill: payload.field(".tool_input.skill"),
            cwd: payload.field(".cwd"),
        }
    }

    /// What stays allowed once a caller is parked or its cost is unreadable:
    /// reading, the closing steps that deliver finished work, and the
    /// caller's own park note.
    fn is_allowed_when_parked(&self, park_note: Option<&Path>) -> bool {
        match self.tool.as_str() {
            tool if SAFE_TOOLS.contains(&tool) => true,
            "Bash" => closing::is_closing_command_in(&self.command, Some(Path::new(&self.cwd))),
            "Skill" => closing::is_closing_skill(&self.skill),
            "Write" => park_note.is_some_and(|note| is_same_path(Path::new(&self.file_path), note)),
            _ => false,
        }
    }

    fn is_mode_escape(&self) -> bool {
        self.tool == "Bash" && self.command == MODE_ESCAPE
    }

    fn changes_the_guard(&self, root: &Path) -> bool {
        match self.tool.as_str() {
            "Bash" => is_guarded_command(&self.command),
            "Write" | "Edit" => is_config_file(Path::new(&self.file_path), root),
            _ => false,
        }
    }
}

/// True when some simple command in `command` runs `playbook` to switch the
/// mode, to set `mode`, `auto.*`, `fix.*` or `watch.*`, to import a config document, or
/// names the config database. Quoted text is data, so prose
/// that merely mentions such a command is left alone.
///
/// This is a guardrail against the model drifting, not a security boundary.
/// It does not look into `bash -c`, `eval` or a script fed to a shell through
/// a heredoc, nor into `$(...)` inside double quotes, and it cannot see what
/// variables, aliases or functions expand to.
fn is_guarded_command(command: &str) -> bool {
    simple_commands(command)
        .iter()
        .any(|words| is_guarded_invocation(words))
}

fn is_guarded_invocation(words: &[String]) -> bool {
    // A write to the config store can change guarded settings. Reading it is
    // fine, the same rule the policy guard applies.
    let home = std::env::var("HOME").unwrap_or_default();
    if super::policy_guard::writes_config_store(words, &home) {
        return true;
    }
    let Some(start) = program_index(words) else {
        return false;
    };
    let mut args = &words[start + 1..];
    match program_name(&words[start]).as_str() {
        "playbook" => {}
        "cargo" => match cargo_run_args(args) {
            Some(rest) => args = rest,
            None => return false,
        },
        _ => return false,
    }
    match args {
        [mode, level, ..] if mode == "mode" => level == "ask" || level == "auto",
        [config, import, ..] if config == "config" && import == "import" => true,
        [config, set, rest @ ..] if config == "config" && set == "set" => rest
            .iter()
            .find(|word| !word.starts_with('-'))
            .is_some_and(|key| {
                key == "mode"
                    || key.starts_with("auto.")
                    || key.starts_with("fix.")
                    || key.starts_with("watch.")
            }),
        _ => false,
    }
}

/// What follows `--` in `cargo run ... -- <args>`, which a workspace binary
/// receives as its own arguments.
pub(super) fn cargo_run_args(args: &[String]) -> Option<&[String]> {
    let subcommand = args.iter().find(|word| !word.starts_with(['-', '+']))?;
    if subcommand != "run" && subcommand != "r" {
        return None;
    }
    let dashes = args.iter().position(|word| word == "--")?;
    Some(&args[dashes + 1..])
}

/// The three config tier files under `root`, with the owner and repo as
/// wildcards. `.` and `..` are resolved lexically, never on disk, and case is
/// ignored because the default macOS and Windows filesystems ignore it.
pub(super) fn is_config_file(path: &Path, root: &Path) -> bool {
    let lowercase = |path: &Path| PathBuf::from(path.to_string_lossy().to_lowercase());
    let (path, root) = (lowercase(path), lowercase(root));
    let mut normal = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            part => normal.push(part),
        }
    }
    let Ok(rel) = normal.strip_prefix(&root) else {
        return false;
    };
    let parts: Vec<&str> = rel
        .components()
        .map(|part| part.as_os_str().to_str().unwrap_or_default())
        .collect();
    matches!(
        parts.as_slice(),
        ["config.json" | "playbook.db" | "playbook.db-wal" | "playbook.db-shm"]
            | ["data", "config.db" | "config.db-wal" | "config.db-shm"]
            | ["data", "memory.db" | "memory.db-wal" | "memory.db-shm"]
            | ["orgs", _, "config.json"]
            | ["repos", _, _, ".config", "config.json"]
    )
}

/// Compares paths lexically, ignoring `.` segments. A `..` never matches, and
/// the path is not resolved on disk, so a symlinked home still compares equal.
fn is_same_path(path: &Path, expected: &Path) -> bool {
    !path.components().any(|part| part == Component::ParentDir)
        && path.components().eq(expected.components())
}

fn refresh(payload: &Payload) -> Option<State> {
    let dir = PathBuf::from(session_dir(payload));
    if dir.as_os_str().is_empty() {
        return None;
    }
    let state_path = dir.join(STATE_FILE);
    let now = crate::common::time::now_ms();
    if let Some(state) = load::<State>(&state_path) {
        if now.saturating_sub(state.computed_at_ms) < interval_ms() {
            return Some(state);
        }
    }

    let lock = dir.join("auto-cost.lock");
    let (acquired, state) = with_dir_lock(&lock, 50, Duration::from_millis(10), || {
        let loaded = load::<State>(&state_path);
        let has_baseline = loaded.is_some();
        let mut state = loaded.unwrap_or_default();
        recompute(&mut state, payload, &dir, now);
        let persisted = write_atomic(&state_path, &state);
        // A baseline that cannot be stored would be retaken on every call, so
        // the spend would never grow past it.
        (persisted || has_baseline).then_some(state)
    });
    if acquired {
        let _ = fs::remove_dir(&lock);
    }
    state
}

fn recompute(state: &mut State, payload: &Payload, dir: &Path, now: u64) {
    let first_call = state.computed_at_ms == 0;
    for path in transcript_files(payload) {
        advance_file(&mut state.transcript.files, &path, first_call);
    }
    if let Some(cents) = telemetry_cents(&dir.join("telemetry.jsonl")) {
        state
            .telemetry
            .get_or_insert(Telemetry {
                baseline_cents: cents,
                cents,
            })
            .cents = cents;
    }
    state.effective_cents = effective_cents(state);
    state.computed_at_ms = now;
}

fn effective_cents(state: &State) -> i64 {
    let files = &state.transcript.files;
    let transcript = (!files.is_empty()).then(|| {
        files
            .values()
            .map(|f| to_cents(f.usd).saturating_sub(f.baseline_cents))
            .fold(0_i64, i64::saturating_add)
    });
    let telemetry = state
        .telemetry
        .as_ref()
        .map(|t| t.cents.saturating_sub(t.baseline_cents));
    transcript.into_iter().chain(telemetry).max().unwrap_or(0)
}

fn to_cents(usd: f64) -> i64 {
    (usd * 100.0).round() as i64
}

/// Every transcript of the tree: the main one and each subagent's own.
fn transcript_files(payload: &Payload) -> Vec<PathBuf> {
    let main = Transcripts::of(payload).main;
    let Some(stem) = main.to_str().and_then(|main| main.strip_suffix(".jsonl")) else {
        return if main.as_os_str().is_empty() {
            Vec::new()
        } else {
            vec![main]
        };
    };
    let subagents = fs::read_dir(Path::new(stem).join("subagents"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_agent_transcript(path));
    let mut files = vec![main.clone()];
    files.extend(subagents);
    files
}

fn is_agent_transcript(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("agent-") && name.ends_with(".jsonl"))
}

/// Reads the complete lines appended to `path` since its stored offset. A file
/// that shrank or was replaced restarts from zero but keeps its baseline.
fn advance_file(files: &mut BTreeMap<String, FileCost>, path: &Path, first_call: bool) {
    let Ok(mut file) = File::open(path) else {
        return;
    };
    let Ok(meta) = file.metadata() else {
        return;
    };
    let key = path.to_string_lossy().into_owned();
    let is_new = !files.contains_key(&key);
    let mut entry = files.get(&key).cloned().unwrap_or_default();
    let inode = inode_of(&meta);
    if meta.len() < entry.offset || (!is_new && entry.inode != inode) {
        entry.restart();
    }
    entry.inode = inode;

    let mut buf = Vec::new();
    if file.seek(SeekFrom::Start(entry.offset)).is_err() || file.read_to_end(&mut buf).is_err() {
        return;
    }
    let complete = buf
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(&buf[..0], |last| &buf[..=last]);
    let batch = read_batch(complete, &entry);
    entry.usd += batch.usd;
    entry.unpriced |= batch.unpriced;
    if let Some((id, usd)) = batch.last {
        entry.last_id = id;
        entry.last_usd = usd;
    }
    entry.offset = entry.offset.saturating_add(complete.len() as u64);
    if is_new && first_call {
        entry.baseline_cents = to_cents(entry.usd);
    }
    files.insert(key, entry);
}

#[cfg(unix)]
fn inode_of(meta: &fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(meta)
}

#[cfg(not(unix))]
fn inode_of(_meta: &fs::Metadata) -> u64 {
    0
}

/// What one read added to a file: the new cost, whether any of it is
/// unpriced, and the last message seen with the cost now counted for it.
struct Batch {
    usd: f64,
    unpriced: bool,
    last: Option<(String, f64)>,
}

/// A streamed message repeats its usage on several lines, so only the largest
/// cost per message id counts. When the first message of the read continues
/// the last one of the previous read, only the growth beyond `prior.last_usd`
/// is added.
fn read_batch(lines: &[u8], prior: &FileCost) -> Batch {
    let mut by_id: BTreeMap<String, f64> = BTreeMap::new();
    let mut last_id = None;
    let mut unpriced = false;
    for line in lines.split(|&b| b == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let Some(message) = message_cost(&value) else {
            continue;
        };
        unpriced |= message.unpriced;
        let slot = by_id.entry(message.id.clone()).or_insert(message.usd);
        *slot = slot.max(message.usd);
        last_id = Some(message.id);
    }
    let usd = by_id
        .iter()
        .map(|(id, usd)| match *id == prior.last_id {
            true => (usd - prior.last_usd).max(0.0),
            false => *usd,
        })
        .sum();
    let last = last_id.map(|id| {
        let counted = by_id[&id];
        let counted = match id == prior.last_id {
            true => counted.max(prior.last_usd),
            false => counted,
        };
        (id, counted)
    });
    Batch {
        usd,
        unpriced,
        last,
    }
}

struct MessageCost {
    id: String,
    usd: f64,
    unpriced: bool,
}

fn message_cost(line: &Value) -> Option<MessageCost> {
    if line.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let message = line.get("message")?;
    let usage = message.get("usage")?;
    let mut event = UsageEvent {
        model: message.get("model")?.as_str()?.to_string(),
        input_tokens: token(usage, "input_tokens"),
        output_tokens: token(usage, "output_tokens"),
        cache_creation_tokens: token(usage, "cache_creation_input_tokens"),
        cache_creation_1h_tokens: one_hour_cache_tokens(usage),
        cache_read_tokens: token(usage, "cache_read_input_tokens"),
        ..UsageEvent::default()
    };
    event.apply_pricing();
    Some(MessageCost {
        id: message.get("id")?.as_str()?.to_string(),
        usd: event.cost_usd,
        unpriced: event.unpriced,
    })
}

/// The `cost_usd` of the newest telemetry line, read from the file's tail.
fn telemetry_cents(path: &Path) -> Option<i64> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TELEMETRY_TAIL_BYTES)))
        .ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    buf.split(|&b| b == b'\n')
        .rev()
        .find_map(|line| {
            serde_json::from_slice::<Value>(line)
                .ok()?
                .get("cost_usd")?
                .as_f64()
        })
        .map(to_cents)
}

fn interval_ms() -> u64 {
    parse_interval(std::env::var(INTERVAL_VAR).ok().as_deref())
}

/// The recompute interval for a raw env value: the default when unset, empty
/// or not a number.
fn parse_interval(raw: Option<&str>) -> u64 {
    raw.and_then(|raw| raw.trim().parse().ok())
        .unwrap_or(DEFAULT_INTERVAL_MS)
}

fn load<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Writes to a sibling temp file and renames it over `path`, so a concurrent
/// reader sees the old or the new file, never a torn one. Returns whether the
/// file was stored.
fn write_atomic(path: &Path, value: &impl Serialize) -> bool {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return false;
    };
    crate::common::atomic::write_atomic(path, bytes).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_cannot_set_a_watch_key_in_auto_mode() {
        assert!(is_guarded_command(
            "playbook config set watch.autonomy auto"
        ));
        assert!(is_guarded_command(
            "playbook config set --global watch.rule.triage '{}'"
        ));
        assert!(!is_guarded_command("playbook config get watch.autonomy"));
    }

    #[test]
    fn the_raise_command_doubles_the_cap_in_whole_dollars() {
        let rows = [
            ("auto.budget.mainUsd", 20_000, None, "playbook config set auto.budget.mainUsd 400"),
            ("auto.budget.agentUsd", 5_000, None, "playbook config set auto.budget.agentUsd 100"),
            ("auto.budget.mainUsd", 250, None, "playbook config set auto.budget.mainUsd 5"),
            ("auto.budget.mainUsd", 1, None, "playbook config set auto.budget.mainUsd 1"),
            ("auto.budget.mainUsd", 20_000, Some(40_000), "playbook config set auto.budget.mainUsd 400"),
            (
                "auto.budget.mainUsd",
                20_000,
                Some(30_000),
                "playbook config set auto.budget.ceilingUsd 400 && playbook config set auto.budget.mainUsd 400",
            ),
        ];
        for (key, cents, ceiling, want) in rows {
            assert_eq!(
                raise_command(key, cents, ceiling),
                want,
                "{cents} {ceiling:?}"
            );
        }
    }

    #[test]
    fn the_warn_percentage_is_built_in_whatever_is_stored() {
        // Arrange
        let home = crate::testing::scratch_root("auto-cost-warn-fixed");
        let root = crate::common::paths::playbook_root_from(&home);
        config::store::put(
            &root,
            config::write::Tier::Global,
            None,
            "auto.warnPct",
            &serde_json::json!(100),
        )
        .unwrap();

        // Act
        let limits = Limits::read(&home, None);

        // Assert
        assert_eq!(limits.pct, WARN_PCT);
    }

    #[test]
    fn the_hook_defaults_match_the_config_defaults() {
        let usd = |key| config::keys::default_value(key).and_then(|v| v.as_i64());
        assert_eq!(usd("auto.budget.mainUsd"), Some(DEFAULT_BUDGET_CENTS / 100));
        assert_eq!(
            usd("auto.budget.agentUsd"),
            Some(DEFAULT_AGENT_BUDGET_CENTS / 100)
        );
    }

    #[test]
    fn the_interval_defaults_to_two_seconds_unless_the_env_value_is_a_number() {
        let rows: [(Option<&str>, u64); 6] = [
            (None, 2000),
            (Some("abc"), 2000),
            (Some(""), 2000),
            (Some("5000"), 5000),
            (Some(" 750 "), 750),
            (Some("0"), 0),
        ];
        for (raw, want) in rows {
            assert_eq!(parse_interval(raw), want, "{raw:?}");
        }
    }
}
