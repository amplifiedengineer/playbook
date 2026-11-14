// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Ports the retired shell original: a Stop hook that pauses the turn once
//! `playbook statusline` `capture-due` marker fires.

//! `{"decision":"block","reason":<text>}` asks the model to write down
//! durable facts before continuing.

//! The marker releases silently once a fact write is detected, otherwise
//! it retains for a bounded re-block; see `REBLOCK_CAP`.
//!
//! Two divergences from the python source, both driven by the "never panic"
//! rule of the port rather than a behaviour choice:
//!
//! 1. python's `rec.get("path")` appends whatever JSON value sits at
//!    `"path"`, including a non-string one, and only breaks later (an
//!    uncaught `TypeError`) when the reason text is assembled with
//!    `"- " + p`. This port instead only accepts a string `"path"` field and
//!    silently skips anything else, so a malformed `edits.jsonl` line can
//!    never crash the Stop hook.
//!
//! 2. python wraps the whole per-line scan in one outer `try`, with only
//!    `json.loads` inside an inner `try`. A line that parses as valid JSON
//!    but is not an object (a bare number, string, array, bool, or null)
//!    makes `rec.get("path")` raise `AttributeError`, which only the outer
//!    `try` catches, so the function returns an EMPTY list, discarding
//!    every path already collected and skipping every later line too. This
//!    port's `serde_json::Value::get` returns `None` for a non-object value
//!    instead of raising, so it skips just that one line and keeps
//!    scanning. This is deliberate, not merely tolerated: a hook must never
//!    lose data it already has (paths gathered from earlier, valid lines)
//!    because one line further down the log happens to be malformed. Note
//!    that a corrupted `edits.jsonl` only happens if something other than
//!    this toolkit's own trusted writer touches the file.

use crate::common::paths::memory_dir;
use crate::common::{emit_block, home_dir, session_dir, Payload};
use crate::hooks::memory_signals;
use crate::init::run::StepStatus;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_PATHS: usize = 5;
/// How many times a no-write invocation retains the marker and re-blocks
/// before it releases unconditionally.
const REBLOCK_CAP: i64 = 2;

const INTRO: &str = "Context usage in this session just crossed the capture threshold. This \
is a good moment to pause, not a problem: write down anything from this \
session worth remembering next time, such as a decision made, a gotcha \
found, or a convention confirmed, using the memory tools or store this \
project keeps. Then continue with the rest of the turn.";

const HANDOFF_NUDGE: &str = "\n\nAlso worth doing now: if this feels like a natural stopping \
point, run /playbook:handoff so the next session can pick up without re-reading \
this one.";

/// `capture-crossings` value at or above which a re-block escalates with
/// `STRONG_HANDOFF_NUDGE`, once no handoff has been written this session.
const CROSSING_ESCALATION_THRESHOLD: i64 = 3;

const STRONG_HANDOFF_NUDGE: &str = "\n\nThis session has crossed the capture threshold several \
times with no handoff saved yet; run /playbook:handoff now, before continuing, so the \
next session does not have to re-read this one.";

/// Memory graph node count at or above which a re-block also scans
/// for consolidation candidates.
const CONSOLIDATION_NODE_THRESHOLD: usize = 10;

/// A fact file's size in bytes above which it counts as oversized.
const OVERSIZED_FACT_BYTES: u64 = 2000;

