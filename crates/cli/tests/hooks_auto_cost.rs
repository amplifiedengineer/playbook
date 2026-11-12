// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `auto-cost` cost computation as seen from the real binary. Each call is a
//! separate process, so every assertion reads the state the hook persists as
//! `<session dir>/auto-cost.json`:
//!
//! ```text
//! {
//!   "effective_cents": -150,            // delta now, may be negative
//!   "computed_at_ms": 1700000000000,    // when effective_cents was computed
//!   "transcript": { "files": {
//!     "<absolute path>": { "baseline_cents": 200, "usd": 2.5, "offset": 812, "inode": 77,
//!                          "last_id": "msg_1", "last_usd": 0.5, "unpriced": false }
//!   } },
//!   "telemetry": { "baseline_cents": 300, "cents": 350 }   // absent until seen
//! }
//! ```
//!
//! `usd` is the running sum for that file since its offset was last reset and
//! `baseline_cents` is `round(usd * 100)` captured when the file was first
//! seen. `effective_cents` is `max(transcript delta, telemetry delta)` where
//! the transcript delta sums every file's `round(usd * 100) - baseline_cents`.
//! `last_id` and `last_usd` track the message still being streamed at the end
//! of the last read, and `unpriced` is set once a model outside the price table
//! was seen. The tests never read `inode`; how a replaced file is detected is
//! up to the hook. Transcript costs are token counts derived from `pricing::cost_usd`.
//!
//! Enforcement is read from stdout. The hook prints nothing to allow a call, a
//! `hookSpecificOutput` with `hookEventName` `PreToolUse` and an
//! `additionalContext` for a note, or the same with `permissionDecision`
//! `deny` and a `permissionDecisionReason`. The warn tier leaves a `warned`
//! marker file in the session dir.

#[path = "support/auto_config.rs"]
mod auto_config;
#[path = "support/auto_env.rs"]
mod auto_env;
#[path = "support/auto_feature_repo.rs"]
mod auto_feature_repo;
#[path = "support/auto_git_shim.rs"]
mod auto_git_shim;
#[path = "support/auto_run.rs"]
mod auto_run;
#[path = "support/auto_warm_up.rs"]
mod auto_warm_up;

use auto_env::{hook_command, scratch, Scratch};
use playbook::usage::pricing::{cost_usd, Tokens};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

const HOOK: &str = "auto-cost";
const PRE_TOOL_USE: &str = include_str!("fixtures/hooks/pre-agent.json");
const MODEL: &str = "claude-sonnet-5";
const STATE_FILE: &str = "auto-cost.json";
const INTERVAL_VAR: &str = "PLAYBOOK_AUTO_COST_INTERVAL_MS";

static LINE_ID: AtomicU64 = AtomicU64::new(0);

/// Where auto comes from: the config file, or `PLAYBOOK_MODE=auto` over a
/// config that says ask.
#[derive(Clone, Copy, PartialEq)]
enum AutoFrom {
    Config,
    Env,
}

struct Session {
    s: Scratch,
    sid: String,
    env: Vec<(&'static str, &'static str)>,
}

impl Session {
    fn auto(tag: &str) -> Self {
        Self::auto_from(tag, AutoFrom::Config)
    }

    fn auto_from(tag: &str, from: AutoFrom) -> Self {
        let s = scratch(tag);
        let sid = format!("sid-{tag}");
        let (mode, env) = match from {
            AutoFrom::Config => ("auto", vec![]),
            AutoFrom::Env => ("ask", vec![("PLAYBOOK_MODE", "auto")]),
        };
        s.seed_mode_config(mode);
        Session { s, sid, env }
    }

    /// Rewrites the global config with the given `auto.budgetUsd`, keeping
    /// the mode the session was built with.
    fn seed_budget(&self, budget_usd: &Value) {
        self.seed_auto(json!({"budgetUsd": budget_usd}));
    }

    /// Rewrites the global config with the given `auto` section, keeping the
    /// mode the session was built with.
    fn seed_auto(&self, auto: Value) {
        let mode = if self.env.is_empty() { "auto" } else { "ask" };
        let config = json!({"mode": mode, "auto": auto});
        auto_config::seed_raw_global_config(&self.s.home, &config.to_string());
    }

    fn park_note(&self) -> String {
        self.dir()
            .join("park-note.md")
            .to_string_lossy()
            .into_owned()
    }

    /// A PreToolUse payload for `tool` with `input`, on this session.
    fn tool_payload(&self, tool: &str, input: Value) -> String {
        let mut value: Value = serde_json::from_str(&self.payload()).expect("payload is JSON");
        value["tool_name"] = json!(tool);
        value["tool_input"] = input;
        value["cwd"] = json!(auto_feature_repo::feature_repo(&self.s).to_string_lossy());
        value.to_string()
    }

    /// The same payload with no `session_id`.
    fn anonymous_tool_payload(&self, tool: &str, input: Value) -> String {
        let mut value: Value =
            serde_json::from_str(&self.tool_payload(tool, input)).expect("payload is JSON");
        value.as_object_mut().expect("object").remove("session_id");
        value.to_string()
    }

    /// Runs the hook on a tool call and parses what it printed.
    fn call(&self, tool: &str, input: Value) -> Outcome {
        self.call_payload(&self.tool_payload(tool, input))
    }

    fn call_payload(&self, payload: &str) -> Outcome {
        let (out, code) = self.run_with(payload, Some("0"), &[]);
        assert_eq!(code, 0, "the hook must exit 0 whatever it decides");
        outcome(&out)
    }

    fn bash(&self, command: &str) -> Outcome {
        self.call("Bash", json!({"command": command}))
    }

    /// Spends nothing on call 1 (telemetry 0.00 baselines at zero), then
    /// reports `usd` on the telemetry feed without calling the hook.
    fn baseline_then_report(&self, usd: &str) {
        self.append_telemetry("0.00");
        assert_eq!(self.bash("git status"), Outcome::Silent, "baseline call");
        self.append_telemetry(usd);
    }

    /// A session whose spend has reached the cap of 5.
    fn at_budget(tag: &str, from: AutoFrom) -> Self {
        let session = Self::auto_from(tag, from);
        session.seed_budget(&json!(5));
        session.baseline_then_report("5.00");
        session
    }

    fn warned_markers(&self) -> Vec<String> {
        fs::read_dir(self.dir())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with("warned"))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn runtime_root(&self) -> PathBuf {
        self.s.home.join(".config/playbook/runtime")
    }

    fn dir(&self) -> PathBuf {
        self.runtime_root().join(&self.sid)
    }

    fn transcript(&self) -> PathBuf {
        self.s.cwd.join("main.jsonl")
    }

    /// Subagent transcripts sit next to the main one, in
    /// `<transcript_path minus .jsonl>/subagents/`.
    fn subagent(&self, name: &str) -> PathBuf {
        self.s.cwd.join("main").join("subagents").join(name)
    }

    fn payload(&self) -> String {
        let mut value: Value = serde_json::from_str(PRE_TOOL_USE).expect("fixture is JSON");
        value["session_id"] = json!(self.sid);
        value["transcript_path"] = json!(self.transcript().to_string_lossy());
        value.to_string()
    }

    /// Runs the hook with the recompute interval forced to 0.
    fn run(&self) -> (String, i32) {
        self.run_with(&self.payload(), Some("0"), &[])
    }

    fn run_with(
        &self,
        payload: &str,
        interval: Option<&str>,
        env: &[(&str, &str)],
    ) -> (String, i32) {
        let mut command = hook_command(&self.s, HOOK);
        command.env_remove(INTERVAL_VAR);
        if let Some(ms) = interval {
            command.env(INTERVAL_VAR, ms);
        }
        let mut child = command
            .envs(self.env.iter().copied())
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("playbook spawns");
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(payload.as_bytes())
            .expect("stdin accepts the payload");
        let out = child.wait_with_output().expect("playbook exits");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    fn state(&self) -> Value {
        let path = self.dir().join(STATE_FILE);
        let raw = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("hook did not persist {}: {e}", path.display()));
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("state is not JSON ({e}): {raw}"))
    }

    /// Rewrites the stored state so its cost was computed `age_ms` ago.
    fn set_cost_age(&self, age_ms: u64) {
        let mut state = self.state();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock is after the epoch")
            .as_millis() as u64;
        state["computed_at_ms"] = json!(now - age_ms);
        fs::write(self.dir().join(STATE_FILE), state.to_string()).expect("state is writable");
    }

    fn append_telemetry(&self, cost_usd: &str) {
        fs::create_dir_all(self.dir()).expect("session dir is creatable");
        append(
            &self.dir().join("telemetry.jsonl"),
            &format!(r#"{{"ts":1,"cost_usd":{cost_usd},"used_pct":0}}"#),
        );
    }
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Silent,
    Note(String),
    Deny(String),
}

fn outcome(out: &str) -> Outcome {
    if out.trim().is_empty() {
        return Outcome::Silent;
    }
    let value: Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {out}"));
    let inner = &value["hookSpecificOutput"];
    assert_eq!(inner["hookEventName"], "PreToolUse", "{out}");
    let text = |key: &str| {
        inner[key]
            .as_str()
            .unwrap_or_else(|| panic!("{key} missing in {out}"))
            .to_string()
    };
    if inner["permissionDecision"] == "deny" {
        Outcome::Deny(text("permissionDecisionReason"))
    } else {
        Outcome::Note(text("additionalContext"))
    }
}

impl Outcome {
    fn is_denied(&self) -> bool {
        matches!(self, Outcome::Deny(_))
    }

    /// The deny reason, panicking with `label` when the call was not denied.
    fn reason(&self, label: &str) -> &str {
        match self {
            Outcome::Deny(reason) => reason,
            other => panic!("{label}: expected a deny, got {other:?}"),
        }
    }
}

fn append(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("path has a parent")).expect("parent is creatable");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("file opens for append");
    writeln!(file, "{text}").expect("line is writable");
}

fn assistant_line(usd: f64) -> String {
    let id = LINE_ID.fetch_add(1, Ordering::Relaxed);
    assistant_line_with_id(&format!("msg_{id}"), usd)
}

/// A line of message `id` costing `usd`. Streaming repeats one id on several
/// lines, each carrying the usage so far.
fn assistant_line_with_id(id: &str, usd: f64) -> String {
    let unit = cost_usd(
        MODEL,
        &Tokens {
            input: 1_000_000,
            ..Tokens::default()
        },
    )
    .expect("model is priced");
    let input = (usd / unit * 1_000_000.0).round() as u64;
    message_line(id, MODEL, input)
}

