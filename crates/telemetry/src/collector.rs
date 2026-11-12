// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook usage collect`: the lifecycle around the local receiver.
//!
//! A state file `collector.json` next to the usage database (owner-only)
//! holds the pid, the port and the bearer token. The token and the port are
//! kept across restarts, so sessions started before a restart keep working
//! and send to the same place. A start finds a live collector by pid and a
//! connect to its port, and then does not start a second one. A stale file
//! (dead pid) is replaced. The port is chosen once: the default 47318 (the
//! standard OTLP port 4318 is left alone because other collectors use it),
//! or a free port when that one is taken on the first run. It is then
//! recorded and never changed on its own, because the OTLP endpoint in
//! Claude Code's settings names it. A restart binds the same port again,
//! retrying for up to a second while a killed predecessor lets go of it; when
//! another program owns it the start fails with a message. It stops by
//! itself after the idle time (default 10 minutes,
//! 0 means never) and clears the pid on a clean exit.
//!
//! Claude Code may drop data that is sent while no receiver listens. That is
//! by design: such sessions are read from transcripts.

use super::daemon::{self, Lock};
use super::receiver::{bind, serve, Config};
use super::run::Paths;
use super::telemetry_db::{migrate_up, prune, Pruned, Retention};
use crate::util::atomic::write_atomic_private;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 47318;

/// How long a start waits for a recorded port that a killed predecessor is
/// still releasing. It stays under the 1.5 second wait `cc` gives a new
/// collector to come up.
const BIND_RETRY: Duration = Duration::from_secs(1);
pub const DEFAULT_IDLE_SECONDS: u64 = 600;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct State {
    /// 0 when no collector is running.
    pub pid: u32,
    pub port: u16,
    pub token: String,
    /// When the collector started, in unix seconds. 0 in a file written by an
    /// older version.
    #[serde(default)]
    pub started_at: i64,
    /// The idle limit it runs with, in seconds; 0 means never.
    #[serde(default)]
    pub idle_seconds: u64,
    /// The version of the program that started it. Empty in a file written
    /// by an older version.
    #[serde(default)]
    pub version: String,
}

pub fn state_path(paths: &Paths) -> PathBuf {
    paths
        .db
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("collector.json")
}

pub fn read_state(path: &Path) -> Option<State> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_state(path: &Path, state: &State) -> Result<(), String> {
    let json = serde_json::to_vec(state).map_err(|e| e.to_string())?;
    write_atomic_private(path, json).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

#[cfg(unix)]
pub(super) fn pid_alive(pid: u32) -> bool {
    // `Pid` rejects 0 and negative ids, which `kill` would take as groups.
    let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    // Signal 0 only checks that the process exists.
    match rustix::process::test_kill_process(pid) {
        Ok(()) => true,
        Err(err) => err == rustix::io::Errno::PERM,
    }
}

#[cfg(not(unix))]
pub(super) fn pid_alive(_pid: u32) -> bool {
    false
}

pub fn port_answers(port: u16) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        Duration::from_millis(300),
    )
    .is_ok()
}

/// The running collector recorded in `state`, if it is really alive.
pub fn running(state: &State) -> bool {
    state.pid != 0 && pid_alive(state.pid) && port_answers(state.port)
}

/// What answers on a port that `running` found open.
#[derive(Debug, PartialEq, Eq)]
pub enum Probe {
    /// A playbook collector replied.
    Collector,
    /// Something replied that is not a collector: the pid was reused.
    Foreign,
    /// Nothing replied in time: a collector stuck in a slow write, or a
    /// program that accepts and never answers.
    Silent,
}

pub(crate) const PROBE_DEADLINE: Duration = Duration::from_millis(1000);

/// Sends a harmless request to `port` and classifies the reply. A collector
/// answers every request, including this unauthenticated `GET`, with the
/// `X-Playbook-Collector` header.
pub fn probe(port: u16) -> Probe {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    match TcpStream::connect_timeout(&addr, PROBE_DEADLINE) {
        Ok(stream) => probe_stream(stream),
        Err(_) => Probe::Silent,
    }
}

/// `probe` on a stream the caller connected, so the daemon's starters can
/// count their connects.
pub(crate) fn probe_stream(mut stream: TcpStream) -> Probe {
    use std::io::{Read, Write};
    let _ = stream.set_read_timeout(Some(PROBE_DEADLINE));
    let _ = stream.set_write_timeout(Some(PROBE_DEADLINE));
    let request = "GET /v1/logs HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    if stream.write_all(request.as_bytes()).is_err() {
        return Probe::Silent;
    }
    let mut buf = [0u8; 1024];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return Probe::Silent,
    };
    classify_reply(&buf[..n])
}

fn classify_reply(reply: &[u8]) -> Probe {
    let text = String::from_utf8_lossy(reply).to_ascii_lowercase();
    if text.contains("x-playbook-collector:") {
        Probe::Collector
    } else {
        Probe::Foreign
    }
}

