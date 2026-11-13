// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! UserPromptSubmit hook: an advisory nudge when the main session's context
//! grows large. A long session replays its whole context on every message, so
//! cache reads dominate the bill. Past [`THRESHOLD_TOKENS`] this injects
//! one short message per crossing that tells the model to save a session
//! handoff itself, then tell the user it is saved and how to continue
//! (`/clear`, then `/playbook:handoff load`). It never clears or compacts
//! anything. The message does not promise an automatic resume: that happens
//! only when the SessionStart hook is installed and Claude Code runs it.
//!
//! The size is the input side of the last main-session assistant message in
//! the transcript (fresh, cache-written and cache-read input tokens). Only
//! the tail of the transcript is read, and only back to the last compaction. Any problem (no transcript, bad JSON,
//! no usage found) means silence, never an error.
//!
//! The state file `hygiene-level` in the session directory holds the last
//! level fired, where level is the size divided by the threshold. A higher
//! level fires once and is stored. A lower one is stored without firing, so
//! a session that shrinks and grows again is nudged again. The next session
//! start loads the handoff the nudge asked for when the SessionStart hook runs (see `crate::handoff`).

use crate::common::transcript_tail::is_compact_boundary;
use crate::common::{emit_prompt_context, home_dir, session_dir, session_id, Payload};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Context size in tokens past which the nudge fires, once per crossing of
/// each multiple.
pub const THRESHOLD_TOKENS: u64 = 250_000;

/// Bytes read from the end of the transcript. The newest usage record is
/// near the end, and one record can be large when a tool returned a lot.
const TAIL_BYTES: u64 = 2 * 1024 * 1024;

/// Name of the per-session state file.
pub const STATE_FILE: &str = "hygiene-level";

pub fn run(payload: &Payload) {
    if crate::common::headless::is_headless() {
        return;
    }
    let home = home_dir();
    let slug = crate::common::mode::hook_slug(&home);
    let value = |key: &str| {
        crate::config::resolve(key, &home, slug.as_deref())
            .ok()
            .map(|(value, _)| value)
    };
    if value("hygiene.enabled").and_then(|v| v.as_bool()) == Some(false) {
        return;
    }

    let sid = session_id(payload);
    let transcript = payload.field(".transcript_path");
    if sid.is_empty() || transcript.is_empty() {
        return;
    }
    let Some(tokens) = context_tokens(Path::new(&transcript)) else {
        return;
    };
    let dir = session_dir(payload);
    if dir.is_empty() {
        return;
    }
    if crossed(&Path::new(&dir).join(STATE_FILE), tokens, THRESHOLD_TOKENS) {
        emit_prompt_context(&message(tokens, &sid));
    }
}

/// The nudge, kept under 400 characters.
fn message(tokens: u64, session: &str) -> String {
    format!(
        "Context is large (~{}K tokens). Run /playbook:handoff now and save it with \
         `playbook handoff save --session {session}`. Then tell the user in one line: the \
         handoff is saved; run /clear, then /playbook:handoff load if it did not load on its \
         own (the automatic load needs the SessionStart hook; check `playbook doctor check`).",
        tokens / 1000
    )
}

/// Whether `tokens` reaches a level above the one stored, updating the state
/// file. A lower level is stored without firing.
fn crossed(state: &Path, tokens: u64, threshold: u64) -> bool {
    let level = tokens / threshold;
    let stored = fs::read_to_string(state)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    if level == stored {
        return false;
    }
    let _ = fs::write(state, level.to_string());
    level > stored
}

/// Input tokens of the newest main-session assistant message after the last
/// compaction in the transcript at `path`, or `None` when it cannot be read
/// or has none.
pub fn context_tokens(path: &Path) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    // A tail that starts mid-file begins inside a line, so drop that line.
    let body = if start > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    // Stop at a compaction: the turns before it describe a context that no
    // longer exists, so there is no figure until a reply follows it.
    body.lines()
        .rev()
        .take_while(|line| !is_compact_boundary(line))
        .find_map(usage_total)
}

