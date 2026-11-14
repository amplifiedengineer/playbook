// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! First-use smoke suite: every `playbook` command runs through the real
//! binary on a fresh machine and must not error, panic, hang or leave stray
//! files.
//!
//! * The command list comes from the clap definition, so a new command fails
//!   `every_command_has_a_smoke_entry` until it gets a row in [`table`].
//! * Each invocation gets its own scratch root used as `HOME`, every `XDG_*`
//!   directory and `TMPDIR`, a scrubbed environment (`env_clear`), a PATH of
//!   only a scratch `bin` plus system dirs (a stub `claude`, the real `git`,
//!   no `gh`), and proxy variables that point at a closed port so any network
//!   use fails fast. Nothing outside the root is written.
//! * Every row runs twice, and the second run must behave the same.
//!
//! Not covered: the interactive `setup` wizard (it needs a pty and no pty crate
//! is a dependency; the non-interactive path is covered) and `usage collect`,
//! which runs a server until it is stopped.

#![cfg(unix)]

use clap::{Command, CommandFactory};
use playbook::common::par;
use playbook::Cli;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as Proc, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);
const PANIC_MARKERS: [&str; 3] = ["panicked at", "RUST_BACKTRACE", "thread 'main'"];
/// Commands that run until stopped, so a smoke run would hit the timeout.
const LONG_RUNNING: [(&str, &str); 1] = [("usage collect", "serves OTLP until stopped")];

/// The `playbook sync` words that run the `pbsync` helper. Whether one sits
/// beside the test binary varies, so `sync_cli.rs` covers them with a fake.
/// `sync config` hands its `--help` to the helper too.
fn helper_backed() -> BTreeSet<String> {
    playbook_common::sync_commands::FORWARDED
        .iter()
        .map(|name| format!("sync {name}"))
        .chain(std::iter::once(SYNC_CONFIG.to_string()))
        .collect()
}

const SYNC_CONFIG: &str = "sync config";

static COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------- environment

/// A scratch machine: one root holding home, XDG dirs, TMPDIR, a PATH dir and
/// two working directories (a git repo and a plain folder).
struct Env {
    root: PathBuf,
    /// Run children with `GIT_CEILING_DIRECTORIES` set to the scratch root.
    ceiling: bool,
}

impl Env {
    fn new(with_repo: bool) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().canonicalize().expect("temp dir");
        let root = base.join(format!("pb-smoke-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&base).expect("temp base");
        fs::create_dir(&root).expect("scratch root must be new");
        for d in ["home", "state", "data", "tmp", "bin", "plain"] {
            fs::create_dir_all(root.join(d)).expect("scratch dir");
        }
        fs::create_dir_all(root.join("home/.config")).expect("config dir");
        let stub = root.join("bin/claude");
        fs::write(&stub, "#!/bin/sh\nexit 0\n").expect("claude stub");
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("chmod stub");
        let git = which("git").expect("git must be on PATH");
        std::os::unix::fs::symlink(git, root.join("bin/git")).expect("git link");
        let env = Self {
            root,
            ceiling: false,
        };
        if with_repo {
            fs::create_dir_all(env.repo()).expect("repo dir");
            for args in [
                &["init", "-q", "-b", "main"][..],
                &["config", "user.name", "Smoke Test"],
                &["config", "user.email", "smoke@example.com"],
                &["config", "commit.gpgsign", "false"],
                &["config", "tag.gpgsign", "false"],
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/test-owner/test-repo.git",
                ],
            ] {
                let out = env.proc("git").args(args).output().expect("git");
                assert!(out.status.success(), "git {args:?}: {out:?}");
            }
        }
        env
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn cwd(&self) -> PathBuf {
        if self.repo().is_dir() {
            self.repo()
        } else {
            self.root.join("plain")
        }
    }

    /// A child process with the scrubbed environment, rooted at [`Self::cwd`].
    fn proc(&self, program: impl AsRef<std::ffi::OsStr>) -> Proc {
        let r = &self.root;
        let proxy = "http://127.0.0.1:9";
        let mut p = Proc::new(program);
        p.env_clear()
            .current_dir(self.cwd())
            .env("PATH", r.join("bin"))
            .env("HOME", r.join("home"))
            .env("XDG_CONFIG_HOME", r.join("home/.config"))
            .env("XDG_STATE_HOME", r.join("state"))
            .env("XDG_DATA_HOME", r.join("data"))
            .env("TMPDIR", r.join("tmp"))
            .env("TERM", "dumb")
            .env("LANG", "C.UTF-8")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("NO_PROXY", "")
            .env("no_proxy", "");
        for k in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            p.env(k, proxy);
        }
        if self.ceiling {
            p.env("GIT_CEILING_DIRECTORIES", r);
        }
        p
    }

    fn subst(&self, s: &str) -> String {
        s.replace("{ROOT}", &self.root.to_string_lossy())
            .replace("{CWD}", &self.cwd().to_string_lossy())
    }

