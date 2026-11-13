// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Ports the retired shell original: a PreCompact hook that logs the
//! compaction event and warns the user, since PreCompact has no
//! `additionalContext` channel to speak to Claude directly.
//!
//! One divergence from the python source, non-observable: this port uses
//! `common::atomic_append` where the python hook appends with a bare `open`.
//!
//! Local system time via `localtime_r` stands in for python's
//! `time.strftime`, which also renders in the local timezone. `std` alone
//! has no timezone database, and spawning `date` cost a process per compaction.

use crate::common::atomic::with_dir_lock;
use crate::common::payload::Payload;
use crate::common::{emit_system_message, session_id};
use crate::hooks::context_hygiene::context_tokens;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Newest lines kept in `compactions.log`. Each line is about 150 bytes, so
/// this bounds the file near 300 KB while holding weeks of real compactions.
const LOG_LINE_CAP: usize = 2000;

/// Log value for a payload with no `trigger` field.
const UNKNOWN_TRIGGER: &str = "unknown";

/// Trigger shown in the user message when the payload has none.
const DEFAULT_MESSAGE_TRIGGER: &str = "auto";

/// Longest value kept for one log field, so a hostile or odd payload cannot
/// grow a line without bound.
const FIELD_MAX_CHARS: usize = 120;

/// Lock retry budget for the log append, as attempts and the pause between.
const LOCK_ATTEMPTS: u32 = 50;
const LOCK_RETRY_PAUSE: Duration = Duration::from_millis(10);

/// PreCompact entry point. Never panics: a failed log write or a failed
/// timestamp lookup still emits the user-facing warning.
///
/// Claude Code always sends `session_id` with a PreCompact event, so a call
/// without one is not a real compaction. It comes from a probe or from the
/// in-process hook tests, which dispatch every hook with malformed input
/// against the real `$HOME`. Those calls used to fill the log with empty
/// `session=` lines, four per `cargo test` run. They still warn, but they do
/// not log.
pub fn run(payload: &Payload) {
    let trigger = payload.field(".trigger");
    let sid = clean_field(&session_id(payload));

    if !sid.is_empty() {
        log_compaction(payload, &sid, &trigger);
    }

    let message_trigger = if trigger.is_empty() {
        DEFAULT_MESSAGE_TRIGGER
    } else {
        trigger.as_str()
    };
    let user_msg = format!(
        "\u{26a0} Context compaction triggered ({message_trigger}). After this point, every turn \
replays a lossy summary instead of the original transcript, so the cache \
savings are gone. Strongly consider: finish the current step, ask me to \
wrap up (a session handoff), then /clear for a fresh session and run /playbook:handoff load there if the handoff did not load."
    );
    emit_system_message(&user_msg);
}