fn usage_total(line: &str) -> Option<u64> {
    let record: Value = serde_json::from_str(line.trim()).ok()?;
    if record.get("type")?.as_str()? != "assistant"
        || record.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let usage = record.get("message")?.get("usage")?;
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
    let total = field("input_tokens")
        + field("cache_creation_input_tokens")
        + field("cache_read_input_tokens");
    // Synthetic messages carry all zeros; they say nothing about the size.
    (total > 0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Scratch;

    fn record(input: u64, created: u64, read: u64, sidechain: bool) -> String {
        format!(
            r#"{{"type":"assistant","isSidechain":{sidechain},"message":{{"usage":{{"input_tokens":{input},"cache_creation_input_tokens":{created},"cache_read_input_tokens":{read}}}}}}}"#
        )
    }

    #[test]
    fn usage_sums_the_three_input_fields() {
        assert_eq!(usage_total(&record(10, 20, 300, false)), Some(330));
    }

    #[test]
    fn a_sidechain_record_is_ignored() {
        assert_eq!(usage_total(&record(10, 20, 300, true)), None);
    }

    #[test]
    fn a_zero_record_and_a_user_record_are_ignored() {
        assert_eq!(usage_total(&record(0, 0, 0, false)), None);
        assert_eq!(usage_total(r#"{"type":"user","message":{}}"#), None);
        assert_eq!(usage_total("not json"), None);
    }

    #[test]
    fn a_level_fires_once_then_again_after_shrinking() {
        let dir = std::env::temp_dir().join(format!("hygiene-unit-{}", crate::testing::run_id()));
        fs::create_dir_all(&dir).unwrap();
        let state = dir.join(STATE_FILE);
        assert!(!crossed(&state, 100, 250));
        assert!(crossed(&state, 300, 250));
        assert!(!crossed(&state, 400, 250));
        assert!(crossed(&state, 600, 250));
        assert!(!crossed(&state, 100, 250));
        assert!(crossed(&state, 300, 250));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_message_fits_the_budget_with_a_real_session_id() {
        let msg = message(999_999, "0123456789abcdef0123456789abcdef0123");
        assert!(msg.chars().count() < 400, "{}", msg.chars().count());
        assert!(msg.contains("playbook handoff save --session"));
    }

    const BOUNDARY: &str = r#"{"type":"system","subtype":"compact_boundary"}"#;
    const USER: &str = r#"{"type":"user","message":{"content":"hi"}}"#;
    const BIG: u64 = 438_000;
    const SMALL: u64 = 103_000;

    fn transcript(scratch: &Scratch, lines: &[String]) -> std::path::PathBuf {
        scratch.write(lines)
    }

    #[test]
    fn a_boundary_then_a_user_record_gives_no_size() {
        let s = Scratch::new("hygiene-user");
        let p = transcript(
            &s,
            &[record(0, 0, BIG, false), BOUNDARY.into(), USER.into()],
        );
        assert_eq!(context_tokens(&p), None);
    }

    #[test]
    fn an_assistant_record_after_the_boundary_gives_its_size() {
        let s = Scratch::new("hygiene-after");
        let p = transcript(
            &s,
            &[
                record(0, 0, BIG, false),
                BOUNDARY.into(),
                USER.into(),
                record(0, 0, SMALL, false),
            ],
        );
        assert_eq!(context_tokens(&p), Some(SMALL));
    }

    #[test]
    fn a_big_turn_after_the_boundary_in_the_same_window_is_the_size() {
        let s = Scratch::new("hygiene-big-after");
        let p = transcript(
            &s,
            &[
                record(0, 0, SMALL, false),
                BOUNDARY.into(),
                USER.into(),
                record(0, 0, BIG, false),
            ],
        );
        assert_eq!(context_tokens(&p), Some(BIG));
    }

    #[test]
    fn a_big_turn_before_the_boundary_in_the_same_window_is_not_the_size() {
        let s = Scratch::new("hygiene-big-before");
        let p = transcript(
            &s,
            &[
                record(0, 0, BIG, false),
                BOUNDARY.into(),
                record(0, 0, SMALL, false),
            ],
        );
        assert_eq!(context_tokens(&p), Some(SMALL));
    }

    #[test]
    fn a_boundary_outside_the_tail_window_still_applies() {
        let filler = format!(
            r#"{{"type":"user","message":{{"content":"{}"}}}}"#,
            "x".repeat(1024)
        );
        let mut lines = vec![record(0, 0, BIG, false), BOUNDARY.into()];
        lines.extend(std::iter::repeat_n(
            filler,
            (TAIL_BYTES as usize / 1024) + 16,
        ));
        lines.push(record(0, 0, SMALL, false));
        let s = Scratch::new("hygiene-window");
        let p = transcript(&s, &lines);
        assert_eq!(context_tokens(&p), Some(SMALL));
    }
}