fn message_line(id: &str, model: &str, input_tokens: u64) -> String {
    json!({
        "type": "assistant",
        "timestamp": "2026-09-01T08:51:22.982Z",
        "message": {
            "id": id,
            "model": model,
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            }
        }
    })
    .to_string()
}

fn append_cost(path: &Path, usd: f64) {
    append(path, &assistant_line(usd));
}

fn write_cost(path: &Path, usd: f64) {
    let _ = fs::remove_file(path);
    append_cost(path, usd);
}

fn effective(state: &Value) -> i64 {
    state["effective_cents"]
        .as_i64()
        .unwrap_or_else(|| panic!("effective_cents missing or not an integer: {state}"))
}

fn file_entry<'a>(state: &'a Value, name: &str) -> &'a Value {
    let files = state["transcript"]["files"]
        .as_object()
        .unwrap_or_else(|| panic!("transcript.files missing: {state}"));
    files
        .iter()
        .find(|(path, _)| path.ends_with(&format!("/{name}")))
        .map(|(_, entry)| entry)
        .unwrap_or_else(|| panic!("no entry for {name} in {state}"))
}

fn has_file_entry(state: &Value, name: &str) -> bool {
    state["transcript"]["files"]
        .as_object()
        .is_some_and(|files| files.keys().any(|p| p.ends_with(&format!("/{name}"))))
}

fn usd(entry: &Value) -> f64 {
    entry["usd"].as_f64().expect("entry usd is a number")
}

fn cents(entry: &Value) -> i64 {
    (usd(entry) * 100.0).round() as i64
}

fn baseline(entry: &Value) -> i64 {
    entry["baseline_cents"]
        .as_i64()
        .expect("entry baseline_cents is an integer")
}

fn offset(entry: &Value) -> u64 {
    entry["offset"]
        .as_u64()
        .expect("entry offset is an integer")
}

fn len(path: &Path) -> u64 {
    fs::metadata(path).expect("file exists").len()
}