    /// Run the playbook binary and enforce the global rules that hold for
    /// every invocation: no timeout, no panic text.
    fn run(&self, args: &[String], stdin: &str) -> Run {
        let mut child = self
            .proc(env!("CARGO_BIN_EXE_playbook"))
            .args(args.iter().map(|a| self.subst(a)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("spawn playbook");
        let mut sin = child.stdin.take().expect("stdin");
        let input = self.subst(stdin);
        let writer = std::thread::spawn(move || {
            let _ = sin.write_all(input.as_bytes());
        });
        let mut so = child.stdout.take().expect("stdout");
        let mut se = child.stderr.take().expect("stderr");
        let rout = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = so.read_to_end(&mut b);
            b
        });
        let rerr = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = se.read_to_end(&mut b);
            b
        });
        let start = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait().expect("wait") {
                break s;
            }
            if start.elapsed() >= TIMEOUT {
                // Kill the whole group so a grandchild cannot keep the pipes open.
                if let Some(pgid) = i32::try_from(child.id())
                    .ok()
                    .and_then(rustix::process::Pid::from_raw)
                {
                    let _ =
                        rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                break std::process::ExitStatus::default();
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let timed_out = start.elapsed() >= TIMEOUT;
        let _ = writer.join();
        let run = Run {
            code: if timed_out {
                -2
            } else {
                status.code().unwrap_or(-1)
            },
            stdout: String::from_utf8_lossy(&rout.join().expect("out")).into_owned(),
            stderr: String::from_utf8_lossy(&rerr.join().expect("err")).into_owned(),
        };
        for m in PANIC_MARKERS {
            assert!(
                !run.stdout.contains(m) && !run.stderr.contains(m),
                "playbook {args:?} panicked: {run:?}"
            );
        }
        run
    }

    /// Path to size to content hash of everything under the root except
    /// `.git`, so a read-only command can be shown to write nothing.
    fn snapshot(&self) -> BTreeMap<PathBuf, u64> {
        let mut map = BTreeMap::new();
        walk(&self.root, &mut map);
        map
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // A detached child (cc housekeep) may still be writing; retry briefly.
        for _ in 0..20 {
            if fs::remove_dir_all(&self.root).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn which(tool: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(tool))
        .find(|p| p.is_file())
}

fn walk(dir: &Path, map: &mut BTreeMap<PathBuf, u64>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_name() == ".git" {
            continue;
        }
        let kind = e.file_type().expect("file type");
        if kind.is_symlink() {
            map.insert(p, 1);
        } else if kind.is_dir() {
            map.insert(p.clone(), 0);
            walk(&p, map);
        } else if e.file_name() == "config.db" || e.file_name() == "sessions.db" {
            // The settings database stamps a time on every write, so a second
            // run that sets the same value changes its bytes. The session
            // database stamps the start time on every hook run. Presence is
            // what the check needs.
            map.insert(p, 2);
        } else {
            let bytes = fs::read(&p).unwrap_or_default();
            // FNV-1a: enough to notice a changed file, no dependency needed.
            let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
                (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
            });
            map.insert(p, hash ^ bytes.len() as u64);
        }
    }
}

// ---------------------------------------------------------------------- table

/// One safe first-use invocation of a command.
struct Case {
    /// Command path the row covers, such as `config get`.
    path: String,
    args: Vec<String>,
    stdin: String,
    code: i32,
    out: Vec<String>,
    err: Vec<String>,
    /// Commands run first, each of which must exit 0.
    pre: Vec<Vec<String>>,
    /// Files created under the scratch root before the run: (relative path, text).
    files: Vec<(String, String)>,
    /// The command may write under the scratch root.
    writes: bool,
    repo: bool,
    /// `git` commands run in the repo before the run, with `{ROOT}` filled in.
    git: Vec<Vec<String>>,
    /// A second run legitimately adds or rewrites files (timestamps, appends).
    grows: bool,
    /// What the second run must do when it differs from the first.
    second: Option<(i32, String)>,
    /// Body of a stub `gh` placed on PATH.
    gh: Option<String>,
    /// Set `GIT_CEILING_DIRECTORIES` to the parent of the repo.
    ceiling: bool,
}

