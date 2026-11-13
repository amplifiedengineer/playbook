// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Binary-level tests for `playbook handoff` and the SessionStart hook's
//! handoff load and log. Every test uses a scratch HOME, never the real one.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Env {
    home: PathBuf,
    cwd: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Env {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "pbk-handoff-{}-{n}-{tag}",
            playbook::testing::run_id()
        ));
        let home = root.join("home");
        let cwd = root.join("work").join("repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        Env {
            home: home.canonicalize().unwrap(),
            cwd: cwd.canonicalize().unwrap(),
        }
    }

    fn run_in(&self, cwd: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .env_remove("CI")
            .env_remove("PLAYBOOK_HEADLESS")
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("PWD", cwd)
            .env("CLAUDE_PLUGIN_ROOT", env!("PLAYBOOK_REPO_ROOT"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn run(&self, args: &[&str], stdin: &str) -> (i32, String, String) {
        self.run_in(&self.cwd, args, stdin)
    }

    fn session_start(&self, cwd: &Path, source: &str) -> String {
        let payload = format!(
            r#"{{"hook_event_name":"SessionStart","source":"{source}","session_id":"sid-{source}"}}"#
        );
        let (code, stdout, _) = self.run_in(cwd, &["hook", "session-init"], &payload);
        assert_eq!(code, 0);
        stdout
    }

    fn handoff_dir(&self) -> PathBuf {
        self.home.join(".config/playbook/runtime/handoff")
    }

    fn log(&self) -> String {
        fs::read_to_string(self.home.join(".config/playbook/runtime/session-start.log"))
            .unwrap_or_default()
    }
}

#[test]
fn save_then_show_round_trips_without_consuming() {
    let env = Env::new("roundtrip");

    let (code, out, _) = env.run(&["handoff", "save"], "# Topic\nNEXT-STEP-ONE\n");
    let (_, shown_once, _) = env.run(&["handoff", "load"], "");
    let (_, shown_twice, _) = env.run(&["handoff", "load"], "");

    assert_eq!(code, 0);
    assert!(out.starts_with("handoff saved: "), "{out}");
    assert!(shown_once.contains("NEXT-STEP-ONE"), "{shown_once}");
    assert!(
        shown_twice.contains("NEXT-STEP-ONE"),
        "show must not consume: {shown_twice}"
    );
}

#[test]
fn the_old_show_name_is_an_unknown_subcommand() {
    // Arrange
    let env = Env::new("show-gone");
    env.run(&["handoff", "save"], "# Topic\nALIAS-STEP\n");

    // Act
    let (code, out, err) = env.run(&["handoff", "show"], "");

    // Assert
    assert_eq!(code, 2, "{err}");
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("unrecognized subcommand 'show'"), "{err}");
}

#[test]
fn save_refuses_empty_input_and_writes_nothing() {
    let env = Env::new("empty");

    let (code, _, err) = env.run(&["handoff", "save"], "   \n");

    assert_eq!(code, 1);
    assert!(err.contains("empty handoff"), "{err}");
    assert!(!env.handoff_dir().exists() || fs::read_dir(env.handoff_dir()).unwrap().count() == 0);
}

#[cfg(unix)]
#[test]
fn saved_files_and_their_directory_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new("mode");

    env.run(&["handoff", "save"], "# x\n");

    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&env.handoff_dir()), 0o700);
    let file = fs::read_dir(env.handoff_dir())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(mode(&file), 0o600);
}

#[test]
fn dir_works_from_an_unrelated_working_directory() {
    let env = Env::new("dir");
    let elsewhere = env.home.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let target = env.cwd.to_str().unwrap();

    let (code, _, _) = env.run_in(
        &elsewhere,
        &["handoff", "save", "--dir", target],
        "# Topic\nVIA-DIR\n",
    );
    let (_, shown, _) = env.run_in(&elsewhere, &["handoff", "load", "--dir", target], "");
    let (_, here, _) = env.run(&["handoff", "load"], "");

    assert_eq!(code, 0);
    assert!(shown.contains("VIA-DIR"), "{shown}");
    assert!(
        here.contains("VIA-DIR"),
        "the directory's own session sees it: {here}"
    );
}