fn tree(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                out.push(format!("{rel}/"));
                walk(&path, root, out);
            } else {
                out.push(format!("{rel} {}", len(&path)));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn recompute_interval_reads_the_env_var_and_falls_back_to_the_default() {
    // Arrange: rows are (label, interval, age of the stored cost in ms,
    // expected cents). The process start-up delay under load only ever makes
    // the stored cost look older, so every row must stay correct when the age
    // grows: recomputed rows are at least a second past the interval, and
    // reused rows sit far inside a long one. That the default is 2000 ms and
    // that unparsable or empty values fall back to it is pinned by the unit
    // test in `src/hooks/auto_cost.rs`, which needs no clock.
    let rows: [(&str, Option<&str>, u64, i64); 6] = [
        ("unset: 3 s old is recomputed", None, 3000, 100),
        ("unparsable: 3 s old is recomputed", Some("abc"), 3000, 100),
        ("empty: 3 s old is recomputed", Some(""), 3000, 100),
        ("600000: 3 s old is reused", Some("600000"), 3000, 0),
        ("5000: 6 s old is recomputed", Some("5000"), 6000, 100),
        ("zero recomputes on every call", Some("0"), 1000, 100),
    ];
    for (n, (label, interval, age_ms, expected)) in rows.into_iter().enumerate() {
        let session = Session::auto(&format!("interval-{n}"));
        session.append_telemetry("1.0");
        session.run();
        session.append_telemetry("2.0");
        session.set_cost_age(age_ms);

        // Act
        session.run_with(&session.payload(), interval, &[]);

        // Assert
        assert_eq!(effective(&session.state()), expected, "{label}");
    }
}

#[test]
fn first_call_stores_the_baseline_so_pre_existing_cost_has_zero_delta() {
    // Arrange
    let rows: [(&str, Option<&str>, Option<f64>); 3] = [
        ("transcript only", None, Some(4.0)),
        ("telemetry only", Some("4.0"), None),
        ("both sources", Some("4.0"), Some(4.0)),
    ];
    for (n, (label, telemetry, transcript)) in rows.into_iter().enumerate() {
        let session = Session::auto(&format!("baseline-{n}"));
        if let Some(t) = telemetry {
            session.append_telemetry(t);
        }
        if let Some(usd) = transcript {
            write_cost(&session.transcript(), usd);
        }

        // Act
        let (_, code) = session.run();

        // Assert
        assert_eq!(code, 0, "{label}");
        let state = session.state();
        assert_eq!(effective(&state), 0, "{label}: delta");
        if telemetry.is_some() {
            assert_eq!(state["telemetry"]["baseline_cents"], 400, "{label}");
        }
        if transcript.is_some() {
            assert_eq!(baseline(file_entry(&state, "main.jsonl")), 400, "{label}");
        }

        // Act: a second call keeps the baseline and measures from it
        if telemetry.is_some() {
            session.append_telemetry("4.5");
        }
        if transcript.is_some() {
            append_cost(&session.transcript(), 0.5);
        }
        session.run();

        // Assert
        let state = session.state();
        assert_eq!(effective(&state), 50, "{label}: delta after growth");
        if telemetry.is_some() {
            assert_eq!(state["telemetry"]["baseline_cents"], 400, "{label}");
        }
        if transcript.is_some() {
            assert_eq!(baseline(file_entry(&state, "main.jsonl")), 400, "{label}");
        }
    }
}

#[test]
fn effective_cost_is_the_larger_of_the_per_source_deltas() {
    // Arrange: rows are (transcript growth, telemetry growth, expected cents)
    // on top of a call 1 that baselines whichever sources are present.
    let rows: [(&str, Option<f64>, Option<f64>, i64); 4] = [
        ("telemetry delta larger", Some(2.0), Some(3.0), 300),
        ("transcript delta larger", Some(3.0), Some(2.0), 300),
        ("transcript only", Some(1.5), None, 150),
        ("telemetry only", None, Some(2.5), 250),
    ];
    for (n, (label, transcript_growth, telemetry_growth, expected)) in rows.into_iter().enumerate()
    {
        let session = Session::auto(&format!("max-{n}"));
        if transcript_growth.is_some() {
            write_cost(&session.transcript(), 1.0);
        }
        if telemetry_growth.is_some() {
            session.append_telemetry("1.0");
        }
        session.run();

        // Act
        if let Some(growth) = transcript_growth {
            append_cost(&session.transcript(), growth);
        }
        if let Some(growth) = telemetry_growth {
            session.append_telemetry(&format!("{}", 1.0 + growth));
        }
        session.run();

        // Assert
        assert_eq!(effective(&session.state()), expected, "{label}");
    }
}

#[test]
fn a_source_that_first_appears_later_is_baselined_at_its_first_value() {
    // Arrange: call 1 has only the transcript (1.0).
    let session = Session::auto("source-switch");
    write_cost(&session.transcript(), 1.0);
    session.run();

    // Act: call 2 also has telemetry (3.0)
    session.append_telemetry("3.0");
    session.run();

    // Assert: telemetry is baselined at 3.0 with delta 0, the transcript keeps 1.0
    let state = session.state();
    assert_eq!(state["telemetry"]["baseline_cents"], 300);
    assert_eq!(baseline(file_entry(&state, "main.jsonl")), 100);
    assert_eq!(effective(&state), 0);

    // Act: telemetry grows by 0.5, then the transcript grows by 2.0
    session.append_telemetry("3.5");
    session.run();
    let after_telemetry = effective(&session.state());
    append_cost(&session.transcript(), 2.0);
    session.run();

    // Assert
    assert_eq!(after_telemetry, 50);
    assert_eq!(effective(&session.state()), 200);
}

#[test]
fn subagent_transcripts_are_summed_with_the_main_transcript() {
    // Arrange: main 1.0 and agent-a 2.0 present at call 1
    let session = Session::auto("subagent-sum");
    write_cost(&session.transcript(), 1.0);
    write_cost(&session.subagent("agent-a.jsonl"), 2.0);

    // Act
    session.run();

    // Assert: total 3.0, both baselined, nothing counted yet
    let state = session.state();
    let total =
        cents(file_entry(&state, "main.jsonl")) + cents(file_entry(&state, "agent-a.jsonl"));
    assert_eq!(total, 300);
    assert_eq!(baseline(file_entry(&state, "agent-a.jsonl")), 200);
    assert_eq!(effective(&state), 0);

    // Act: append to the subagent file only
    let before = offset(file_entry(&state, "agent-a.jsonl"));
    append_cost(&session.subagent("agent-a.jsonl"), 0.5);
    session.run();

    // Assert: only the new bytes were counted
    let state = session.state();
    let agent = file_entry(&state, "agent-a.jsonl");
    assert_eq!(cents(agent), 250);
    assert!(offset(agent) > before);
    assert_eq!(offset(agent), len(&session.subagent("agent-a.jsonl")));
    assert_eq!(effective(&state), 50);
}

#[test]
fn a_subagent_file_first_seen_after_the_baseline_call_counts_whole() {
    // Arrange: call 1 sees the main transcript only
    let session = Session::auto("subagent-late");
    write_cost(&session.transcript(), 1.0);
    session.run();

    // Act
    write_cost(&session.subagent("agent-b.jsonl"), 0.7);
    session.run();

    // Assert
    let state = session.state();
    let agent = file_entry(&state, "agent-b.jsonl");
    assert_eq!(baseline(agent), 0);
    assert_eq!(cents(agent), 70);
    assert_eq!(effective(&state), 70);
}

#[test]
fn only_agent_jsonl_files_in_the_subagents_dir_are_read() {
    // Arrange: expensive decoys next to one real, baselined subagent file
    let session = Session::auto("subagent-ignore");
    write_cost(&session.transcript(), 1.0);
    write_cost(&session.subagent("agent-a.jsonl"), 1.0);
    for decoy in ["notes.txt", "agent-x.json", "other.jsonl"] {
        write_cost(&session.subagent(decoy), 9.0);
    }
    session.run();

    // Act: decoys grow too
    for decoy in ["notes.txt", "agent-x.json", "other.jsonl"] {
        append_cost(&session.subagent(decoy), 9.0);
    }
    session.run();

    // Assert
    let state = session.state();
    for decoy in ["notes.txt", "agent-x.json", "other.jsonl"] {
        assert!(!has_file_entry(&state, decoy), "{decoy} must be ignored");
    }
    assert!(has_file_entry(&state, "agent-a.jsonl"));
    assert_eq!(effective(&state), 0);
}

#[test]
fn a_missing_subagents_dir_means_the_main_transcript_alone() {
    // Arrange
    let session = Session::auto("subagent-none");
    write_cost(&session.transcript(), 1.0);
    session.run();

    // Act
    append_cost(&session.transcript(), 0.4);
    session.run();

    // Assert
    let state = session.state();
    assert_eq!(effective(&state), 40);
    assert_eq!(
        state["transcript"]["files"]
            .as_object()
            .map(serde_json::Map::len),
        Some(1)
    );
}

#[test]
fn a_truncated_subagent_file_resets_only_its_own_offset() {
    // Arrange: main 1.0, agent-a 2.0, agent-b 1.0 baselined at call 1
    let session = Session::auto("subagent-truncate");
    write_cost(&session.transcript(), 1.0);
    write_cost(&session.subagent("agent-a.jsonl"), 1.0);
    append_cost(&session.subagent("agent-a.jsonl"), 1.0);
    write_cost(&session.subagent("agent-b.jsonl"), 1.0);
    session.run();
    let before = session.state();
    let main_offset = offset(file_entry(&before, "main.jsonl"));
    let b_offset = offset(file_entry(&before, "agent-b.jsonl"));

    // Act: agent-a is truncated in place and rewritten as one line, so it is
    // strictly shorter than before (a rewrite of equal length is undetectable)
    fs::write(session.subagent("agent-a.jsonl"), "").expect("truncate");
    append_cost(&session.subagent("agent-a.jsonl"), 0.5);
    session.run();

    // Assert: agent-a restarts from zero, the others keep their offsets
    let state = session.state();
    let a = file_entry(&state, "agent-a.jsonl");
    assert_eq!(cents(a), 50);
    assert_eq!(offset(a), len(&session.subagent("agent-a.jsonl")));
    assert_eq!(baseline(a), 200);
    assert_eq!(offset(file_entry(&state, "main.jsonl")), main_offset);
    assert_eq!(offset(file_entry(&state, "agent-b.jsonl")), b_offset);
    assert_eq!(effective(&state), -150);
}

#[test]
fn a_replaced_subagent_file_is_recomputed_from_zero() {
    // Arrange: agent-a 2.0 baselined at call 1, main untouched afterwards
    let session = Session::auto("subagent-replace");
    write_cost(&session.transcript(), 1.0);
    write_cost(&session.subagent("agent-a.jsonl"), 2.0);
    session.run();
    let main_offset = offset(file_entry(&session.state(), "main.jsonl"));

    // Act: a new file replaces the old one (new inode) and is larger
    let replacement = session.subagent("agent-a.jsonl.new");
    write_cost(&replacement, 1.0);
    append_cost(&replacement, 2.0);
    fs::rename(&replacement, session.subagent("agent-a.jsonl")).expect("rename over");
    session.run();

    // Assert
    let state = session.state();
    let a = file_entry(&state, "agent-a.jsonl");
    assert_eq!(cents(a), 300);
    assert_eq!(offset(a), len(&session.subagent("agent-a.jsonl")));
    assert_eq!(offset(file_entry(&state, "main.jsonl")), main_offset);
    assert_eq!(effective(&state), 100);
}

#[test]
fn events_appended_between_two_calls_are_counted_once() {
    // Arrange
    let session = Session::auto("incremental");
    write_cost(&session.transcript(), 1.0);
    session.run();

    // Act
    append_cost(&session.transcript(), 2.0);
    session.run();
    let after_growth = session.state();
    session.run();

    // Assert: the sum once, and an idle call changes nothing
    let main = file_entry(&after_growth, "main.jsonl");
    assert_eq!(cents(main), 300);
    assert_eq!(offset(main), len(&session.transcript()));
    assert_eq!(effective(&after_growth), 200);
    let idle = session.state();
    assert_eq!(cents(file_entry(&idle, "main.jsonl")), 300);
    assert_eq!(effective(&idle), 200);
}

#[test]
fn lines_sharing_a_message_id_in_one_read_count_the_largest_once() {
    // Arrange: a baselined message, then one streamed message of three lines
    let session = Session::auto("stream-one-read");
    write_cost(&session.transcript(), 1.0);
    session.run();

    // Act
    for usd in [0.5, 2.0, 1.0] {
        append(
            &session.transcript(),
            &assistant_line_with_id("msg_stream", usd),
        );
    }
    session.run();

    // Assert: 1.0 baselined plus the largest line, 2.0
    let state = session.state();
    assert_eq!(cents(file_entry(&state, "main.jsonl")), 300);
    assert_eq!(effective(&state), 200);
}

#[test]
fn a_message_streamed_across_two_reads_counts_its_largest_usage_once() {
    // Arrange
    let session = Session::auto("stream-two-reads");
    write_cost(&session.transcript(), 1.0);
    session.run();
    append(
        &session.transcript(),
        &assistant_line_with_id("msg_stream", 0.5),
    );
    session.run();
    assert_eq!(
        effective(&session.state()),
        50,
        "first lines of the message"
    );

    // Act: the same message grows, split over two more lines
    append(
        &session.transcript(),
        &assistant_line_with_id("msg_stream", 2.0),
    );
    append(
        &session.transcript(),
        &assistant_line_with_id("msg_stream", 3.0),
    );
    session.run();

    // Assert: the file holds 1.0 plus the largest line, not the sum of the reads
    let state = session.state();
    assert_eq!(cents(file_entry(&state, "main.jsonl")), 400);
    assert_eq!(effective(&state), 300);

    // Act: a late line with less usage, then a new message
    append(
        &session.transcript(),
        &assistant_line_with_id("msg_stream", 1.0),
    );
    session.run();
    let after_smaller = effective(&session.state());
    append(
        &session.transcript(),
        &assistant_line_with_id("msg_next", 0.25),
    );
    session.run();

    // Assert
    assert_eq!(after_smaller, 300, "a smaller line adds nothing");
    assert_eq!(effective(&session.state()), 325, "a new id counts whole");
}

#[test]
fn a_model_missing_from_the_price_table_is_flagged_unpriced_in_the_state() {
    // Arrange: rows are (model, expected flag)
    let rows = [(MODEL, false), ("claude-model-not-in-the-table", true)];
    for (n, (model, unpriced)) in rows.into_iter().enumerate() {
        let session = Session::auto(&format!("unpriced-flag-{n}"));
        write_cost(&session.transcript(), 1.0);
        session.run();

        // Act
        append(&session.transcript(), &message_line("msg_x", model, 5_000));
        session.run();

        // Assert
        let entry = file_entry(&session.state(), "main.jsonl").clone();
        assert_eq!(entry["unpriced"], unpriced, "{model}");
    }
}

#[test]
fn a_partially_written_last_line_is_counted_once_it_is_complete() {
    // Arrange
    let session = Session::auto("partial-line");
    write_cost(&session.transcript(), 1.0);
    session.run();
    let complete_len = len(&session.transcript());
    let line = assistant_line(2.0);
    let (head, tail) = line.split_at(line.len() / 2);
    let mut file = OpenOptions::new()
        .append(true)
        .open(session.transcript())
        .expect("transcript opens");
    file.write_all(head.as_bytes()).expect("partial write");

    // Act
    session.run();

    // Assert: the unfinished line is left for the next call
    let state = session.state();
    assert_eq!(offset(file_entry(&state, "main.jsonl")), complete_len);
    assert_eq!(effective(&state), 0);

    // Act: the line is completed
    writeln!(file, "{tail}").expect("rest of the line");
    session.run();

    // Assert
    assert_eq!(effective(&session.state()), 200);
}

#[test]
fn a_truncated_main_transcript_is_recomputed_from_zero() {
    // Arrange
    let session = Session::auto("main-truncate");
    write_cost(&session.transcript(), 1.0);
    append_cost(&session.transcript(), 1.0);
    session.run();

    // Act: shrinks in place, then holds one 0.3 event
    fs::write(session.transcript(), "").expect("truncate");
    append_cost(&session.transcript(), 0.3);
    session.run();

    // Assert
    let state = session.state();
    let main = file_entry(&state, "main.jsonl");
    assert_eq!(cents(main), 30);
    assert_eq!(offset(main), len(&session.transcript()));
    assert_eq!(baseline(main), 200);
    assert_eq!(effective(&state), -170);
}

#[test]
fn an_unreadable_transcript_falls_back_to_telemetry() {
    // Arrange: rows are how the transcript is unreadable
    #[derive(Clone, Copy)]
    enum Unreadable {
        Missing,
        NoPermission,
        PathAbsentFromPayload,
    }
    let rows = [
        ("missing file", Unreadable::Missing),
        ("no read permission", Unreadable::NoPermission),
        ("no transcript_path", Unreadable::PathAbsentFromPayload),
    ];
    for (n, (label, how)) in rows.into_iter().enumerate() {
        let session = Session::auto(&format!("unreadable-{n}"));
        if matches!(how, Unreadable::NoPermission) {
            write_cost(&session.transcript(), 9.0);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(session.transcript(), fs::Permissions::from_mode(0o000))
                    .expect("chmod");
            }
        }
        let payload = if matches!(how, Unreadable::PathAbsentFromPayload) {
            let mut value: Value = serde_json::from_str(&session.payload()).unwrap();
            value.as_object_mut().unwrap().remove("transcript_path");
            value.to_string()
        } else {
            session.payload()
        };
        session.append_telemetry("4.0");
        session.run_with(&payload, Some("0"), &[]);
        session.append_telemetry("4.5");

        // Act
        session.run_with(&payload, Some("0"), &[]);

        // Assert
        let state = session.state();
        assert_eq!(effective(&state), 50, "{label}");
        assert_eq!(state["telemetry"]["baseline_cents"], 400, "{label}");
    }
}

#[test]
fn a_negative_delta_is_kept_and_the_baseline_is_not_moved() {
    // Arrange
    let session = Session::auto("negative");
    session.append_telemetry("4.0");
    session.run();

    // Act
    session.append_telemetry("3.0");
    let (out, code) = session.run();

    // Assert
    assert_eq!(code, 0);
    assert!(
        out.trim().is_empty(),
        "no output on a negative delta: {out}"
    );
    let state = session.state();
    assert_eq!(effective(&state), -100);
    assert_eq!(state["telemetry"]["baseline_cents"], 400);

    // Act: cost climbs back above the baseline
    session.append_telemetry("4.5");
    session.run();

    // Assert
    assert_eq!(effective(&session.state()), 50);
}

#[test]
fn deltas_are_integer_cents_rounded_per_value_before_subtracting() {
    // Arrange: rows are (baseline, cost, expected delta cents). The float
    // products 4.35 * 100, 0.29 * 100, 1.15 * 100 and 4.6 - 1.1 all land just
    // under the integer, so truncating or subtracting first is off by one.
    let rows: [(&str, &str, i64); 6] = [
        ("0.00", "4.35", 435),
        ("0.00", "0.29", 29),
        ("0.00", "1.15", 115),
        ("1.10", "4.60", 350),
        ("0.004", "0.506", 51),
        ("0.006", "0.994", 98),
    ];
    for (n, (base, cost, expected)) in rows.into_iter().enumerate() {
        let session = Session::auto(&format!("cents-{n}"));
        session.append_telemetry(base);
        session.run();

        // Act
        session.append_telemetry(cost);
        session.run();

        // Assert
        assert_eq!(
            effective(&session.state()),
            expected,
            "baseline {base}, cost {cost}"
        );
    }
}