fn case(path: &str, args: &[&str]) -> Case {
    Case {
        path: path.to_string(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
        stdin: String::new(),
        code: 0,
        out: Vec::new(),
        err: Vec::new(),
        pre: Vec::new(),
        files: Vec::new(),
        writes: false,
        repo: true,
        git: Vec::new(),
        grows: false,
        second: None,
        gh: None,
        ceiling: false,
    }
}

impl Case {
    fn ceiling(mut self) -> Self {
        self.ceiling = true;
        self
    }
    fn code(mut self, c: i32) -> Self {
        self.code = c;
        self
    }
    fn stdin(mut self, s: &str) -> Self {
        self.stdin = s.to_string();
        self
    }
    fn out(mut self, s: &str) -> Self {
        self.out.push(s.to_string());
        self
    }
    fn err(mut self, s: &str) -> Self {
        self.err.push(s.to_string());
        self
    }
    fn pre(mut self, args: &[&str]) -> Self {
        self.pre
            .push(args.iter().map(|a| (*a).to_string()).collect());
        self
    }
    fn file(mut self, rel: &str, text: &str) -> Self {
        self.files.push((rel.to_string(), text.to_string()));
        self
    }
    fn writes(mut self) -> Self {
        self.writes = true;
        self
    }
    fn plain(mut self) -> Self {
        self.repo = false;
        self
    }
    fn git(mut self, args: &[&str]) -> Self {
        self.git
            .push(args.iter().map(|a| (*a).to_string()).collect());
        self
    }
    /// One commit on `main`.
    fn commit(self) -> Self {
        self.git(&["commit", "-q", "--allow-empty", "-m", "chore: start"])
    }
    /// One commit on `main`, pushed to a bare repo on disk used as `origin`,
    /// so fetch and push work with no network.
    fn local_origin(self) -> Self {
        self.commit()
            .git(&["init", "-q", "--bare", "-b", "main", "{ROOT}/origin.git"])
            .git(&["remote", "set-url", "origin", "{ROOT}/origin.git"])
            .git(&["push", "-q", "origin", "main"])
            .git(&["fetch", "-q", "origin"])
    }
    /// The second run is expected to exit `code` and say `err`.
    fn then(mut self, code: i32, err: &str) -> Self {
        self.second = Some((code, err.to_string()));
        self
    }
    fn gh(mut self, script: &str) -> Self {
        self.gh = Some(script.to_string());
        self
    }
    fn grows(mut self) -> Self {
        self.grows = true;
        self.writes = true;
        self
    }
}

const BASH_PAYLOAD: &str = r#"{"session_id":"smoke","hook_event_name":"PreToolUse","cwd":"{CWD}","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
const EDIT_PAYLOAD: &str = r#"{"session_id":"smoke","hook_event_name":"PreToolUse","cwd":"{CWD}","tool_name":"Read","tool_input":{"file_path":"{ROOT}/missing.txt"}}"#;
const SESSION_PAYLOAD: &str = r#"{"session_id":"smoke","hook_event_name":"SessionStart","source":"startup","cwd":"{CWD}","transcript_path":"{ROOT}/none.jsonl"}"#;

/// Every hook name, with the JSON Claude Code would send it.
#[rustfmt::skip]
fn hook_cases() -> Vec<Case> {
    let hooks: [(&str, &str); 28] = [
        ("session-init", SESSION_PAYLOAD),
        ("preread-edit-check", EDIT_PAYLOAD),
        ("preread-size-check", EDIT_PAYLOAD),
        ("search-counter", BASH_PAYLOAD),
        ("memory-anchors", EDIT_PAYLOAD),
        ("post-edit-track", EDIT_PAYLOAD),
        ("rebuild-memory-graph", EDIT_PAYLOAD),
        ("auto-model-detect", SESSION_PAYLOAD),
        ("precompact-warn", SESSION_PAYLOAD),
        ("session-clean-exit", SESSION_PAYLOAD),
        ("memory-capture", SESSION_PAYLOAD),
        ("rm-workspace-guard", BASH_PAYLOAD),
        ("bg-await-guard", BASH_PAYLOAD),
        ("no-slop-guard", BASH_PAYLOAD),
        ("commit-message-sanitizer", BASH_PAYLOAD),
        ("policy-guard", BASH_PAYLOAD),
        ("precommit-check", BASH_PAYLOAD),
        ("auto-guard", BASH_PAYLOAD),
        ("auto-cost", SESSION_PAYLOAD),
        ("worktree-create", SESSION_PAYLOAD),
        ("worktree-remove", SESSION_PAYLOAD),
        ("context-hygiene", SESSION_PAYLOAD),
        ("doc-read-hint", EDIT_PAYLOAD),
        ("usage-warm", SESSION_PAYLOAD),
        ("handoff-on-clear", SESSION_PAYLOAD),
        ("session-activity", SESSION_PAYLOAD),
        // Retired, kept so an old plugin cache or settings entry stays quiet:
        // no output, no files.
        ("migration-check", SESSION_PAYLOAD),
        ("disk-guard", BASH_PAYLOAD),
    ];
    hooks.iter().map(|(name, payload)| {
            let c = case(&format!("hook {name}"), &["hook", name]).stdin(payload);
            match *name {
                // A worktree hook with no worktree name refuses and says so.
                "worktree-create" => c.code(1).err("unsafe worktree name"),
                "worktree-remove" => c.err("worktree_path is missing"),
                // Off by default and ignores a payload with no clear reason.
                "migration-check" | "disk-guard" | "no-slop-guard" | "handoff-on-clear" => c,
                // These keep a log or a counter, so a second run adds to it.
                "session-init" | "session-clean-exit" | "search-counter" | "precompact-warn"
                | "usage-warm" => c.grows(),
                _ => c.writes(),
            }
        }).collect()
}

#[allow(clippy::too_many_lines)]
#[rustfmt::skip]
fn table() -> Vec<Case> {
    let mut t = vec![
        // setup and lifecycle
        case("setup", &["setup", "--dry-run"]).out("nothing is changed"),
        case("setup", &["setup", "--yes"])
            .out("Applying")
            .err("plugin: installing playbook@amplifiedengineer")
            .writes(),
        case("setup", &["setup", "--explain"]).plain(),
        case("setup", &["setup", "--only", "telemetry"])
            .out("changes only the parts you named")
            .writes(),
        // the deprecated name runs the same code
        case("init", &["init", "--dry-run"]).out("nothing is changed"),
        case("init", &["init", "--yes"])
            .out("Applying")
            .err("plugin: installing playbook@amplifiedengineer")
            .writes(),
        case("hooks list", &["hooks", "list"]).out("Safety guards").plain(),
        case("skills sync", &["skills", "sync", "--dry-run"]).out("no other agents found"),
        case("uninstall", &["uninstall"]).code(1).err("nothing was changed"),
        case("uninstall", &["uninstall", "--dry-run"]).pre(&["setup", "--yes"]),
        case("uninstall", &["uninstall", "--yes"]).pre(&["setup", "--yes"]).writes(),
        case("plugin install", &["plugin", "install"]).out("plugin: installed playbook@amplifiedengineer"),
        case("plugin upgrade", &["plugin", "upgrade"]).out("plugin: not installed; run: claude plugin marketplace add"),
        case("upgrade", &["upgrade", "--check"]).code(1).err("could not read the release list"),
        case("effort", &["effort"]).writes(),
        case("effort", &["effort", "high"]).grows(),
        case("usage", &["usage", "--summary"]).writes(),
        case("version", &["version"]).out("playbook ").plain(),
        case("deps ensure", &["deps", "ensure", "{ROOT}/Brewfile"]).err("no Brewfile"),
        case("doc read", &["doc", "read", "{ROOT}/notes.xyz"]).file("notes.xyz", "x").code(1).err("unsupported file type"),
        // A configured path that does not exist keeps the row independent of any
        // `pbdoc` built next to the test binary. The default, nothing configured,
        // is covered in `doc_read.rs` on a copy of the binary with no sibling.
        case("doc status", &["doc", "status"])
            .pre(&["config", "set", "docs.anydocPath", "{ROOT}/missing-anydoc"])
            .code(1)
            .out("anydoc: not found. docs.anydocPath is set to"),
        // what takes space on disk
        case("gc", &["gc"]).plain(),
        // health facts
        case("doctor check", &["doctor", "check"]).out("FAIL").plain().writes(),
        case("doctor models", &["doctor", "models"]).out("haiku").plain(),
        case("doctor", &["doctor"]).out("FAIL").plain().writes(),
        case("doctor", &["doctor", "--fix"]).out("nothing to fix").plain().writes(),
        case("migrate", &["migrate"]).out("nothing to fix").plain().writes(),
        case("migrate", &["migrate", "--json"]).out("\"recorded\":true").plain().writes(),
        case("migrate", &["migrate", "--status"]).out("running: playbook").plain(),
        case("migrate", &["migrate", "--dry-run"]).out("dry run: nothing was written").plain(),
        case("migrate", &["migrate", "--status", "--dry-run"]).code(2).err("cannot be used with").plain(),
        case( "doctor migrations", &["doctor", "migrations"], ).writes(),
        // mode and settings
        case("mode status", &["mode", "status"]).out("mode: ask"),
        case("mode auto", &["mode", "auto"]).out("auto").grows(),
        case("mode ask", &["mode", "ask"]).out("ask").writes(),
        case("config get", &["config", "get", "pr.review.enabled"]).out("true"),
        case("config get", &["config", "get", "nope.key"]).code(1).err("unknown config key"),
        case( "config set", &["config", "set", "pr.review.enabled", "false"], ).out("updated").writes(),
        case("config set", &["config", "set", "nope.key", "1"]).code(1).err("unknown config key"),
        case("config list", &["config", "list"]).out("pr.review.enabled").plain(),
        case("config export", &["config", "export"]).out("\"version\": 1"),
        case("config import", &["config", "import", "{ROOT}/cfg.json"]).file( "cfg.json", r#"{"version":1,"global":{},"orgs":{},"repos":{}}"#, ).writes(),
        case("sync config refresh", &["sync", "config", "refresh"]).out("refreshed").writes(),
        case("sync config apply", &["sync", "config", "apply"]).out("No sync proposal to apply."),
        case("state list", &["state", "list"]).writes(),
        case( "effort resolve", &["effort", "resolve", "agent", "reviewer"], ).out("no such component"),
        case("effort suggest", &["effort", "suggest"]).code(1).err("no plugin root"),
        case("effort apply", &["effort", "apply"]).code(1).err("no plugin root"),
        case("effort choose", &["effort", "choose", "reviewer"]).out("playbook:reviewer"),
        case("effort log", &["effort", "log"]).out("No automatic effort choices"),
        case("effort reset", &["effort", "reset"]).out("nothing to reset"),
        case("effort list", &["effort", "list"]).out("no plugin root"),
        case("route", &["route", "review"]).out("review:"),
        // launchers (a stub claude stands in for the real one)
        case("cc", &["cc"]).writes(),
        case("cc clean", &["cc", "clean"]),
        case("cc fresh", &["cc", "fresh"]).writes(),
        case("cc raw", &["cc", "raw"]),
        case("cc list", &["cc", "list"]).out("no sessions"),
        case("cc prune", &["cc", "prune"]),
        case("cc cache clear", &["cc", "cache", "clear"]),
        case("cc launch", &["cc", "launch"]).writes(),
        case("cc worktree", &["cc", "worktree", "smoke-branch"]).local_origin().grows().err("Ready:"),
        case( "cc housekeep", &[ "cc", "housekeep", "--repo-root", "{CWD}", "--worktree", "{ROOT}/none", "--branch", "smoke", "--no-push", ], ).grows(),
        case("ccd", &["ccd"]).writes(),
        // usage and sessions
        case("usage status", &["usage", "status"]).out("telemetry: off").writes(),
        case("usage warm", &["usage", "warm"]).writes(),
        case("usage prune", &["usage", "prune"]).out("removed 0").writes(),
        case("usage doctor", &["usage", "doctor"]).out("Where pbtop can read limits"),
        case("usage machine", &["usage", "machine"]).out("id: ").writes(),
        case("usage stop", &["usage", "stop"]).out("not running"),
        case("usage account fingerprint", &["usage", "account", "fingerprint"]).code(1).err("CLAUDE_CODE_OAUTH_TOKEN is not set").writes(),
        case("usage account assign", &["usage", "account", "assign", "--from", "2026-01-01", "--to", "2026-01-02", "--as", "x"]).out("dry run: 0 events").writes(),
        case("usage account reconcile", &["usage", "account", "reconcile", "--from", "2026-01-01", "--to", "2026-01-02", "--dashboard", "2026-01-01=1"]).out("No sessions start").writes(),
        case("usage account sessions",&["usage", "account", "sessions"]).out("No sessions start").writes(),
        case("usage account undo",&["usage", "account", "undo"]).out("no applied move").writes(),
        case("usage account list",&["usage", "account", "list"]).out("No usage recorded yet").writes(),
        case("usage account name", &["usage", "account", "name", "k", "Home"]).out("k is shown as Home").writes(),
        case("sessions", &["sessions"]).writes().out("No recent sessions"),
        case("sessions list", &["sessions", "list"]).out("No recent sessions").writes(),
        // watch mode: nothing is enabled and no token is set on a fresh home
        case("watch", &["watch"]).out("watch is off"),
        case("watch once", &["watch", "once"]).out("watch is off"),
        case("watch approve", &["watch", "approve", "1"]).code(1).err("GITHUB_TOKEN"),
        case("watch reject", &["watch", "reject", "1"]).code(1).err("not waiting").grows(),
        case("watch status", &["watch", "status"]).out("enabled: false").grows(),
        case("watch pause", &["watch", "pause"]).out("watch paused").grows(),
        case("watch resume", &["watch", "resume"]).out("watch resumed").grows(),
        case("watch retry", &["watch", "retry", "1"]).code(1).err("not on the dead list").grows(),
        case("watch service install", &["watch", "service", "install"]).code(1).err("not changed"),
        case("watch service uninstall", &["watch", "service", "uninstall"]).code(1).err("not changed"),
        case("watch log", &["watch", "log"]).grows(),
        case("sessions sweep", &["sessions", "sweep", "--dry-run"])
            .out("No finished or stale sessions")
            .gh("exit 1")
            .writes(),
        // network or repo-bound tools, failing fast with a message
        case("eval triage", &["eval", "triage"]).code(1).err("gh is required"),
        case("eval bench", &["eval", "bench"]).code(1).err("No such file"),
        case("pr prepare", &["pr", "prepare"]).plain().code(1).err("not a git repository"),
        case( "pr create", &["pr", "create", "--title", "t", "--body-file", "{ROOT}/b.md"], ).file("b.md", "body").code(3).err("feature branch"),
        case("pr status", &["pr", "status", "1", "--json"]).out("\"ok\":false"),
        case("pr review triage", &["pr", "review", "triage"]).out("review="),
        case("pr rules", &["pr", "rules"]).writes().grows().out("extracted and verified").err("embedded"),
        case("pr comments", &["pr", "comments"]).code(1).err("no PR"),
        case( "pr reply", &[ "pr", "reply", "--pr", "1", "--body-file", "{ROOT}/b.md", "--thread", "1", ], ).file("b.md", "body").code(1).err("gh repo view"),
        case("pr stack", &["pr", "stack"]).code(1).err("gh repo view"),
        case( "pr ci-wait", &["pr", "ci-wait", "1", "--timeout", "1", "--interval", "1"], ).out("CI_VERDICT=NONE"),
        case( "pr land-wait", &["pr", "land-wait", "1", "--timeout", "1", "--interval", "1"], ).out("LAND_VERDICT=TIMEOUT"),
        case("pr checks", &["pr", "checks", "1"]).code(5).out("CHECKS=UNREADABLE"),
        case("pr merge", &["pr", "merge", "1", "--no-wait"]).out("merge_rc=-1"),
        case("pr land", &["pr", "land", "1", "--settle", "0"])
            .code(6)
            .out("LAND=FAIL"),
        // worktrees and memory
        case("worktree sweep", &["worktree", "sweep"]).gh("exit 1").writes(),
        case("worktree remove", &["worktree", "remove", "{ROOT}/none"]).code(1).err("not a registered git worktree"),
        case( "worktree review setup", &["worktree", "review", "setup", "1", "abc123"], ).code(1).err("failed to resolve repo URL").writes(),
        case( "worktree review teardown", &["worktree", "review", "teardown", "{ROOT}/none"], ),
        case("memory rebuild", &["memory", "rebuild"]).out("rebuilt").grows(),
        case("memory stats", &["memory", "stats"]).out("database:"),
        case("memory export", &["memory", "export"]).out("\"nodes\"").writes(),
        case("memory claude import", &["memory", "claude", "import"]).out("copied 0").writes(),
        case("memory context", &["memory", "context"]).out("Memory context").writes(),
        // handoff and statusline
        case("handoff save", &["handoff", "save"]).stdin("# Handoff\n").out("handoff saved").grows(),
        case("handoff show", &["handoff", "show"]).out("No handoff saved"),
        case("handoff status", &["handoff", "status"]).out("No SessionStart"),
        case("handoff template", &["handoff", "template"]).plain(),
        case("handoff facts", &["handoff", "facts"]).out("Branch: main"),
        case("handoff check", &["handoff", "check"]).stdin("nothing\n").code(1).err("missing section"),
        case("statusline", &["statusline"]).stdin("{}").plain().writes(),
        // repo checks
        case( "settings gen", &["settings", "gen", "{ROOT}/src.json", "{ROOT}/seed.json"], ).code(2).err("not readable"),
        case( "settings check", &[ "settings", "check", "{ROOT}/t.json", "{CWD}", ], ).code(1).err("not readable"),
        case("manifest check", &["manifest", "check"]).out("check-manifest: OK"),
        case("agents check", &["agents", "check"]).code(1).err("agents directory not found"),
        case( "gate record", &["gate", "record", "smoke", "plan", "design", "{ROOT}/in.txt"], ).file("in.txt", "x").code(1).err("no VERDICT"),
        case("gate snapshot", &["gate", "snapshot", "smoke", "design"]).code(1).err("does not exist"),
        case("gate check", &["gate", "check", "smoke", "plan"]).code(1).err("no phases specified"),
        case("code index", &["code", "index"]).out("indexed 0 files").grows(),
        case("code index", &["code", "index"]).ceiling().out("indexed 0 files").grows(),
        case("code status", &["code", "status"]).out("missing"),
        case("code search", &["code", "search", "foo"]).code(1).err("no index yet"),
        case("code symbol", &["code", "symbol", "foo"]).code(1).err("no index yet"),
        case("code outline", &["code", "outline", "src/lib.rs"]).code(1).err("no such file"),
        case("code read", &["code", "read", "src/lib.rs", "1:5"]).code(1).err("no such file"),
        case("code refs", &["code", "refs", "foo"]).code(1).err("no index yet"),
        case("code context", &["code", "context", "foo"]).code(1).err("no index yet"),
        case( "sanitize commit", &["sanitize", "commit", "{ROOT}/m.txt"], ).file("m.txt", "feat: add a thing\n"),
        case("sanitize pr", &["sanitize", "pr", "{ROOT}/m.txt"]).file("m.txt", "A plain description.\n"),
        case("ci", &["ci"]).out("ci: 0 passed"),
        // plans, glossary, skills, adr
        case("plan checkpoint", &["plan", "checkpoint", "smoke"]).stdin("notes\n").writes(),
        case("plan cleanup", &["plan", "cleanup", "smoke"]),
        case( "plan brief", &[ "plan", "brief", "--out", "{ROOT}/brief.md", "{ROOT}/plan.md", "WU1", ], ).file("plan.md", "# Plan\n").code(1).err("no section for WU1"),
        case("glossary add", &["glossary", "add", "term: a definition"]).grows(),
        case("adr next", &["adr", "next"]).out("Next number: 0001").writes(),
        case("prose check", &["prose", "check", "{ROOT}/a.md"]).file("a.md", "Let that sink in.\n").out("1 hard"),
        case("tools map", &["tools", "map"]).out("playbook tools:"),
        case("commands", &["commands"]).out("playbook commands:").out("setup: "),
        case("commands", &["commands", "worktree", "review"]).out("worktree review setup <"),
        case("commands", &["commands", "--json", "pr"]).out("\"command\": \"pr reply\""),
        case("completions bash", &["completions", "bash"]).out("playbook"),
        case("completions zsh", &["completions", "zsh"]).out("playbook"),
        case("completions fish", &["completions", "fish"]).out("playbook"),
        case("commands", &["commands", "nope"]).code(2).err("no command `nope`"),
        case("atlassian check", &["atlassian", "check"]).code(1).out("acli: missing"),
        case("learn preflight", &["learn", "preflight"]).plain().code(1).err("not in a git repo"),
        case("learn collect git", &["learn", "collect", "git"]).out("{"),
        case( "learn collect structure", &["learn", "collect", "structure"], ).out("{"),
        // commit, deliver, review, segment
        case("commit all", &["commit", "all"]).out("NO_STAGED_CHANGES"),
        case("commit prepare", &["commit", "prepare"]).out("NO_STAGED_CHANGES"),
        case("commit run", &["commit", "run"]).code(1).err("nothing to commit"),
        case("pr create", &["pr", "create"]).code(3).err("feature branch"),
        case("pr create", &["pr", "create"]).local_origin().git(&["checkout", "-q", "-b", "feat/smoke"]).gh("echo '[]'").code(9).err("nothing to deliver"),
        case("review prepare quick", &["review", "prepare", "quick"]).code(1).err("no PR found"),
        case("review prepare deep", &["review", "prepare", "deep"]).code(1).err("no PR found"),
        case("review checks", &["review", "checks", "{ROOT}/none"]).out("checks skipped"),
        case("review size", &["review", "size", "--base", "main"]).commit(),
        case("review suggest", &["review", "suggest"]).commit().code(1).err("no changed files"),
        case( "review triage merge", &["review", "triage", "merge", "--lenses", "logic"], ).out("Triage:"),
        case( "segment branch", &[ "segment", "branch", "--type", "feat", "--plan-slug", "smoke", "--n", "1", "--title", "add a thing", "--topology", "independent", ], ).commit().writes().then(1, "already exists"),
        case("segment size", &["segment", "size", "--base", "main"]).commit(),
        case("plans", &["plans"]).out("NO_PLANS"),
        case("trust", &["trust", "{ROOT}/repo"]).writes(),
        case("trust", &["trust"]).code(2).err("required arguments").err("Usage: playbook trust"),
        case("route", &["route"]).code(2).err("required arguments").err("Usage: playbook route"),
    ];
    for kind in ["plans", "implement", "worktrees", "handoff", "data"] {
        t.push(case(&format!("path {kind}"), &["path", kind]).out("/.config/playbook/"));
    }
    t.push(case("path plugin-root", &["path", "plugin-root"]).code(1).err("playbook plugin not found"));
    t.extend(hook_cases());
    t
}

// ---------------------------------------------------------------------- tests

/// Every command in the clap tree, hidden ones included, as a space-joined
/// path. A command whose first positional lists possible values (`path`,
/// `hook`) is expanded to one path per value.
fn leaves() -> Vec<String> {
    fn go(cmd: &Command, path: &str, out: &mut Vec<String>) {
        let subs: Vec<&Command> = cmd
            .get_subcommands()
            .filter(|s| s.get_name() != "help")
            .collect();
        // A group whose subcommand is optional also runs on its own.
        if !subs.is_empty() && !cmd.is_subcommand_required_set() && !path.is_empty() {
            out.push(path.to_string());
        }
        if subs.is_empty() {
            let values = cmd
                .get_positionals()
                .next()
                .map(|a| a.get_possible_values())
                .unwrap_or_default();
            if values.is_empty() {
                out.push(path.to_string());
            }
            for v in values {
                out.push(format!("{path} {}", v.get_name()));
            }
            return;
        }
        for s in subs {
            let child = if path.is_empty() {
                s.get_name().to_string()
            } else {
                format!("{path} {}", s.get_name())
            };
            go(s, &child, out);
        }
    }
    let mut out = Vec::new();
    go(&Cli::command(), "", &mut out);
    out
}

/// Every command path, leaves and the groups above them, for the help check.
fn all_paths() -> Vec<Vec<String>> {
    fn go(cmd: &Command, path: &[String], out: &mut Vec<Vec<String>>) {
        for s in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
            let mut p = path.to_vec();
            p.push(s.get_name().to_string());
            out.push(p.clone());
            go(s, &p, out);
        }
    }
    let mut out = Vec::new();
    go(&Cli::command(), &[], &mut out);
    out
}

#[test]
fn every_command_has_a_smoke_entry() {
    let table = table();
    let covered: BTreeSet<&str> = table.iter().map(|c| c.path.as_str()).collect();
    let backed = helper_backed();
    let skipped: BTreeSet<&str> = LONG_RUNNING
        .iter()
        .map(|(p, _)| *p)
        .chain(backed.iter().map(String::as_str))
        .collect();
    let all = leaves();
    let missing: Vec<&String> = all
        .iter()
        .filter(|l| !covered.contains(l.as_str()) && !skipped.contains(l.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "commands with no smoke row: {missing:?}"
    );
    let known: BTreeSet<&str> = all.iter().map(String::as_str).collect();
    let stale: Vec<&&str> = covered
        .iter()
        .chain(skipped.iter())
        .filter(|p| !known.contains(**p))
        .collect();
    assert!(
        stale.is_empty(),
        "smoke rows for commands that are gone: {stale:?}"
    );
}

#[test]
fn every_command_prints_help_and_exits_zero_with_no_stderr() {
    let paths = all_paths();
    let top = vec!["--help".to_string()];
    let mut runs = vec![top];
    let backed = helper_backed();
    for p in paths.iter().filter(|p| !backed.contains(&p.join(" "))) {
        let mut args = p.clone();
        args.push("--help".to_string());
        runs.push(args);
    }
    // Eight chunks at a time, each with its own home: one after another the
    // runs took 11 s on a macOS runner.
    let chunks: Vec<&[Vec<String>]> = runs.chunks(runs.len().div_ceil(8)).collect();
    let failures: Vec<String> = par::map(&chunks, 8, |chunk| {
        let env = Env::new(true);
        let mut bad = Vec::new();
        for args in *chunk {
            let r = env.run(args, "");
            if r.code != 0 || r.stdout.trim().is_empty() || !r.stderr.is_empty() {
                bad.push(format!("{args:?}: {r:?}"));
            }
        }
        bad
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(
        failures.is_empty(),
        "help failures:\n{}",
        failures.join("\n")
    );
}

/// Run one row in its own fresh environment, twice, and return what is wrong.
fn check(c: &Case) -> Vec<String> {
    let mut env = Env::new(c.repo);
    env.ceiling = c.ceiling;
    let mut bad = Vec::new();
    if let Some(script) = &c.gh {
        let gh = env.root.join("bin/gh");
        fs::write(&gh, format!("#!/bin/sh\n{script}\n")).expect("gh stub");
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).expect("chmod gh");
    }
    for step in &c.git {
        let args: Vec<String> = step.iter().map(|a| env.subst(a)).collect();
        let out = env.proc("git").args(&args).output().expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    for (rel, text) in &c.files {
        fs::write(env.root.join(rel), text).expect("fixture file");
    }
    // A real home has the upgrade record from install, so the first-run step
    // (ADR 0034 decision 13) has nothing to do during the case. The rows for
    // `migrate` and `doctor` keep a fresh home, since they test the heal.
    if !(c.path.starts_with("migrate") || c.path.starts_with("doctor")) {
        let seed = env.run(&["migrate".to_string()], "");
        if seed.code != 0 {
            bad.push(format!("setup migrate failed: {seed:?}"));
        }
    }
    for pre in &c.pre {
        let r = env.run(pre, "");
        if r.code != 0 {
            bad.push(format!("setup {pre:?} failed: {r:?}"));
        }
    }
    let before = env.snapshot();
    let first = env.run(&c.args, &c.stdin);
    let after_first = env.snapshot();
    let second = env.run(&c.args, &c.stdin);
    let after_second = env.snapshot();
    for (n, r) in [("first", &first), ("second", &second)] {
        let (code, second_err) = match (&c.second, n) {
            (Some((code, err)), "second") => (*code, Some(err)),
            _ => (c.code, None),
        };
        if r.code != code {
            bad.push(format!("{n} run exit {} want {code}: {r:?}", r.code));
        }
        if let Some(err) = second_err {
            if !r.stderr.contains(err.as_str()) {
                bad.push(format!("{n} run stderr lacks {err:?}: {r:?}"));
            }
            continue;
        }
        for want in &c.out {
            if !r.stdout.contains(want.as_str()) {
                bad.push(format!("{n} run stdout lacks {want:?}: {r:?}"));
            }
        }
        if c.err.is_empty() {
            if !r.stderr.is_empty() {
                bad.push(format!("{n} run wrote stderr: {r:?}"));
            }
        } else {
            for want in &c.err {
                if !r.stderr.contains(want.as_str()) {
                    bad.push(format!("{n} run stderr lacks {want:?}: {r:?}"));
                }
            }
        }
    }
    if c.second.is_none() && first.code != second.code {
        bad.push(format!("second run differs: {first:?} vs {second:?}"));
    }
    if !c.writes && before != after_first {
        bad.push(format!("wrote files: {:?}", diff(&before, &after_first)));
    }
    let (after_first, after_second) = (
        without_sidecars(after_first),
        without_sidecars(after_second),
    );
    if !c.grows && after_first != after_second {
        bad.push(format!(
            "not idempotent: {:?}",
            diff(&after_first, &after_second)
        ));
    }
    bad
}

/// The snapshot without the `-wal` and `-shm` files of `config.db` and
/// `sessions.db`. Their contents are compared by presence only (see `walk`),
/// and these files stay or go depending on which connection closes last (for
/// `config.db`, `doctor check` opens it from parallel probes, read-write and
/// read-only). The other
/// databases keep their sidecars in the comparison, and the "wrote files"
/// check still sees all of them, so a read-only command that leaves them
/// fails.
fn without_sidecars(mut map: BTreeMap<PathBuf, u64>) -> BTreeMap<PathBuf, u64> {
    map.retain(|p, _| {
        let name = p.file_name().unwrap_or_default();
        ![
            "config.db-wal",
            "config.db-shm",
            "sessions.db-wal",
            "sessions.db-shm",
        ]
        .iter()
        .any(|ignored| name == *ignored)
    });
    map
}

fn diff(a: &BTreeMap<PathBuf, u64>, b: &BTreeMap<PathBuf, u64>) -> Vec<String> {
    let keys: BTreeSet<&PathBuf> = a.keys().chain(b.keys()).collect();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .map(|k| k.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn every_table_row_behaves_on_a_fresh_home_and_again_on_the_second_run() {
    let table = table();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failures = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(c) = table.get(i) else { break };
                for b in check(c) {
                    failures
                        .lock()
                        .expect("lock")
                        .push(format!("[{}] {b}", c.args.join(" ")));
                }
            });
        }
    });
    let failures = failures.into_inner().expect("lock");
    assert!(
        failures.is_empty(),
        "{} problems:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn a_fresh_home_creates_the_data_folder_private_and_prints_no_migration_notice() {
    let env = Env::new(true);
    let mut seen = String::new();
    for args in [
        vec!["usage", "status"],
        vec!["usage", "prune"],
        vec!["memory", "rebuild"],
        vec!["config", "list"],
        vec!["code", "index"],
        vec!["doctor", "check"],
    ] {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        let r = env.run(&args, "");
        assert_eq!(r.code, 0, "{args:?}: {r:?}");
        assert!(r.stderr.is_empty(), "{args:?}: {r:?}");
        seen.push_str(&r.stdout);
    }
    assert!(
        !seen.to_lowercase().contains("migrat"),
        "migration notice on a fresh home: {seen}"
    );
    let data = env.root.join("home/.config/playbook/data");
    let mode = |p: &Path| fs::metadata(p).expect("metadata").permissions().mode() & 0o777;
    assert!(data.is_dir(), "no data folder was created");
    assert_eq!(mode(&data), 0o700, "data folder mode");
    let mut files = 0;
    for e in fs::read_dir(&data).expect("data dir").flatten() {
        if e.path().is_file() {
            files += 1;
            assert_eq!(mode(&e.path()), 0o600, "{:?}", e.path());
        }
    }
    assert!(files > 0, "the data folder holds no files");
}

#[test]
fn doctor_on_a_fresh_home_reports_only_known_not_installed_failures() {
    let env = Env::new(true);
    let r = env.run(&["doctor".into(), "check".into()], "");
    assert_eq!(r.code, 0, "{r:?}");
    assert!(r.stderr.is_empty(), "{r:?}");
    for line in r.stdout.lines().filter(|l| !l.trim().is_empty()) {
        let known = [
            "PASS",
            "INFO",
            "WARN",
            "FAIL",
            "OK",
            "Run /playbook:setup",
            "/playbook:setup",
        ];
        assert!(
            known.iter().any(|k| line.starts_with(k)),
            "unexpected doctor line: {line}"
        );
        if line.starts_with("FAIL") {
            let documented = [
                "safety guards wired 0/",
                "playbook binary not on PATH",
                "plugin not installed",
            ];
            assert!(
                documented.iter().any(|d| line.contains(d)),
                "undocumented failure: {line}"
            );
        }
    }
}

#[test]
fn commands_also_run_outside_any_git_repository() {
    let env = Env::new(false);
    for args in [
        vec!["version"],
        vec!["config", "list"],
        vec!["mode", "status"],
        vec!["doctor", "models"],
        vec!["hooks", "list"],
        vec!["handoff", "template"],
        vec!["sessions", "list"],
        vec!["usage", "status"],
        vec!["statusline"],
    ] {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        let r = env.run(&args, "{}");
        assert_eq!(r.code, 0, "{args:?}: {r:?}");
        assert!(r.stderr.is_empty(), "{args:?}: {r:?}");
    }
}

#[test]
fn the_json_group_is_gone() {
    // Arrange
    let env = Env::new(false);
    let args = ["json".to_string(), "field".to_string()];

    // Act
    let r = env.run(&args, "");

    // Assert
    assert_eq!(r.code, 2, "{r:?}");
    assert!(r.stderr.contains("unrecognized subcommand 'json'"), "{r:?}");
}