#[test]
fn a_bad_dir_fails_with_a_clear_message() {
    let env = Env::new("baddir");

    let (code, _, err) = env.run(
        &["handoff", "save", "--dir", "/no/such/dir/anywhere"],
        "# x\n",
    );

    assert_eq!(code, 1);
    assert!(err.contains("is not a directory"), "{err}");
}

#[test]
fn a_clear_start_injects_the_handoff_archives_it_and_logs_the_start() {
    let env = Env::new("clear");
    env.run(&["handoff", "save"], "# Topic\nINJECT-ME\n");

    let stdout = env.session_start(&env.cwd, "clear");

    assert!(
        stdout.contains("INJECT-ME"),
        "the hook must inject it: {stdout}"
    );
    let index = fs::read_to_string(env.home.join(".config/playbook/gc/index.jsonl")).unwrap();
    assert!(index.contains("\"state\":\"done\""), "{index}");
    assert!(
        !fs::read_dir(env.handoff_dir())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".md")),
        "the loaded handoff must leave the folder"
    );
    let (_, shown, _) = env.run(&["handoff", "load"], "");
    assert!(
        shown.contains("already loaded by the last session start"),
        "{shown}"
    );
    assert!(shown.contains("INJECT-ME"), "{shown}");
    let log = env.log();
    let fields: Vec<&str> = log.lines().last().unwrap().split('\t').collect();
    assert_eq!(fields.len(), 5, "{log}");
    assert_eq!(
        (fields[1], fields[2], fields[4]),
        ("clear", "sid-clear", "1")
    );
}

#[test]
fn a_start_with_no_handoff_still_logs_with_zero_injected() {
    let env = Env::new("none");

    env.session_start(&env.cwd, "startup");

    let log = env.log();
    let fields: Vec<&str> = log.lines().last().unwrap().split('\t').collect();
    assert_eq!((fields[1], fields[4]), ("startup", "0"), "{log}");
}

#[test]
fn another_directorys_handoff_is_listed_by_show_but_never_injected() {
    let env = Env::new("other");
    let other = env.home.join("work").join("other-repo");
    fs::create_dir_all(&other).unwrap();
    env.run_in(&other, &["handoff", "save"], "# Other topic\nOTHER-ONLY\n");

    let stdout = env.session_start(&env.cwd, "clear");
    let (_, shown, _) = env.run(&["handoff", "load"], "");

    assert!(!stdout.contains("OTHER-ONLY"), "{stdout}");
    assert!(
        shown.starts_with("No handoff saved for this directory."),
        "{shown}"
    );
    assert!(shown.contains("Other topic"), "{shown}");
}

#[test]
fn a_child_directory_handoff_is_not_loaded_by_its_parent() {
    let env = Env::new("child");
    let child = env.cwd.join("sub");
    fs::create_dir_all(&child).unwrap();
    env.run_in(&child, &["handoff", "save"], "# Child\nCHILD-ONLY\n");

    let stdout = env.session_start(&env.cwd, "clear");

    assert!(!stdout.contains("CHILD-ONLY"), "{stdout}");
}

#[test]
fn status_shows_the_clear_event_and_the_counts() {
    let env = Env::new("status");
    env.run(&["handoff", "save"], "# Topic\nSTATUS\n");
    env.session_start(&env.cwd, "clear");
    env.run(&["handoff", "save"], "# Second\nSTATUS-2\n");

    let (code, out, _) = env.run(&["handoff", "status"], "");

    assert_eq!(code, 0);
    assert!(out.contains("clear"), "{out}");
    assert!(out.contains("1 unread, 1 already loaded"), "{out}");
    assert!(out.contains("/playbook:handoff load"), "{out}");
}

#[cfg(unix)]
#[test]
fn a_read_only_runtime_dir_does_not_fail_the_hook() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new("ro");
    let runtime = env.home.join(".config/playbook/runtime");
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o500)).unwrap();

    let (code, _, _) = env.run(
        &["hook", "session-init"],
        r#"{"source":"clear","session_id":"ro"}"#,
    );

    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        code, 0,
        "logging is best-effort and must never fail the hook"
    );
}

// ---- context hygiene loop: nudge, save, /clear, resume ----

const BIG: u64 = 300_000;

