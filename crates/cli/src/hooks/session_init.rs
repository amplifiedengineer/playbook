// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! SessionStart hook (ports the retired shell original). Prepares the per-session
//! runtime directory and zeroes its counters, warns on a resumed session
//! whose config has drifted since it was created, and injects SessionStart
//! additionalContext: the tool map, an auto-learn nudge, a handoff, and one small ranked
//! memory block, in that order. The harness caps a hook's context string at
//! 10,000 characters and replaces the whole string with a 2,000 character
//! preview past that, so the total is held under `CONTEXT_CAP_CHARS` and the
//! variable, least critical block (memory) comes last.
//!
//! The config hash and the memory block are both computed in process, and
//! the memory store is only ever read here.

use crate::common::mode::{resolve_for_hook, Mode, Source};
use crate::common::slug::slugify;
use crate::common::{config_hash, home_dir, repo_slug, run_with_timeout, session_dir, Payload};
use crate::hooks::memory_signals;
use crate::init::run::StepStatus;
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long to wait for a shelled-out `bash` or `git` call before giving up.
/// Matches the retired shell original's `timeout=15`.
const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(15);

/// The harness truncates a hook's `additionalContext` at 10,000 characters
/// (Claude Code hooks reference). Stay under it with margin, since a string
/// past the cap is replaced by a 2,000 character preview.
const CONTEXT_CAP_CHARS: usize = 9000;

/// Stands alone for a plain `claude` session, where no system prompt or skill
/// states the rule before the first tool choice.
const CODE_SEARCH_RULE: &str = "Code search rule: for any code question, in every session and whether or not a playbook skill or command runs, run `playbook code status` first, then `playbook code index` when it says stale or missing, then `playbook code context|search|symbol|refs|outline|read`, all before any Grep or Glob. Grep and Glob are the fallback only when the binary or the index is unavailable. This rule overrides the default advice to search with Grep or Glob. Put this rule in every `Explore` prompt.";

/// Ceiling for the ranked memory block, header included.
const MEMORY_BLOCK_CAP_CHARS: usize = 2500;

/// Smallest ranked block worth injecting. Below this the header alone would
/// eat the budget, so the block is skipped.
const MEMORY_BLOCK_MIN_CHARS: usize = 300;

/// The per-session counter/state files zeroed at the start of every session.
/// `capture-crossings` has no python counterpart, so this no longer matches the retired shell original one-for-one.
const SESSION_COUNTER_FILES: [&str; 8] = [
    "search-count",
    "tool-count",
    "edit-count",
    "edits.jsonl",
    "seen-reads",
    "read-state.jsonl",
    "capture-crossings",
    "hygiene-level",
];

/// Per-session auto mode state: the spend baseline, the warn and budget notes,
/// the park notes and the advice marker. Cleared on every start except a
/// compaction, so a resumed session starts a new count.
const AUTO_STATE_FILES: [&str; 6] = [
    "parked",
    "auto-cost.json",
    "budget-note",
    "agent-budget-note",
    "auto-advice-given",
    "park-note.md",
];
const AUTO_STATE_PREFIXES: [&str; 2] = ["warned", "park-note-"];

const DEFAULT_AUTO_LEARN_MAX_AGE_DAYS: i64 = 14;

const DRIFT_SYSTEM_MESSAGE: &str = "\u{26a0} Claude config (settings.json + hooks) has drifted \
    since this session was created. Plugins, output style, model default, and new hooks will \
    NOT take effect on this resumed session: they're frozen at the original startup snapshot. \
    To apply current config: exit and run `playbook cc fresh` (or `claude` without --resume).";

const DRIFT_EXTRA_CONTEXT: &str = "The user resumed this session, but the config hash has \
    changed since session creation. The harness has the OLD settings loaded. If the user asks \
    about why a recent settings change isn't showing up, point them to 'playbook cc fresh' or starting \
    a new `claude` invocation.";