#[test]
fn a_second_call_inside_a_long_interval_reuses_the_cached_cost() {
    // Arrange
    let session = Session::auto("throttle");
    write_cost(&session.transcript(), 1.0);
    session.run_with(&session.payload(), Some("600000"), &[]);
    let first = session.state();
    append_cost(&session.transcript(), 2.0);

    // Act
    session.run_with(&session.payload(), Some("600000"), &[]);

    // Assert: nothing recomputed and the transcript was not read again
    let second = session.state();
    assert_eq!(effective(&second), 0);
    assert_eq!(second["computed_at_ms"], first["computed_at_ms"]);
    assert_eq!(
        offset(file_entry(&second, "main.jsonl")),
        offset(file_entry(&first, "main.jsonl"))
    );

    // Act: with the interval elapsed (zero) the new event is counted
    session.run();

    // Assert
    assert_eq!(effective(&session.state()), 200);
}

#[test]
fn flipping_config_to_ask_mid_session_stops_the_hook_touching_files() {
    // Arrange: one call in auto, then one more that changes state
    let session = Session::auto("config-flip");
    session.append_telemetry("1.0");
    session.run();
    session.append_telemetry("2.0");
    session.run();
    assert_eq!(effective(&session.state()), 100);
    let state_file = session.dir().join(STATE_FILE);
    let state_before = fs::read(&state_file).expect("state exists");
    let tree_before = tree(&session.s.home);

    // Act: rewrite the config as ask, then call again
    session.s.seed_mode_config("ask");
    session.append_telemetry("3.0");
    let tree_with_telemetry = tree(&session.s.home);
    let (out, code) = session.run();

    // Assert
    assert_eq!(code, 0);
    assert!(out.trim().is_empty(), "ask mode prints nothing: {out}");
    assert_eq!(fs::read(&state_file).expect("state exists"), state_before);
    assert_eq!(tree(&session.s.home), tree_with_telemetry);
    assert_ne!(tree_before, tree_with_telemetry, "sanity: telemetry grew");
}

#[test]
fn ask_mode_writes_nothing_in_the_runtime_root() {
    // Arrange: rows are (config mode, PLAYBOOK_MODE)
    let rows: [(&str, Option<&str>, Option<&str>); 3] = [
        ("default ask", None, None),
        ("config ask", Some("ask"), None),
        ("env ask beats config auto", Some("auto"), Some("ask")),
    ];
    for (n, (label, config, env_mode)) in rows.into_iter().enumerate() {
        let session = Session::auto(&format!("ask-{n}"));
        match config {
            Some(mode) => session.s.seed_mode_config(mode),
            None => {
                auto_config::clear_config(&session.s.home);
            }
        }
        session.append_telemetry("4.0");
        write_cost(&session.transcript(), 4.0);
        write_cost(&session.subagent("agent-a.jsonl"), 1.0);
        let before = tree(&session.runtime_root());
        let env: Vec<(&str, &str)> = env_mode.map(|m| ("PLAYBOOK_MODE", m)).into_iter().collect();

        // Act
        let (out, code) = session.run_with(&session.payload(), Some("0"), &env);

        // Assert
        assert_eq!(code, 0, "{label}");
        assert!(out.trim().is_empty(), "{label}: printed {out}");
        assert_eq!(tree(&session.runtime_root()), before, "{label}");
        assert!(!session.dir().join(STATE_FILE).exists(), "{label}");
    }
}

#[test]
fn ask_mode_never_creates_the_runtime_root() {
    // Arrange
    let session = Session::auto("ask-fresh");
    session.s.seed_mode_config("ask");

    // Act
    let (out, code) = session.run();

    // Assert
    assert_eq!(code, 0);
    assert!(out.trim().is_empty(), "printed {out}");
    assert!(
        !session.runtime_root().exists(),
        "ask mode must not create {}",
        session.runtime_root().display()
    );
}

#[test]
fn concurrent_first_calls_leave_exactly_one_baseline_file() {
    // Smoke check: it can catch a gross failure, not prove the race is absent.
    // Arrange: a fresh session with 4.0 already spent
    let session = Session::auto("concurrent");
    session.append_telemetry("4.0");
    write_cost(&session.transcript(), 4.0);
    let payload = session.payload();

    // Act: start eight hook processes before any is waited on
    let mut children: Vec<_> = (0..8)
        .map(|_| {
            let mut command = hook_command(&session.s, HOOK);
            command
                .env(INTERVAL_VAR, "0")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("playbook spawns")
        })
        .collect();
    for child in &mut children {
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(payload.as_bytes())
            .expect("stdin accepts the payload");
    }
    for child in children {
        let out = child.wait_with_output().expect("playbook exits");
        assert_eq!(out.status.code(), Some(0));
    }

    // Assert: one state file, no stray temp or lock entries, baseline intact
    let leftovers: Vec<String> = fs::read_dir(session.dir())
        .expect("session dir exists")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("auto-cost"))
        .collect();
    assert_eq!(leftovers, vec![STATE_FILE.to_string()]);
    let state = session.state();
    assert_eq!(baseline(file_entry(&state, "main.jsonl")), 400);
    assert_eq!(state["telemetry"]["baseline_cents"], 400);
    assert_eq!(effective(&state), 0);
}

#[test]
fn the_warn_tier_fires_once_at_the_warn_percentage_of_the_cap() {
    // Arrange: cap 5, warnPct 70, so the tier starts at 350 cents
    let session = Session::auto("warn-edges");
    session.seed_budget(&json!(5));
    session.append_telemetry("0.00");
    assert_eq!(session.bash("npm test"), Outcome::Silent, "baseline call");

    // Act and Assert: one cent under the tier
    session.append_telemetry("3.49");
    assert_eq!(session.bash("npm test"), Outcome::Silent, "3.49");
    assert!(session.warned_markers().is_empty(), "no marker at 3.49");

    // Act and Assert: exactly on the tier, one note and one marker
    session.append_telemetry("3.50");
    let Outcome::Note(note) = session.bash("npm test") else {
        panic!("3.50 must give a pre-context note");
    };
    assert!(note.to_lowercase().contains("budget"), "note: {note}");
    assert_eq!(session.warned_markers(), vec!["warned".to_string()]);

    // Act and Assert: still at 3.50 and then higher, no second note
    assert_eq!(session.bash("npm test"), Outcome::Silent, "second 3.50");
    session.append_telemetry("4.99");
    assert_eq!(
        session.bash("npm test"),
        Outcome::Silent,
        "4.99 denies nothing"
    );
}

#[test]
fn the_hard_cap_denies_non_safe_tools_at_exactly_the_cap_and_keeps_the_safe_list() {
    // Arrange
    let session = Session::auto("cap-edge");
    session.seed_budget(&json!(5));
    session.baseline_then_report("4.99");
    assert!(!session.bash("npm test").is_denied(), "4.99 denies nothing");

    // Act
    session.append_telemetry("5.00");
    let denied = session.bash("npm test");
    let read = session.call("Read", json!({"file_path": "/etc/hosts"}));

    // Assert
    denied.reason("5.00 on a non-safe tool");
    assert!(!read.is_denied(), "5.00 still allows the safe list");
}

#[test]
fn the_deny_at_budget_reached_asks_for_a_park_note_and_a_report() {
    // Arrange
    let session = Session::at_budget("deny-reason", AutoFrom::Config);

    // Act
    let outcome = session.bash("npm test");

    // Assert
    let reason = outcome.reason("budget reached");
    assert!(reason.contains("budget reached"), "reason: {reason}");
    assert!(reason.contains(&session.park_note()), "reason: {reason}");
    assert!(reason.contains("report to the user"), "reason: {reason}");
}

#[test]
fn the_safe_list_at_budget_reached_allows_read_only_tools() {
    // Arrange
    let session = Session::at_budget("safe-tools", AutoFrom::Config);
    let cases = [
        ("Read", json!({"file_path": "/etc/hosts"})),
        ("Grep", json!({"pattern": "fn main"})),
        ("Glob", json!({"pattern": "**/*.rs"})),
    ];

    for (tool, input) in cases {
        // Act
        let outcome = session.call(tool, input);

        // Assert
        assert!(!outcome.is_denied(), "{tool} is on the safe list");
    }
}

#[test]
fn bash_at_budget_reached_allows_the_read_only_commands_and_denies_the_rest() {
    // Arrange
    let session = Session::at_budget("safe-bash", AutoFrom::Config);
    let allowed = [
        "git status",
        "git diff",
        "git status --short",
        "playbook mode status",
        "playbook mode status --json",
    ];
    let denied = [
        "git push --force",
        "playbook mode status --jsonx",
        "playbook mode ask",
        "playbook mode auto",
        "npm test",
    ];

    // Act and Assert
    for command in allowed {
        assert!(
            !session.bash(command).is_denied(),
            "{command:?} must be allowed"
        );
    }
    for command in denied {
        session.bash(command).reason(command);
    }
}

#[test]
fn a_safe_command_chained_to_other_work_is_denied() {
    // Arrange
    let session = Session::at_budget("safe-bash-meta", AutoFrom::Config);
    let suffixes = [
        " && rm -rf x",
        "; rm -rf x",
        " | sh",
        " `x`",
        " $(x)",
        " > out",
        " < in",
        "\nrm -rf x",
    ];

    for suffix in suffixes {
        let command = format!("git status{suffix}");

        // Act
        let outcome = session.bash(&command);

        // Assert
        outcome.reason(&format!("{command:?}"));
    }
}

#[test]
fn write_is_safe_at_budget_reached_only_to_the_park_note_path() {
    // Arrange
    let session = Session::at_budget("safe-write", AutoFrom::Config);
    let dir = session.dir().to_string_lossy().into_owned();
    let cases = [
        (session.park_note(), true),
        (format!("{dir}/./park-note.md"), true),
        ("park-note.md".to_string(), false),
        (format!("{dir}/../park-note.md"), false),
        (format!("{dir}/park-note.md.bak"), false),
        (format!("{dir}/../x"), false),
    ];

    for (path, allowed) in cases {
        // Act
        let outcome = session.call("Write", json!({"file_path": path, "content": "stopped"}));

        // Assert
        assert_eq!(
            !outcome.is_denied(),
            allowed,
            "Write to {path}: {outcome:?}"
        );
    }
}