/// One log value: control characters (tab, newline) become spaces so a field
/// cannot split a line or a column, and the length is capped.
fn clean_field(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(FIELD_MAX_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Project name for the log: the last component of the payload `cwd`.
fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .map(|name| clean_field(&name.to_string_lossy()))
        .unwrap_or_default()
}

/// The identity prefix of a log line: time, session and trigger. Two lines
/// with the same prefix are the same compaction reported twice.
fn line_key(ts: &str, sid: &str, trigger: &str) -> String {
    format!("{ts}\tsession={sid}\ttrigger={trigger}")
}

/// Build the full log line. The first three columns keep the original
/// format; `project`, `model` and `tokens` follow, empty when unknown.
/// `tokens` is the context size before the compaction, read from the newest
/// usage record at the tail of the transcript; no message text is read out.
fn build_line(payload: &Payload, key: &str) -> String {
    let transcript = payload.field(".transcript_path");
    let tokens = if transcript.is_empty() {
        None
    } else {
        context_tokens(Path::new(&transcript))
    };
    format!(
        "{key}\tproject={}\tmodel={}\ttokens={}",
        project_name(&payload.field(".cwd")),
        clean_field(&payload.field(".model")),
        tokens.map(|t| t.to_string()).unwrap_or_default(),
    )
}

/// Append one line for this compaction, unless the log already ends with the
/// same time, session and trigger (the hook ran twice for one event).
fn log_compaction(payload: &Payload, sid: &str, trigger: &str) {
    let ts = current_timestamp();
    let log_trigger = if trigger.is_empty() {
        UNKNOWN_TRIGGER.to_string()
    } else {
        clean_field(trigger)
    };
    let key = line_key(&ts, sid, &log_trigger);
    let line = build_line(payload, &key);

    let log_path = crate::common::paths::runtime_root().join("compactions.log");
    if let Some(log_path_str) = log_path.to_str() {
        // The append and the trim must serialize together, not just each
        // against itself: `common::atomic_append` releases its own lock the
        // moment it returns, so calling it and then `cap_lines` separately
        // leaves a window where another session's append lands in between,
        // and this session's trim (reading the file before that append,
        // writing after) silently drops the line that just landed. Sharing
        // one `with_dir_lock` acquisition across both closes that window.
        // This inlines the raw append instead of calling `atomic_append`,
        // since nesting two acquisitions of the same lock path from one
        // process would make the inner one retry its full budget pointlessly
        // before failing open, the lock isn't reentrant. `atomic_append`
        // also created the log's parent directory before writing; do the
        // same here, since a fresh runtime dir has nothing under it yet.
        if let Some(parent) = log_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let lock_path = PathBuf::from(format!("{log_path_str}.lock"));
        let (acquired, ()) = with_dir_lock(&lock_path, LOCK_ATTEMPTS, LOCK_RETRY_PAUSE, || {
            // Without a timestamp the key cannot tell two events apart.
            let repeat = !ts.is_empty() && last_line_starts_with(&log_path, &key);
            if !repeat {
                append_line_unlocked(&log_path, &line);
            }
            cap_lines(&log_path, LOG_LINE_CAP);
        });
        if acquired {
            let _ = fs::remove_dir(&lock_path);
        }
    }
}

/// Current local time as `%Y-%m-%d %H:%M:%S`.
pub fn current_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Append `line` plus a trailing newline to `path`, with no locking of its
/// own: the caller holds a `with_dir_lock` around this and `cap_lines`
/// together (see the call site's comment for why). Never panics; a failure
/// is swallowed, matching every other hook write in this codebase.
fn append_line_unlocked(path: &Path, line: &str) {
    if let Ok(mut opened) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(opened, "{line}");
    }
}

/// Whether the last line of the log at `path` starts with `key`. False when
/// the file is missing or unreadable.
fn last_line_starts_with(path: &Path, key: &str) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| {
            contents
                .lines()
                .next_back()
                .and_then(|l| l.strip_prefix(key))
                .map(|rest| rest.starts_with('\t'))
        })
        .unwrap_or(false)
}

/// Trim `path` down to its last `limit` lines in place, via a temp file
/// plus rename so a reader never observes a partial write. Ports
/// `_cap_lines` (the retired shell original). Never panics; any failure
/// leaves the file as it was.
fn cap_lines(path: &Path, limit: usize) {
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = contents.split_inclusive('\n').collect();
    if lines.len() <= limit {
        return;
    }
    let kept = &lines[lines.len() - limit..];
    let _ = crate::common::atomic::write_atomic(path, kept.concat());
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn a_field_loses_control_characters_and_is_length_capped() {
        // Arrange
        let raw = format!("a\tb\nc{}", "x".repeat(500));

        // Act
        let got = clean_field(&raw);

        // Assert
        assert!(!got.contains(['\t', '\n']), "{got:?}");
        assert_eq!(got.chars().count(), FIELD_MAX_CHARS);
        assert!(got.starts_with("a b c"));
    }

    #[test]
    fn the_project_is_the_last_component_of_the_cwd() {
        // Arrange, Act, Assert
        assert_eq!(project_name("/work/acme-api"), "acme-api");
        assert_eq!(project_name(""), "");
    }

    #[test]
    fn timestamp_has_the_format_the_date_command_produced() {
        // Arrange, Act
        let before = current_timestamp();
        let out = Command::new("date")
            .arg("+%Y-%m-%d %H:%M:%S")
            .output()
            .expect("date runs");
        let after = current_timestamp();
        let date = String::from_utf8_lossy(&out.stdout).trim().to_string();

        // Assert: `date` ran between the two reads, so it equals one of them.
        assert_eq!(before.len(), 19, "got {before:?}");
        assert!(
            date == before || date == after,
            "{date} vs {before}/{after}"
        );
    }
}