/// Run the session-init hook: reset per-session state, warn on config drift,
/// and emit a single SessionStart payload with whatever additionalContext
/// applies. Never panics; every failure along the way degrades to "say
/// nothing" rather than breaking the session.
pub fn run(payload: &Payload) {
    let home = home_dir().to_string_lossy().into_owned();
    let dir = session_dir(payload);
    let repo_root = git_toplevel_with_plain_git_fallback();

    prepare_memory_store();
    zero_session_state(&dir);
    if payload.field(".source") != "compact" {
        clear_auto_state(&dir);
    }
    clear_statusline_cache();
    ensure_telemetry_collector();
    // Headless runs (CI, `claude -p`) have no stale worktrees, no person to nudge,
    // and no handoff to consume: they get the tool map and the auto mode note,
    // and memory only when it is opted in.
    let headless = crate::common::headless::is_headless();
    if !headless {
        maybe_sweep_worktrees(&home, &repo_root);
        maybe_gc(Path::new(&home));
    }

    let (mut system_message, mut extra_context) = check_config_drift(payload, &dir, &home);
    // Before the heal line: a started update hides the plugin fix from it.
    let plugin_lines = if headless {
        Vec::new()
    } else {
        plugin_update_lines(&home)
    };
    for line in heal_line(&home).into_iter().chain(plugin_lines) {
        push_message_line(&mut system_message, &line);
    }
    if !headless {
        if let Some(line) = missing_keys_line(&home) {
            push_message_line(&mut system_message, line);
        }
    }
    append_auto_mode_note(&mut extra_context);
    // Always on, headless included: skills ask for a task by name, and this map
    // is how the session learns which tasks this binary can do mechanically.
    push_context(&mut extra_context, crate::tools_map::render().trim_end());
    push_context(&mut extra_context, CODE_SEARCH_RULE);

    if !headless {
        append_auto_learn_nudge(&mut extra_context, &repo_root);
    }
    let loaded = if headless {
        Vec::new()
    } else {
        append_handoff_slice(
            &mut extra_context,
            &payload.field(".source"),
            &payload.field(".session_id"),
        )
    };
    for line in &loaded {
        push_message_line(&mut system_message, line);
    }
    if !headless || crate::common::headless::headless_memory_enabled() {
        append_memory_context(&mut extra_context, &repo_root);
    }
    crate::handoff::log_start(
        &payload.field(".source"),
        &payload.field(".session_id"),
        &crate::handoff::current_slug(),
        loaded.len(),
    );

    emit(&system_message, &extra_context);
}

/// Adds `line` to the user-visible message on its own line.
fn push_message_line(system_message: &mut String, line: &str) {
    if !system_message.is_empty() {
        system_message.push('\n');
    }
    system_message.push_str(line);
}

/// Runs the safe migrations when this binary is newer than the version
/// record, and lists what needs the user, the plugin checks included, as
/// `crate::pending` shows them in a session (ADR 0034).
fn heal_line(home: &str) -> Option<String> {
    crate::heal::session_line(Path::new(home))
}

/// The stored results of earlier background refreshes, shown once, then the
/// line for a plugin update started now. Without one, a detached
/// `skills sync --refresh` starts when a synced agent is behind the plugin.
/// It deletes the notices it shows and writes the claim marker, and never
/// waits: the work runs in a detached child and the hook never writes
/// skills. Any panic is dropped so the hook cannot fail.
fn plugin_update_lines(home: &str) -> Vec<String> {
    std::panic::catch_unwind(|| {
        use crate::notices::{take_notice, PLUGIN_UPDATE_KEY, SKILLS_SYNC_KEY};
        use crate::plugin_update::{
            claim, due, now, record_done, seen, settle_spawn, spawn_refresh_plugin,
            spawn_refresh_skills, Due, Mode,
        };
        let home = Path::new(home);
        let root = crate::common::paths::playbook_root_from(home);
        let mut lines: Vec<String> = [PLUGIN_UPDATE_KEY, SKILLS_SYNC_KEY]
            .into_iter()
            .filter_map(|key| take_notice(&root, key))
            .collect();
        let running = env!("CARGO_PKG_VERSION");
        let seen_before = seen(&root);
        let mut plugin_started = false;
        match due(home, seen_before.as_deref()) {
            Due::Run(Mode::Auto)
                if claim(&root, seen_before.as_deref(), running, now())
                    && settle_spawn(&root, running, spawn_refresh_plugin(), now()) =>
            {
                plugin_started = true;
                lines.push(format!(
                    "playbook: updating the playbook plugin to {running} in the background. \
                    Start Claude Code again to load it."
                ));
            }
            Due::Current => record_done(&root, running, now()),
            _ => {}
        }
        if !plugin_started && crate::skills_sync::refresh_needed(home) {
            spawn_refresh_skills();
        }
        lines
    })
    .unwrap_or_default()
}