#[test]
fn edit_is_denied_at_budget_reached_even_on_the_park_note() {
    // Arrange
    let session = Session::at_budget("safe-edit", AutoFrom::Config);

    // Act
    let outcome = session.call(
        "Edit",
        json!({"file_path": session.park_note(), "old_string": "a", "new_string": "b"}),
    );

    // Assert
    outcome.reason("Edit");
}

#[test]
fn playbook_mode_ask_is_denied_at_budget_reached_so_the_model_cannot_lift_the_cap() {
    // Arrange
    let session = Session::at_budget("cap-no-escape", AutoFrom::Config);

    // Act
    let outcome = session.bash("playbook mode ask");

    // Assert
    let reason = outcome.reason("mode ask");
    assert!(reason.contains("budget reached"), "reason: {reason}");
}

#[test]
fn an_unreadable_cost_fails_closed_and_only_then_allows_mode_ask() {
    // Arrange: no transcript file and no telemetry
    let session = Session::auto("unreadable-cost");

    // Act
    let bash = session.bash("npm test");
    let write = session.call(
        "Write",
        json!({"file_path": session.park_note(), "content": "x"}),
    );
    let read = session.call("Read", json!({"file_path": "/etc/hosts"}));
    let escape = session.bash("playbook mode ask");
    let not_exact = session.bash("playbook mode ask; rm -rf x");

    // Assert
    let reason = bash.reason("Bash");
    assert!(reason.contains("cost unreadable"), "reason: {reason}");
    assert!(reason.contains("Stop here"), "reason: {reason}");
    assert!(reason.contains("report to the user"), "reason: {reason}");
    assert!(
        reason.contains("`playbook mode ask` is allowed"),
        "reason: {reason}"
    );
    assert!(!write.is_denied(), "the park note stays writable");
    assert!(!read.is_denied(), "the safe list stays allowed");
    assert!(!escape.is_denied(), "exact playbook mode ask is the escape");
    not_exact.reason("mode ask is exact match only");
}

#[test]
fn a_missing_session_id_denies_non_safe_tools_and_keeps_the_escape() {
    // Arrange
    let session = Session::auto("no-sid");
    let call = |tool: &str, input: Value| {
        session.call_payload(&session.anonymous_tool_payload(tool, input))
    };

    // Act
    let bash = call("Bash", json!({"command": "npm test"}));
    let read = call("Read", json!({"file_path": "/etc/hosts"}));
    let escape = call("Bash", json!({"command": "playbook mode ask"}));

    // Assert
    let reason = bash.reason("Bash");
    assert!(reason.contains("cost unreadable"), "reason: {reason}");
    assert!(!read.is_denied(), "the safe list stays allowed");
    assert!(!escape.is_denied(), "the escape still works");
}

#[derive(Clone, Copy, Debug)]
enum State {
    BudgetReached,
    CostUnreadable,
    MissingSessionId,
}

#[test]
fn the_escape_table_holds_for_every_state_and_auto_source() {
    // Arrange: rows are (state, auto source). Whether `mode ask` is allowed
    // and which reason is shown depend on the state alone.
    let states = [
        State::BudgetReached,
        State::CostUnreadable,
        State::MissingSessionId,
    ];
    for (n, state) in states.into_iter().enumerate() {
        for from in [AutoFrom::Config, AutoFrom::Env] {
            let tag = format!("escape-{n}-{}", from == AutoFrom::Env);
            let label = format!("{state:?} / env={}", from == AutoFrom::Env);
            let session = match state {
                State::BudgetReached => Session::at_budget(&tag, from),
                State::CostUnreadable | State::MissingSessionId => {
                    let session = Session::auto_from(&tag, from);
                    session.seed_budget(&json!(5));
                    session
                }
            };
            let call = |tool: &str, input: Value| match state {
                State::MissingSessionId => {
                    session.call_payload(&session.anonymous_tool_payload(tool, input))
                }
                _ => session.call(tool, input),
            };

            // Act
            let ask = call("Bash", json!({"command": "playbook mode ask"}));
            let other = call("Bash", json!({"command": "npm test"}));
            let read = call("Read", json!({"file_path": "/etc/hosts"}));

            // Assert
            let reason = other.reason(&label);
            match state {
                State::BudgetReached => {
                    ask.reason(&label);
                    assert!(reason.contains("budget reached"), "{label}: {reason}");
                    assert!(reason.contains(&session.park_note()), "{label}: {reason}");
                }
                State::CostUnreadable | State::MissingSessionId => {
                    assert!(!ask.is_denied(), "{label}: mode ask must be allowed");
                    assert!(reason.contains("cost unreadable"), "{label}: {reason}");
                    assert!(reason.contains("Stop here"), "{label}: {reason}");
                }
            }
            assert!(!read.is_denied(), "{label}: the safe list stays allowed");
        }
    }
}

#[test]
fn auto_cost_alone_allows_ask_user_question_under_the_cap() {
    // Arrange: the real AskUserQuestion capture on this session, 1.00 spent
    let session = Session::auto("ask-user-question");
    session.seed_budget(&json!(5));
    session.baseline_then_report("1.00");
    let mut value: Value =
        serde_json::from_str(include_str!("fixtures/hooks/pre-askuserquestion.json"))
            .expect("fixture is JSON");
    value["session_id"] = json!(session.sid);
    value["transcript_path"] = json!(session.transcript().to_string_lossy());

    // Act
    let (out, code) = session.run_with(&value.to_string(), Some("0"), &[]);

    // Assert
    assert_eq!(code, 0);
    assert!(out.trim().is_empty(), "printed {out}");
}

#[test]
fn the_default_main_cap_allows_five_to_twenty_dollars_and_denies_at_two_hundred() {
    // Arrange: only the mode is stored, so the default cap of 200 applies
    let session = Session::auto("default-cap");
    session.baseline_then_report("5.00");

    // Act and Assert: the old cap of 5 and up to 20 deny nothing
    for usd in ["5.00", "12.50", "20.00"] {
        session.append_telemetry(usd);
        assert_eq!(session.bash("npm test"), Outcome::Silent, "{usd}");
    }

    // Act and Assert: one cent under the new cap only warns
    session.append_telemetry("199.99");
    assert!(
        !session.bash("npm test").is_denied(),
        "199.99 denies nothing"
    );

    // Act and Assert: the new cap denies new work and says how to raise it
    session.append_telemetry("200.00");
    let denied = session.bash("npm test");
    let reason = denied.reason("200.00 reaches the cap");
    assert!(reason.contains("$200.00"), "reason: {reason}");
    assert!(reason.contains("`auto.budgetUsd`"), "reason: {reason}");
    assert!(
        reason.contains("`playbook config set auto.budgetUsd 400`"),
        "reason: {reason}"
    );
}

#[test]
fn an_invalid_budget_in_the_config_falls_back_to_the_default_with_a_one_time_note() {
    // Arrange: rows are the invalid values a hand-edited file can hold
    let rows = [json!(0), json!("banana"), json!(-3), json!(0.004)];
    for (n, invalid) in rows.into_iter().enumerate() {
        let session = Session::auto(&format!("bad-budget-{n}"));
        session.seed_budget(&invalid);
        session.append_telemetry("0.00");

        // Act
        let first = session.bash("npm test");
        session.append_telemetry("1.00");
        let second = session.bash("npm test");
        session.append_telemetry("20.00");
        let past_the_old_cap = session.bash("npm test");
        session.append_telemetry("200.00");
        let at_default_cap = session.bash("npm test");

        // Assert
        let Outcome::Note(note) = first else {
            panic!("{invalid}: the first call must warn about the budget, got {first:?}");
        };
        assert!(note.contains("auto.budgetUsd"), "{invalid}: {note}");
        assert!(note.contains("0.01"), "{invalid}: {note}");
        assert!(note.contains("$200.00"), "{invalid}: {note}");
        assert_eq!(second, Outcome::Silent, "{invalid}: the note is one-time");
        assert!(!past_the_old_cap.is_denied(), "{invalid}: 20.00 is allowed");
        at_default_cap.reason(&format!("{invalid}: the default cap of 200 applies"));
    }
}

#[test]
fn a_stored_warn_percentage_is_ignored_and_the_note_fires_at_seventy() {
    // Arrange: rows are (stored warnPct, cents where the first note fires)
    // against a cap of 5, so the built-in 70 means 350 cents.
    let rows = [
        (json!(0), 350),
        (json!(150), 350),
        (json!(70.5), 350),
        (json!("high"), 350),
        (json!(50), 350),
    ];
    for (n, (pct, tier_cents)) in rows.into_iter().enumerate() {
        let session = Session::auto(&format!("bad-warn-pct-{n}"));
        session.seed_auto(json!({"budgetUsd": 5, "warnPct": pct}));
        session.append_telemetry("0.00");
        assert_eq!(
            session.bash("git status"),
            Outcome::Silent,
            "{pct}: baseline"
        );

        // Act
        session.append_telemetry(&format!("{:.2}", (tier_cents - 1) as f64 / 100.0));
        let below = session.bash("npm test");
        session.append_telemetry(&format!("{:.2}", tier_cents as f64 / 100.0));
        let at_tier = session.bash("npm test");

        // Assert
        assert_eq!(below, Outcome::Silent, "{pct}: one cent under the tier");
        assert!(
            matches!(at_tier, Outcome::Note(_)),
            "{pct}: the note is due at {tier_cents} cents: {at_tier:?}"
        );
    }
}

#[test]
fn pre_existing_and_falling_cost_never_deny() {
    // Arrange: rows are how call 1 meets a cost of 4.0 against a cap of 5
    for (n, via_transcript) in [false, true].into_iter().enumerate() {
        let session = Session::auto(&format!("no-deny-{n}"));
        session.seed_budget(&json!(5));
        if via_transcript {
            write_cost(&session.transcript(), 4.0);
        } else {
            session.append_telemetry("4.0");
        }

        // Act: the first call already sees 4.0, which is past the warn tier in
        // absolute terms but is the baseline
        let first = session.bash("npm test");

        // Assert
        assert_eq!(
            first,
            Outcome::Silent,
            "first call, transcript={via_transcript}"
        );
        assert!(session.warned_markers().is_empty());
    }

    // Arrange: a telemetry cost that drops below the baseline
    let session = Session::auto("no-deny-negative");
    session.seed_budget(&json!(5));
    session.append_telemetry("4.0");
    assert_eq!(session.bash("git status"), Outcome::Silent, "baseline call");

    // Act
    session.append_telemetry("3.0");
    let lower = session.bash("npm test");
    session.append_telemetry("4.5");
    let climbing = session.bash("npm test");

    // Assert
    assert_eq!(lower, Outcome::Silent, "negative delta");
    assert_eq!(climbing, Outcome::Silent, "delta of 50 cents");
}

