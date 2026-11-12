// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The release check that runs at the start of a command. `update.mode`
//! picks `notify` (the default), `auto` or `off`. The last result lives in the
//! `state` table under `update-check/cache`, so a command reads one row and
//! never waits on the network: a stale row starts a detached
//! `playbook upgrade --refresh-check`, and the notice shows on a later run.
//! In `auto` mode that process also installs the release (see `auto`), and
//! the next command prints what happened. Every failed check is silent and
//! runs again after the next interval.

use super::download::Fetcher;
use super::resolve::{self, compare};
use crate::Command;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

/// The `state` table key that holds the cached result.
pub const CACHE_KEY: &str = "update-check/cache";

/// The limit on the release list request. It runs in a detached process,
/// so no command waits for it.
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

const DAY_SECS: u64 = 86_400;

/// How long after a spawn an auto update counts as running. Within it no
/// command starts another process or shows the notice; after it, an attempt
/// that was never claimed (offline, say) falls back to the notice.
const APPLY_GRACE_SECS: u64 = 15 * 60;
/// Hours a release check stays fresh before a command starts a new one.
pub const INTERVAL_HOURS: u64 = 24;

/// What `update.mode` asks for. This is separate from the playbook
/// permission mode (`ask` or `auto`) that `Env::auto_mode` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateMode {
    Off,
    Notify,
    Auto,
}

impl UpdateMode {
    pub fn parse(value: &str) -> UpdateMode {
        match value {
            "off" => UpdateMode::Off,
            "auto" => UpdateMode::Auto,
            _ => UpdateMode::Notify,
        }
    }
}

/// The two config keys the check reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub mode: UpdateMode,
    pub interval_secs: u64,
}

impl Settings {
    /// Whether the check acts at all: never when off, and without a terminal
    /// only in `auto`, which needs no person to see it.
    pub fn acts(&self, stderr_tty: bool) -> bool {
        self.mode != UpdateMode::Off && (stderr_tty || self.mode == UpdateMode::Auto)
    }

    /// Reads only `update.mode`, through a read-only connection that
    /// creates and changes nothing, for a piped run that stops unless the
    /// mode is `auto`.
    pub fn peek_mode(root: &Path) -> UpdateMode {
        crate::config::store::global_read_only(root, "update.mode")
            .and_then(|v| v.as_str().map(UpdateMode::parse))
            .unwrap_or(UpdateMode::Notify)
    }

    /// Reads `update.mode` from the global tier, where `config set` puts this
    /// per-user key. An unreadable store or a bad value falls back to the
    /// default.
    pub fn read(home: &Path) -> Settings {
        let mode = crate::config::resolve_valid("update.mode", home, None)
            .ok()
            .and_then(|(v, _, _)| v.as_str().map(UpdateMode::parse))
            .unwrap_or(UpdateMode::Notify);
        Settings::with_mode(mode)
    }

    /// The settings for a mode already read.
    pub fn with_mode(mode: UpdateMode) -> Settings {
        Settings {
            mode,
            interval_secs: INTERVAL_HOURS * 3600,
        }
    }
}

/// The cached result of the last check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cache {
    /// When the last check started, in unix seconds.
    #[serde(default)]
    pub checked_at: u64,
    /// The newest plain `vX.Y.Z` release, without the `v`.
    #[serde(default)]
    pub latest: Option<String>,
    /// When that release was published, as GitHub wrote it.
    #[serde(default)]
    pub published_at: Option<String>,
    /// `<version>@<unix seconds>` of the last notice.
    #[serde(default)]
    pub notified: Option<String>,
    /// `<version>@<unix seconds>` of the last auto update attempt.
    #[serde(default)]
    pub tried: Option<String>,
    /// `<version>@<unix seconds>` of the last time a command started the
    /// detached process to install `version`.
    #[serde(default)]
    pub spawned: Option<String>,
    /// Why auto holds a version back, found by the detached process.
    #[serde(default)]
    pub held: Option<Held>,
    /// Lines the auto update left for the next command in a terminal.
    #[serde(default)]
    pub report: Vec<String>,
}

/// A version auto does not install, why, and until when (unix seconds).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub version: String,
    pub reason: String,
    pub until: u64,
}

/// The `<version>@<unix seconds>` stamp `notified` and `tried` hold.
pub fn stamp(version: &str, now: u64) -> String {
    format!("{version}@{now}")
}

/// Whether a `<version>@<unix seconds>` stamp names `version` and is less
/// than a day old at `now`.
fn stamped_recently(stamp: Option<&str>, version: &str, now: u64) -> bool {
    stamp
        .and_then(|n| n.rsplit_once('@'))
        .filter(|(v, _)| *v == version)
        .and_then(|(_, at)| at.parse::<u64>().ok())
        .is_some_and(|at| at <= now && now - at < DAY_SECS)
}