/// 32 hex characters from the OS random source.
fn new_token() -> Result<String, String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("failed to read random bytes: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The listener for a first run: the preferred port when it is free, else a
/// free one. One bind, so nothing can take the port between a check and the use.
fn first_listener_from(preferred: u16) -> Result<std::net::TcpListener, String> {
    bind(preferred)
        .or_else(|_| bind(0))
        .map_err(|e| format!("cannot listen on 127.0.0.1: {e}"))
}

fn first_port_from(preferred: u16) -> Result<u16, String> {
    first_listener_from(preferred)?
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| e.to_string())
}

/// Binds the recorded port. A port a killed process is still releasing is
/// retried for up to `wait`; a port another program owns is an error, never
/// a different port. Any other bind error is returned at once.
fn bind_recorded(
    port: u16,
    wait: Duration,
    on_foreign: impl FnOnce(),
) -> Result<std::net::TcpListener, String> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        match bind(port) {
            Ok(l) => return Ok(l),
            Err(e) if e.kind() != std::io::ErrorKind::AddrInUse => {
                return Err(format!("cannot listen on 127.0.0.1:{port}: {e}"));
            }
            Err(e) if std::time::Instant::now() >= deadline => {
                on_foreign();
                return Err(format!(
                    "port {port} is in use by another program ({e}). The collector keeps its recorded port so the Claude Code settings stay valid. Free the port, or move to a new one with `{} --port <N>` and then run `playbook setup --only telemetry` (older versions: `playbook init --only telemetry`) to rewrite the endpoint in your settings",
                    super::host::command("usage collect")
                ));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// The collector state with a token and port recorded, creating the file when
/// there is none (no process, `pid` 0). `setup --telemetry` uses this so the
/// settings it writes name the same port and token the collector reuses.
pub fn ensure_state(paths: &Paths) -> Result<State, String> {
    let path = state_path(paths);
    if let Some(state) = read_state(&path).filter(|s| !s.token.is_empty() && s.port != 0) {
        return Ok(state);
    }
    let previous = read_state(&path).unwrap_or_default();
    let state = State {
        pid: if running(&previous) { previous.pid } else { 0 },
        port: if previous.port == 0 {
            first_port_from(DEFAULT_PORT)?
        } else {
            previous.port
        },
        token: if previous.token.is_empty() {
            new_token()?
        } else {
            previous.token
        },
        started_at: previous.started_at,
        idle_seconds: previous.idle_seconds,
        version: previous.version,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    }
    write_state(&path, &state)?;
    Ok(state)
}

/// What `playbook usage status` reports.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub running: bool,
    pub pid: Option<u32>,
    /// The port of the running collector, else the one it used last.
    pub port: Option<u16>,
    pub started_at: Option<i64>,
    pub idle_seconds: Option<u64>,
    pub requests: i64,
    pub tool_calls: i64,
    pub api_errors: i64,
    pub metric_points: i64,
    /// The limits telemetry rows are pruned to.
    pub retention: Retention,
    /// Time of the newest stored event, unix seconds.
    pub last_event: Option<i64>,
    /// How telemetry is configured, in words, and the one-line fix if any.
    pub telemetry: String,
    pub telemetry_fix: Option<String>,
}

/// The time of the newest stored telemetry event, unix seconds. Opens the
/// database read-only with a short wait, so a busy collector cannot stall the
/// caller, and writes nothing. `None` when there is no database or no event.
pub fn last_event(paths: &Paths) -> Option<i64> {
    let conn = playbook_store::Connection::open_with_flags(
        &paths.db,
        playbook_store::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    conn.busy_timeout(Duration::from_millis(300)).ok()?;
    let mut newest = None;
    for table in [
        "telemetry_requests",
        "telemetry_tools",
        "telemetry_errors",
        "telemetry_metrics",
    ] {
        let t: Option<i64> = conn
            .query_row(&format!("SELECT MAX(timestamp) FROM {table}"), [], |r| {
                r.get(0)
            })
            .ok()
            .flatten();
        newest = newest.max(t);
    }
    newest
}

/// Reads the collector state file and the telemetry row counts. Starts
/// nothing, and never shows the token.
pub fn status(paths: &Paths, retention: Retention) -> Result<Status, String> {
    let state = read_state(&state_path(paths));
    let mode = super::telemetry_status::detect_here(paths);
    let live = state.as_ref().filter(|s| running(s));
    let (mut requests, mut tool_calls, mut api_errors, mut metric_points, mut last_event) =
        (0, 0, 0, 0, None);
    if paths.db.exists() {
        let conn = super::db::open_db(&paths.db)?;
        let count = |table: &str| -> Result<(i64, Option<i64>), String> {
            conn.query_row(
                &format!("SELECT COUNT(*), MAX(timestamp) FROM {table}"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| format!("failed to read {table}: {e}"))
        };
        let (r, tr) = count("telemetry_requests")?;
        let (t, tt) = count("telemetry_tools")?;
        let (e, te) = count("telemetry_errors")?;
        let (m, tm) = count("telemetry_metrics")?;
        (requests, tool_calls, api_errors, metric_points) = (r, t, e, m);
        last_event = [tr, tt, te, tm].into_iter().flatten().max();
    }
    Ok(Status {
        running: live.is_some(),
        pid: live.map(|s| s.pid),
        port: state.as_ref().map(|s| s.port),
        started_at: live.map(|s| s.started_at).filter(|t| *t > 0),
        idle_seconds: live.map(|s| s.idle_seconds),
        requests,
        tool_calls,
        api_errors,
        metric_points,
        retention,
        last_event,
        telemetry: mode.text().to_string(),
        telemetry_fix: mode.fix().map(str::to_string),
    })
}

impl Status {
    /// The text a person reads.
    pub fn text(&self, now: i64) -> String {
        use crate::tz::iso;
        let mut lines = vec![self.telemetry.clone()];
        if let Some(fix) = &self.telemetry_fix {
            lines.push(format!("fix: {fix}"));
        }
        match (self.running, self.pid, self.port) {
            (true, Some(pid), Some(port)) => {
                lines.push(format!(
                    "collector: running (pid {pid}) on 127.0.0.1:{port}"
                ));
                if let Some(t) = self.started_at {
                    lines.push(format!("started: {}", iso(t)));
                }
                lines.push(match self.idle_seconds {
                    Some(0) | None => "idle shutdown: never".to_string(),
                    Some(s) => format!("idle shutdown: after {s} s with no request"),
                });
            }
            _ => {
                lines.push("collector: not running".to_string());
                lines.push(format!("fix: {}", super::host::start_hint()));
                if let Some(port) = self.port {
                    lines.push(format!("last port: {port}"));
                }
            }
        }
        lines.push(format!(
            "rows: {} requests, {} tool calls, {} api errors, {} metric points",
            self.requests, self.tool_calls, self.api_errors, self.metric_points
        ));
        let limit = |n: u64, unit: &str| {
            if n == 0 {
                "no limit".to_string()
            } else {
                format!("{n} {unit}")
            }
        };
        lines.push(format!(
            "retention: {}, {} per table",
            limit(self.retention.days, "days"),
            limit(self.retention.max_rows, "rows")
        ));
        lines.push(match self.last_event {
            Some(t) => format!(
                "last event: {} ({} ago)",
                iso(t),
                age(now.saturating_sub(t))
            ),
            None => "last event: none".to_string(),
        });
        lines.join("\n")
    }
}

/// `45s`, `7m`, `3h`, `2d`.
fn age(seconds: i64) -> String {
    match seconds.max(0) {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

/// `playbook usage prune`: deletes telemetry rows past the limits, or with
/// `dry_run` only counts them. Creates no database when there is none.
pub fn run_prune(paths: &Paths, dry_run: bool) -> Result<String, String> {
    if !paths.db.exists() {
        return Ok("nothing to prune: no usage database yet".to_string());
    }
    let conn = super::db::open_db(&paths.db)?;
    migrate_up(&conn)?;
    let retention = Retention::default();
    let p: Pruned = prune(&conn, crate::util::time::now_secs(), retention, dry_run)?;
    let verb = if dry_run { "would remove" } else { "removed" };
    Ok(format!(
        "{verb} {} requests, {} tool calls, {} api errors, {} metric points (keeping {} days, {} rows per table; 0 means no limit)",
        p.requests, p.tools, p.errors, p.metrics, retention.days, retention.max_rows
    ))
}

/// Runs the collector in the foreground until idle or stopped. `port` is an
/// explicit request (an error when busy; it also replaces the recorded port);
/// `None` binds the recorded port (an error when another program owns it) or,
/// on a first run, the default port or else a free one.
pub fn run_collect(
    paths: &Paths,
    port: Option<u16>,
    idle_seconds: u64,
    stop: &Arc<AtomicBool>,
) -> Result<String, String> {
    run_collect_with(paths, port, idle_seconds, stop, DEFAULT_PORT, BIND_RETRY)
}

fn run_collect_with(
    paths: &Paths,
    port: Option<u16>,
    idle_seconds: u64,
    stop: &Arc<AtomicBool>,
    default_port: u16,
    retry: Duration,
) -> Result<String, String> {
    // First local, so it drops last: after the listener closes and after the
    // pid is cleared, which lets a successor bind the port at once.
    let _lock = match daemon::acquire(paths) {
        Lock::Held(file) => file,
        Lock::Busy => return Ok("collector already running".to_string()),
        Lock::Unavailable(e) => {
            return Err(format!(
                "cannot take the lock {}: {e}",
                daemon::lock_path(paths).display()
            ));
        }
    };
    let path = state_path(paths);
    let previous = read_state(&path);
    let mut foreign = false;
    if let Some(prev) = previous.as_ref().filter(|p| running(p)) {
        match probe(prev.port) {
            Probe::Collector => {
                return Ok(format!(
                    "collector already running on port {} (pid {})",
                    prev.port, prev.pid
                ));
            }
            Probe::Silent => {
                return Err(format!(
                    "something on port {} does not answer (recorded collector pid {}); check what holds the port before stopping anything, then run again",
                    prev.port, prev.pid
                ));
            }
            // The pid now belongs to another program: the state is stale.
            Probe::Foreign => foreign = true,
        }
    }
    let conn = super::db::open_db(&paths.db)?;
    migrate_up(&conn)?;
    let listener = match port {
        Some(p) => bind(p).map_err(|e| format!("cannot listen on 127.0.0.1:{p}: {e}"))?,
        None => {
            let recorded = previous.as_ref().map(|p| p.port).filter(|p| *p != 0);
            match recorded {
                Some(p) => {
                    // A foreign owner answers now, so waiting would not help.
                    let wait = if foreign { Duration::ZERO } else { retry };
                    // The token is kept: it is also in the settings that init
                    // wrote, and a new one would lock every session out.
                    // Every spawn would fail the same way, so starters hold
                    // off and doctor can say why.
                    bind_recorded(p, wait, || {
                        let reason = daemon::port_busy_reason(p);
                        let _ = daemon::hold_off(paths, daemon::PORT_BUSY_WINDOW, &reason);
                    })?
                }
                None => first_listener_from(default_port)?,
            }
        }
    };
    let actual = listener.local_addr().map_err(|e| e.to_string())?.port();
    // A foreign program held the recorded port, so sessions may have sent it
    // the old bearer token; a fresh collector gets a fresh one.
    let token = match previous {
        Some(p) if !p.token.is_empty() && !foreign => p.token,
        _ => new_token()?,
    };
    let mut state = State {
        pid: std::process::id(),
        port: actual,
        token: token.clone(),
        started_at: crate::util::time::now_secs(),
        idle_seconds,
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    write_state(&path, &state)?;
    let retention = Retention::default();
    let _ = prune(&conn, crate::util::time::now_secs(), retention, false);
    let cfg = Config {
        token: Some(token),
        idle: (idle_seconds > 0).then(|| Duration::from_secs(idle_seconds)),
        prune: Some(retention),
    };
    crate::say::notice(&format!(
        "usage collect: listening on 127.0.0.1:{actual} (pid {})",
        state.pid
    ));
    let result = serve(&listener, conn, &cfg, stop, crate::util::time::now_secs);
    state.pid = 0;
    let _ = write_state(&path, &state);
    match result {
        Ok(why) => Ok(format!("collector stopped ({why})")),
        Err(e) => Err(format!("collector failed: {e}")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::util::scratch_paths as paths;
    use std::sync::atomic::Ordering;

    #[test]
    fn a_collector_records_the_version_that_started_it() {
        let p = paths("collector-version");
        let stop = Arc::new(AtomicBool::new(false));
        run_collect(&p, Some(0), 1, &stop).unwrap();
        let written = read_state(&state_path(&p)).unwrap();
        assert_eq!(written.version, env!("CARGO_PKG_VERSION"));
    }

    /// Sets the stop flag and wakes the blocking accept on `port`.
    fn halt(stop: &AtomicBool, port: u16) {
        stop.store(true, Ordering::SeqCst);
        crate::receiver::wake(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
    }

    /// Runs a collector on a thread and always stops it, so a failed
    /// assertion cannot leave one serving.
    pub(crate) struct Running {
        stop: Arc<AtomicBool>,
        port: u16,
        thread: Option<std::thread::JoinHandle<Result<String, String>>>,
    }

    /// How many ports `Running::start` tries when another program takes each.
    const START_PORT_TRIES: u32 = 5;

    impl Running {
        pub(crate) fn start(p: &Paths) -> Self {
            // A port read as free can be taken before the collector binds it
            // (a probe and a bind are two steps), and the collector then
            // fails to start at once. Only that failure picks another port.
            let mut last = String::new();
            for _ in 0..START_PORT_TRIES {
                match Self::start_on(p, free_port()) {
                    Ok(running) => return running,
                    Err(e) if e.contains("cannot listen on") => last = e,
                    Err(e) => panic!("{e}"),
                }
            }
            panic!("no collector came up in {START_PORT_TRIES} ports: {last}");
        }

        fn start_on(p: &Paths, port: u16) -> Result<Self, String> {
            let stop = Arc::new(AtomicBool::new(false));
            let (paths, flag) = (p.clone(), Arc::clone(&stop));
            let thread = std::thread::spawn(move || run_collect(&paths, Some(port), 0, &flag));
            let mut me = Self {
                stop,
                port: 0,
                thread: Some(thread),
            };
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(s) = read_state(&state_path(p)).filter(|s| s.pid != 0) {
                    me.port = s.port;
                    return Ok(me);
                }
                if me.thread.as_ref().is_some_and(|t| t.is_finished()) {
                    let ended = me.thread.take().unwrap().join().unwrap();
                    return Err(format!("the collector ended at once: {ended:?}"));
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "collector never came up"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn finish(mut self) -> Result<String, String> {
            halt(&self.stop, self.port);
            self.thread.take().unwrap().join().unwrap()
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            // With no port known nothing can wake accept, so a join could hang;
            // the flag still ends a serve that has not reached accept yet.
            if self.port == 0 {
                self.stop.store(true, Ordering::SeqCst);
                return;
            }
            halt(&self.stop, self.port);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn lock_is_free(p: &Paths) -> bool {
        let file = std::fs::File::open(crate::daemon::lock_path(p)).unwrap();
        file.try_lock().is_ok()
    }

    #[test]
    fn a_second_run_while_the_lock_is_held_binds_nothing_and_leaves_the_state() {
        let p = paths("collector-lock-busy");
        let crate::daemon::Lock::Held(_held) = crate::daemon::acquire(&p) else {
            panic!("the first acquire must take the lock");
        };
        let known = State {
            pid: 1,
            port: 1,
            token: "known".into(),
            ..State::default()
        };
        write_state(&state_path(&p), &known).unwrap();
        let before = std::fs::read(state_path(&p)).unwrap();
        let stop = Arc::new(AtomicBool::new(false));

        let msg = run_collect(&p, Some(0), 0, &stop).unwrap();

        assert!(msg.contains("already running"), "{msg}");
        assert_eq!(
            std::fs::read(state_path(&p)).unwrap(),
            before,
            "a busy lock leaves the state as it was"
        );
    }

    #[test]
    fn the_lock_is_held_while_serving_and_names_this_process() {
        let p = paths("collector-lock-held");
        let running = Running::start(&p);

        let lock = crate::daemon::lock_path(&p);
        assert!(!lock_is_free(&p));
        assert_eq!(
            std::fs::read_to_string(&lock).unwrap(),
            format!("{} {}\n", std::process::id(), env!("CARGO_PKG_VERSION"))
        );
        running.finish().unwrap();
        assert!(crate::util::wait_unlocked(&crate::daemon::lock_path(&p)));
    }

    #[test]
    fn a_successor_gets_the_lock_and_the_port_once_run_collect_returns() {
        let p = paths("collector-lock-order");
        let running = Running::start(&p);
        let port = read_state(&state_path(&p)).unwrap().port;

        running.finish().unwrap();

        assert!(crate::util::wait_unlocked(&crate::daemon::lock_path(&p)));
        // `run_collect` has returned, so its listener is closed and the port
        // is free unless another program took it in the moment since. A probe
        // or a collector of another test holds it for a moment or for good and
        // answers when asked. A listener this collector leaked would hold it
        // for good and never answer, so the port counts as taken only when the
        // bind keeps failing and something answers there.
        let free = crate::util::within(Duration::from_secs(2), || bind(port).is_ok());
        if !free {
            let answer = probe(port);
            assert_ne!(
                answer,
                Probe::Silent,
                "port {port} stays busy after the stop and nothing answers there"
            );
        }
        assert_eq!(read_state(&state_path(&p)).unwrap().pid, 0);
    }

    #[cfg(unix)]
    #[test]
    fn the_lock_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let p = paths("collector-lock-mode");
        let crate::daemon::Lock::Held(_held) = crate::daemon::acquire(&p) else {
            panic!("acquire failed");
        };
        let mode = std::fs::metadata(crate::daemon::lock_path(&p))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_data_folder_is_an_error_naming_the_lock_file() {
        use std::os::unix::fs::PermissionsExt;
        let p = paths("collector-lock-readonly");
        let dir = state_path(&p).parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let stop = Arc::new(AtomicBool::new(false));

        let result = run_collect(&p, Some(0), 0, &stop);

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let err = result.unwrap_err();
        assert!(err.contains("collector.lock"), "{err}");
    }

    #[test]
    fn a_dead_pid_is_not_running_and_our_own_pid_is() {
        let dead = State {
            pid: 0,
            port: 1,
            token: "t".into(),
            ..State::default()
        };
        assert!(!running(&dead));
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn it_idles_out_keeps_token_and_port_and_clears_the_pid() {
        let p = paths("collector-idle");
        let stop = Arc::new(AtomicBool::new(false));
        let msg = run_collect(&p, Some(0), 1, &stop).unwrap();
        assert_eq!(msg, "collector stopped (idle)");
        let first = read_state(&state_path(&p)).unwrap();
        assert_eq!(first.pid, 0);
        assert_eq!(first.token.len(), 32);

        // A restart reuses the token (sessions started earlier keep working).
        run_collect(&p, Some(0), 1, &stop).unwrap();
        assert_eq!(read_state(&state_path(&p)).unwrap().token, first.token);
    }

    #[test]
    fn a_second_start_reports_the_live_one_and_does_not_start() {
        let p = paths("collector-live");
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = State {
            pid: std::process::id(),
            port,
            token: "t".into(),
            ..State::default()
        };
        std::fs::create_dir_all(state_path(&p).parent().unwrap()).unwrap();
        write_state(&state_path(&p), &state).unwrap();
        // A real receiver answers the probe.
        let db = crate::db::open_db(&p.db).unwrap();
        migrate_up(&db).unwrap();
        let serve_stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&serve_stop);
        let thread = std::thread::spawn(move || {
            let cfg = Config {
                token: None,
                idle: None,
                prune: None,
            };
            let _ = serve(&listener, db, &cfg, &server_stop, || 0);
        });
        let _server = Serving {
            stop: serve_stop,
            port,
            thread: Some(thread),
        };
        let stop = Arc::new(AtomicBool::new(false));

        let msg = run_collect(&p, None, 1, &stop).unwrap();

        assert!(msg.contains("already running"), "{msg}");
    }

    /// A bare `serve` on a thread, stopped and joined in `Drop`.
    struct Serving {
        stop: Arc<AtomicBool>,
        port: u16,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for Serving {
        fn drop(&mut self) {
            halt(&self.stop, self.port);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn state_on(p: &Paths, port: u16) {
        let state = State {
            pid: std::process::id(),
            port,
            token: "t".into(),
            ..State::default()
        };
        std::fs::create_dir_all(state_path(p).parent().unwrap()).unwrap();
        write_state(&state_path(p), &state).unwrap();
    }

    #[test]
    fn a_listener_that_never_answers_is_reported_not_called_running() {
        let p = paths("collector-silent");
        let listener = bind(0).unwrap();
        state_on(&p, listener.local_addr().unwrap().port());
        let stop = Arc::new(AtomicBool::new(false));

        let err = run_collect(&p, None, 1, &stop).unwrap_err();

        assert!(err.contains("does not answer"), "{err}");
        drop(listener);
    }

    #[test]
    fn a_foreign_program_that_still_holds_the_port_is_an_error_without_a_wait() {
        use std::io::{Read, Write};
        let p = paths("collector-foreign");
        let foreign = bind(0).unwrap();
        let port = foreign.local_addr().unwrap().port();
        state_on(&p, port);
        let done = Arc::new(AtomicBool::new(false));
        let done_in_thread = Arc::clone(&done);
        foreign.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            while !done_in_thread.load(Ordering::SeqCst) {
                if let Ok((mut s, _)) = foreign.accept() {
                    let mut buf = [0u8; 512];
                    let _ = s.read(&mut buf);
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        });
        let stop = Arc::new(AtomicBool::new(false));
        let started = std::time::Instant::now();

        let result = run_collect_with(&p, None, 1, &stop, DEFAULT_PORT, Duration::from_secs(30));

        done.store(true, Ordering::SeqCst);
        let _ = server.join();
        let message = result.unwrap_err();
        assert!(message.contains("in use by another program"), "{message}");
        assert!(started.elapsed() < Duration::from_secs(10));
        let after = read_state(&state_path(&p)).unwrap();
        assert_eq!((after.port, after.token.as_str()), (port, "t"));
        assert_eq!(
            daemon::stamp_reason(&p),
            Some(daemon::port_busy_reason(port))
        );
    }

    #[test]
    fn replies_are_classified_by_the_collector_header() {
        assert_eq!(
            classify_reply(b"HTTP/1.1 405 Method Not Allowed\r\nX-Playbook-Collector: 1\r\n\r\n"),
            Probe::Collector
        );
        assert_eq!(classify_reply(b"HTTP/1.1 200 OK\r\n\r\n"), Probe::Foreign);
    }

    #[test]
    fn a_stale_state_is_replaced() {
        let p = paths("collector-stale");
        std::fs::create_dir_all(state_path(&p).parent().unwrap()).unwrap();
        let stale = State {
            pid: 999_999,
            port: free_port(),
            token: "keep".into(),
            ..State::default()
        };
        let recorded = stale.port;
        write_state(&state_path(&p), &stale).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        run_collect(&p, None, 1, &stop).unwrap();
        let after = read_state(&state_path(&p)).unwrap();
        assert_eq!(after.token, "keep");
        assert_eq!(after.port, recorded);
    }

    /// A port nothing listens on, from a range the system never hands out for
    /// `bind(0)`, so a sibling test that binds port 0 cannot be given it
    /// between this probe and the bind it is picked for. Each call tries a
    /// new candidate, and the process id spreads separate test processes.
    pub(crate) fn free_port() -> u16 {
        use std::sync::atomic::AtomicU32;
        const FIRST: u32 = 20_000;
        const SPAN: u32 = 10_000;
        static NEXT: AtomicU32 = AtomicU32::new(0);
        for _ in 0..SPAN {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let port =
                u16::try_from(FIRST + (std::process::id().wrapping_mul(97) + n) % SPAN).unwrap();
            if bind(port).is_ok() {
                return port;
            }
        }
        panic!("no free port from {FIRST} to {}", FIRST + SPAN);
    }

    #[test]
    fn an_explicit_busy_port_is_an_error() {
        let p = paths("collector-busy");
        let holder = bind(0).unwrap();
        let busy = holder.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));

        let result = run_collect(&p, Some(busy), 1, &stop);

        let message = result.unwrap_err();
        assert!(
            message.contains(&format!("cannot listen on 127.0.0.1:{busy}")),
            "{message}"
        );
    }

    #[test]
    fn the_first_run_records_a_port_and_a_restart_binds_the_same_one() {
        let p = paths("collector-first-run");
        let stop = Arc::new(AtomicBool::new(false));
        let wanted = free_port();
        let retry = Duration::from_millis(200);

        run_collect_with(&p, None, 1, &stop, wanted, retry).unwrap();
        let first = read_state(&state_path(&p)).unwrap();
        run_collect_with(&p, None, 1, &stop, free_port(), retry).unwrap();
        let second = read_state(&state_path(&p)).unwrap();

        assert_eq!(first.port, wanted);
        assert_eq!(second.port, first.port);
    }

    #[test]
    fn the_port_init_records_is_the_port_the_collector_binds() {
        let p = paths("collector-handoff");
        let stop = Arc::new(AtomicBool::new(false));

        let recorded = ensure_state(&p).unwrap().port;
        run_collect_with(&p, None, 1, &stop, DEFAULT_PORT, Duration::from_millis(200)).unwrap();

        assert_eq!(read_state(&state_path(&p)).unwrap().port, recorded);
    }

    #[test]
    fn a_default_port_that_is_taken_on_the_first_run_falls_back_to_a_free_port() {
        let holder = bind(0).unwrap();
        let taken = holder.local_addr().unwrap().port();

        let chosen = first_port_from(taken).unwrap();

        assert_ne!(chosen, taken);
        assert_ne!(chosen, 0);
    }

    #[test]
    fn a_free_default_port_is_chosen_on_the_first_run() {
        // A port read as free can be taken by another program before the bind
        // (the probe and the bind are two steps), and the first run then falls
        // back to a free port, which is right. A first run that never takes a
        // free preferred port fails every try, so a few tries tell the two apart.
        for _ in 0..FREE_PORT_TRIES {
            let wanted = free_port();

            if first_port_from(wanted).unwrap() == wanted {
                return;
            }
        }
        panic!("the first run never took a free preferred port in {FREE_PORT_TRIES} tries");
    }

    /// How many free ports the free default port test tries when another
    /// program takes one first.
    const FREE_PORT_TRIES: u32 = 20;

    #[test]
    fn a_port_a_killed_predecessor_is_still_releasing_binds_once_it_lets_go() {
        let holder = bind(0).unwrap();
        let port = holder.local_addr().unwrap().port();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(holder);
        });
        let started = std::time::Instant::now();

        let again = bind_recorded(port, Duration::from_secs(5), || ());

        let _ = releaser.join();
        assert!(again.is_ok(), "{:?}", again.err());
        assert!(started.elapsed() >= Duration::from_millis(250));
    }

    #[test]
    fn a_port_left_in_time_wait_still_binds() {
        use std::io::Write;
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (mut accepted, _) = listener.accept().unwrap();
        // The server side closes first, which leaves the port in TIME_WAIT.
        let _ = accepted.write_all(b"y");
        drop(accepted);
        drop(listener);
        drop(client);

        let again = bind_recorded(port, Duration::from_secs(1), || ());

        assert!(again.is_ok(), "{:?}", again.err());
    }

    #[test]
    fn a_recorded_port_owned_by_another_program_is_an_error_and_the_record_is_unchanged() {
        let p = paths("collector-owned");
        let holder = bind(0).unwrap();
        let port = holder.local_addr().unwrap().port();
        let state = State {
            pid: 0,
            port,
            token: "t".into(),
            ..State::default()
        };
        std::fs::create_dir_all(state_path(&p).parent().unwrap()).unwrap();
        write_state(&state_path(&p), &state).unwrap();
        let stop = Arc::new(AtomicBool::new(false));

        let result = run_collect_with(&p, None, 1, &stop, DEFAULT_PORT, Duration::from_millis(200));

        let message = result.unwrap_err();
        assert!(message.contains("in use by another program"), "{message}");
        assert!(
            message.contains("playbook setup --only telemetry"),
            "{message}"
        );
        let after = read_state(&state_path(&p)).unwrap();
        assert_eq!((after.port, after.token.as_str()), (port, "t"));
    }

    #[test]
    fn a_stale_pid_file_is_replaced_and_the_recorded_port_is_kept() {
        let p = paths("collector-stale-pid");
        let port = free_port();
        let state = State {
            pid: 999_999,
            port,
            token: "t".into(),
            ..State::default()
        };
        std::fs::create_dir_all(state_path(&p).parent().unwrap()).unwrap();
        write_state(&state_path(&p), &state).unwrap();
        let stop = Arc::new(AtomicBool::new(false));

        run_collect_with(&p, None, 1, &stop, DEFAULT_PORT, Duration::from_millis(200)).unwrap();

        assert_eq!(read_state(&state_path(&p)).unwrap().port, port);
    }

    #[cfg(unix)]
    #[test]
    fn the_state_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let p = paths("collector-mode");
        let stop = Arc::new(AtomicBool::new(true));
        run_collect(&p, Some(0), 0, &stop).unwrap();
        let mode = std::fs::metadata(state_path(&p))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn status_with_nothing_set_up_says_not_running_and_creates_no_files() {
        let p = paths("collector-status-none");
        let s = status(&p, Retention::default()).unwrap();
        assert!(!s.running && s.pid.is_none() && s.last_event.is_none());
        assert_eq!((s.requests, s.tool_calls, s.api_errors), (0, 0, 0));
        assert!(!p.db.exists(), "status must not create the database");
        let text = s.text(1_000);
        assert!(
            text.contains("not running") && text.contains("last event: none"),
            "{text}"
        );
    }

    #[test]
    fn status_reports_a_live_collector_its_rows_and_the_last_event_without_the_token() {
        use crate::telemetry_db::{insert_request, insert_tool, TelemetryRequest, TelemetryTool};
        let p = paths("collector-status-live");
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::create_dir_all(state_path(&p).parent().unwrap()).unwrap();
        let state = State {
            pid: std::process::id(),
            port,
            token: "supersecrettoken".into(),
            started_at: 1_760_000_000,
            idle_seconds: 600,
            ..State::default()
        };
        write_state(&state_path(&p), &state).unwrap();
        let conn = crate::db::open_db(&p.db).unwrap();
        let request = TelemetryRequest {
            session_id: "s".into(),
            request_id: "r".into(),
            timestamp: 1_760_000_100,
            ..TelemetryRequest::default()
        };
        insert_request(&conn, &request).unwrap();
        let tool = TelemetryTool {
            session_id: "s".into(),
            tool_use_id: "t".into(),
            timestamp: 1_760_000_200,
            ..TelemetryTool::default()
        };
        insert_tool(&conn, &tool).unwrap();
        drop(conn);

        let s = status(&p, Retention::default()).unwrap();
        assert!(s.running);
        assert_eq!((s.pid, s.port), (Some(std::process::id()), Some(port)));
        assert_eq!((s.requests, s.tool_calls, s.api_errors), (1, 1, 0));
        assert_eq!(s.last_event, Some(1_760_000_200));
        let text = s.text(1_760_000_260);
        assert!(
            text.contains("running") && text.contains("after 600 s"),
            "{text}"
        );
        assert!(
            text.contains("1 requests, 1 tool calls, 0 api errors"),
            "{text}"
        );
        assert!(text.contains("(1m ago)"), "{text}");
        assert!(
            text.contains("0 metric points")
                && text.contains("retention: 90 days, 500000 rows per table"),
            "{text}"
        );
        let json = serde_json::to_string(&s).unwrap();
        assert!(!text.contains("supersecret") && !json.contains("supersecret"));
    }

    #[test]
    fn prune_says_so_without_a_database_and_otherwise_removes_old_rows() {
        let p = paths("collector-prune");
        assert!(run_prune(&p, false).unwrap().contains("no usage database"));
        assert!(!p.db.exists(), "prune must not create the database");
        std::fs::create_dir_all(p.db.parent().unwrap()).unwrap();
        let conn = crate::db::open_db(&p.db).unwrap();
        migrate_up(&conn).unwrap();
        let old = crate::telemetry_db::TelemetryTool {
            session_id: "s".into(),
            tool_use_id: "old".into(),
            timestamp: 1_000,
            ..Default::default()
        };
        crate::telemetry_db::insert_tool(&conn, &old).unwrap();
        drop(conn);
        let dry = run_prune(&p, true).unwrap();
        assert!(
            dry.starts_with("would remove 0 requests, 1 tool calls"),
            "{dry}"
        );
        let done = run_prune(&p, false).unwrap();
        assert!(
            done.starts_with("removed 0 requests, 1 tool calls"),
            "{done}"
        );
        assert!(run_prune(&p, false)
            .unwrap()
            .starts_with("removed 0 requests, 0 tool calls"));
    }

    #[test]
    fn a_stale_state_reads_as_not_running_and_keeps_the_last_port() {
        let p = paths("collector-status-stale");
        std::fs::create_dir_all(state_path(&p).parent().unwrap()).unwrap();
        let stale = State {
            pid: 999_999,
            port: 4318,
            token: "t".into(),
            ..State::default()
        };
        write_state(&state_path(&p), &stale).unwrap();
        let s = status(&p, Retention::default()).unwrap();
        assert!(!s.running);
        assert!(s.text(1).contains("last port: 4318"));
    }

    #[test]
    fn a_state_file_from_an_older_version_still_reads() {
        let old = br#"{"pid":5,"port":4318,"token":"abc"}"#;
        let state: State = serde_json::from_slice(old).unwrap();
        assert_eq!((state.started_at, state.idle_seconds), (0, 0));
    }

    #[test]
    fn ages_read_in_their_largest_unit() {
        assert_eq!(
            [age(5), age(125), age(7_300), age(200_000)],
            ["5s", "2m", "2h", "2d"]
        );
    }
}