#[test]
fn eight_processes_crossing_the_warn_tier_together_leave_one_marker() {
    // Smoke check: it can catch a gross failure, not prove the race is absent.
    // Arrange: baseline 0.00 settled, then the tier is crossed before any call
    let session = Session::auto("concurrent-warn");
    session.seed_budget(&json!(5));
    session.baseline_then_report("3.50");
    let payload = session.tool_payload("Bash", json!({"command": "npm test"}));

    // Act: start eight hook processes before any is waited on
    let mut children: Vec<_> = (0..8)
        .map(|_| {
            let mut command = hook_command(&session.s, HOOK);
            command
                .env(INTERVAL_VAR, "0")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("playbook spawns")
        })
        .collect();
    for child in &mut children {
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(payload.as_bytes())
            .expect("stdin accepts the payload");
    }
    let outcomes: Vec<Outcome> = children
        .into_iter()
        .map(|child| {
            let out = child.wait_with_output().expect("playbook exits");
            assert_eq!(out.status.code(), Some(0));
            outcome(&String::from_utf8_lossy(&out.stdout))
        })
        .collect();

    // Assert: the marker was created once, and only its creator wrote a note
    assert_eq!(session.warned_markers(), vec!["warned".to_string()]);
    let notes = outcomes
        .iter()
        .filter(|o| matches!(o, Outcome::Note(_)))
        .count();
    assert_eq!(notes, 1, "outcomes: {outcomes:?}");
    assert!(!outcomes.iter().any(Outcome::is_denied));
}

/// Makes `dir` read-only until dropped.
#[cfg(unix)]
struct ReadOnly(PathBuf);

#[cfg(unix)]
impl ReadOnly {
    /// `None` when the mode does not stop this process writing, as for root.
    fn lock(dir: &Path) -> Option<Self> {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).expect("chmod");
        let guard = ReadOnly(dir.to_path_buf());
        fs::write(dir.join("probe"), "").is_err().then_some(guard)
    }
}

#[cfg(unix)]
impl Drop for ReadOnly {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

#[cfg(unix)]
#[test]
fn a_baseline_that_cannot_be_stored_fails_closed() {
    // Arrange: the session dir exists but cannot be written, so every call
    // would otherwise take a fresh baseline and never see the spend grow
    let session = Session::auto("unwritable-baseline");
    session.seed_budget(&json!(5));
    write_cost(&session.transcript(), 1.0);
    fs::create_dir_all(session.dir()).expect("session dir");
    let Some(_locked) = ReadOnly::lock(&session.dir()) else {
        return;
    };

    // Act
    let bash = session.bash("npm test");
    let read = session.call("Read", json!({"file_path": "/etc/hosts"}));
    let escape = session.bash("playbook mode ask");

    // Assert
    let reason = bash.reason("Bash");
    assert!(reason.contains("cost unreadable"), "reason: {reason}");
    assert!(!read.is_denied(), "the safe list stays allowed");
    assert!(!escape.is_denied(), "the escape stays allowed");
}

#[cfg(unix)]
#[test]
fn a_stored_baseline_keeps_enforcing_when_a_later_write_fails() {
    // Arrange: call 1 stores the baseline, then the session dir turns read-only
    let session = Session::auto("unwritable-later");
    session.seed_budget(&json!(5));
    session.baseline_then_report("0.00");
    let Some(_locked) = ReadOnly::lock(&session.dir()) else {
        return;
    };

    // Act
    session.append_telemetry("1.00");
    let under = session.bash("npm test");
    session.append_telemetry("5.00");
    let at_cap = session.bash("npm test");

    // Assert: the loaded baseline still measures the spend
    assert_eq!(under, Outcome::Silent);
    assert!(at_cap.reason("5.00 spent").contains("budget reached"));
}

#[test]
fn a_model_missing_from_the_price_table_without_telemetry_fails_closed() {
    // Arrange: a transcript whose only spend is on an unpriced model
    let session = Session::auto("unpriced-no-telemetry");
    session.seed_budget(&json!(5));
    append(
        &session.transcript(),
        &message_line("msg_unpriced", "claude-model-not-in-the-table", 9_000_000),
    );

    // Act
    let bash = session.bash("npm test");
    let read = session.call("Read", json!({"file_path": "/etc/hosts"}));
    let escape = session.bash("playbook mode ask");

    // Assert
    let reason = bash.reason("Bash");
    assert!(reason.contains("cost unreadable"), "reason: {reason}");
    assert!(!read.is_denied(), "the safe list stays allowed");
    assert!(!escape.is_denied(), "the escape stays allowed");
}

#[test]
fn an_unpriced_model_is_tolerated_when_telemetry_reports_the_cost() {
    // Arrange
    let session = Session::auto("unpriced-with-telemetry");
    session.seed_budget(&json!(5));
    session.append_telemetry("0.00");
    append(
        &session.transcript(),
        &message_line("msg_unpriced", "claude-model-not-in-the-table", 9_000_000),
    );

    // Act
    let first = session.bash("npm test");
    session.append_telemetry("5.00");
    let at_cap = session.bash("npm test");

    // Assert: telemetry is the readable source, and it still trips the cap
    assert_eq!(first, Outcome::Silent);
    assert!(at_cap.reason("5.00 spent").contains("budget reached"));
}

#[test]
fn a_config_that_turns_unreadable_mid_session_keeps_the_cap_on() {
    // Arrange: a session at its cap whose config file then becomes garbage,
    // so the default cap of 200 applies, and the spend reaches it
    let session = Session::at_budget("config-broken", AutoFrom::Config);
    auto_config::seed_raw_global_config(&session.s.home, "{not json");
    session.append_telemetry("200.00");

    // Act
    let bash = session.bash("npm test");

    // Assert
    let reason = bash.reason("npm test");
    assert!(reason.contains("budget reached"), "reason: {reason}");
}

#[test]
fn an_unreadable_config_does_not_start_enforcing_in_a_session_it_never_tracked() {
    // Arrange: no state file exists for this session
    let session = Session::auto("config-broken-fresh");
    auto_config::seed_raw_global_config(&session.s.home, "{not json");

    // Act
    let (out, code) = session.run();

    // Assert
    assert_eq!(code, 0);
    assert!(out.trim().is_empty(), "printed {out}");
    assert!(!session.dir().join(STATE_FILE).exists());
}

// Self-protection. While `auto` runs under the cap the model must not be able
// to lower or switch off its own guard, so the hook denies the commands and
// file writes that change `mode`, `auto.*` or `fix.*`. A command is denied
// when it CONTAINS a guarded invocation. It is split into simple commands on
// unquoted `;`, `&`, `|`, `(`, `)`, backticks and line breaks, with quotes
// removed from the words, so quoted prose is data and never an invocation.
// Heredoc bodies and comments are skipped. In each simple command, leading
// `NAME=value` words, redirections, shell keywords (`if`, `then`, `do`, ...)
// and the wrappers `env`, `command`, `exec`, `time`, `nice`, `timeout` and
// `sudo` (with their options) are skipped, and the next word, by its file
// name and ignoring case, must be `playbook`, or `cargo run -- ...` for the
// same arguments. The invocation is guarded when it is `mode ask`,
// `mode auto`, or `config set` whose key (the first word after `set` that does
// not start with `-`) is `mode` or starts with `auto.` or `fix.`. Words after
// the guarded ones do not matter. `bash -c` and the like are not looked into.

const SELF_PROTECTION_REASON: &str = "locked while auto is on";

impl Session {
    /// Auto from `from`, cap 5, 1.00 spent since the baseline: under the cap
    /// and under the warn tier.
    fn under_budget(tag: &str, from: AutoFrom) -> Self {
        let session = Self::auto_from(tag, from);
        session.seed_budget(&json!(5));
        session.baseline_then_report("1.00");
        session
    }