impl Cache {
    pub fn read(root: &Path) -> Result<Cache, String> {
        let raw = crate::state::get(root, CACHE_KEY).map_err(|e| e.to_string())?;
        Ok(raw
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default())
    }

    /// Reads the row through a read-only connection, which creates and
    /// changes nothing. A missing database or row is the default.
    pub fn read_only(root: &Path) -> Result<Cache, String> {
        let rows = crate::state::list_read_only(root, CACHE_KEY).map_err(|e| e.to_string())?;
        Ok(rows
            .into_iter()
            .find(|(key, _)| key == CACHE_KEY)
            .and_then(|(_, text)| serde_json::from_str(&text).ok())
            .unwrap_or_default())
    }

    /// Reads, changes and writes the row in one transaction and returns what
    /// `change` returns, so two processes never both claim the same action.
    pub fn update<T>(root: &Path, change: impl FnOnce(&mut Cache) -> T) -> Result<T, String> {
        crate::state::transaction(root, |tx| {
            let mut cache: Cache = tx
                .get(CACHE_KEY)?
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
            let out = change(&mut cache);
            let text = serde_json::to_string(&cache)
                .map_err(|e| crate::state::StateError(e.to_string()))?;
            tx.set(CACHE_KEY, &text)?;
            Ok(out)
        })
        .map_err(|e| e.to_string())
    }

    /// The cached release when it is newer than `current`.
    pub fn newer_than(&self, current: &str) -> Option<&str> {
        self.latest
            .as_deref()
            .filter(|latest| compare(latest, current).is_gt())
    }

    /// Whether the notice for `version` showed less than a day before `now`.
    fn shown_recently(&self, version: &str, now: u64) -> bool {
        stamped_recently(self.notified.as_deref(), version, now)
    }

    /// Whether auto tried `version` less than a day before `now`.
    pub fn tried_recently(&self, version: &str, now: u64) -> bool {
        stamped_recently(self.tried.as_deref(), version, now)
    }

    /// Whether a command started an auto update of `version` less than a day
    /// before `now`.
    fn spawned_recently(&self, version: &str, now: u64) -> bool {
        stamped_recently(self.spawned.as_deref(), version, now)
    }

    /// Why auto still holds `version` back at `now`, if it does.
    pub fn held_reason(&self, version: &str, now: u64) -> Option<&str> {
        self.held
            .as_ref()
            .filter(|h| h.version == version && now < h.until)
            .map(|h| h.reason.as_str())
    }
}

/// Whether `command` may run the check. Hooks, the status line, `version`,
/// `json`, `commands`, `update` itself, the background `usage warm` and `usage collect`
/// runs that the session hooks start, and any run with a `--json` argument
/// never do.
pub fn applies(command: &Command, args: &[OsString]) -> bool {
    use crate::UsageCommand;
    let skipped = command.is_hot_path()
        || matches!(
            command,
            Command::Upgrade { .. }
                | Command::Usage {
                    sub: Some(UsageCommand::Warm | UsageCommand::Collect { .. }),
                    ..
                }
        );
    !skipped && !has_json_arg(args)
}

/// Whether the arguments ask for `--json` output, which no on-start step
/// may add a line to.
pub fn has_json_arg(args: &[OsString]) -> bool {
    args.iter().any(|a| {
        a.to_str()
            .is_some_and(|s| s == "--json" || s.starts_with("--json="))
    })
}

/// CI vendor variables. `CI` and `PLAYBOOK_HEADLESS` go through
/// `common::headless`, the switch every hook uses.
const CI_VARS: [&str; 6] = [
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "JENKINS_URL",
    "TF_BUILD",
];

/// Whether a CI vendor variable is set to anything other than empty, `0` or
/// `false`.
pub fn ci_vendor_env(get: impl Fn(&str) -> Option<String>) -> bool {
    CI_VARS.iter().any(|name| {
        get(name).is_some_and(|v| {
            let v = v.trim().to_ascii_lowercase();
            !v.is_empty() && v != "0" && v != "false"
        })
    })
}

/// The facts about this run that the plan needs.
#[derive(Debug, Clone)]
pub struct Probe {
    pub current: String,
    pub now: u64,
    pub stderr_tty: bool,
    pub install: super::auto::Install,
}

/// A notice to print, and the version it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub version: String,
    pub line: String,
}

/// What this run should do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Start a detached refresh and record the start time.
    pub refresh: bool,
    /// The `checked_at` the plan saw, so only one process claims a refresh.
    pub seen_checked_at: u64,
    /// Print this line on stderr and record it.
    pub notice: Option<Notice>,
    /// Start the detached process for an auto update of this version, and
    /// record the start. The process reads its target again from the cache.
    pub apply: Option<String>,
    /// Print and clear the report the last auto update left.
    pub report: bool,
}