/// The warning for a session that started without the OpenTelemetry keys, or
/// nothing. A panic is dropped so the hook cannot fail.
fn missing_keys_line(home: &str) -> Option<&'static str> {
    std::panic::catch_unwind(|| crate::cc::telemetry::missing_keys_line(Path::new(home)))
        .ok()
        .flatten()
}

/// Starts the telemetry collector when Claude Code's settings point at it and
/// none runs at this version, so plain `claude` sessions reach it. It waits a
/// short, fixed time at most and never fails the hook: any panic or error is
/// dropped, and the collector writes its own messages to `collector.log`.
fn ensure_telemetry_collector() {
    let _ = std::panic::catch_unwind(|| {
        crate::cc::telemetry::ensure_for(crate::cc::telemetry::Trigger::SessionStart)
    });
}

/// Tells the model it is running unattended and tells the user how to turn
/// it off, since a `mode` set in config persists across sessions. Runs
/// regardless of headless: headless quiets nudges, it does not end auto.
fn append_auto_mode_note(extra_context: &mut String) {
    let resolved = resolve_for_hook();
    if resolved.mode != Mode::Auto {
        return;
    }
    // `playbook mode ask` only writes the config, so it cannot undo an
    // environment variable.
    let (origin, off_switch) = match resolved.source {
        Source::Env => ("source: env", "unset `PLAYBOOK_MODE`"),
        _ => (
            "source: config, set for this repo",
            "run `playbook mode ask`",
        ),
    };
    let note = format!(
        "AUTO MODE is on ({origin}): wherever the command allows it, take the recommended \
        answer and log it as an assumption instead of asking. To turn it off, {off_switch}."
    );
    push_context(extra_context, &note);
}

/// Defensive fallback for a session starting before `playbook setup` is
/// re-run after an upgrade, so the memory store is in its current location
/// before the reads below need it.
fn prepare_memory_store() {
    let home = home_dir();
    let claude_home = home.join(".claude");
    let migration = crate::init::memory_migrate::migrate_memory(&home, &claude_home);
    if migration.status == StepStatus::Failed {
        eprintln!("memory-migrate: {}", migration.detail);
    }
    let _ = fs::create_dir_all(crate::common::paths::memory_dir());
}

/// Zeroes the per-session counter files, clears the code hint marker and stamps `start-ts`. A no-op when there is no session directory (no session id in the payload).
fn zero_session_state(dir: &str) {
    if dir.is_empty() {
        return;
    }
    let base = Path::new(dir);
    for name in SESSION_COUNTER_FILES {
        let _ = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(base.join(name));
    }
    // Removed, not truncated: the hint fires only when it can create the marker.
    let _ = fs::remove_file(base.join(super::search_counter::HINT_MARKER));
    let now = crate::common::time::now_epoch_secs();
    let _ = fs::write(base.join("start-ts"), now.to_string());
}