    fn tier_files(&self) -> [(&'static str, PathBuf); 9] {
        let root = self.s.home.join(".config/playbook");
        [
            ("global", root.join("config.json")),
            ("org", root.join("orgs/acme/config.json")),
            ("repo", root.join("repos/acme/widgets/.config/config.json")),
            ("database", root.join("playbook.db")),
            ("database wal", root.join("playbook.db-wal")),
            ("database shm", root.join("playbook.db-shm")),
            ("config database", root.join("data/config.db")),
            ("config database wal", root.join("data/config.db-wal")),
            ("memory database", root.join("data/memory.db")),
        ]
    }
}

fn both_sources() -> [(AutoFrom, &'static str); 2] {
    [(AutoFrom::Config, "config"), (AutoFrom::Env, "env")]
}

#[test]
fn mode_switches_and_guarded_config_writes_are_denied_in_auto_under_the_cap() {
    // Arrange
    let guarded = [
        "playbook mode ask",
        "playbook mode auto",
        "playbook config set auto.budgetUsd 1000",
        "playbook config set auto.warnPct 100",
        "playbook config set auto.anything 1",
        "playbook config set fix.maxFiles 99",
        "playbook config set fix.maxLines 1",
        "playbook config set mode ask",
        "playbook config import saved.json",
        "sqlite3 ~/.config/playbook/playbook.db 'update config set value_json=1'",
        "sqlite3 ~/.config/playbook/data/config.db 'update config set value_json=1'",
        "sqlite3 ~/.config/playbook/data/memory.db 'delete from nodes'",
        "playbook config set mode auto",
        "playbook config set --global mode ask",
        "playbook config set mode ask --org",
        "playbook config set --org auto.budgetUsd 1000",
    ];
    for (from, source) in both_sources() {
        let session = Session::under_budget(&format!("guard-{source}"), from);
        for command in guarded {
            // Act
            let outcome = session.bash(command);

            // Assert
            let reason = outcome.reason(&format!("{source}: {command}"));
            assert!(
                reason.to_lowercase().contains(SELF_PROTECTION_REASON),
                "{source}: {command}: {reason}"
            );
        }
    }
}

#[test]
fn read_only_mode_and_config_commands_and_near_misses_are_not_denied() {
    // Arrange
    let allowed = [
        "playbook mode status",
        "playbook mode status --json",
        "playbook config get mode",
        "playbook config get auto.budgetUsd",
        "playbook config list",
        "playbook config set other.key x",
        "playbook config set pr.review.enabled false",
        "playbook config set model x",
        "playbook config set modes x",
        "playbook config set fixes.maxFiles 1",
        "playbook config set worktreeCleanup.staleAfterDays 7",
        "playbook mode",
        "playbook modes ask",
        "playbookx mode ask",
        "echo playbook mode ask",
        "git commit -m 'document playbook mode ask'",
        "sqlite3 ~/.config/playbook/data/config.db 'select * from config'",
        "sqlite3 ~/.config/playbook/data/memory.db .tables 2>/dev/null",
        "ls ~/.config/playbook/data 2>/dev/null",
    ];
    for (from, source) in both_sources() {
        let session = Session::under_budget(&format!("near-miss-{source}"), from);
        for command in allowed {
            // Act
            let outcome = session.bash(command);

            // Assert
            assert!(
                !outcome.is_denied(),
                "{source}: {command:?} must not be denied: {outcome:?}"
            );
        }
    }
}

#[test]
fn a_guarded_invocation_anywhere_in_the_command_string_is_denied() {
    // Arrange
    let guarded = [
        "playbook mode ask && echo done",
        "playbook mode ask; echo done",
        "playbook mode ask | cat",
        "playbook mode ask || true",
        "playbook mode ask &",
        "cd x && playbook mode ask",
        "cd x; playbook mode auto",
        "true || playbook config set mode ask",
        "echo hi | playbook config set auto.budgetUsd 1000",
        "echo hi\nplaybook mode ask",
        "echo hi\r\nplaybook mode ask",
        "playbook   mode   ask",
        "  playbook mode ask  ",
        "playbook\tmode\task",
        "playbook config  set   auto.warnPct   100",
        "PLAYBOOK_MODE=ask playbook mode ask",
        "A=1 B=2 playbook config set mode ask",
        "env playbook mode ask",
        "env A=1 playbook mode ask",
        "command playbook mode auto",
        "exec playbook mode ask",
        "/usr/local/bin/playbook mode ask",
        "./target/debug/playbook config set fix.maxFiles 99",
        "playbook mode ask --extra",
        "playbook config set \\\nauto.budgetUsd 1000",
        "playbook \\\nmode auto",
        "playbook \\\n  mode auto",
        "playbook mode \\\nask",
        "echo $((1<<2))\nplaybook mode auto",
        "echo \"$((1<<2))\"\nplaybook mode ask",
        "echo \"a $(echo \"b\") c\"; playbook mode ask",
        r#"playbook mode "ask""#,
        r#"playbook mode 'auto'"#,
        r#"playbook config set "auto.budgetUsd" 1000"#,
        "playbook config set 'mode' ask",
        "'playbook' mode ask",
        r#""playbook" mode ask"#,
        r"play\book mode ask",
        "(playbook mode ask)",
        "( cd x && playbook mode ask )",
        "echo $(playbook mode ask)",
        "echo `playbook mode ask`",
        "if true; then playbook mode ask; fi",
        "for i in 1; do playbook mode ask; done",
        "{ playbook mode ask; }",
        "! playbook mode ask",
        "env -i playbook mode ask",
        "env -u HOME A=1 playbook mode ask",
        "command -p playbook mode ask",
        "exec -a x playbook mode ask",
        "time playbook mode ask",
        "time -p playbook mode ask",
        "nice playbook mode ask",
        "nice -n 5 playbook mode ask",
        "timeout 5 playbook mode ask",
        "timeout -s KILL 5 playbook mode ask",
        "sudo playbook mode ask",
        "sudo -u root playbook config set auto.budgetUsd 100",
        "sudo env -i nice timeout 9 playbook mode auto",
        "cargo run -- config set auto.budgetUsd 100",
        "cargo run -q --release -- mode ask",
        "cargo r -- config set mode ask",
        "PLAYBOOK mode ask",
        "/usr/local/bin/Playbook mode auto",
        ">/dev/null playbook mode ask",
        "2>/dev/null playbook mode ask",
        "cat <<EOF >/dev/null\nnote\nEOF\nplaybook mode ask",
        "cat <<'EOF'\nplaybook mode auto\nEOF\nplaybook mode ask",
        "cat <<-EOF\n\tbody\n\tEOF\nplaybook mode ask",
        "echo hi # note\nplaybook mode ask",
    ];
    let session = Session::under_budget("whole-command", AutoFrom::Config);

    for command in guarded {
        // Act
        let outcome = session.bash(command);

        // Assert
        outcome.reason(&format!("{command:?}"));
    }
}

#[test]
fn a_guarded_word_that_is_not_a_leading_playbook_invocation_is_not_denied() {
    // Arrange: the match is on the invocation, not on the words anywhere
    let allowed = [
        "echo done && echo playbook mode ask",
        "cat notes.md | grep playbook mode ask",
        "ls; echo mode ask",
        "playbook mode status && echo mode ask",
        r#"git commit -m "docs: explain it; playbook mode ask turns it off""#,
        "git commit -m 'fix: x && playbook config set auto.budgetUsd 1000'",
        r#"echo "playbook mode ask""#,
        r#"echo 'a | playbook mode auto'"#,
        r#"git commit -m "a (playbook mode ask) b""#,
        "cat <<EOF\nplaybook config set auto.budgetUsd 10\nEOF",
        "git commit -m \"$(cat <<'EOF'\nsay \"playbook mode ask\" (twice)\nEOF\n)\"",
        "cat <<'EOF' > notes.md\nplaybook mode ask\nEOF",
        "cat <<-EOF\n\tplaybook mode ask\n\tEOF",
        "cat <<EOF | grep x\nplaybook mode ask\nEOF",
        "echo hi # playbook mode ask",
        "# playbook mode ask",
        "cargo run -- mode status",
        "cargo test -- mode ask",
        "cargo build --release",
        "time ls",
        "sudo ls",
        "env",
        "timeout 5 npm test",
        "git commit -m x <<< 'playbook mode ask'",
    ];
    let session = Session::under_budget("not-leading", AutoFrom::Config);

    for command in allowed {
        // Act
        let outcome = session.bash(command);

        // Assert
        assert!(!outcome.is_denied(), "{command:?}: {outcome:?}");
    }
}

#[test]
fn write_and_edit_of_every_config_tier_file_are_denied_in_auto_under_the_cap() {
    // Arrange
    for (from, source) in both_sources() {
        let session = Session::under_budget(&format!("config-files-{source}"), from);
        let root = session.s.home.join(".config/playbook");
        let mut targets: Vec<(String, String)> = session
            .tier_files()
            .into_iter()
            .map(|(tier, path)| (tier.to_string(), path.to_string_lossy().into_owned()))
            .collect();
        let root = root.to_string_lossy();
        targets.push(("dot segment".into(), format!("{root}/./config.json")));
        targets.push((
            "parent segment".into(),
            format!("{root}/runtime/../config.json"),
        ));
        targets.push(("upper case file".into(), format!("{root}/CONFIG.JSON")));
        targets.push((
            "upper case directories".into(),
            root.to_uppercase() + "/Repos/Acme/Widgets/.Config/Config.json",
        ));

        for (label, path) in targets {
            for tool in ["Write", "Edit"] {
                // Act
                let input = json!({"file_path": path, "content": "{}", "old_string": "a", "new_string": "b"});
                let outcome = session.call(tool, input);

                // Assert
                let reason = outcome.reason(&format!("{source}: {tool} {label} {path}"));
                assert!(
                    reason.to_lowercase().contains(SELF_PROTECTION_REASON),
                    "{source}: {tool} {label}: {reason}"
                );
            }
        }
    }
}

#[test]
fn write_and_edit_of_other_files_are_not_denied_in_auto_under_the_cap() {
    // Arrange
    let session = Session::under_budget("other-files", AutoFrom::Config);
    let root = session.s.home.join(".config/playbook");
    let root = root.to_string_lossy();
    let home = session.s.home.to_string_lossy();
    let paths = [
        format!("{root}/config.json.bak"),
        format!("{root}/orgs/acme/notes.md"),
        format!("{root}/repos/acme/widgets/.config/other.json"),
        format!("{root}/runtime/{}/note.md", session.sid),
        format!("{home}/project/config.json"),
        "src/main.rs".to_string(),
    ];

    for path in paths {
        for tool in ["Write", "Edit"] {
            // Act
            let outcome = session.call(tool, json!({"file_path": path}));

            // Assert
            assert!(!outcome.is_denied(), "{tool} {path}: {outcome:?}");
        }
    }
}

#[test]
fn a_self_protection_deny_does_not_use_up_the_warn_note() {
    // Arrange: 3.50 of a 5.00 cap, which is exactly the warn tier
    let session = Session::auto("guard-then-warn");
    session.seed_budget(&json!(5));
    session.baseline_then_report("3.50");

    // Act
    let guarded = session.bash("playbook mode ask");
    let marker_after_deny = session.warned_markers();
    let next = session.bash("npm test");

    // Assert
    guarded.reason("guarded command at the warn tier");
    assert!(
        marker_after_deny.is_empty(),
        "the deny must not claim the note"
    );
    assert!(
        matches!(next, Outcome::Note(_)),
        "the warn note is still owed: {next:?}"
    );
}

#[test]
fn ask_mode_leaves_self_protection_commands_and_config_writes_alone() {
    // Arrange: rows are (config mode, PLAYBOOK_MODE)
    let rows: [(&str, &str, Option<&str>); 2] = [
        ("config ask", "ask", None),
        ("env ask beats config auto", "auto", Some("ask")),
    ];
    for (n, (label, config, env_mode)) in rows.into_iter().enumerate() {
        let session = Session::auto(&format!("guard-ask-{n}"));
        session.s.seed_mode_config(config);
        session.append_telemetry("1.0");
        let before = tree(&session.runtime_root());
        let env: Vec<(&str, &str)> = env_mode.map(|m| ("PLAYBOOK_MODE", m)).into_iter().collect();
        let config_file = session.tier_files()[0].1.to_string_lossy().into_owned();
        let calls = [
            ("Bash", json!({"command": "playbook mode auto"})),
            ("Bash", json!({"command": "cd x && playbook mode ask"})),
            (
                "Bash",
                json!({"command": "playbook config set auto.budgetUsd 1000"}),
            ),
            ("Write", json!({"file_path": config_file})),
            ("Edit", json!({"file_path": config_file})),
        ];

        for (tool, input) in calls {
            // Act
            let (out, code) =
                session.run_with(&session.tool_payload(tool, input.clone()), Some("0"), &env);

            // Assert
            assert_eq!(code, 0, "{label}: {tool} {input}");
            assert!(
                out.trim().is_empty(),
                "{label}: {tool} {input}: printed {out}"
            );
        }
        assert_eq!(tree(&session.runtime_root()), before, "{label}");
    }
}

#[cfg(unix)]
fn bash_call(s: &Scratch) -> String {
    json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": "ls"},
        "session_id": "slug-lookup",
        "cwd": s.cwd,
    })
    .to_string()
}

#[cfg(unix)]
#[test]
fn default_mode_with_no_scoped_config_never_spawns_git() {
    // Arrange
    let s = scratch("no-spawn");
    let shim = auto_git_shim::git_shim(&s);

    // Act
    let (out, code) = auto_run::run_hook(&s, "auto-cost", &bash_call(&s), &[("PATH", &shim.path)]);

    // Assert
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "");
    assert_eq!(
        shim.calls(),
        0,
        "the slug cannot matter without a scoped tier"
    );
}