impl Env {
    fn transcript(&self, name: &str, tokens: u64) -> PathBuf {
        let path = self.home.join(name);
        let line = format!(
            r#"{{"type":"assistant","isSidechain":false,"message":{{"usage":{{"input_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":{tokens}}}}}}}"#
        );
        fs::write(&path, format!("{{\"type\":\"user\"}}\n{line}\n")).unwrap();
        path
    }

    /// A transcript of `lines`, one JSON record each.
    fn transcript_of(&self, name: &str, lines: &[String]) -> PathBuf {
        let path = self.home.join(name);
        fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    fn prompt(&self, sid: &str, transcript: &Path) -> (i32, String) {
        let payload = format!(
            r#"{{"hook_event_name":"UserPromptSubmit","session_id":"{sid}","transcript_path":"{}"}}"#,
            transcript.display()
        );
        let (code, out, _) = self.run(&["hook", "context-hygiene"], &payload);
        (code, out)
    }

    fn end_clear(&self, sid: &str) {
        let payload =
            format!(r#"{{"hook_event_name":"SessionEnd","session_id":"{sid}","reason":"clear"}}"#);
        self.run(&["hook", "session-clean-exit"], &payload);
    }

    fn start_with_session(&self, cwd: &Path, source: &str, sid: &str) -> String {
        let payload = format!(
            r#"{{"hook_event_name":"SessionStart","source":"{source}","session_id":"{sid}"}}"#
        );
        let (code, out, _) = self.run_in(cwd, &["hook", "session-init"], &payload);
        assert_eq!(code, 0);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_default();
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    /// A start after `/clear` or at startup: the session id is new.
    fn clear_start(&self, cwd: &Path, source: &str) -> String {
        self.start_with_session(cwd, source, &format!("new-{source}"))
    }

    /// A start after `/compact`: the session id stays the same.
    fn compact_start(&self, cwd: &Path, sid: &str) -> String {
        self.start_with_session(cwd, "compact", sid)
    }

    fn save_for(&self, sid: &str, text: &str) {
        let (code, _, err) = self.run(&["handoff", "save", "--session", sid], text);
        assert_eq!(code, 0, "{err}");
    }
}

#[test]
fn below_the_threshold_the_hook_is_silent() {
    let env = Env::new("hy-below");
    let t = env.transcript("t.jsonl", 100_000);
    let (code, out) = env.prompt("sess1", &t);
    assert_eq!((code, out.trim()), (0, ""));
}

#[test]
fn a_crossing_fires_once_with_a_short_message_naming_the_save_command() {
    let env = Env::new("hy-once");
    let t = env.transcript("t.jsonl", BIG);

    let (_, first) = env.prompt("sess1", &t);
    let (_, second) = env.prompt("sess1", &t);

    let v: serde_json::Value = serde_json::from_str(&first).expect("one JSON message");
    let msg = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(msg.chars().count() < 400, "{}", msg.chars().count());
    assert!(
        msg.contains("playbook handoff save --session sess1"),
        "{msg}"
    );
    assert!(msg.contains("/clear"), "{msg}");
    assert!(msg.contains("/playbook:handoff load"), "{msg}");
    assert!(msg.contains("needs the SessionStart hook"), "{msg}");
    assert!(!msg.contains("to resume from the handoff"), "{msg}");
    assert_eq!(second.trim(), "", "must not fire again at the same level");
}

fn assistant_line(tokens: u64) -> String {
    format!(
        r#"{{"type":"assistant","isSidechain":false,"message":{{"usage":{{"input_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":{tokens}}}}}}}"#
    )
}

#[test]
fn after_a_compaction_the_hook_is_silent_until_a_big_reply_follows() {
    let env = Env::new("hy-compact");
    let boundary = r#"{"type":"system","subtype":"compact_boundary"}"#.to_string();
    let user = r#"{"type":"user","message":{"content":"hi"}}"#.to_string();
    let mut lines = vec![assistant_line(BIG), boundary.clone(), user];

    // The big turn sits before the compaction, so it must not fire.
    let t = env.transcript_of("t.jsonl", &lines);
    let (code, out) = env.prompt("sess1", &t);
    assert_eq!((code, out.trim()), (0, ""));

    // A small reply after the compaction stays silent.
    lines.push(assistant_line(100_000));
    let t = env.transcript_of("t.jsonl", &lines);
    let (code, out) = env.prompt("sess1", &t);
    assert_eq!((code, out.trim()), (0, ""));

    // A big reply after the compaction fires.
    lines.push(assistant_line(BIG));
    let t = env.transcript_of("t.jsonl", &lines);
    let (_, out) = env.prompt("sess1", &t);
    assert!(out.contains("handoff save --session sess1"), "{out}");
}

#[test]
fn a_disabled_config_keeps_the_hook_silent() {
    let env = Env::new("hy-off");
    env.run(
        &["config", "set", "--global", "hygiene.enabled", "false"],
        "",
    );
    let t = env.transcript("t.jsonl", BIG);
    assert_eq!(env.prompt("sess1", &t).1.trim(), "");
}

#[test]
fn a_stored_threshold_is_ignored_and_the_built_in_one_applies() {
    // Arrange
    let env = Env::new("hy-thresh");
    let root = playbook::common::paths::playbook_root_from(&env.home);
    playbook::config::store::put(
        &root,
        playbook::config::write::Tier::Global,
        None,
        "hygiene.thresholdTokens",
        &serde_json::json!(400_000),
    )
    .unwrap();
    let t = env.transcript("t.jsonl", BIG);

    // Act
    let (_, out) = env.prompt("sess1", &t);

    // Assert
    assert!(out.contains("Context is large"), "{out}");
}

#[test]
fn a_bad_transcript_fails_safe() {
    let env = Env::new("hy-bad");
    let garbage = env.home.join("garbage.jsonl");
    fs::write(&garbage, "{{{ not json\n\u{0}\u{1}\n").unwrap();
    let empty = env.home.join("empty.jsonl");
    fs::write(&empty, "").unwrap();
    for path in [garbage, empty, env.home.join("missing.jsonl")] {
        let (code, out) = env.prompt("sess1", &path);
        assert_eq!((code, out.trim()), (0, ""), "{}", path.display());
    }
    let (code, out, _) = env.run(&["hook", "context-hygiene"], "{\"session_id\":\"s\"}");
    assert_eq!((code, out.trim()), (0, ""));
}

#[test]
fn after_clear_the_sessions_own_handoff_loads_once_with_a_pointer() {
    let env = Env::new("hy-loop");
    env.save_for("sessA", "# Topic\nRESUME-ALPHA\n");
    env.end_clear("sessA");

    let ctx = env.clear_start(&env.cwd, "clear");
    let again = env.clear_start(&env.cwd, "clear");

    assert!(ctx.contains("RESUME-ALPHA"), "{ctx}");
    assert!(ctx.contains("Full handoff: "), "{ctx}");
    assert!(
        ctx.contains("/gc/tombstones/"),
        "pointer names the archived file: {ctx}"
    );
    assert!(!again.contains("RESUME-ALPHA"), "consumed: {again}");
}

#[test]
fn after_clear_the_loaded_line_names_the_archived_file() {
    // Arrange
    let env = Env::new("hy-visible");
    env.save_for("sessV", "# Session Handoff - Resume the sync work\nBODY\n");
    env.end_clear("sessV");
    let payload = r#"{"hook_event_name":"SessionStart","source":"clear","session_id":"new-clear"}"#;

    // Act
    let (code, out, _) = env.run(&["hook", "session-init"], payload);

    // Assert
    assert_eq!(code, 0);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    let message = value["systemMessage"].as_str().unwrap_or_default();
    let line = message
        .lines()
        .find(|l| l.starts_with("Loaded handoff: Resume the sync work ("))
        .unwrap_or_else(|| panic!("no loaded line in: {message}"));
    assert!(line.contains("/gc/tombstones/"), "{line}");
}

#[test]
fn after_a_compact_the_sessions_own_handoff_loads_once_with_a_pointer() {
    let env = Env::new("hy-compact");
    env.save_for("sessA", "# Topic\nCOMPACT-ALPHA\n");

    let ctx = env.compact_start(&env.cwd, "sessA");
    let again = env.compact_start(&env.cwd, "sessA");

    assert!(ctx.contains("COMPACT-ALPHA"), "{ctx}");
    assert!(ctx.contains("was compacted"), "{ctx}");
    assert!(
        ctx.contains("/gc/tombstones/"),
        "pointer names the archived file: {ctx}"
    );
    assert!(!again.contains("COMPACT-ALPHA"), "consumed: {again}");
}

#[test]
fn a_compact_does_not_load_the_handoff_of_another_session() {
    let env = Env::new("hy-compact-other");
    env.save_for("sessB", "# Other\nNOT-MINE\n");

    let ctx = env.compact_start(&env.cwd, "sessA");

    assert!(!ctx.is_empty(), "the hook ran");
    assert!(!ctx.contains("was compacted"), "{ctx}");
    assert!(!ctx.contains("NOT-MINE"), "{ctx}");
    env.end_clear("sessB");
    let later = env.clear_start(&env.cwd, "clear");
    assert!(
        later.contains("NOT-MINE"),
        "still there for its own clear: {later}"
    );
}

#[test]
fn a_handoff_whose_session_never_cleared_is_not_injected() {
    let env = Env::new("hy-live");
    env.save_for("sessA", "# Topic\nSTILL-RUNNING\n");

    let ctx = env.clear_start(&env.cwd, "clear");
    let startup = env.clear_start(&env.cwd, "startup");

    assert!(!ctx.contains("STILL-RUNNING"), "{ctx}");
    assert!(!startup.contains("STILL-RUNNING"), "{startup}");
}

#[test]
fn only_the_cleared_session_loads_when_two_share_a_directory() {
    let env = Env::new("hy-parallel");
    env.save_for("sessA", "# A\nONLY-A\n");
    env.save_for("sessB", "# B\nONLY-B\n");
    env.end_clear("sessA");

    let ctx = env.clear_start(&env.cwd, "clear");

    assert!(ctx.contains("ONLY-A"), "{ctx}");
    assert!(!ctx.contains("ONLY-B"), "{ctx}");
    env.end_clear("sessB");
    let later = env.clear_start(&env.cwd, "clear");
    assert!(later.contains("ONLY-B"), "{later}");
}

#[test]
fn another_repos_handoff_is_not_injected() {
    let env = Env::new("hy-repo");
    let other = env.home.join("work").join("other");
    fs::create_dir_all(&other).unwrap();
    let (code, _, err) = env.run(
        &[
            "handoff",
            "save",
            "--session",
            "sessA",
            "--dir",
            other.to_str().unwrap(),
        ],
        "# O\nOTHER-REPO\n",
    );
    assert_eq!(code, 0, "{err}");
    env.end_clear("sessA");

    let ctx = env.clear_start(&env.cwd, "clear");

    assert!(!ctx.contains("OTHER-REPO"), "{ctx}");
}

#[test]
fn a_stale_cleared_handoff_is_not_injected() {
    let env = Env::new("hy-stale");
    env.save_for("sessA", "# Topic\nTOO-OLD\n");
    env.end_clear("sessA");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 86_400);
    for entry in fs::read_dir(env.handoff_dir()).unwrap().flatten() {
        if entry.path().extension().is_some_and(|e| e == "md") {
            fs::File::options()
                .write(true)
                .open(entry.path())
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
    }

    let ctx = env.clear_start(&env.cwd, "clear");

    assert!(!ctx.contains("TOO-OLD"), "{ctx}");
}

#[test]
fn a_huge_handoff_is_cut_to_stay_inside_the_hook_budget() {
    let env = Env::new("hy-cap");
    let huge = format!(
        "# Big\nHEAD-MARK\n{}",
        "filler line of the handoff\n".repeat(5000)
    );
    env.save_for("sessA", &huge);
    env.end_clear("sessA");

    let ctx = env.clear_start(&env.cwd, "clear");

    assert!(ctx.contains("HEAD-MARK"), "{}", &ctx[..200.min(ctx.len())]);
    assert!(ctx.chars().count() <= 9000, "{}", ctx.chars().count());
    assert!(ctx.contains("Full handoff: "));
}

#[test]
fn save_exits_one_on_unreadable_stdin_and_writes_nothing() {
    let env = Env::new("badutf8");
    let mut child = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["handoff", "save"])
        .current_dir(&env.cwd)
        .env("HOME", &env.home)
        .env("CLAUDE_PLUGIN_ROOT", env!("PLAYBOOK_REPO_ROOT"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&[0xff, 0xfe])
        .unwrap();

    let out = child.wait_with_output().unwrap();

    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot read stdin"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!env.handoff_dir().exists() || fs::read_dir(env.handoff_dir()).unwrap().count() == 0);
}

#[test]
fn template_check_and_facts_back_the_handoff_command() {
    let env = Env::new("compose");
    let (code, out, _) = env.run(&["handoff", "template"], "");
    assert_eq!(code, 0);
    assert!(out.contains("## Next steps") && out.contains("Clean stop. No pending work."));
    let good = "## Where we are now\n- a\n\n## Decisions made\n- b. Why: c.\n\n## Next steps\nClean stop. No pending work.\n";
    let (code, out, _) = env.run(&["handoff", "check"], good);
    assert_eq!(code, 0, "{out}");
    let (code, _, err) = env.run(&["handoff", "check"], "## Next steps\n1. x \u{2014} y\n");
    assert_eq!(code, 1);
    assert!(
        err.contains("dash") && err.contains("missing section"),
        "{err}"
    );
    let (code, out, _) = env.run(&["handoff", "facts"], "");
    assert_eq!(code, 0);
    assert!(out.starts_with("- "), "{out}");
}

/// A scratch repo with an `origin` remote, so handoffs resolve to the repo folder.
fn repo_env(tag: &str) -> Env {
    let env = Env::new(tag);
    for args in [
        vec!["init", "-q"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(&env.cwd)
            .args(&args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }
    env
}

impl Env {
    fn repo_handoff_dir(&self) -> PathBuf {
        self.home
            .join(".config/playbook/repos/acme/widgets/handoff")
    }
}

#[test]
fn a_repo_saves_its_handoff_under_the_repo_folder() {
    let env = repo_env("repo-folder");

    let (code, out, _) = env.run(&["handoff", "save"], "# Topic\nIN-THE-REPO-FOLDER\n");
    let (_, shown, _) = env.run(&["handoff", "load"], "");

    assert_eq!(code, 0);
    assert!(out.contains("repos/acme/widgets/handoff"), "{out}");
    assert!(shown.contains("IN-THE-REPO-FOLDER"), "{shown}");
    assert!(fs::read_dir(env.repo_handoff_dir()).unwrap().count() > 0);
    assert!(
        !env.handoff_dir().exists(),
        "nothing should go to the old folder"
    );
}

#[test]
fn an_old_global_handoff_is_copied_in_loaded_once_and_the_original_stays() {
    let env = repo_env("repo-migrate");
    let key: String = env
        .cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    fs::create_dir_all(env.handoff_dir()).unwrap();
    let old = env.handoff_dir().join(format!("{key}-1-1.md"));
    fs::write(&old, "# Old\nOLD-GLOBAL-HANDOFF\n").unwrap();

    let first = env.session_start(&env.cwd, "startup");
    let second = env.session_start(&env.cwd, "startup");

    assert!(first.contains("OLD-GLOBAL-HANDOFF"), "{first}");
    assert!(
        !second.contains("OLD-GLOBAL-HANDOFF"),
        "it came back: {second}"
    );
    assert!(old.is_file(), "the original must stay");
    assert!(env.handoff_dir().join("MOVED.txt").is_file());
}

#[test]
fn dir_resolves_the_folder_from_the_target_repo_not_the_current_one() {
    let env = repo_env("dir-other-repo");
    let other = env.cwd.parent().unwrap().join("other");
    fs::create_dir_all(&other).unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/acme/gadgets.git",
        ],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(&other)
            .args(&args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }

    let (code, out, _) = env.run(
        &["handoff", "save", "--dir", other.to_str().unwrap()],
        "# Topic\nFOR-GADGETS\n",
    );
    let (_, shown, _) = env.run(&["handoff", "load", "--dir", other.to_str().unwrap()], "");

    assert_eq!(code, 0);
    assert!(out.contains("repos/acme/gadgets/handoff"), "{out}");
    assert!(shown.contains("FOR-GADGETS"), "{shown}");
    assert!(!env.repo_handoff_dir().exists());
}