/// Stop entry point. A detected write releases the marker silently;
/// otherwise it re-blocks up to `REBLOCK_CAP` times, then fails open.
pub fn run(payload: &Payload) {
    // Headless runs have no person to nudge, and a Stop hook that blocks only
    // adds turns. Interactive runs keep the bounded re-block below (ADR 0009).
    if crate::common::headless::is_headless() {
        return;
    }
    let dir = session_dir(payload);
    if dir.is_empty() {
        return;
    }
    let dir_path = Path::new(&dir);
    // A parked auto session cannot write the memory store, so asking for it
    // only wastes turns.
    if dir_path.join("parked").exists()
        && crate::common::mode::resolve_for_hook().mode == crate::common::mode::Mode::Auto
    {
        return;
    }
    let marker = dir_path.join("capture-due");
    let attempts_path = dir_path.join("capture-attempts");

    let marker_mtime = match fs::metadata(&marker) {
        Ok(meta) => match meta.modified() {
            Ok(t) => t,
            Err(_) => return release(&marker, &attempts_path),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => return,
        Err(_) => return release(&marker, &attempts_path),
    };

    // Must run before the time comparison below: it's what puts the
    // memory folder (and any graph file in it) where that comparison reads.
    let home = home_dir();
    let migration = crate::init::memory_migrate::migrate_memory(&home, &home.join(".claude"));
    if migration.status == StepStatus::Failed {
        eprintln!("memory-migrate: {}", migration.detail);
    }

    let mem_dir = memory_dir();
    let _ = fs::create_dir_all(&mem_dir);
    let write_detected = match crate::hooks::rebuild_memory_graph::graph_touched_at(&mem_dir) {
        Ok(Some(graph_mtime)) => graph_mtime > marker_mtime,
        Ok(None) => false,
        Err(_) => return release(&marker, &attempts_path),
    };
    if write_detected {
        return release(&marker, &attempts_path);
    }

    let Some(attempts) = read_attempts(&attempts_path) else {
        return release(&marker, &attempts_path);
    };
    if attempts >= REBLOCK_CAP {
        eprintln!("memory-capture: capture skipped, re-block cap reached");
        return release(&marker, &attempts_path);
    }

    let next_attempt = attempts + 1;
    let _ = fs::write(&attempts_path, next_attempt.to_string());

    let edits_path = dir_path.join("edits.jsonl");
    let unique = unique_paths_recent_first(&edits_path);
    let escalate_handoff = should_escalate_handoff(dir_path);
    let consolidation = consolidation_mention(&mem_dir);
    emit_block(&build_block_body(
        &unique,
        next_attempt,
        escalate_handoff,
        consolidation.as_deref(),
    ));
}

/// Best-effort remove both state files, releasing without blocking. Used by
/// every release path: a detected write, a fail-open error, and the cap.
fn release(marker: &Path, attempts_path: &Path) {
    let _ = fs::remove_file(marker);
    let _ = fs::remove_file(attempts_path);
}

/// Parse `capture-attempts`'s integer contents. Absent means no attempt yet
/// (`Some(0)`); unparseable or unreadable is `None`, the fail-open signal.
fn read_attempts(path: &Path) -> Option<i64> {
    match fs::read_to_string(path) {
        Ok(contents) => contents.trim().parse::<i64>().ok(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Some(0),
        Err(_) => None,
    }
}

/// Build a no-write re-block reason: `INTRO`, the edited-path listing,
/// `HANDOFF_NUDGE`, the attempt count, then `escalate_handoff`/`consolidation` if present.
fn build_block_body(
    unique: &[String],
    attempt: i64,
    escalate_handoff: bool,
    consolidation: Option<&str>,
) -> String {
    let mut body = String::from(INTRO);
    if !unique.is_empty() {
        let total = unique.len();
        let listed = &unique[..total.min(MAX_PATHS)];
        let more = total.saturating_sub(MAX_PATHS);

        let path_lines: Vec<String> = listed.iter().map(|p| format!("- {p}")).collect();
        body.push_str(
            "\n\nFiles edited this session, most recent first, worth checking \
for capture worthy facts:\n",
        );
        body.push_str(&path_lines.join("\n"));
        if more > 0 {
            body.push_str(&format!("\n...and {more} more not shown."));
        }
    }
    body.push_str(HANDOFF_NUDGE);
    body.push_str(&format!(
        "\n\nThis is re-block {attempt} of {REBLOCK_CAP} since context usage crossed the \
threshold; still no new memory fact detected."
    ));
    if escalate_handoff {
        body.push_str(STRONG_HANDOFF_NUDGE);
    }
    if let Some(text) = consolidation {
        body.push_str(text);
    }
    body
}

/// Whether a re-block reason should escalate with `STRONG_HANDOFF_NUDGE`:
/// crossings at or above the threshold, and no fresher handoff file.
fn should_escalate_handoff(session_dir: &Path) -> bool {
    let crossings = read_crossings(&session_dir.join("capture-crossings"));
    if crossings < CROSSING_ESCALATION_THRESHOLD {
        return false;
    }
    let start = read_start_ts(session_dir);
    match freshest_handoff_mtime() {
        Some(mtime) => mtime <= start,
        None => true,
    }
}

/// Parse `capture-crossings`'s integer contents; a missing or unparseable
/// file starts from 0, matching `counter.rs::incr_counter`'s own fallback.
fn read_crossings(path: &Path) -> i64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| contents.trim().parse::<i64>().ok())
        .unwrap_or(0)
}

/// The session's recorded `start-ts`, or `SystemTime::now()` when missing or
/// unparseable, so a broken start-ts can never swallow a nudge that should show.
fn read_start_ts(session_dir: &Path) -> SystemTime {
    fs::read_to_string(session_dir.join("start-ts"))
        .ok()
        .and_then(|contents| contents.trim().parse::<u64>().ok())
        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap_or_else(SystemTime::now)
}

/// Freshest mtime among this directory's unread handoff files, keyed the same
/// way as the SessionStart read side.
fn freshest_handoff_mtime() -> Option<SystemTime> {
    let slug = crate::handoff::current_slug();
    if slug.is_empty() {
        return None;
    }
    crate::handoff::freshest_mtime(&crate::handoff::root(), &slug)
}