#[cfg(unix)]
#[test]
fn a_repo_tier_auto_mode_still_looks_up_the_slug_and_tracks_the_session() {
    // Arrange
    let s = scratch("repo-tier");
    s.seed_repo_mode_config("acme", "widgets", "auto");
    let shim = auto_git_shim::git_shim(&s);

    // Act
    let (_, code) = auto_run::run_hook(&s, "auto-cost", &bash_call(&s), &[("PATH", &shim.path)]);

    // Assert
    assert_eq!(code, 0);
    assert_eq!(shim.calls(), 1);
    assert!(
        tree(&s.home)
            .iter()
            .any(|p| p.contains("slug-lookup") && p.contains(STATE_FILE)),
        "auto mode from the repo tier must start tracking cost"
    );
}

#[test]
fn env_sourced_auto_never_names_a_remedy_that_cannot_work() {
    // Arrange: PLAYBOOK_MODE=auto in the environment beats `playbook mode ask`
    let unreadable = Session::auto_from("env-unreadable", AutoFrom::Env);
    unreadable.seed_budget(&json!(5));
    let capped = Session::at_budget("env-capped", AutoFrom::Env);

    // Act
    let unreadable_reason = unreadable.bash("npm test");
    let capped_reason = capped.bash("npm test");

    // Assert
    for (label, outcome) in [
        ("unreadable", &unreadable_reason),
        ("capped", &capped_reason),
    ] {
        let reason = outcome.reason(label);
        assert!(reason.contains("PLAYBOOK_MODE"), "{label}: {reason}");
        assert!(reason.contains("unset"), "{label}: {reason}");
        assert!(
            !reason.contains("`playbook mode ask` is allowed"),
            "{label}: {reason}"
        );
        assert!(
            !reason.contains("or `playbook mode ask`"),
            "{label}: {reason}"
        );
    }
}

#[test]
fn config_sourced_auto_still_names_mode_ask() {
    let session = Session::at_budget("cfg-capped", AutoFrom::Config);
    let reason = session.bash("npm test");
    assert!(
        reason.reason("capped").contains("`playbook mode ask`"),
        "{reason:?}"
    );
}

#[test]
fn the_invalid_budget_note_asks_the_user_instead_of_telling_the_model_to_run_a_guarded_command() {
    let session = Session::auto("bad-budget-wording");
    session.seed_budget(&json!(0));
    session.append_telemetry("0.00");
    let Outcome::Note(note) = session.bash("npm test") else {
        panic!("expected a note");
    };
    assert!(note.contains("Ask the user to run"), "{note}");
    assert!(!note.contains("Fix it with"), "{note}");
}

#[test]
fn at_the_cap_the_skills_status_probe_with_a_flag_stays_allowed() {
    let session = Session::at_budget("status-flag", AutoFrom::Config);
    for command in [
        "playbook mode status --flag auto --json",
        "playbook mode status --flag ask --json",
        "playbook mode status --json --flag auto",
    ] {
        assert!(!session.bash(command).is_denied(), "{command:?}");
    }
    for command in [
        "playbook mode status --flag bogus --json",
        "playbook mode status --flag auto --json extra",
        "playbook mode status --flag",
    ] {
        session.bash(command).reason(command);
    }
}

#[test]
fn at_the_cap_an_angle_bracket_inside_quotes_is_not_a_redirection() {
    let session = Session::at_budget("quoted-angle", AutoFrom::Config);
    for command in [
        "git commit -m \"feat: a -> b\"",
        "git commit -m 'fix: x < y and y > z'",
        "playbook pr create --title \"fix: a > b\" --body-file /tmp/pr-body.md",
    ] {
        assert!(!session.bash(command).is_denied(), "{command:?} must pass");
    }
    for command in [
        "git status > out.txt",
        "git status >out.txt",
        "git log 2>err.txt",
        "git log < in.txt",
        "git status \"x\" > out.txt",
    ] {
        session.bash(command).reason(command);
    }
}

/// A repo on `branch` with a commit, a bare `origin`, and the branch pushed
/// with its upstream set, so a bare `git push` resolves its destination.
fn repo_on_branch(tag: &str, branch: &str) -> PathBuf {
    let root = scratch(tag).cwd;
    let dir = root.join("repo");
    let origin = root.join("origin.git");
    fs::create_dir_all(&dir).expect("repo dir");
    let git = |at: &Path, args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(at)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    };
    git(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
    git(&dir, &["init", "-q", "-b", branch]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "seed"]);
    git(
        &dir,
        &["remote", "add", "origin", origin.to_str().expect("utf8")],
    );
    git(&dir, &["push", "-q", "-u", "origin", branch]);
    dir
}

fn call_in(session: &Session, repo: &Path, command: &str) -> Outcome {
    let mut value: Value =
        serde_json::from_str(&session.tool_payload("Bash", json!({"command": command})))
            .expect("json");
    value["cwd"] = json!(repo.to_string_lossy());
    session.call_payload(&value.to_string())
}

#[test]
fn at_the_cap_a_bare_push_is_allowed_only_on_a_non_protected_current_branch() {
    let feature = Session::at_budget("push-feature", AutoFrom::Config);
    let main = Session::at_budget("push-main", AutoFrom::Config);
    let feature_repo = repo_on_branch("push-feature-repo", "feat/x");
    let main_repo = repo_on_branch("push-main-repo", "main");
    for command in ["git push", "git push origin", "git push -u origin HEAD"] {
        assert!(
            !call_in(&feature, &feature_repo, command).is_denied(),
            "{command:?} on feat/x must be allowed"
        );
        call_in(&main, &main_repo, command).reason(command);
    }
    for command in [
        "git push --force",
        "git push origin main",
        "git push --force origin HEAD",
        "git push origin v1.0.0",
        "git push --follow-tags origin feat/x",
    ] {
        call_in(&feature, &feature_repo, command).reason(command);
    }
    // A push option's value is not a remote, so `-o x origin` is a bare push
    call_in(&main, &main_repo, "git push -o ci.skip origin").reason("-o on main");
    assert!(!call_in(&feature, &feature_repo, "git push -o ci.skip origin").is_denied());
    // The branch a push resolves is the one in the repo it runs in.
    let sibling = main_repo.to_string_lossy().into_owned();
    call_in(&feature, &feature_repo, &format!("git -C {sibling} push")).reason("git -C main push");
}

#[test]
fn a_bare_push_that_git_would_send_to_a_protected_branch_is_denied() {
    // feat/x tracks origin/main and push.default=upstream sends it there
    let session = Session::at_budget("push-upstream-main", AutoFrom::Config);
    let repo = repo_on_branch("push-upstream-main-repo", "feat/x");
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    };
    git(&["push", "-q", "origin", "feat/x:main"]);
    git(&["branch", "-u", "origin/main"]);
    git(&["config", "push.default", "upstream"]);
    call_in(&session, &repo, "git push").reason("bare push to main");
}

#[test]
fn a_subagent_at_the_session_ceiling_can_still_close_its_work_and_is_told_what_to_do() {
    // Arrange: the whole tree has spent the ceiling, so every caller is parked
    let session = Session::auto("ceiling-agent");
    let config = json!({"mode": "auto", "auto": {"budgetUsd": 50, "sessionCeilingUsd": 5}});
    auto_config::seed_raw_global_config(&session.s.home, &config.to_string());
    session.baseline_then_report("5.00");
    let as_agent = |tool: &str, input: Value| {
        let mut value: Value =
            serde_json::from_str(&session.tool_payload(tool, input)).expect("json");
        value["agent_id"] = json!("abc123");
        session.call_payload(&value.to_string())
    };

    // Act
    let push = as_agent("Bash", json!({"command": "git push origin feat/x"}));
    let pr = as_agent(
        "Bash",
        json!({"command": "playbook pr create --title t --body-file b.md"}),
    );
    let skill = as_agent("Skill", json!({"skill": "playbook:commit"}));
    let build = as_agent("Bash", json!({"command": "cargo build"}));

    // Assert
    assert!(!push.is_denied() && !pr.is_denied() && !skill.is_denied());
    let reason = build.reason("build");
    assert!(reason.contains("park-note-abc123.md"), "{reason}");
    assert!(reason.contains("report to the user"), "{reason}");
}

#[test]
fn a_parked_main_session_leaves_a_marker_that_clears_once_it_is_under_the_cap() {
    // Arrange
    let session = Session::at_budget("parked-marker", AutoFrom::Config);
    let marker = session.dir().join("parked");

    // Act and Assert: the deny leaves the marker the Stop hook reads
    session.bash("npm test").reason("capped");
    assert!(marker.exists());

    // A raised cap lets the next call through and drops the marker
    session.seed_budget(&json!(50));
    assert!(!session.bash("git status").is_denied());
    assert!(!marker.exists());
}

#[test]
fn a_parked_subagent_does_not_set_the_main_session_marker() {
    let session = Session::auto("parked-agent");
    let config = json!({"mode": "auto", "auto": {"budgetUsd": 50, "agentBudgetUsd": 1}});
    auto_config::seed_raw_global_config(&session.s.home, &config.to_string());
    session.baseline_then_report("0.00");
    let mut value: Value =
        serde_json::from_str(&session.tool_payload("Bash", json!({"command": "npm test"})))
            .expect("json");
    value["agent_id"] = json!("zz");
    append(&session.subagent("agent-zz.jsonl"), &assistant_line(2.0));
    session
        .call_payload(&value.to_string())
        .reason("agent capped");
    assert!(!session.dir().join("parked").exists());
}

#[test]
fn an_unreadable_main_session_leaves_the_parked_marker_but_a_subagent_does_not() {
    let session = Session::auto("parked-unreadable");
    session.seed_budget(&json!(5));
    let mut agent: Value =
        serde_json::from_str(&session.tool_payload("Bash", json!({"command": "npm test"})))
            .expect("json");
    agent["agent_id"] = json!("zz");
    session.call_payload(&agent.to_string()).reason("agent");
    assert!(!session.dir().join("parked").exists());
    session.bash("npm test").reason("main");
    assert!(session.dir().join("parked").exists());
}