fn clear_auto_state(dir: &str) {
    if dir.is_empty() {
        return;
    }
    for name in AUTO_STATE_FILES {
        let _ = fs::remove_file(Path::new(dir).join(name));
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if AUTO_STATE_PREFIXES.iter().any(|p| name.starts_with(p)) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Clear the statusline PR/CI cache entries for the current repo+branch, so
/// the first render of a new session fetches fresh data. Matches
/// the retired shell original.
fn clear_statusline_cache() {
    let home = home_dir().to_string_lossy().into_owned();
    let sl_cache = std::env::var("STATUSLINE_CACHE_DIR").unwrap_or_else(|_| {
        let xdg = std::env::var("XDG_CACHE_HOME").unwrap_or_else(|_| format!("{home}/.cache"));
        format!("{xdg}/statusline")
    });
    let branch = git_branch();
    if branch.is_empty() || branch == "HEAD" {
        return;
    }
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let slug = slugify(&format!("{cwd}::{branch}"));
    for prefix in ["pr", "ci"] {
        let _ = fs::remove_file(Path::new(&sl_cache).join(format!("{prefix}-{slug}.json")));
    }
}

/// Resume-only config-drift check: computes the current config hash,
/// compares it against the hash
/// stored at session creation, and returns the `(system_message,
/// extra_context)` warning pair to emit if they differ. Always refreshes
/// the stored hash on a `startup` source, which becomes the new baseline.
/// Matches the retired shell original.
fn check_config_drift(payload: &Payload, dir: &str, home: &str) -> (String, String) {
    let current_hash = config_hash(&Path::new(home).join(".claude"));
    if current_hash.is_empty() || dir.is_empty() {
        return (String::new(), String::new());
    }
    let hash_file = Path::new(dir).join("config-hash");
    let source = payload.field(".source");

    let mut system_message = String::new();
    let mut extra_context = String::new();
    if source == "resume" {
        let prev_hash = fs::read_to_string(&hash_file)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !prev_hash.is_empty() && prev_hash != current_hash {
            system_message = DRIFT_SYSTEM_MESSAGE.to_string();
            extra_context = DRIFT_EXTRA_CONTEXT.to_string();
        }
    }

    if source == "startup" {
        let _ = fs::write(&hash_file, &current_hash);
    }

    (system_message, extra_context)
}

/// Reads `memory.db` and `memory.signals.json` (read only) and injects
/// one small ranked block of the repo's most relevant facts. Nothing is added
/// when there is no repo, no slug, no readable graph, nothing ranks, or the
/// rest of the context has already used the budget.
fn append_memory_context(extra_context: &mut String, repo_root: &str) {
    let mem_slug = repo_slug();
    if repo_root.is_empty() || mem_slug.is_empty() {
        return;
    }
    let mem_dir = crate::common::paths::memory_dir();
    let Some(content) = crate::hooks::rebuild_memory_graph::graph_text(&mem_dir) else {
        return;
    };
    let Some(graph) = crate::json::memorycontext::parse_graph(&content) else {
        return;
    };
    let room = CONTEXT_CAP_CHARS
        .saturating_sub(extra_context.chars().count() + BLOCK_SEPARATOR_CHARS)
        .min(MEMORY_BLOCK_CAP_CHARS);
    if room < MEMORY_BLOCK_MIN_CHARS {
        return;
    }
    let header = format!(
        "Top memory facts for this repo ({mem_slug}), ranked by pins, use and links. Prompt and \
        edit recall surface the rest. Facts live in ~/.config/playbook/memory/{mem_slug}/."
    );
    let signals = memory_signals::ranking_signals(&mem_dir);
    let block =
        crate::json::memorycontext::render_ranked(&graph, &mem_slug, &signals, &header, room);
    if !block.is_empty() {
        push_context(extra_context, &block);
    }
}

/// ADR 0008 WU-3: reload persisted session-handoffs (saved by
/// `playbook handoff save`) at every `SessionStart`, including
/// `source: "clear"` and `"compact"`: this is called unconditionally regardless
/// of `.source`.
/// The shared `crate::handoff` module keys, loads, and archives them: up to
/// three fresh ones are injected and tombstoned by gc, stale and
/// over-cap ones are removed. Returns one `Loaded handoff:` line per injected
/// handoff, for the user-visible message. Never panics.
fn append_handoff_slice(extra_context: &mut String, source: &str, session_id: &str) -> Vec<String> {
    let slug = crate::handoff::current_slug();
    if slug.is_empty() {
        return Vec::new();
    }
    let taken = crate::handoff::take_in(&crate::handoff::root(), &slug, SystemTime::now());
    // Leave the memory block its share of the cap. The handoff files are
    // tombstoned by gc once taken, so a cut loses nothing.
    let budget = handoff_room(extra_context).max(MIN_HANDOFF_CHARS) / taken.contents.len().max(1);
    let mut loaded = Vec::new();
    for (i, (contents, path)) in taken.contents.iter().zip(&taken.paths).enumerate() {
        loaded.push(crate::handoff::loaded_line(contents, path));
        let contents = &fit_handoff(contents, budget);
        let ctx = if taken.total > 1 {
            format!(
                "Handoff from a previous session in this directory \
                ({} of {} found):\n\n{contents}",
                i + 1,
                taken.total
            )
        } else {
            format!("Handoff from your previous session in this directory:\n\n{contents}")
        };
        push_context(extra_context, &ctx);
    }
    let resumed = match source {
        "clear" => append_cleared_handoff(extra_context, &slug),
        "compact" => append_compacted_handoff(extra_context, &slug, session_id),
        _ => None,
    };
    loaded.extend(resumed);
    loaded
}

/// Ceiling for the summary of the handoff a context hygiene nudge asked for.
/// The full text stays on disk at the path the context names.
const CLEARED_HANDOFF_CAP_CHARS: usize = 3000;

/// After `/clear`: the handoff the ended session saved for itself, cut to a
/// short summary with a pointer to the full file. Only that session's own
/// handoff loads (see `crate::handoff::take_cleared_in`), so a parallel
/// session in the same directory never receives it.
fn append_cleared_handoff(extra_context: &mut String, slug: &str) -> Option<String> {
    let cleared =
        crate::handoff::take_cleared_in(&crate::handoff::root(), slug, SystemTime::now())?;
    push_resumed_handoff(
        extra_context,
        "You ran /clear after saving a handoff. Resume from it:",
        &cleared,
    );
    Some(crate::handoff::loaded_line(&cleared.text, &cleared.path))
}

/// After `/compact`: the handoff this session saved before the compact. The
/// session id survives a compact, so it picks the handoff tagged with it (see
/// `crate::handoff::take_session_in`).
fn append_compacted_handoff(
    extra_context: &mut String,
    slug: &str,
    session_id: &str,
) -> Option<String> {
    let cleared = crate::handoff::take_session_in(
        &crate::handoff::root(),
        slug,
        session_id,
        SystemTime::now(),
    )?;
    push_resumed_handoff(
        extra_context,
        "The conversation was compacted. This session saved a handoff first. Resume from it:",
        &cleared,
    );
    Some(crate::handoff::loaded_line(&cleared.text, &cleared.path))
}

/// `lead`, the handoff cut to `CLEARED_HANDOFF_CAP_CHARS`, and a pointer to the
/// full file.
fn push_resumed_handoff(extra_context: &mut String, lead: &str, handoff: &crate::handoff::Resumed) {
    let pointer = format!("Full handoff: {}", handoff.path.display());
    let room = handoff_room(extra_context)
        .saturating_sub(pointer.chars().count() + 200)
        .min(CLEARED_HANDOFF_CAP_CHARS);
    let body = fit_handoff(&handoff.text, room);
    push_context(extra_context, &format!("{lead}\n\n{body}\n\n{pointer}"));
}

/// Characters left for handoffs once the memory block has its share.
fn handoff_room(extra_context: &str) -> usize {
    CONTEXT_CAP_CHARS.saturating_sub(extra_context.chars().count() + MEMORY_BLOCK_CAP_CHARS)
}

/// Room a handoff keeps even when the rest of the context is large.
const MIN_HANDOFF_CHARS: usize = 3000;

/// Characters a `push_context` separator adds.
const BLOCK_SEPARATOR_CHARS: usize = 2;

const HANDOFF_CUT_NOTE: &str =
    "\n[handoff cut to fit the hook output cap; the full text is in the gc tombstone folder; run `playbook handoff load`]";

/// `text`, or its leading `budget` characters plus a note when longer.
fn fit_handoff(text: &str, budget: usize) -> String {
    if text.chars().count() <= budget {
        return text.to_string();
    }
    let head: String = text.chars().take(budget).collect();
    format!("{head}{HANDOFF_CUT_NOTE}")
}

/// Nudge the user to refresh project memory if a previous session queued an
/// auto-learn flag for this repo. Prunes stale flags first. Matches
/// the retired shell original.
fn append_auto_learn_nudge(extra_context: &mut String, repo_root: &str) {
    if std::env::var("AUTO_LEARN_NUDGE").unwrap_or_else(|_| "1".to_string()) == "0" {
        return;
    }
    if repo_root.is_empty() {
        return;
    }
    let qdir = crate::common::paths::runtime_root().join("to-learn");
    // Trim before parsing: python's `int(...)` strips surrounding
    // whitespace, so a padded value must parse the same way here rather
    // than silently falling back to the default. Matches
    // the retired shell original.
    let max_age_days = std::env::var("AUTO_LEARN_MAX_AGE_DAYS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_AUTO_LEARN_MAX_AGE_DAYS);
    prune_old(&qdir, max_age_days);

    let learn_flag = qdir.join(format!("{}.json", slugify(repo_root)));
    if !learn_flag.is_file() {
        return;
    }
    let edits = learn_flag_edits(&learn_flag);
    let nudge = format!(
        "A previous session in this repo made {edits} edits, so project memory may be stale. \
        Consider running /playbook:learn to refresh it, or /playbook:learn --stage to queue \
        candidate facts for review."
    );
    push_context(extra_context, &nudge);
    let _ = fs::remove_file(&learn_flag);
}

/// Remove `*.json` flags older than `max_age_days` from `qdir`. Silently
/// does nothing if `qdir` does not exist. Never panics.
fn prune_old(qdir: &Path, max_age_days: i64) {
    let Ok(entries) = fs::read_dir(qdir) else {
        return;
    };
    let seconds = (max_age_days.max(0) as u64).saturating_mul(86400);
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(seconds))
        .unwrap_or(UNIX_EPOCH);
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        if let Ok(modified) = metadata.modified() {
            if modified < cutoff {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

/// The `edits` count to report in the auto-learn nudge: the flag file's
/// `edits` field if it parses as a JSON object, `"0"` if the field is
/// absent, `"some"` on any read or parse failure. Matches
/// the retired shell original.
fn learn_flag_edits(path: &Path) -> String {
    let Ok(contents) = fs::read_to_string(path) else {
        return "some".to_string();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return "some".to_string();
    };
    let Some(object) = value.as_object() else {
        return "some".to_string();
    };
    match object.get("edits") {
        None => "0".to_string(),
        Some(serde_json::Value::Number(n)) => n
            .as_i64()
            .map(|i| i.to_string())
            .unwrap_or_else(|| n.to_string()),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Append `addition` to `ctx`, separated by a blank line if `ctx` already
/// has content. Matches the `extra_context = extra_context + "\n\n" + x if
/// extra_context else x` pattern repeated through the retired shell original.
fn push_context(ctx: &mut String, addition: &str) {
    if ctx.is_empty() {
        ctx.push_str(addition);
    } else {
        ctx.push_str("\n\n");
        ctx.push_str(addition);
    }
}

/// How often the periodic sweep this hook triggers may run, matching
/// `cc::worktree::cleanup_due`'s own daily interval.
const WORKTREE_SWEEP_INTERVAL_SECS: i64 = 86_400;

/// The `state` table key of the rate-limit marker for the periodic worktree
/// sweep, one per repo (it used to be one file per repo): the sweep
/// itself is scoped to one `repo_root` per call, so a single machine-wide
/// marker would let whichever repo's `SessionStart` fires first after the
/// window claims the slot for every other repo too, regardless of how long
/// it has actually been since each one was last swept.
pub fn worktree_sweep_marker_key(repo_root: &Path) -> String {
    crate::state::sweep_key(repo_root)
}

/// Whether the periodic sweep is due, given the marker's mtime. Mirrors
/// [`crate::cc::worktree::cleanup_due`]'s own signature and rationale:
/// `None` (no marker yet) counts as due, so a machine that has never swept
/// sweeps on its very first `SessionStart`.
pub fn worktree_sweep_due(marker_mtime_epoch: Option<i64>, now_epoch: i64) -> bool {
    match marker_mtime_epoch {
        None => true,
        Some(stamped) => now_epoch - stamped >= WORKTREE_SWEEP_INTERVAL_SECS,
    }
}

/// Runs `playbook worktree sweep` at most once every
/// [`WORKTREE_SWEEP_INTERVAL_SECS`], skipping entirely, with no sweep
/// attempt and no marker write, when `worktree.cleanup.enabled` resolves
/// false: writing the marker in that case would make enabling the policy
/// later wait out a stale marker before its first real sweep. Degrades
/// silently on any failure (config error, no repo, sweep error), matching
/// this hook's own "never break the session" contract.
///
/// This overlaps, deliberately, with the playbook cc launcher's own eager sweep
/// (`cc::worktree_run::run_housekeep`, unthrottled): a repo worked in
/// through `playbook cc launch worktree` gets swept on every invocation there and again
/// here at most daily, while a repo only ever opened directly relies on
/// this path alone. The redundancy is accepted as defense in depth, not an
/// oversight.
fn maybe_sweep_worktrees(home: &str, repo_root: &str) {
    if repo_root.is_empty() {
        return;
    }
    let home = Path::new(home);
    let slug = repo_slug();
    let repo_slug_opt = (!slug.is_empty()).then_some(slug.as_str());

    // Reuses `resolve_policy` rather than a separate hand-rolled resolve, so
    // a wrong-typed config value (a hand-edited tier file, say) fails closed
    // here the same way it would inside `sweep` itself, instead of reading
    // as enabled, running a sweep that then errors internally, and still
    // stamping the marker for the next `WORKTREE_SWEEP_INTERVAL_SECS`.
    let enabled = match crate::worktree::resolve_policy(home, repo_slug_opt) {
        Ok(policy) => policy.enabled,
        Err(_) => false,
    };
    if !enabled {
        return;
    }

    let now_epoch = crate::common::time::now_secs();
    let root = crate::common::paths::playbook_root_from(home);
    let marker = worktree_sweep_marker_key(Path::new(repo_root));
    let marker_mtime_epoch = crate::state::get(&root, &marker)
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok());

    if !worktree_sweep_due(marker_mtime_epoch, now_epoch) {
        return;
    }

    // Stamped BEFORE the sweep runs, matching `cleanup_stale_with`'s own
    // ordering and its stated reason (src/cc/worktree.rs): a sweep killed
    // partway through still rate-limits the next run, rather than retrying
    // the same destructive pass on every SessionStart until one finishes.
    let _ = crate::state::set(&root, &marker, &now_epoch.to_string());
    let clock = crate::gc::default_clock();
    match crate::gc::worktrees::sweep_locked(
        home,
        &*clock,
        (Path::new(repo_root), repo_slug_opt),
        false,
    ) {
        Ok(crate::gc::worktrees::SweepRun::Busy) => {
            // Nothing was swept, so give the stamp back and let the next
            // SessionStart try again instead of waiting a full interval.
            let before = marker_mtime_epoch.unwrap_or(0);
            let _ = crate::state::set(&root, &marker, &before.to_string());
        }
        Ok(crate::gc::worktrees::SweepRun::Swept(_)) => {}
        Err(err) => eprintln!("worktree-sweep: {err}"),
    }
}

/// Runs a slice of the daily gc sweep unless `gc.enabled` is off. A value of
/// the wrong type reads as off, so a hand-edited config never starts a sweep.
fn maybe_gc(home: &Path) {
    let enabled = crate::config::resolve_valid("gc.enabled", home, None)
        .ok()
        .and_then(|(value, _, _)| value.as_bool())
        .unwrap_or(false);
    let clock = crate::gc::default_clock();
    crate::gc::sweep::maybe_gc(enabled, &crate::gc::registry(home), &*clock);
}

/// `git rev-parse --show-toplevel`, trimmed. Empty outside a repo or on any
/// failure. Never panics. Unlike `hooks::git_toplevel`, the fallback runs
/// plain `git` under this hook's own timeout instead of the shared git runner.
fn git_toplevel_with_plain_git_fallback() -> String {
    if let Some(top) = std::env::current_dir()
        .ok()
        .and_then(|cwd| crate::common::gitfacts::toplevel(&cwd))
    {
        return top.to_string_lossy().into_owned();
    }
    run_git(&["--no-optional-locks", "rev-parse", "--show-toplevel"])
}

/// `git rev-parse --abbrev-ref HEAD`, trimmed. Empty outside a repo or on
/// any failure. Never panics.
fn git_branch() -> String {
    if let Some(name) = std::env::current_dir()
        .ok()
        .and_then(|cwd| crate::common::gitfacts::abbrev_head(&cwd))
    {
        return name;
    }
    run_git(&["--no-optional-locks", "rev-parse", "--abbrev-ref", "HEAD"])
}

fn run_git(args: &[&str]) -> String {
    let mut command = Command::new("git");
    command.args(args);
    match run_with_timeout(&mut command, SUBPROCESS_TIMEOUT) {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

#[derive(Serialize)]
struct SessionStartOutput<'a> {
    #[serde(rename = "systemMessage", skip_serializing_if = "Option::is_none")]
    system_message: Option<&'a str>,
    #[serde(rename = "hookSpecificOutput", skip_serializing_if = "Option::is_none")]
    hook_specific_output: Option<SessionStartContext<'a>>,
}

#[derive(Serialize)]
struct SessionStartContext<'a> {
    #[serde(rename = "hookEventName")]
    hook_event_name: &'static str,
    #[serde(rename = "additionalContext")]
    additional_context: &'a str,
}

/// Print the single SessionStart payload, or nothing at all if there is
/// nothing to say. Matches the retired shell original.
fn emit(system_message: &str, extra_context: &str) {
    if system_message.is_empty() && extra_context.is_empty() {
        return;
    }
    let output = SessionStartOutput {
        system_message: (!system_message.is_empty()).then_some(system_message),
        hook_specific_output: (!extra_context.is_empty()).then_some(SessionStartContext {
            hook_event_name: "SessionStart",
            additional_context: extra_context,
        }),
    };
    if let Ok(rendered) = serde_json::to_string(&output) {
        println!("{rendered}");
    }
}