impl Plan {
    /// Whether the plan has nothing to do.
    pub fn is_empty(&self) -> bool {
        !self.refresh && self.notice.is_none() && self.apply.is_none() && !self.report
    }
}

/// What the notice adds after the command to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail<'a> {
    Nothing,
    /// Why auto did not install the release.
    Skipped(&'a str),
    /// How to turn on auto, for an install that can update itself.
    Hint,
}

/// The command that updates the install.
fn update_command(homebrew: bool) -> &'static str {
    if homebrew {
        "brew upgrade playbook"
    } else {
        "playbook upgrade"
    }
}

/// The one line the notice prints.
pub fn notice_text(latest: &str, current: &str, homebrew: bool, tail: Tail) -> String {
    let fix = update_command(homebrew);
    let tail = match tail {
        Tail::Nothing => String::new(),
        Tail::Skipped(reason) => format!(" (auto update skipped: {reason})"),
        Tail::Hint => {
            ". To update on its own: playbook config set --global update.mode auto".to_string()
        }
    };
    format!("playbook {latest} is available (you have {current}). Run: {fix}{tail}")
}

/// Decides what to do from the settings, the cached row and this run. In
/// `auto` mode a newer release that nothing holds back goes to the detached
/// process, once a day at most, and the notice stays quiet while that
/// attempt runs or after it claimed the version. Otherwise the notice shows.
/// A run without a terminal on stderr prints nothing, and in `notify` mode
/// it does nothing.
pub fn plan(settings: Settings, cache: &Cache, probe: &Probe) -> Plan {
    if !settings.acts(probe.stderr_tty) {
        return Plan::default();
    }
    let now = probe.now;
    let age = now.saturating_sub(cache.checked_at);
    // A clock that moved back counts as due too.
    let due = |secs: u64| cache.checked_at > now || age >= secs;
    let mut plan = Plan {
        refresh: due(settings.interval_secs),
        seen_checked_at: cache.checked_at,
        report: probe.stderr_tty && !cache.report.is_empty(),
        ..Plan::default()
    };
    let Some(latest) = cache.newer_than(&probe.current) else {
        return plan;
    };
    let auto = settings.mode == UpdateMode::Auto;
    let held = if auto {
        super::auto::hold_reason(&probe.install, cache, latest, now)
    } else {
        None
    };
    if auto && held.is_none() {
        let running = !due(APPLY_GRACE_SECS);
        if running || cache.tried_recently(latest, now) {
            return plan;
        }
        if !cache.spawned_recently(latest, now) {
            plan.apply = Some(latest.to_string());
            return plan;
        }
        // The last start never claimed its attempt, so the notice shows.
    }
    if !probe.stderr_tty || cache.shown_recently(latest, now) {
        return plan;
    }
    let tail = match &held {
        Some(reason) => Tail::Skipped(reason),
        None if !auto && super::auto::install_reason(&probe.install).is_none() => Tail::Hint,
        None => Tail::Nothing,
    };
    plan.notice = Some(Notice {
        version: latest.to_string(),
        line: notice_text(latest, &probe.current, probe.install.homebrew, tail),
    });
    plan
}

/// The newest plain release in a `GET /releases` body, with its publish
/// time. Drafts, pre-releases and plugin or telemetry tags are skipped.
pub fn newest_plain(body: &str) -> Result<Option<(String, Option<String>)>, String> {
    Ok(resolve::parse_releases(body)?
        .into_iter()
        .filter(resolve::Release::is_plain_stable)
        .max_by(|a, b| compare(a.version(), b.version()))
        .map(|r| (r.version().to_string(), r.published_at)))
}

/// Fetches the release list and stores the newest plain release. On an
/// error nothing is stored; the start time the caller recorded makes the
/// next try wait one interval.
pub fn refresh(root: &Path, fetcher: &dyn Fetcher, api_base: &str) -> Result<(), String> {
    let body = fetcher.get(&format!("{api_base}{}", resolve::RELEASES_PAGE))?;
    let text = String::from_utf8(body).map_err(|_| "release data is not text".to_string())?;
    let Some((latest, published)) = newest_plain(&text)? else {
        return Ok(());
    };
    Cache::update(root, |cache| {
        cache.latest = Some(latest);
        cache.published_at = published;
    })
}