/// Scans facts touched since `memory.signals.json`'s cursor for
/// consolidation candidates, once the store is at or above `CONSOLIDATION_NODE_THRESHOLD`.
fn consolidation_mention(mem_dir: &Path) -> Option<String> {
    let content = crate::hooks::rebuild_memory_graph::graph_text(mem_dir)?;
    let graph: Value = serde_json::from_str(&content).ok()?;
    let nodes = graph.get("nodes").and_then(Value::as_array)?;
    if nodes.len() < CONSOLIDATION_NODE_THRESHOLD {
        return None;
    }

    let cursor_since = memory_signals::read_cursor(mem_dir);
    let touched: HashSet<String> = nodes
        .iter()
        .filter_map(|n| touched_node_id(mem_dir, n, cursor_since))
        .collect();

    let superseded_count = graph
        .get("edges")
        .and_then(Value::as_array)
        .map(|edges| count_touched_superseded(edges, &touched))
        .unwrap_or(0);
    let oversized_count = nodes
        .iter()
        .filter(|n| is_touched_oversized(mem_dir, n, &touched))
        .count();

    memory_signals::advance_cursor(mem_dir);

    if superseded_count == 0 && oversized_count == 0 {
        return None;
    }
    Some(build_consolidation_text(superseded_count, oversized_count))
}

/// `node`'s id, if its fact file's mtime is newer than `cursor_since`
/// (or `cursor_since` is `None`, meaning no pass has ever run).
fn touched_node_id(mem_dir: &Path, node: &Value, cursor_since: Option<u64>) -> Option<String> {
    let id = node.get("id").and_then(Value::as_str)?;
    let file = node.get("file").and_then(Value::as_str)?;
    let mtime = fs::metadata(mem_dir.join(file)).ok()?.modified().ok()?;
    let touched = match cursor_since {
        Some(since) => mtime.duration_since(UNIX_EPOCH).ok()?.as_secs() > since,
        None => true,
    };
    touched.then(|| id.to_string())
}

/// Count of `supersedes` edges whose superseded (`to`) node is in `touched`.
fn count_touched_superseded(edges: &[Value], touched: &HashSet<String>) -> usize {
    edges
        .iter()
        .filter(|e| e.get("relation").and_then(Value::as_str) == Some("supersedes"))
        .filter_map(|e| e.get("to").and_then(Value::as_str))
        .filter(|to| touched.contains(*to))
        .count()
}

/// Whether `node` is touched and its fact file exceeds `OVERSIZED_FACT_BYTES`.
fn is_touched_oversized(mem_dir: &Path, node: &Value, touched: &HashSet<String>) -> bool {
    let Some(id) = node.get("id").and_then(Value::as_str) else {
        return false;
    };
    if !touched.contains(id) {
        return false;
    }
    node.get("file")
        .and_then(Value::as_str)
        .and_then(|file| fs::metadata(mem_dir.join(file)).ok())
        .is_some_and(|meta| meta.len() > OVERSIZED_FACT_BYTES)
}

/// The consolidation-mention sentence naming whichever candidate kinds were found.
fn build_consolidation_text(superseded_count: usize, oversized_count: usize) -> String {
    let mut parts = Vec::new();
    if superseded_count > 0 {
        parts.push(format!(
            "{superseded_count} superseded fact{} still in the store",
            if superseded_count == 1 { "" } else { "s" }
        ));
    }
    if oversized_count > 0 {
        parts.push(format!(
            "{oversized_count} fact{} grown large enough to split",
            if oversized_count == 1 { "" } else { "s" }
        ));
    }
    format!(
        "\n\nThe memory store has also grown large enough to flag a consolidation candidate \
from recent activity: {}. Worth a pass before it grows further.",
        parts.join(" and ")
    )
}

/// Unique edited paths from `edits.jsonl`, most recently edited first.
/// Ports `_unique_paths_recent_first` (the retired shell original:85): reverse
/// the append log, keep the first occurrence of each path. Never panics; a
/// missing, empty, or unreadable file yields an empty list, and any line
/// that fails to parse as JSON or lacks a string `"path"` is skipped.
fn unique_paths_recent_first(path: &Path) -> Vec<String> {
    let Ok(metadata) = fs::metadata(path) else {
        return Vec::new();
    };
    if metadata.len() == 0 {
        return Vec::new();
    }
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };

    let mut paths = Vec::new();
    for raw in contents.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(p) = record.get("path").and_then(Value::as_str) {
            paths.push(p.to_string());
        }
    }

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in paths.into_iter().rev() {
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    out
}