/// Applies `plan`. Each action is claimed inside one transaction against
/// the current row, so of two commands that start together only one spawns
/// the detached process and only one prints. A store that cannot be written
/// stops everything. `spawn` starts the process and `print` writes a line.
pub fn execute(root: &Path, plan: &Plan, now: u64, spawn: &dyn Fn(), print: &mut dyn FnMut(&str)) {
    if plan.is_empty() {
        return;
    }
    let claimed = Cache::update(root, |cache| {
        let claim =
            (plan.refresh || plan.apply.is_some()) && cache.checked_at == plan.seen_checked_at;
        if claim {
            cache.checked_at = now;
            if let Some(version) = &plan.apply {
                cache.spawned = Some(stamp(version, now));
            }
        }
        let notice = plan
            .notice
            .as_ref()
            .filter(|n| !cache.shown_recently(&n.version, now));
        if let Some(n) = notice {
            cache.notified = Some(stamp(&n.version, now));
        }
        let mut lines = if plan.report {
            std::mem::take(&mut cache.report)
        } else {
            Vec::new()
        };
        lines.extend(notice.map(|n| n.line.clone()));
        (claim, lines)
    });
    let Ok((spawned, lines)) = claimed else {
        return;
    };
    if spawned {
        spawn();
    }
    for line in &lines {
        print(line);
    }
}

impl UpdateMode {
    pub fn label(self) -> &'static str {
        match self {
            UpdateMode::Off => "off",
            UpdateMode::Notify => "notify",
            UpdateMode::Auto => "auto",
        }
    }
}

/// One line on the check for `doctor` and `update --check`: the mode, when
/// the last background check started and what it found, and in `auto` mode
/// why a newer release is held back.
pub fn status_line(mode: UpdateMode, cache: Result<&Cache, &str>, probe: &Probe) -> String {
    let mut line = format!("update.mode is {}", mode.label());
    if mode == UpdateMode::Off {
        line.push_str(", so no background release check runs");
        return line;
    }
    let cache = match cache {
        Ok(cache) => cache,
        Err(err) => {
            line.push_str(&format!(
                "; the release check cache could not be read: {err}"
            ));
            return line;
        }
    };
    if cache.checked_at == 0 {
        line.push_str("; no background release check has run yet");
        return line;
    }
    let at = crate::common::time::iso_utc(i64::try_from(cache.checked_at).unwrap_or(0));
    line.push_str(&format!("; last background check started {at}"));
    if let Some(latest) = &cache.latest {
        line.push_str(&format!(", latest release found {latest}"));
    }
    let held = cache
        .newer_than(&probe.current)
        .filter(|_| mode == UpdateMode::Auto)
        .and_then(|latest| super::auto::hold_reason(&probe.install, cache, latest, probe.now));
    if let Some(reason) = held {
        line.push_str(&format!(", auto update held: {reason}"));
    }
    line
}

/// The release check status of the playbook root under `home`.
pub struct Status {
    pub line: String,
    /// The cached release when it is newer than the running binary.
    pub newer: Option<String>,
    /// The command that updates this install.
    pub fix: &'static str,
}

/// The status for `home`, read through read-only connections, so it creates
/// and changes nothing on disk.
pub fn status_at(home: &Path) -> Status {
    let root = crate::common::paths::playbook_root_from(home);
    let mode = Settings::peek_mode(&root);
    let cache = Cache::read_only(&root);
    let exe = std::env::current_exe().unwrap_or_default();
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let current = env!("CARGO_PKG_VERSION");
    let probe = Probe {
        current: current.to_string(),
        now: crate::common::time::now_epoch_secs(),
        stderr_tty: false,
        install: super::auto::Install::real(&exe, current),
    };
    let newer = cache
        .as_ref()
        .ok()
        .and_then(|c| c.newer_than(current))
        .map(str::to_string);
    Status {
        line: status_line(mode, cache.as_ref().map_err(String::as_str), &probe),
        newer,
        fix: update_command(probe.install.homebrew),
    }
}

/// Starts `<exe> update --refresh-check` detached, in its own process group
/// so a Ctrl-C in the terminal does not reach it, with no stdio.
fn spawn_refresh() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["upgrade", "--refresh-check"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let _ = cmd.spawn();
}

/// The entry point `main` calls before it dispatches a command. It never
/// fails and never waits on the network.
pub fn on_start(command: &Command, args: &[OsString]) {
    use std::io::IsTerminal;
    // The cheap checks come first.
    let stderr_tty = std::io::stderr().is_terminal();
    if !applies(command, args)
        || crate::common::headless::is_headless()
        || ci_vendor_env(|name| std::env::var(name).ok())
    {
        return;
    }
    let current = env!("CARGO_PKG_VERSION");
    let home = crate::common::home_dir();
    let root = crate::common::paths::playbook_root_from(&home);
    let settings = if stderr_tty {
        Settings::read(&home)
    } else {
        // Without a terminal only an auto update acts. A development build
        // never installs itself, and the mode is read through a read-only
        // connection, so a piped run that stops here writes nothing.
        if super::auto::Install::is_dev_build(current) {
            return;
        }
        match Settings::peek_mode(&root) {
            UpdateMode::Auto => Settings::with_mode(UpdateMode::Auto),
            _ => return,
        }
    };
    if !settings.acts(stderr_tty) {
        return;
    }
    let Ok(cache) = Cache::read(&root) else {
        return;
    };
    let exe = std::env::current_exe().unwrap_or_default();
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let probe = Probe {
        current: current.to_string(),
        now: crate::common::time::now_epoch_secs(),
        stderr_tty,
        install: super::auto::Install::real(&exe, current),
    };
    let plan = plan(settings, &cache, &probe);
    execute(&root, &plan, probe.now, &spawn_refresh, &mut |line| {
        eprintln!("{line}")
    });
}

/// The body of the hidden `playbook upgrade --refresh-check`: refresh the
/// cache, then run the auto update when `update.mode` is `auto`. A failed
/// refresh stops there, so an offline machine leaves no warning.
pub fn run_refresh() {
    let home = crate::common::home_dir();
    let root = crate::common::paths::playbook_root_from(&home);
    let fetcher = super::download::HttpFetcher {
        local_http: false,
        timeout: REFRESH_TIMEOUT,
    };
    refresh_then(&root, &fetcher, &super::api_base(), &|| {
        let settings = Settings::read(&home);
        if settings.mode == UpdateMode::Auto {
            super::auto::run_detached(&root, settings);
        }
    });
}

/// Refreshes the cache and runs `next` only when the refresh worked.
pub fn refresh_then(root: &Path, fetcher: &dyn Fetcher, api_base: &str, next: &dyn Fn()) {
    if refresh(root, fetcher, api_base).is_ok() {
        next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::scratch_root as root;
    use crate::update::download::fake::FakeFetcher;
    use clap::Parser;
    use std::cell::Cell;

    const API: &str = "https://api.test/repos/x";
    const NOW: u64 = 1_800_000_000;
    const HOUR: u64 = 3600;

    const LIST: &str = r#"[
        {"tag_name":"playbook-plugin-v0.31.1","prerelease":false,"published_at":"2026-10-15T04:32:18Z"},
        {"tag_name":"telemetry-v0.9.0","prerelease":false},
        {"tag_name":"v0.28.0-rc.1","prerelease":true},
        {"tag_name":"v0.29.0","draft":true},
        {"tag_name":"v0.27.0","prerelease":false,"published_at":"2026-10-15T00:37:06Z"},
        {"tag_name":"v0.26.0","prerelease":false}
    ]"#;

    fn settings(mode: UpdateMode) -> Settings {
        Settings {
            mode,
            interval_secs: 24 * HOUR,
        }
    }

    fn notify() -> Settings {
        settings(UpdateMode::Notify)
    }

    fn probe(now: u64) -> Probe {
        Probe {
            current: "0.26.0".into(),
            now,
            stderr_tty: true,
            install: super::super::auto::Install {
                homebrew: false,
                dev_build: false,
                dir_writable: true,
                supported: true,
            },
        }
    }

    /// Runs one command start against `root` and returns the printed lines
    /// and how many refreshes it started.
    fn start(root: &Path, settings: Settings, probe: &Probe) -> (Vec<String>, u32) {
        let cache = Cache::read(root).unwrap();
        run_plan(root, &plan(settings, &cache, probe), probe.now)
    }

    fn run_plan(root: &Path, plan: &Plan, now: u64) -> (Vec<String>, u32) {
        let spawned = Cell::new(0);
        let mut printed = Vec::new();
        execute(
            root,
            plan,
            now,
            &|| spawned.set(spawned.get() + 1),
            &mut |line| printed.push(line.to_string()),
        );
        (printed, spawned.get())
    }

    fn fetched(root: &Path) {
        let mut f = FakeFetcher::default();
        f.0.insert(format!("{API}{}", resolve::RELEASES_PAGE), LIST.into());
        refresh(root, &f, API).unwrap();
    }

    fn line(version: &str) -> String {
        format!(
            "playbook {version} is available (you have 0.26.0). Run: playbook upgrade. \
             To update on its own: playbook config set --global update.mode auto"
        )
    }

    #[test]
    fn only_plain_stable_tags_count_as_the_latest_release() {
        let (version, published) = newest_plain(LIST).unwrap().unwrap();
        assert_eq!(version, "0.27.0");
        assert_eq!(published.as_deref(), Some("2026-10-15T00:37:06Z"));
        assert_eq!(newest_plain("[]").unwrap(), None);
        assert!(newest_plain("not json").is_err());
    }

    #[test]
    fn the_notice_prints_once_a_day_for_each_version() {
        // Arrange
        let root = root("check-once");
        fetched(&root);
        Cache::update(&root, |c| c.checked_at = NOW).unwrap();

        // Act
        let (first, _) = start(&root, notify(), &probe(NOW));
        let (same_day, _) = start(&root, notify(), &probe(NOW + 23 * HOUR));
        Cache::update(&root, |c| c.latest = Some("0.28.0".into())).unwrap();
        let (newer, _) = start(&root, notify(), &probe(NOW + 23 * HOUR));
        let (next_day, _) = start(&root, notify(), &probe(NOW + 23 * HOUR + DAY_SECS));

        // Assert
        assert_eq!(first, vec![line("0.27.0")]);
        assert!(same_day.is_empty(), "{same_day:?}");
        assert_eq!(newer, vec![line("0.28.0")]);
        assert_eq!(next_day, vec![line("0.28.0")]);
    }

    #[test]
    fn auto_mode_hands_a_newer_release_to_the_detached_process_once_a_day() {
        // Arrange
        let root = root("check-auto");
        fetched(&root);
        Cache::update(&root, |c| c.checked_at = NOW - HOUR).unwrap();
        let auto = settings(UpdateMode::Auto);

        // Act
        let first = start(&root, auto, &probe(NOW));
        // The detached process claims the attempt.
        Cache::update(&root, |c| c.tried = Some(stamp("0.27.0", NOW))).unwrap();
        let again = start(&root, auto, &probe(NOW + HOUR));
        let mut piped = probe(NOW + DAY_SECS);
        piped.stderr_tty = false;
        let next_day = plan(auto, &Cache::read(&root).unwrap(), &piped);

        // Assert
        assert_eq!(first, (vec![], 1));
        assert_eq!(again, (vec![], 0));
        assert_eq!(next_day.apply.as_deref(), Some("0.27.0"));
        assert_eq!(run_plan(&root, &next_day, piped.now), (vec![], 1));
    }

    #[test]
    fn the_next_terminal_run_prints_the_auto_report_once() {
        let root = root("check-report");
        let lines = vec!["playbook updated from 0.25.0 to 0.26.0".to_string()];
        Cache::update(&root, |c| {
            c.checked_at = NOW;
            c.report = lines.clone();
        })
        .unwrap();
        let auto = settings(UpdateMode::Auto);
        let mut piped = probe(NOW);
        piped.stderr_tty = false;
        assert_eq!(start(&root, auto, &piped), (vec![], 0));
        assert_eq!(start(&root, auto, &probe(NOW)), (lines, 0));
        assert_eq!(start(&root, auto, &probe(NOW)), (vec![], 0));
    }

    #[test]
    fn auto_falls_back_to_the_notice_with_the_reason() {
        // Arrange
        let brew_root = root("check-auto-brew");
        fetched(&brew_root);
        let held_root = root("check-auto-held");
        fetched(&held_root);
        Cache::update(&held_root, |c| {
            c.held = Some(Held {
                version: "0.27.0".into(),
                reason: "this release has breaking changes; run playbook upgrade to see them"
                    .into(),
                until: u64::MAX,
            })
        })
        .unwrap();
        let auto = settings(UpdateMode::Auto);
        let mut brew = probe(NOW);
        brew.install.homebrew = true;
        let brew_plan = plan(auto, &Cache::read(&brew_root).unwrap(), &brew);
        let held_plan = plan(auto, &Cache::read(&held_root).unwrap(), &probe(NOW));

        // Act
        let (brew_lines, _) = run_plan(&brew_root, &brew_plan, NOW);
        let (held_lines, _) = run_plan(&held_root, &held_plan, NOW);

        // Assert
        assert_eq!(
            brew_lines,
            vec![
                "playbook 0.27.0 is available (you have 0.26.0). Run: brew upgrade playbook \
                 (auto update skipped: Homebrew manages this install)"
            ]
        );
        assert!(
            held_lines[0].ends_with(
                "(auto update skipped: this release has breaking changes; run playbook upgrade to see them)"
            ),
            "{held_lines:?}"
        );
        assert_eq!((brew_plan.apply, held_plan.apply), (None, None));
    }

    #[test]
    fn the_auto_hint_shows_only_where_auto_can_run() {
        let root = root("check-hint");
        fetched(&root);
        let mut dev = probe(NOW);
        dev.install.dev_build = true;
        let (printed, _) = start(&root, notify(), &dev);
        assert_eq!(
            printed,
            vec!["playbook 0.27.0 is available (you have 0.26.0). Run: playbook upgrade"]
        );
    }

    #[test]
    fn a_fresh_cache_starts_no_refresh_until_the_interval_passes() {
        let root = root("check-interval");
        let (_, first) = start(&root, notify(), &probe(NOW));
        let (_, soon) = start(&root, notify(), &probe(NOW + 23 * HOUR));
        let (_, later) = start(&root, notify(), &probe(NOW + 24 * HOUR));
        assert_eq!((first, soon, later), (1, 0, 1));
        assert_eq!(Cache::read(&root).unwrap().checked_at, NOW + 24 * HOUR);
    }

    #[test]
    fn two_commands_that_start_together_claim_each_action_once() {
        // Arrange
        let root = root("check-race");
        fetched(&root);
        let cache = Cache::read(&root).unwrap();
        let first = plan(notify(), &cache, &probe(NOW));
        let second = plan(notify(), &cache, &probe(NOW));

        // Act
        let one = run_plan(&root, &first, NOW);
        let two = run_plan(&root, &second, NOW);

        // Assert
        assert_eq!(one, (vec![line("0.27.0")], 1));
        assert_eq!(two, (vec![], 0));
    }

    #[test]
    fn a_store_that_cannot_be_written_spawns_and_prints_nothing() {
        let file = root("check-broken").join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        let plan = Plan {
            refresh: true,
            seen_checked_at: 0,
            notice: Some(Notice {
                version: "9.0.0".into(),
                line: "x".into(),
            }),
            ..Plan::default()
        };
        assert_eq!(run_plan(&file, &plan, NOW), (vec![], 0));
    }

    #[test]
    fn a_clock_moved_back_starts_a_refresh() {
        let root = root("check-skew");
        Cache::update(&root, |c| c.checked_at = NOW + DAY_SECS).unwrap();
        assert!(plan(notify(), &Cache::read(&root).unwrap(), &probe(NOW)).refresh);
    }

    #[test]
    fn an_offline_check_is_silent_and_waits_for_the_next_interval() {
        // Arrange
        let root = root("check-offline");
        let offline = FakeFetcher::default();

        // Act
        let (printed, spawned) = start(&root, notify(), &probe(NOW));
        let failed = refresh(&root, &offline, API);
        let (again, respawned) = start(&root, notify(), &probe(NOW + HOUR));

        // Assert
        assert!(failed.is_err());
        assert!(printed.is_empty() && again.is_empty());
        assert_eq!((spawned, respawned), (1, 0));
        assert_eq!(Cache::read(&root).unwrap().latest, None);
    }

    #[test]
    fn a_homebrew_install_is_told_to_use_brew() {
        let root = root("check-brew");
        fetched(&root);
        let mut p = probe(NOW);
        p.install.homebrew = true;
        let (printed, _) = start(&root, notify(), &p);
        assert_eq!(
            printed,
            vec!["playbook 0.27.0 is available (you have 0.26.0). Run: brew upgrade playbook"]
        );
    }

    #[test]
    fn off_mode_and_a_non_terminal_stderr_do_nothing() {
        let root = root("check-off");
        fetched(&root);
        let mut piped = probe(NOW);
        piped.stderr_tty = false;
        assert_eq!(
            start(&root, settings(UpdateMode::Off), &probe(NOW)),
            (vec![], 0)
        );
        assert_eq!(start(&root, notify(), &piped), (vec![], 0));
        assert_eq!(Cache::read(&root).unwrap().notified, None);
    }

    #[test]
    fn an_up_to_date_binary_sees_no_notice() {
        let root = root("check-current");
        fetched(&root);
        let mut p = probe(NOW);
        p.current = "0.27.0".into();
        assert_eq!(start(&root, notify(), &p).0, Vec::<String>::new());
    }

    fn applies_to(parts: &[&str]) -> bool {
        let args: Vec<OsString> = parts.iter().map(OsString::from).collect();
        let cli = crate::Cli::try_parse_from(&args).unwrap();
        applies(&cli.command, &args)
    }

    #[test]
    fn hooks_statusline_and_update_never_run_the_check() {
        for parts in [
            &["playbook", "hook", "session-init"][..],
            &["playbook", "statusline"],
            &["playbook", "mode", "status", "--json"],
            &["playbook", "update", "--check"],
            &["playbook", "upgrade"],
            &["playbook", "version"],
            &["playbook", "usage", "warm"],
            &["playbook", "usage", "collect"],
        ] {
            assert!(!applies_to(parts), "{parts:?}");
        }
        assert!(applies_to(&["playbook", "doctor"]));
        assert!(applies_to(&["playbook", "mode", "status"]));
    }

    #[test]
    fn ci_vendor_variables_turn_the_check_off() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert!(ci_vendor_env(env(&[("GITHUB_ACTIONS", "true")])));
        assert!(ci_vendor_env(env(&[("JENKINS_URL", "https://ci")])));
        assert!(!ci_vendor_env(env(&[("GITLAB_CI", "false")])));
        assert!(!ci_vendor_env(env(&[("BUILDKITE", "")])));
        assert!(!ci_vendor_env(env(&[])));
    }

    #[test]
    fn mode_values_parse_with_notify_as_the_fallback() {
        assert_eq!(UpdateMode::parse("off"), UpdateMode::Off);
        assert_eq!(UpdateMode::parse("auto"), UpdateMode::Auto);
        assert_eq!(UpdateMode::parse("notify"), UpdateMode::Notify);
        assert_eq!(UpdateMode::parse("bogus"), UpdateMode::Notify);
    }

    #[test]
    fn the_interval_is_built_in_and_the_mode_comes_from_config() {
        // Arrange
        let home = root("check-settings");
        let before = Settings::read(&home);
        let root = crate::common::paths::playbook_root_from(&home);
        let global = crate::config::write::Tier::Global;
        let put = |key: &str, value: serde_json::Value| {
            crate::config::store::put(&root, global, None, key, &value).unwrap()
        };

        // Act
        put("update.checkIntervalHours", 6.into());
        put("update.mode", "off".into());
        let set = Settings::read(&home);

        // Assert
        assert_eq!(before, notify());
        assert_eq!(
            set,
            Settings {
                mode: UpdateMode::Off,
                interval_secs: INTERVAL_HOURS * HOUR
            }
        );
    }

    #[test]
    fn an_offline_refresh_stops_before_the_auto_update() {
        let root = root("check-offline-auto");
        let ran = Cell::new(false);
        refresh_then(&root, &FakeFetcher::default(), API, &|| ran.set(true));
        assert!(!ran.get());
        let mut f = FakeFetcher::default();
        f.0.insert(format!("{API}{}", resolve::RELEASES_PAGE), LIST.into());
        refresh_then(&root, &f, API, &|| ran.set(true));
        assert!(ran.get());
    }

    #[test]
    fn an_auto_update_that_never_claims_its_attempt_falls_back_to_the_notice() {
        // Arrange
        let root = root("check-respawn");
        fetched(&root);
        Cache::update(&root, |c| c.checked_at = NOW - APPLY_GRACE_SECS).unwrap();
        let auto = settings(UpdateMode::Auto);

        // Act: the detached process exits without claiming `tried`.
        let first = start(&root, auto, &probe(NOW));
        let soon = start(&root, auto, &probe(NOW + 60));
        let later = start(&root, auto, &probe(NOW + 60 + APPLY_GRACE_SECS));

        // Assert
        assert_eq!(first, (vec![], 1));
        assert_eq!(soon, (vec![], 0), "the attempt may still be running");
        assert_eq!(
            later,
            (
                vec![
                    "playbook 0.27.0 is available (you have 0.26.0). Run: playbook upgrade".into()
                ],
                0
            )
        );
    }

    #[test]
    fn the_status_line_names_the_mode_the_last_check_and_a_hold() {
        let cache = Cache {
            checked_at: 1_792_024_626,
            latest: Some("0.27.0".into()),
            published_at: Some("2026-10-15T00:37:06Z".into()),
            ..Cache::default()
        };
        let mut brew = probe(NOW);
        brew.install.homebrew = true;
        assert_eq!(
            status_line(UpdateMode::Auto, Ok(&cache), &brew),
            "update.mode is auto; last background check started 2026-10-15T00:37:06Z, \
             latest release found 0.27.0, auto update held: Homebrew manages this install"
        );
        assert_eq!(
            status_line(UpdateMode::Notify, Ok(&cache), &brew),
            "update.mode is notify; last background check started 2026-10-15T00:37:06Z, \
             latest release found 0.27.0"
        );
        assert_eq!(
            status_line(UpdateMode::Notify, Ok(&Cache::default()), &probe(NOW)),
            "update.mode is notify; no background release check has run yet"
        );
        assert_eq!(
            status_line(UpdateMode::Auto, Err("disk I/O error"), &probe(NOW)),
            "update.mode is auto; the release check cache could not be read: disk I/O error"
        );
        assert_eq!(
            status_line(UpdateMode::Off, Ok(&cache), &probe(NOW)),
            "update.mode is off, so no background release check runs"
        );
    }

    #[test]
    fn the_read_only_cache_read_creates_nothing() {
        let root = root("check-read-only");
        assert_eq!(Cache::read_only(&root), Ok(Cache::default()));
        assert!(!crate::config::store::db_path(&root).exists());
        fetched(&root);
        assert_eq!(
            Cache::read_only(&root).unwrap().latest.as_deref(),
            Some("0.27.0")
        );
    }
}
