// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Updates the installed playbook plugin through the `claude` CLI after
//! `playbook upgrade` and `playbook migrate` (ADR 0034, 2026-10-19
//! amendment). It runs `claude plugin update <id>` for the playbook plugin
//! only, never for another plugin. A missing CLI, a failed update, a plugin
//! that is off in Claude Code or `upgrade.plugin` set to off never fails
//! the command: the result names the command to run by hand.
//!
//! [`sync`] adds an install of a missing plugin for a caller that allows it,
//! guarded by [`setup_blocked`]. Whichever path ran, a plugin on an older
//! MAJOR.MINOR line than the binary comes back as [`Outcome::Behind`].

use crate::common::plugin_state::{known_marketplaces, MARKETPLACE, PLUGIN_KEY};
use crate::common::proc::{try_run_with_timeout, RunFailure};
use crate::plugin_root::minor_line;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

/// The config key that turns the plugin update off.
pub const CONFIG_KEY: &str = "upgrade.plugin";

/// Set by `playbook upgrade` for the new binary's `migrate`, which then
/// leaves the plugin to the update itself, so it runs once.
pub const SKIP_ENV: &str = "PLAYBOOK_SKIP_PLUGIN_UPDATE";

/// The value of [`SKIP_ENV`] that turns the update off.
const SKIP_VALUE: &str = "1";

/// The id prefix of the playbook plugin in `installed_plugins.json`.
const ID_PREFIX: &str = "playbook@";

/// The most characters of `claude`'s last error line shown to the user.
const WHY_MAX_CHARS: usize = 200;

/// How long `claude plugin update` may take.
const TIMEOUT: Duration = Duration::from_secs(120);

/// How long `claude plugin marketplace update` may take.
const MARKETPLACE_TIMEOUT: Duration = Duration::from_secs(60);

/// The repository `claude plugin marketplace add` reads the marketplace from.
const MARKETPLACE_SOURCE: &str = "amplifiedengineer/marketplace";

/// What a plugin update did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `claude plugin update` ran and the installed version moved.
    Updated {
        id: String,
        from: Option<String>,
        to: Option<String>,
    },
    /// `claude plugin update` ran and the version stayed.
    Current { id: String, version: Option<String> },
    /// Nothing ran, for this reason.
    Skipped(String),
    /// An install caller was held back by [`setup_blocked`], for this reason.
    LeftAlone(String),
    /// The playbook plugin is not installed, so there is nothing to say.
    NotInstalled,
    /// `claude` is not on PATH. `install` is set when the plugin was to be
    /// installed, not updated.
    NoCli { id: String, install: bool },
    /// `claude plugin update` failed or timed out, or with `install` set, the
    /// marketplace add or the install did.
    Failed {
        id: String,
        why: String,
        install: bool,
    },
    /// `claude plugin install` ran and the plugin is now installed.
    Installed { id: String, version: Option<String> },
    /// `playbook plugin install` found the plugin installed already and ran
    /// nothing.
    Present { id: String, version: Option<String> },
    /// The update ran and the installed plugin is still older than the
    /// binary. With `marketplace` unset, the marketplace was refreshed and
    /// has not published the binary's version yet. With it set, the refresh
    /// failed for that reason, so the marketplace may hold a newer plugin.
    Behind {
        id: String,
        version: Option<String>,
        running: String,
        marketplace: Option<String>,
    },
}

impl Outcome {
    /// One line for the user, with the command to run when it did not run.
    /// `None` when the plugin is not installed.
    pub fn line(&self) -> Option<String> {
        Some(match self {
            Outcome::NotInstalled => return None,
            Outcome::Updated { id, from, to } => format!(
                "plugin: updated {id}{}{}. Restart Claude Code to load it",
                from.as_deref()
                    .map(|v| format!(" from {v}"))
                    .unwrap_or_default(),
                to.as_deref()
                    .map(|v| format!(" to {v}"))
                    .unwrap_or_default(),
            ),
            Outcome::Current { id, version } => format!(
                "plugin: {id} is up to date{}",
                version
                    .as_deref()
                    .map(|v| format!(" ({v})"))
                    .unwrap_or_default()
            ),
            Outcome::Skipped(why) => format!("plugin: not updated, {why}"),
            Outcome::LeftAlone(why) => format!("plugin: left alone, {why}"),
            Outcome::Installed { id, version } => format!(
                "plugin: installed {id}{}. Restart Claude Code to load it",
                version
                    .as_deref()
                    .map(|v| format!(" {v}"))
                    .unwrap_or_default(),
            ),
            Outcome::Present { id, version } => format!(
                "plugin: {id} is already installed{}",
                version
                    .as_deref()
                    .map(|v| format!(" ({v})"))
                    .unwrap_or_default(),
            ),
            Outcome::NoCli { id, install: true } => format!(
                "plugin: the claude command is not on PATH, so the plugin was not installed; run: {}",
                install_commands(id)
            ),
            Outcome::NoCli { id, install: false } => format!(
                "plugin: the claude command is not on PATH, so the plugin was not updated; run: {}",
                command(id)
            ),
            Outcome::Failed {
                id,
                why,
                install: true,
            } => format!(
                "plugin: the install failed ({why}); run: {}",
                install_commands(id)
            ),
            Outcome::Failed {
                id,
                why,
                install: false,
            } => format!("plugin: the update failed ({why}); run: {}", command(id)),
            Outcome::Behind {
                id,
                version,
                running,
                marketplace,
            } => {
                let installed = version.as_deref().unwrap_or(UNKNOWN_VERSION);
                match marketplace {
                    None => format!(
                        "plugin: {id} {installed} is older than this binary ({running}), and the marketplace has not published a newer plugin yet; playbook tries again after 24 hours"
                    ),
                    Some(why) => format!(
                        "plugin: {id} {installed} is older than this binary ({running}); the marketplace refresh failed ({why}); run: {} && {}",
                        marketplace_command(id),
                        command(id)
                    ),
                }
            }
        })
    }
}

impl Outcome {
    /// Whether the update ran against a refreshed marketplace that has not
    /// published the binary's plugin version, so `claude plugin update`
    /// would change nothing.
    pub fn awaits_release(&self) -> bool {
        matches!(
            self,
            Outcome::Behind {
                marketplace: None,
                ..
            }
        )
    }
}

/// The start of every [`command`], for code that tells a plugin fix apart.
pub const COMMAND_PREFIX: &str = "claude plugin update ";

/// The command that updates plugin `id`.
pub fn command(id: &str) -> String {
    format!("{COMMAND_PREFIX}{id}")
}

/// The commands that install plugin `id` from scratch, as the README gives
/// them.
fn install_commands(id: &str) -> String {
    format!("claude plugin marketplace add {MARKETPLACE_SOURCE} && claude plugin install {id}")
}

/// Shown where the installed plugin version cannot be read.
const UNKNOWN_VERSION: &str = "unknown version";

/// The marketplace name in plugin id `id`: the text after the last `@`.
fn marketplace_of(id: &str) -> Option<&str> {
    id.rsplit_once('@')
        .map(|(_, name)| name)
        .filter(|name| !name.is_empty())
}

/// The command that refreshes the marketplace clone of plugin `id`.
fn marketplace_command(id: &str) -> String {
    format!(
        "claude plugin marketplace update {}",
        marketplace_of(id).unwrap_or_default()
    )
}

/// Spawn attempts when the executable is briefly busy, and the pause between.
const BUSY_TRIES: u32 = 5;
const BUSY_PAUSE: Duration = Duration::from_millis(20);

/// Runs `cmd`, trying again while the OS reports the executable busy, which
/// happens for a moment after a script was written and closed.
fn run_retrying_busy(
    cmd: &mut std::process::Command,
    timeout: Duration,
) -> Result<Output, RunFailure> {
    let mut tries = 1;
    loop {
        match try_run_with_timeout(cmd, timeout) {
            Err(RunFailure::Spawn(e))
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < BUSY_TRIES =>
            {
                tries += 1;
                std::thread::sleep(BUSY_PAUSE);
            }
            result => return result,
        }
    }
}

/// Test seam that shortens every `claude` call's timeout, in seconds. Read
/// only in debug builds, so a release binary cannot be fooled by it.
#[cfg(debug_assertions)]
const TIMEOUT_SECS_ENV: &str = "PLAYBOOK_TEST_PLUGIN_TIMEOUT_SECS";

#[cfg(debug_assertions)]
fn test_timeout() -> Option<Duration> {
    let secs = std::env::var(TIMEOUT_SECS_ENV).ok()?.parse().ok()?;
    Some(Duration::from_secs(secs))
}

/// Runs `claude` with `args` under the probe's home, the skip variable, no
/// stdin and `timeout`. `Err` carries why it failed.
fn run_claude(
    probe: &Probe,
    claude: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    let timeout = test_timeout().unwrap_or(timeout);
    let mut cmd = std::process::Command::new(claude);
    cmd.args(args)
        .env("HOME", &probe.home)
        .env(SKIP_ENV, SKIP_VALUE)
        .stdin(std::process::Stdio::null());
    let out = match run_retrying_busy(&mut cmd, timeout) {
        Ok(out) => out,
        Err(RunFailure::Spawn(_)) => return Err("it could not be started".to_string()),
        Err(RunFailure::Incomplete) => {
            return Err(format!("it did not finish within {} s", timeout.as_secs()));
        }
    };
    if out.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&out.stderr);
    let last = text
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(|l| crate::ui::render::sanitize(&l.chars().take(WHY_MAX_CHARS).collect::<String>()))
        .unwrap_or_default();
    Err(if last.is_empty() {
        format!("exit {}", out.status.code().unwrap_or(-1))
    } else {
        last
    })
}

/// What `upgrade.plugin` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Ask,
    Off,
}

const MODE_AUTO: &str = "auto";
const MODE_ASK: &str = "ask";
const MODE_OFF: &str = "off";

/// The mode a stored value names, or `None` when it names none. The old
/// boolean values still count, so a store written before the key took words
/// keeps its meaning.
fn parse_mode(value: &serde_json::Value) -> Option<Mode> {
    match value {
        serde_json::Value::String(s) if s == MODE_AUTO => Some(Mode::Auto),
        serde_json::Value::String(s) if s == MODE_ASK => Some(Mode::Ask),
        serde_json::Value::String(s) if s == MODE_OFF => Some(Mode::Off),
        serde_json::Value::Bool(true) => Some(Mode::Auto),
        serde_json::Value::Bool(false) => Some(Mode::Off),
        _ => None,
    }
}

/// The mode for a stored value; a missing or unknown one is `Auto`, the
/// default.
fn mode_from(value: Option<&serde_json::Value>) -> Mode {
    value.and_then(parse_mode).unwrap_or(Mode::Auto)
}

/// The `upgrade.plugin` mode for `home`. The raw stored value is read, not
/// the enum-checked one, because a `true` or `false` from before the key
/// took words is still valid here.
pub fn mode(home: &Path) -> Mode {
    let Ok((value, _)) = crate::config::resolve(CONFIG_KEY, home, None) else {
        return Mode::Auto;
    };
    if parse_mode(&value).is_none() {
        eprintln!(
            "warning: ignoring invalid value {value} for config key {CONFIG_KEY}, valid values are: {MODE_AUTO}, {MODE_ASK}, {MODE_OFF}"
        );
    }
    mode_from(Some(&value))
}

/// The id Claude Code lists for the playbook plugin in
/// `installed_plugins.json`: the first of `plugin_keys()` that is listed
/// (`playbook@amplifiedengineer`, then the old names), else the one `playbook@<marketplace>` entry. `None` when the plugin is
/// not installed, or more than one other marketplace lists it.
pub fn plugin_id(claude_home: &Path) -> Option<String> {
    use crate::common::plugin_state::{installed_entry_at, plugin_keys};
    for key in plugin_keys() {
        if installed_entry_at(claude_home, &key).is_some() {
            return Some(key);
        }
    }
    let text = std::fs::read_to_string(claude_home.join("plugins/installed_plugins.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let map = value.get("plugins").unwrap_or(&value).as_object()?;
    let mut ids = map
        .keys()
        .filter(|k| k.starts_with(ID_PREFIX) && !k.chars().any(char::is_control));
    let id = ids.next()?.clone();
    ids.next().is_none().then_some(id)
}

/// Whether `settings.json` turns plugin `id` off. Unlike the hook pause,
/// this follows the id Claude Code lists, so a fork id counts too.
fn is_off_in_claude_code(claude_home: &Path, id: &str) -> bool {
    std::fs::read_to_string(claude_home.join("settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.pointer("/enabledPlugins")?.get(id).cloned())
        == Some(serde_json::Value::Bool(false))
}

/// The folder Claude Code loads for plugin `id` here: the entry for this
/// project first, then the user entry, the same pick `pending` checks.
fn installed_path(claude_home: &Path, id: &str) -> Option<PathBuf> {
    let entry = crate::common::plugin_state::installed_entry_at(claude_home, id);
    let cwd = std::env::current_dir().unwrap_or_default();
    crate::plugin_root::ranked_install_paths(entry, &cwd)
        .into_iter()
        .next()
}

/// The version Claude Code lists for plugin `id`, from its `installPath`.
fn installed_version(claude_home: &Path, id: &str) -> Option<String> {
    crate::plugin_root::plugin_version(&installed_path(claude_home, id)?)
        .filter(|v| crate::plugin_root::is_plain_version(v))
}

/// Copies the new plugin's system prompt over the placed one, so a user who
/// opted in gets new prompt text without running `playbook setup`. A copy the
/// user edited, or a prompt that was never placed, stays as it is.
fn refresh_system_prompt(home: &Path, plugin_dir: &Path) {
    use crate::init::system_prompt::{place_system_prompt, Placement};
    if let Ok(Placement::Placed(_)) = place_system_prompt(plugin_dir, home, false) {
        crate::init::migrate::record_system_prompt(home);
    }
}

/// What the update reads, passed in so tests use a scratch home and a fake
/// `claude`.
#[derive(Debug, Clone)]
pub struct Probe {
    pub home: PathBuf,
    /// The `claude` executable, or `None` when it is not on PATH.
    pub claude: Option<PathBuf>,
}

impl Probe {
    /// The probe for this process: `claude` from PATH.
    pub fn for_home(home: &Path) -> Probe {
        let path = std::env::var("PATH").unwrap_or_default();
        Probe {
            home: home.to_path_buf(),
            claude: crate::statusline::gh::find_in_path("claude", &path),
        }
    }
}

/// What [`sync`] may do beyond updating a plugin that is already installed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncOpts {
    /// Install the plugin when none is installed.
    pub allow_install: bool,
    /// Ignore [`setup_blocked`] and `upgrade.plugin` set to off, for a caller
    /// the user asked to touch the plugin.
    pub force: bool,
    /// Print one stderr line before each `claude` call, so a slow call is
    /// not silent. Only `playbook setup` sets it; the automatic path is quiet.
    pub progress: bool,
}

/// Prints `line` to stderr when the caller asked for progress.
fn say(opts: SyncOpts, line: &str) {
    if opts.progress {
        eprintln!("{line}");
    }
}

/// Why a command that sets the machine up must leave the plugin alone, or
/// `None` when it may touch it. The one owner of this guard, so a caller
/// cannot drift from [`sync`].
pub fn setup_blocked(home: &Path) -> Option<&'static str> {
    blocked_by(home, &|name| std::env::var(name).ok())
}

fn blocked_by(home: &Path, env: &dyn Fn(&str) -> Option<String>) -> Option<&'static str> {
    if mode(home) == Mode::Off {
        return Some("upgrade.plugin is off");
    }
    if env(SKIP_ENV).is_some_and(|v| v == SKIP_VALUE) {
        return Some("PLAYBOOK_SKIP_PLUGIN_UPDATE is set");
    }
    // A vendor variable, or the `CI` and `PLAYBOOK_HEADLESS` switch every hook
    // reads: a plain `CI=true` is not a vendor variable.
    let headless = crate::common::headless::headless_from(
        env("PLAYBOOK_HEADLESS").as_deref(),
        env("CI").as_deref(),
    );
    (crate::update::check::ci_vendor_env(env) || headless)
        .then_some("a CI environment was detected")
}

/// Whether plugin version `installed` is on an older MAJOR.MINOR line than
/// binary version `running`. A newer patch is not behind, because a plugin
/// ships only with a minor release and the binary's patch releases carry no
/// new plugin. A version that does not parse is not behind.
pub fn behind(installed: &str, running: &str) -> bool {
    matches!(
        (minor_line(installed), minor_line(running)),
        (Some(have), Some(want)) if have < want
    )
}

/// [`behind`] for a version that may be unreadable, which counts as behind
/// because nothing shows the plugin caught up.
fn lags(version: Option<&str>, running: &str) -> bool {
    version.is_none_or(|v| behind(v, running))
}

/// Updates the playbook plugin, unless `upgrade.plugin` is off, the plugin
/// is not installed, or it is off in Claude Code.
pub fn run(probe: &Probe) -> Outcome {
    sync(probe, SyncOpts::default())
}

/// [`run`], and with `opts.allow_install` also installs a missing plugin. An
/// install is held back by [`setup_blocked`] unless `opts.force`.
pub fn sync(probe: &Probe, opts: SyncOpts) -> Outcome {
    sync_with_env(probe, opts, &|name| std::env::var(name).ok())
}

fn sync_with_env(probe: &Probe, opts: SyncOpts, env: &dyn Fn(&str) -> Option<String>) -> Outcome {
    sync_at(probe, opts, env!("CARGO_PKG_VERSION"), env)
}

/// [`sync`] for a binary at version `running`: an install, an update or an
/// unchanged plugin that is older than the binary comes back as
/// [`Outcome::Behind`].
fn sync_at(
    probe: &Probe,
    opts: SyncOpts,
    running: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Outcome {
    if opts.allow_install && !opts.force {
        if let Some(why) = blocked_by(&probe.home, env) {
            return Outcome::LeftAlone(why.to_string());
        }
    }
    let (outcome, marketplace) = run_with_marketplace(probe, opts);
    judge(outcome, marketplace, running)
}

/// Turns an install, an update or an unchanged plugin that is older than the
/// binary at version `running` into [`Outcome::Behind`]; any other outcome
/// comes back as it is.
fn judge(outcome: Outcome, marketplace: Option<String>, running: &str) -> Outcome {
    // An unreadable version after an update counts as behind; after an
    // install it does not, since nothing yet says the install lags.
    let (id, v, unreadable_lags) = match &outcome {
        Outcome::Current { id, version }
        | Outcome::Updated {
            id, to: version, ..
        } => (id, version, true),
        Outcome::Installed { id, version } => (id, version, false),
        _ => return outcome,
    };
    if !v.as_deref().map_or(unreadable_lags, |v| behind(v, running)) {
        return outcome;
    }
    Outcome::Behind {
        id: id.clone(),
        version: v.clone(),
        running: running.to_string(),
        marketplace,
    }
}

/// Installs the plugin. A marketplace known only under an older name is used
/// as it is, so the old name stays fixed; with none known, the marketplace is
/// added first.
fn install(probe: &Probe, opts: SyncOpts) -> Outcome {
    let known = known_marketplaces(&probe.home);
    let marketplace = known.first().copied().unwrap_or(MARKETPLACE);
    let id = format!("playbook@{marketplace}");
    let Some(claude) = &probe.claude else {
        return Outcome::NoCli {
            id: PLUGIN_KEY.to_string(),
            install: true,
        };
    };
    let failed = |id: String, why: String| Outcome::Failed {
        id,
        why,
        install: true,
    };
    if known.is_empty() {
        let add = ["plugin", "marketplace", "add", MARKETPLACE_SOURCE, "--json"];
        say(
            opts,
            &format!("plugin: adding the {MARKETPLACE} marketplace"),
        );
        if let Err(why) = run_claude(probe, claude, &add, TIMEOUT) {
            return failed(id, why);
        }
    }
    say(opts, &format!("plugin: installing {id}"));
    if let Err(why) = run_claude(
        probe,
        claude,
        &["plugin", "install", &id, "--json"],
        TIMEOUT,
    ) {
        return failed(id, why);
    }
    let version = installed_version(&probe.home.join(".claude"), &id);
    Outcome::Installed { id, version }
}

/// What [`mark_auto_update`] did to the marketplace entry in Claude Code's
/// user settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoUpdate {
    /// `autoUpdate: true` was written beside `source`.
    Set,
    AlreadyOn,
    /// The user set it to `false`, which is never overridden.
    UserOff,
    /// The marketplace has no usable entry in the user settings, so there is
    /// nothing to mark. It may come from a project or managed scope.
    NotRegistered,
    /// `autoUpdate` holds something other than true or false.
    NotBoolean,
    Failed(String),
}

impl AutoUpdate {
    /// A stable word for `--json`.
    pub fn status(&self) -> &'static str {
        match self {
            AutoUpdate::Set => "set",
            AutoUpdate::AlreadyOn => "already-on",
            AutoUpdate::UserOff => "user-off",
            AutoUpdate::NotRegistered => "not-registered",
            AutoUpdate::NotBoolean => "not-boolean",
            AutoUpdate::Failed(_) => "failed",
        }
    }

    /// One line for the setup report. `None` when there is nothing to say.
    pub fn line(&self) -> Option<String> {
        let name = MARKETPLACE;
        Some(match self {
            AutoUpdate::Set => format!("plugin: auto-update on for {name}"),
            AutoUpdate::AlreadyOn => format!("plugin: auto-update already on for {name}"),
            AutoUpdate::UserOff => {
                format!("plugin: auto-update is off for {name} by your choice")
            }
            AutoUpdate::NotBoolean => format!(
                "plugin: the auto-update setting for {name} is not true or false, left as it is"
            ),
            AutoUpdate::Failed(why) => {
                format!("plugin: could not turn on auto-update for {name} ({why})")
            }
            AutoUpdate::NotRegistered => return None,
        })
    }
}

/// Turns on auto-update for the `amplifiedengineer` marketplace by writing
/// `autoUpdate: true` beside `source` in `~/.claude/settings.json`. Only that
/// entry changes; Claude Code's own marketplace list is never edited. The
/// file is read right before the write, but a running Claude Code can still
/// save between the two, so a concurrent edit may be lost.
pub fn mark_auto_update(home: &Path, marketplace: &str) -> AutoUpdate {
    if marketplace != MARKETPLACE {
        return AutoUpdate::NotRegistered;
    }
    let path = home.join(".claude/settings.json");
    if !path.is_file() {
        return AutoUpdate::NotRegistered;
    }
    let Ok(mut root) = crate::init::telemetry::load(&path) else {
        return AutoUpdate::Failed("settings.json is not a readable JSON object".to_string());
    };
    let Some(entry) = root
        .get_mut("extraKnownMarketplaces")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|marketplaces| marketplaces.get_mut(MARKETPLACE))
        .and_then(serde_json::Value::as_object_mut)
        .filter(|entry| entry.contains_key("source"))
    else {
        return AutoUpdate::NotRegistered;
    };
    match entry.get("autoUpdate") {
        Some(serde_json::Value::Bool(true)) => return AutoUpdate::AlreadyOn,
        Some(serde_json::Value::Bool(false)) => return AutoUpdate::UserOff,
        Some(_) => return AutoUpdate::NotBoolean,
        None => {}
    }
    entry.insert("autoUpdate".to_string(), serde_json::Value::Bool(true));
    let was_sorted = root.keys().is_sorted();
    // A symlinked settings.json (stow, chezmoi) stays a symlink.
    let target = std::fs::canonicalize(&path).unwrap_or(path);
    match crate::init::telemetry::write_keeping_order(&target, root, was_sorted, now()) {
        Ok(()) => AutoUpdate::Set,
        Err(why) => AutoUpdate::Failed(why),
    }
}

/// Marks the marketplace auto-update after setup's plugin step, when the plugin
/// is installed or current under the `playbook@amplifiedengineer` id. Other
/// ids and outcomes that left no plugin return `None`. Only setup calls this,
/// since only setup has the user's consent to edit their settings.
pub fn auto_update_after(outcome: &Outcome, home: &Path) -> Option<AutoUpdate> {
    match outcome {
        Outcome::Installed { id, .. }
        | Outcome::Present { id, .. }
        | Outcome::Updated { id, .. }
        | Outcome::Current { id, .. }
        | Outcome::Behind { id, .. }
            if id == PLUGIN_KEY =>
        {
            Some(mark_auto_update(home, MARKETPLACE))
        }
        _ => None,
    }
}

/// What [`ensure`] does for a plugin that is already installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted {
    /// Install a missing plugin; leave an installed one as it is
    /// (`playbook plugin install`).
    InstallOnly,
    /// Install a missing plugin and update an installed one (`playbook
    /// setup` and `playbook upgrade`).
    InstallOrUpdate,
}

/// What happened to the marketplace on the way to the plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketplaceStep {
    /// Claude Code already lists the `amplifiedengineer` marketplace.
    Present,
    /// Claude Code lists only this older name, which stays as it is.
    OldName(&'static str),
    /// `claude plugin marketplace add` ran.
    Added,
}

impl MarketplaceStep {
    /// A stable word for `--json`.
    pub fn status(&self) -> &'static str {
        match self {
            MarketplaceStep::Present => "present",
            MarketplaceStep::OldName(_) => "old-name",
            MarketplaceStep::Added => "added",
        }
    }

    /// One line for the report.
    pub fn line(&self) -> String {
        match self {
            MarketplaceStep::Present => {
                format!("plugin: the {MARKETPLACE} marketplace is already registered")
            }
            MarketplaceStep::OldName(name) => format!(
                "plugin: the marketplace is registered under its old name {name}; left alone"
            ),
            MarketplaceStep::Added => {
                format!("plugin: added the {MARKETPLACE} marketplace ({MARKETPLACE_SOURCE})")
            }
        }
    }
}

/// What [`ensure`] did, one field per step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ensured {
    /// `None` when no step touched the marketplace, such as a plugin that was
    /// already installed or a failed install.
    pub marketplace: Option<MarketplaceStep>,
    pub outcome: Outcome,
    /// `None` when the outcome left no `playbook@amplifiedengineer` plugin.
    pub auto: Option<AutoUpdate>,
}

/// The one plugin step `playbook setup`, `playbook plugin install` and
/// `playbook upgrade` share: registers the marketplace and installs the
/// plugin when it is missing (with `wanted` set to
/// [`Wanted::InstallOrUpdate`], updates it otherwise), then turns on
/// auto-update for the marketplace. Claude Code updates a plugin with its
/// marketplace, so that one flag covers the plugin; there is no per-plugin
/// key. `running` is the binary version that judges a plugin as behind.
pub fn ensure(probe: &Probe, opts: SyncOpts, wanted: Wanted, running: &str) -> Ensured {
    let known = known_marketplaces(&probe.home);
    let installed = plugin_id(&probe.home.join(".claude"));
    let outcome = match (wanted, installed, &probe.claude) {
        (Wanted::InstallOnly, _, None) => Outcome::NoCli {
            id: PLUGIN_KEY.to_string(),
            install: true,
        },
        (Wanted::InstallOnly, Some(id), Some(_)) => Outcome::Present {
            version: installed_version(&probe.home.join(".claude"), &id),
            id,
        },
        _ => sync_at(probe, opts, running, &|name| std::env::var(name).ok()),
    };
    let marketplace = match (&outcome, known.first()) {
        (Outcome::Installed { .. }, None) => Some(MarketplaceStep::Added),
        (_, Some(&MARKETPLACE)) => Some(MarketplaceStep::Present),
        (_, Some(&name)) => Some(MarketplaceStep::OldName(name)),
        _ => None,
    };
    let auto = auto_update_after(&outcome, &probe.home);
    Ensured {
        marketplace,
        outcome,
        auto,
    }
}

/// The reminder that one flag covers marketplace and plugin.
pub const AUTO_UPDATE_NOTE: &str = "plugin: Claude Code updates a plugin with its marketplace, so auto-update for the marketplace covers the plugin";

impl Ensured {
    /// Whether the plugin is now installed.
    fn installed(&self) -> bool {
        matches!(
            self.outcome,
            Outcome::Installed { .. }
                | Outcome::Present { .. }
                | Outcome::Updated { .. }
                | Outcome::Current { .. }
                | Outcome::Behind { .. }
        )
    }

    fn auto_unset_line(&self) -> String {
        format!(
            "plugin: auto-update not set; {MARKETPLACE} is not listed in ~/.claude/settings.json"
        )
    }

    /// Whether `claude` is missing or an install step failed.
    pub fn failed(&self) -> bool {
        matches!(self.outcome, Outcome::Failed { .. } | Outcome::NoCli { .. })
    }

    /// Whether the user must restart Claude Code or reload plugins.
    pub fn needs_reload(&self) -> bool {
        matches!(
            self.outcome,
            Outcome::Installed { .. } | Outcome::Updated { .. }
        )
    }

    /// One line per step, then the auto-update note and the reload hint.
    pub fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        lines.extend(self.marketplace.map(|m| m.line()));
        lines.extend(match &self.outcome {
            // The reload hint closes the report once, so the line stops short of it.
            Outcome::Installed { id, version } => Some(format!(
                "plugin: installed {id}{}",
                version
                    .as_deref()
                    .map(|v| format!(" {v}"))
                    .unwrap_or_default()
            )),
            other => other.line(),
        });
        if let Outcome::NoCli { .. } = self.outcome {
            lines.push(format!(
                "plugin: install Claude Code or add it to PATH, then run: {}",
                install_commands(PLUGIN_KEY)
            ));
        }
        if self.installed() {
            lines.push(match (&self.auto, self.marketplace) {
                (Some(auto), _) => auto.line().unwrap_or_else(|| self.auto_unset_line()),
                (None, Some(MarketplaceStep::OldName(name))) => format!(
                    "plugin: auto-update left as it is; the marketplace is registered as {name}, not {MARKETPLACE}"
                ),
                (None, _) => self.auto_unset_line(),
            });
            lines.push(AUTO_UPDATE_NOTE.to_string());
        }
        if self.needs_reload() {
            lines.push(format!("plugin: {RELOAD_HINT}"));
        }
        lines
    }

    /// The `--json` fields beside the envelope: one entry per step.
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "steps": [
                {
                    "step": "marketplace",
                    "status": self.marketplace.map_or("skipped", |m| m.status()),
                    "line": self.marketplace.map(|m| m.line()),
                },
                {
                    "step": "plugin",
                    "status": status_of(&self.outcome),
                    "line": self.outcome.line(),
                },
                {
                    "step": "auto-update",
                    "status": self.auto.as_ref().map_or("skipped", AutoUpdate::status),
                    "line": self.auto.as_ref().and_then(AutoUpdate::line),
                },
            ],
            "reload": self.needs_reload(),
            "lines": self.lines(),
        })
    }
}

/// `playbook plugin install`: runs [`ensure`] as an explicit request, so
/// `upgrade.plugin` set to off, the skip variable and a CI environment do not
/// stop it, and prints the result. Returns the exit code: 1 on a missing
/// `claude` or a failed step, else 0. With `json` it prints the shared
/// envelope and returns 0.
pub fn print_install_command(probe: &Probe, json: bool) -> i32 {
    let opts = SyncOpts {
        allow_install: true,
        force: true,
        progress: false,
    };
    let report = ensure(probe, opts, Wanted::InstallOnly, env!("CARGO_PKG_VERSION"));
    if matches!(report.outcome, Outcome::Installed { .. }) {
        record_setup_run(&probe.home, &report.outcome);
    }
    let code = i32::from(report.failed());
    if json {
        let error = if code == 0 {
            String::new()
        } else {
            report.lines().join("; ")
        };
        println!(
            "{}",
            crate::json::envelope::wrap(code, &error, report.json())
        );
        return 0;
    }
    for line in report.lines() {
        println!("{line}");
    }
    code
}

/// The commands a setup run would start for this home, for a dry run.
pub fn planned_commands(home: &Path) -> String {
    match plugin_id(&home.join(".claude")) {
        Some(id) => format!("{} && {}", marketplace_command(&id), command(&id)),
        None => install_commands(PLUGIN_KEY),
    }
}

/// Records how setup's plugin run ended in the marker the session hook
/// reads, so it does not repeat the work. A run that did not try anything is
/// not recorded.
pub fn record_setup_run(home: &Path, outcome: &Outcome) {
    if matches!(outcome, Outcome::Skipped(_) | Outcome::NotInstalled) {
        return;
    }
    let claude_home = home.join(".claude");
    let after = plugin_id(&claude_home).and_then(|id| installed_version(&claude_home, &id));
    finish(
        &crate::common::paths::playbook_root_from(home),
        outcome,
        env!("CARGO_PKG_VERSION"),
        after.as_deref(),
        now(),
    );
}

/// [`run`] for a binary at version `running`.
pub(crate) fn run_for_binary(probe: &Probe, running: &str) -> Outcome {
    sync_at(probe, SyncOpts::default(), running, &|name| {
        std::env::var(name).ok()
    })
}

/// [`sync`], also giving why the marketplace refresh failed, if it did.
fn run_with_marketplace(probe: &Probe, opts: SyncOpts) -> (Outcome, Option<String>) {
    let claude_home = probe.home.join(".claude");
    if !opts.force && mode(&probe.home) == Mode::Off {
        return (
            Outcome::Skipped(format!(
                "{CONFIG_KEY} is off; to turn it on, run: playbook config set {CONFIG_KEY} {MODE_AUTO}"
            )),
            None,
        );
    }
    let Some(id) = plugin_id(&claude_home) else {
        let outcome = if opts.allow_install {
            install(probe, opts)
        } else {
            Outcome::NotInstalled
        };
        return (outcome, None);
    };
    update_installed(probe, id, opts)
}

/// Updates the installed plugin `id`: refreshes its marketplace, runs
/// `claude plugin update` and reads the version before and after. The one
/// path [`sync`] and [`upgrade_command`] share.
fn update_installed(probe: &Probe, id: String, opts: SyncOpts) -> (Outcome, Option<String>) {
    let claude_home = probe.home.join(".claude");
    if is_off_in_claude_code(&claude_home, &id) {
        return (
            Outcome::Skipped(format!(
                "{id} is turned off in Claude Code; turn it on there, then run: {}",
                command(&id)
            )),
            None,
        );
    }
    let Some(claude) = &probe.claude else {
        return (Outcome::NoCli { id, install: false }, None);
    };
    let before = installed_version(&claude_home, &id);
    // A failed refresh never stops the plugin update; it is named later only
    // when the plugin ends older than the binary.
    let marketplace = marketplace_of(&id).and_then(|name| {
        say(opts, &format!("plugin: refreshing the {name} marketplace"));
        run_claude(
            probe,
            claude,
            &["plugin", "marketplace", "update", name],
            MARKETPLACE_TIMEOUT,
        )
        .err()
    });
    say(opts, &format!("plugin: updating {id}"));
    if let Err(why) = run_claude(probe, claude, &["plugin", "update", &id], TIMEOUT) {
        let failed = Outcome::Failed {
            id,
            why,
            install: false,
        };
        return (failed, marketplace);
    }
    if let Some(dir) = installed_path(&claude_home, &id) {
        refresh_system_prompt(&probe.home, &dir);
    }
    let after = installed_version(&claude_home, &id);
    let outcome = if after.is_some() && after != before {
        Outcome::Updated {
            id,
            from: before,
            to: after,
        }
    } else {
        Outcome::Current {
            id,
            version: after.or(before),
        }
    };
    (outcome, marketplace)
}

/// Every playbook plugin id Claude Code lists as installed, the preferred
/// marketplace first. A machine that kept the old marketplace name lists the
/// old id, and one that moved lists both.
pub fn installed_ids(claude_home: &Path) -> Vec<String> {
    crate::common::plugin_state::plugin_keys()
        .into_iter()
        .filter(|key| crate::common::plugin_state::installed_entry_at(claude_home, key).is_some())
        .collect()
}

/// What `playbook plugin upgrade` did, one [`Outcome`] per installed id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandReport {
    pub outcomes: Vec<Outcome>,
    /// The plugin is not installed, so there is nothing to update.
    pub not_installed: bool,
}

/// The hint after an update changed the plugin on disk.
const RELOAD_HINT: &str = "Restart Claude Code or run /reload-plugins to load it";

impl CommandReport {
    /// Whether the user must restart Claude Code or reload plugins.
    pub fn needs_reload(&self) -> bool {
        self.outcomes
            .iter()
            .any(|o| matches!(o, Outcome::Updated { .. }))
    }

    /// Whether a real failure happened: a failed update, a missing `claude`
    /// or a marketplace refresh that failed while the plugin lags.
    pub fn failed(&self) -> bool {
        self.outcomes.iter().any(|o| {
            matches!(
                o,
                Outcome::Failed { .. }
                    | Outcome::NoCli { .. }
                    | Outcome::Behind {
                        marketplace: Some(_),
                        ..
                    }
            )
        })
    }

    /// One line per outcome, then the reload hint when the plugin changed.
    pub fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = if self.not_installed {
            vec![format!(
                "plugin: not installed; run: {}",
                install_commands(PLUGIN_KEY)
            )]
        } else {
            self.outcomes.iter().filter_map(command_line).collect()
        };
        if self.needs_reload() {
            lines.push(format!("plugin: {RELOAD_HINT}"));
        }
        lines
    }
}

/// The line `playbook plugin upgrade` shows for `outcome`. It differs from
/// [`Outcome::line`] where an explicit command has no 24 hour retry to
/// mention.
fn command_line(outcome: &Outcome) -> Option<String> {
    match outcome {
        Outcome::Updated { id, from, to } => Some(format!(
            "plugin: updated {id}{}{}",
            from.as_deref()
                .map(|v| format!(" from {v}"))
                .unwrap_or_default(),
            to.as_deref()
                .map(|v| format!(" to {v}"))
                .unwrap_or_default(),
        )),
        Outcome::Behind {
            id,
            version,
            running,
            marketplace: None,
        } => Some(format!(
            "plugin: {id} {} is older than this binary ({running}); the marketplace has not published a newer plugin yet",
            version.as_deref().unwrap_or(UNKNOWN_VERSION)
        )),
        other => other.line(),
    }
}

/// `playbook plugin upgrade`: refreshes the marketplace and runs
/// `claude plugin update` for every installed playbook plugin id. It is an
/// explicit request, so `upgrade.plugin` set to off, the skip variable and a
/// CI environment do not stop it.
pub fn upgrade_command(probe: &Probe) -> CommandReport {
    upgrade_command_at(probe, env!("CARGO_PKG_VERSION"))
}

fn upgrade_command_at(probe: &Probe, running: &str) -> CommandReport {
    let opts = SyncOpts {
        progress: true,
        ..SyncOpts::default()
    };
    let ids = installed_ids(&probe.home.join(".claude"));
    let outcomes: Vec<Outcome> = ids
        .into_iter()
        .map(|id| {
            let (outcome, marketplace) = update_installed(probe, id, opts);
            judge(outcome, marketplace, running)
        })
        .collect();
    CommandReport {
        not_installed: outcomes.is_empty(),
        outcomes,
    }
}

/// The `--json` fields of [`CommandReport`], beside the envelope.
fn report_json(report: &CommandReport) -> serde_json::Value {
    serde_json::json!({
        "plugins": report.outcomes.iter().map(|o| serde_json::json!({
            "status": status_of(o),
            "line": command_line(o),
        })).collect::<Vec<_>>(),
        "not_installed": report.not_installed,
        "reload": report.needs_reload(),
        "lines": report.lines(),
    })
}

/// A stable word for what happened to one plugin id.
fn status_of(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Updated { .. } => "updated",
        Outcome::Installed { .. } => "installed",
        Outcome::Present { .. } => "present",
        Outcome::Current { .. } => "current",
        Outcome::Behind { .. } => "behind",
        Outcome::NoCli { .. } => "no-claude",
        Outcome::Failed { .. } => "failed",
        _ => "skipped",
    }
}

/// Runs [`upgrade_command`], prints its result and returns the exit code: 1
/// on a real failure, else 0. With `json` it prints the shared envelope and
/// returns 0.
pub fn print_upgrade_command(probe: &Probe, json: bool) -> i32 {
    let report = upgrade_command(probe);
    let code = i32::from(report.failed());
    if json {
        let error = if code == 0 {
            String::new()
        } else {
            report.lines().join("; ")
        };
        let value = crate::json::envelope::wrap(code, &error, report_json(&report));
        println!("{value}");
        return 0;
    }
    for line in report.lines() {
        println!("{line}");
    }
    code
}

/// Why a command that runs the plugin step may not.
enum Guard {
    /// `--no-plugin` or [`SKIP_ENV`]: say nothing.
    Skip,
    /// Another playbook process holds the plugin claim.
    Busy,
    Go,
}

fn guard(home: &Path, no_plugin: bool) -> Guard {
    if no_plugin || std::env::var_os(SKIP_ENV).is_some_and(|v| v == SKIP_VALUE) {
        return Guard::Skip;
    }
    let root = crate::common::paths::playbook_root_from(home);
    if in_flight(&root, env!("CARGO_PKG_VERSION"), now()) {
        return Guard::Busy;
    }
    Guard::Go
}

/// The outcome for a plugin step that another process already runs.
fn busy() -> Outcome {
    Outcome::Skipped("another playbook process is already updating it".to_string())
}

/// [`run`] for a command, unless `--no-plugin` was passed or the caller set
/// [`SKIP_ENV`]. `None` when it was not asked to run.
pub fn run_for_command(home: &Path, no_plugin: bool) -> Option<Outcome> {
    match guard(home, no_plugin) {
        Guard::Skip => None,
        Guard::Busy => Some(busy()),
        Guard::Go => Some(run_for_binary(
            &Probe::for_home(home),
            env!("CARGO_PKG_VERSION"),
        )),
    }
}

/// The plugin step of `playbook upgrade`: [`ensure`] with an install or an
/// update, judged against `running`, the version the upgrade installed. It
/// follows `upgrade.plugin`, `--no-plugin` and [`SKIP_ENV`], so it is the same
/// step `playbook plugin install` and `playbook plugin upgrade` run, minus
/// their force. `None` when it was not asked to run.
pub fn run_for_upgrade(home: &Path, no_plugin: bool, running: &str) -> Option<Ensured> {
    let held = |outcome| Ensured {
        marketplace: None,
        outcome,
        auto: None,
    };
    match guard(home, no_plugin) {
        Guard::Skip => None,
        Guard::Busy => Some(held(busy())),
        Guard::Go => Some(ensure(
            &Probe::for_home(home),
            SyncOpts {
                allow_install: true,
                ..SyncOpts::default()
            },
            Wanted::InstallOrUpdate,
            running,
        )),
    }
}

/// The state key of the once-per-binary-version marker.
const MARKER_KEY: &str = "versions/plugin-update";

/// A failed update, or a plugin still behind the binary, is tried again after
/// this long.
pub(crate) const FAILED_RETRY: Duration = Duration::from_secs(24 * 60 * 60);

/// A `running` marker older than this belongs to a run that never finished
/// (Ctrl-C, a crash), so the update is due again.
pub(crate) const RUNNING_STALE: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Done,
    Failed,
    /// The update ran and the plugin is still on an older minor line, so the
    /// marketplace has not caught up. Not a failure, but retried like one.
    Behind,
}

impl State {
    const fn word(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Done => "done",
            State::Failed => "failed",
            State::Behind => "behind",
        }
    }
}

/// What the last attempt did: `<binary version> <state> <epoch secs>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub version: String,
    pub state: State,
    pub at: u64,
}

impl Marker {
    /// `None` for text that is not a marker; callers treat that as another
    /// version, so the update is due.
    pub fn parse(text: &str) -> Option<Marker> {
        let mut parts = text.split_whitespace();
        let version = parts.next()?.to_string();
        let state = match parts.next()? {
            "running" => State::Running,
            "done" => State::Done,
            "failed" => State::Failed,
            "behind" => State::Behind,
            _ => return None,
        };
        let at = parts.next()?.parse().ok()?;
        parts
            .next()
            .is_none()
            .then_some(Marker { version, state, at })
    }

    pub fn format(&self) -> String {
        format!("{} {} {}", self.version, self.state.word(), self.at)
    }
}

/// Everything the due check reads, so the rules run without a process
/// environment.
#[derive(Debug, Clone)]
pub struct DueInputs<'a> {
    /// `None` when there is no marker or it cannot be parsed.
    pub marker: Option<Marker>,
    pub running: &'a str,
    pub installed: Option<&'a str>,
    pub mode: Mode,
    pub skip_env: Option<&'a str>,
    pub headless: bool,
    pub ci_vendor: bool,
    pub disabled_in_claude: bool,
    pub plugin_id: Option<&'a str>,
}

/// Whether the plugin update should run now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Due {
    Run(Mode),
    /// The plugin is already at the running version and the marker does not
    /// say so yet: record `done`, run nothing.
    Current,
    NotDue(&'static str),
}

/// The rules of the due check, in order. Skips come first so a skipped run
/// never writes the marker.
pub fn due_from(i: &DueInputs<'_>, now: u64) -> Due {
    if i.mode == Mode::Off {
        return Due::NotDue("upgrade.plugin is off");
    }
    if i.skip_env == Some(SKIP_VALUE) {
        return Due::NotDue("the skip variable is set");
    }
    if i.headless {
        return Due::NotDue("headless run");
    }
    if i.ci_vendor {
        return Due::NotDue("CI run");
    }
    if i.plugin_id.is_none() {
        return Due::NotDue("the plugin is not installed");
    }
    if i.disabled_in_claude {
        return Due::NotDue("the plugin is turned off in Claude Code");
    }
    let Some(installed) = i.installed else {
        return Due::NotDue("the installed plugin version is unknown");
    };
    let this_version = i.marker.as_ref().filter(|m| m.version == i.running);
    if crate::update::resolve::compare(installed, i.running).is_ge() {
        return match this_version {
            Some(m) if m.state == State::Done => Due::NotDue("already recorded as current"),
            _ => Due::Current,
        };
    }
    let age = |m: &Marker| Duration::from_secs(now.saturating_sub(m.at));
    match this_version {
        None => Due::Run(i.mode),
        Some(m) => match m.state {
            State::Done => Due::NotDue("already done for this version"),
            State::Failed | State::Behind if age(m) > FAILED_RETRY => Due::Run(i.mode),
            State::Failed => Due::NotDue("failed recently"),
            State::Behind => Due::NotDue("behind, tried recently"),
            State::Running if age(m) > RUNNING_STALE => Due::Run(i.mode),
            State::Running => Due::NotDue("another process is running it"),
        },
    }
}

/// The raw marker text, the `seen` value [`claim`] compares against.
pub fn seen(root: &Path) -> Option<String> {
    crate::state::list_read_only(root, MARKER_KEY)
        .ok()?
        .into_iter()
        .find_map(|(k, v)| (k == MARKER_KEY && !v.is_empty()).then_some(v))
}

pub(crate) fn now() -> u64 {
    u64::try_from(crate::common::time::now_secs()).unwrap_or(0)
}

/// [`due_from`] with the inputs read from `home` and the process
/// environment. `seen_marker` is the marker text the caller already read, so
/// the store is opened once. Reads only.
pub fn due(home: &Path, seen_marker: Option<&str>) -> Due {
    let claude_home = home.join(".claude");
    let id = plugin_id(&claude_home);
    let installed = id
        .as_deref()
        .and_then(|id| installed_version(&claude_home, id));
    let skip_env = std::env::var(SKIP_ENV).ok();
    due_from(
        &DueInputs {
            marker: seen_marker.and_then(Marker::parse),
            running: env!("CARGO_PKG_VERSION"),
            installed: installed.as_deref(),
            mode: mode(home),
            skip_env: skip_env.as_deref(),
            headless: crate::common::headless::is_headless(),
            ci_vendor: crate::update::check::ci_vendor_env(|k| std::env::var(k).ok()),
            disabled_in_claude: id
                .as_deref()
                .is_some_and(|id| is_off_in_claude_code(&claude_home, id)),
            plugin_id: id.as_deref(),
        },
        now(),
    )
}

/// Moves the marker from `seen` to `running` in one transaction. Only the
/// caller that moved it runs the update, so two sessions that start
/// together run it once.
pub fn claim(root: &Path, seen: Option<&str>, running: &str, now: u64) -> bool {
    let marker = Marker {
        version: running.to_string(),
        state: State::Running,
        at: now,
    };
    crate::state::transaction(root, |tx| {
        if tx.get(MARKER_KEY)?.filter(|v| !v.is_empty()).as_deref() == seen {
            tx.set(MARKER_KEY, &marker.format())?;
            return Ok(true);
        }
        Ok(false)
    })
    .unwrap_or(false)
}

/// The state an attempt ends in. A fresh install at the binary's line ends
/// `done`: the marketplace holds what it holds, and a retry cannot change
/// that. A plugin left on an older minor line than the binary, changed or
/// not, ends `behind`, so it retries after [`FAILED_RETRY`]. A failed update
/// or a missing CLI ends `failed`.
fn end_state(outcome: &Outcome, running: &str, installed_after: Option<&str>) -> State {
    match outcome {
        Outcome::Failed { .. } | Outcome::NoCli { .. } => State::Failed,
        Outcome::Behind { .. } => State::Behind,
        Outcome::Current { .. } | Outcome::Updated { .. } if lags(installed_after, running) => {
            State::Behind
        }
        Outcome::Current { .. }
        | Outcome::Updated { .. }
        | Outcome::Installed { .. }
        | Outcome::Present { .. }
        | Outcome::Skipped(_)
        | Outcome::LeftAlone(_)
        | Outcome::NotInstalled => State::Done,
    }
}

/// Records how the attempt ended and returns the state it wrote. Best
/// effort: a store that cannot be written leaves the `running` marker, which
/// goes stale on its own.
pub fn finish(
    root: &Path,
    outcome: &Outcome,
    running: &str,
    installed_after: Option<&str>,
    now: u64,
) -> State {
    let state = end_state(outcome, running, installed_after);
    write(root, running, state, now);
    state
}

/// The line to keep for the next session. A run that ended `behind` with the
/// plugin changed or unchanged but still older must not say "up to date".
fn notice_line(outcome: &Outcome, state: State) -> Option<String> {
    match outcome {
        Outcome::Current { id, .. } | Outcome::Updated { id, .. } if state == State::Behind => {
            Some(format!(
                "plugin: the playbook plugin is still older than this playbook binary; run: {}",
                command(id)
            ))
        }
        // The next session may already run the new plugin, so no restart advice.
        Outcome::Updated { id, from, to } => Some(format!(
            "plugin: updated {id}{}{}",
            from.as_deref()
                .map(|v| format!(" from {v}"))
                .unwrap_or_default(),
            to.as_deref()
                .map(|v| format!(" to {v}"))
                .unwrap_or_default(),
        )),
        _ => outcome.line(),
    }
}

/// Whether an update for `running` is in progress: its marker is `running`
/// and not yet stale.
pub fn in_flight(root: &Path, running: &str, now: u64) -> bool {
    seen(root).and_then(|t| Marker::parse(&t)).is_some_and(|m| {
        m.version == running
            && m.state == State::Running
            && Duration::from_secs(now.saturating_sub(m.at)) <= RUNNING_STALE
    })
}

/// Records `done` for the running version: the plugin is already current
/// ([`Due::Current`]), or the user declined the update.
pub fn record_done(root: &Path, running: &str, now: u64) {
    write(root, running, State::Done, now);
}

fn write(root: &Path, running: &str, state: State, now: u64) {
    let marker = Marker {
        version: running.to_string(),
        state,
        at: now,
    };
    let _ = crate::state::set(root, MARKER_KEY, &marker.format());
}

/// The detached body behind `update --refresh-plugin`: updates the plugin,
/// records the marker and leaves the outcome line for the next session. It
/// prints nothing. [`run`] does not read [`SKIP_ENV`], so the child the
/// hook started cannot be stopped by its own variable.
pub fn refresh_plugin(home: &Path) {
    let root = crate::common::paths::playbook_root_from(home);
    // Only a run the hook claimed proceeds, so a hand-started flag cannot
    // run a second update beside it.
    if !in_flight(&root, env!("CARGO_PKG_VERSION"), now()) {
        return;
    }
    let (outcome, state) = attempt(home, &root);
    if let Some(line) = notice_line(&outcome, state) {
        crate::notices::store_notice(&root, crate::notices::PLUGIN_UPDATE_KEY, &line);
    }
    // The plugin may have moved, so synced agents follow it now.
    let refreshed = crate::skills_sync::refresh_report(home, false);
    if let Some(line) = crate::skills_sync::notice_line(&refreshed) {
        crate::notices::store_notice(&root, crate::notices::SKILLS_SYNC_KEY, &line);
    }
}

/// Test seam that keeps the hook from starting the detached refresh. Read
/// only in debug builds.
#[cfg(debug_assertions)]
const NO_SPAWN_ENV: &str = "PLAYBOOK_TEST_NO_SPAWN";

#[cfg(debug_assertions)]
const NO_SPAWN_VALUE: &str = "1";

/// Starts `<exe> update --refresh-plugin` detached, like the release check
/// refresh: own process group, no stdio, and the skip variable removed so
/// the child is not mistaken for a nested run. Returns whether the child
/// started; the no-spawn seam counts as started, since nothing failed.
pub(crate) fn spawn_refresh_plugin() -> bool {
    spawn_detached(&["upgrade", "--refresh-plugin"])
}

/// Starts `<exe> skills sync --refresh` detached, the same way.
pub(crate) fn spawn_refresh_skills() -> bool {
    spawn_detached(&["skills", "sync", "--refresh"])
}

fn spawn_detached(args: &[&str]) -> bool {
    #[cfg(debug_assertions)]
    if std::env::var_os(NO_SPAWN_ENV).is_some_and(|v| v == NO_SPAWN_VALUE) {
        return true;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .env_remove(SKIP_ENV)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().is_ok()
}

/// Settles a claim after the spawn attempt. A child that never started
/// leaves no one to end the `running` marker, so it is set to `failed` and
/// retries after [`FAILED_RETRY`] instead of blocking for [`RUNNING_STALE`].
pub(crate) fn settle_spawn(root: &Path, running: &str, spawned: bool, now: u64) -> bool {
    if !spawned {
        write(root, running, State::Failed, now);
    }
    spawned
}

/// The test seam that stands in for a terminal. Read only in debug builds,
/// so a release binary cannot be fooled by it.
#[cfg(debug_assertions)]
const ASSUME_TTY_ENV: &str = "PLAYBOOK_TEST_ASSUME_TTY";

/// Seam value for a terminal on stderr and stdin.
#[cfg(debug_assertions)]
const ASSUME_BOTH_TTY: &str = "1";

/// Seam value for a terminal on stderr only, with stdin left as it is.
#[cfg(debug_assertions)]
const ASSUME_STDERR_TTY: &str = "stderr";

pub(crate) fn assumed_tty(for_stdin: bool) -> bool {
    #[cfg(debug_assertions)]
    if let Some(v) = std::env::var_os(ASSUME_TTY_ENV) {
        return v == ASSUME_BOTH_TTY || (!for_stdin && v == ASSUME_STDERR_TTY);
    }
    let _ = for_stdin;
    false
}

fn is_terminal(stream: &impl std::io::IsTerminal, for_stdin: bool) -> bool {
    assumed_tty(for_stdin) || stream.is_terminal()
}

/// Printed when a yes loses the claim to another running process.
const ALREADY_UPDATING: &str = "playbook: another playbook process is already updating the plugin";

/// The question `Ask` mode puts on stderr.
const QUESTION: &str = "Update the playbook plugin now? [Y/n] ";

/// Updates the plugin at the first command after an upgrade, when the
/// command is at a terminal (ADR 0034, 2026-10-19 amendment). `Auto` runs
/// the update; `Ask` asks first and needs a terminal on stdin too. Output
/// goes to stderr, so the command's own stdout stays as it was, and nothing
/// here changes the exit code.
pub fn on_start(command: &crate::Command, args: &[std::ffi::OsString]) {
    if !crate::heal::runs_on_start(command)
        || crate::update::check::has_json_arg(args)
        || !is_terminal(&std::io::stderr(), false)
    {
        return;
    }
    let home = crate::common::home_dir();
    if crate::common::home_error(&home).is_some() {
        return;
    }
    let stdin_is_terminal = is_terminal(&std::io::stdin(), true);
    let _ = std::panic::catch_unwind(|| start(&home, stdin_is_terminal));
}

/// The steps of one trigger: due check, mode, claim, update, report.
fn start(home: &Path, stdin_is_terminal: bool) {
    let root = crate::common::paths::playbook_root_from(home);
    let running = env!("CARGO_PKG_VERSION");
    let seen_before = seen(&root);
    let mode = match due(home, seen_before.as_deref()) {
        Due::Run(mode) => mode,
        Due::Current => {
            record_done(&root, running, now());
            return;
        }
        Due::NotDue(_) => return,
    };
    let claim_now = || claim(&root, seen_before.as_deref(), running, now());
    match mode {
        Mode::Auto => {
            if claim_now() {
                update_and_report(home, &root);
            }
        }
        Mode::Ask if stdin_is_terminal => match ask() {
            Some(true) if claim_now() => update_and_report(home, &root),
            Some(true) => lost_claim(&root, home, running),
            Some(false) if claim_now() => {
                record_done(&root, running, now());
                if let Some(id) = plugin_id(&home.join(".claude")) {
                    eprintln!(
                        "playbook: not updating the plugin; to update it, run: {}",
                        command(&id)
                    );
                }
            }
            _ => {}
        },
        Mode::Ask | Mode::Off => {}
    }
}

/// A yes lost the claim: say another process is updating only when the marker
/// still says so, else the marker moved while the question was open (a
/// no or a finished update), so name the manual command.
fn lost_claim(root: &Path, home: &Path, running: &str) {
    let busy = seen(root)
        .and_then(|t| Marker::parse(&t))
        .is_some_and(|m| m.version == running && m.state == State::Running);
    match plugin_id(&home.join(".claude")) {
        _ if busy => eprintln!("{ALREADY_UPDATING}"),
        Some(id) => eprintln!("playbook: to update the plugin, run: {}", command(&id)),
        None => {}
    }
}

/// `Some(answer)` for a yes or no; `None` for end of input or any other
/// reply, so the question comes back at the next command.
fn ask() -> Option<bool> {
    use crate::ui::wizard::{parse_answer, Answer};
    use std::io::{BufRead, Write};
    eprint!("{QUESTION}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).ok()? == 0 {
        return None;
    }
    match parse_answer(&line, true) {
        Answer::Yes => Some(true),
        Answer::No => Some(false),
        _ => None,
    }
}

fn update_and_report(home: &Path, root: &Path) {
    if let Some(id) = plugin_id(&home.join(".claude")) {
        eprintln!("playbook: updating the playbook plugin ({})", command(&id));
    }
    let (outcome, state) = attempt(home, root);
    let line = match outcome {
        Outcome::Updated { .. } => outcome.line(),
        _ => notice_line(&outcome, state),
    };
    if let Some(line) = line {
        eprintln!("{line}");
    }
}

/// Runs the update, records how it ended and returns both.
fn attempt(home: &Path, root: &Path) -> (Outcome, State) {
    let claude_home = home.join(".claude");
    let outcome = run_for_binary(&Probe::for_home(home), env!("CARGO_PKG_VERSION"));
    let after = plugin_id(&claude_home).and_then(|id| installed_version(&claude_home, &id));
    let state = finish(
        root,
        &outcome,
        env!("CARGO_PKG_VERSION"),
        after.as_deref(),
        now(),
    );
    (outcome, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use std::fs;

    fn plugin(home: &Path, version: &str) -> PathBuf {
        let dir = home.join(format!(
            ".claude/plugins/cache/amplifiedengineer/playbook/{version}"
        ));
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::create_dir_all(dir.join("skills")).unwrap();
        fs::write(
            dir.join(".claude-plugin/plugin.json"),
            format!(r#"{{"name":"playbook","version":"{version}"}}"#),
        )
        .unwrap();
        dir
    }

    fn install(home: &Path, key: &str, dir: &Path) {
        fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            format!(
                r#"{{"version":2,"plugins":{{"{key}":[{{"scope":"user","installPath":"{}"}}],"other@x":[{{"scope":"user","installPath":"/nowhere"}}]}}}}"#,
                dir.display()
            ),
        )
        .unwrap();
    }

    /// A fake `claude` that logs its arguments and, for `plugin update`,
    /// points installed_plugins.json at `to` when given.
    fn fake_claude(home: &Path, to: Option<&Path>, exit: i32) -> PathBuf {
        let bin = home.join("bin/claude");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        let swap = to.map_or(String::new(), |dir| {
            format!(
                "printf '%s' '{{\"version\":2,\"plugins\":{{\"playbook@amplifiedengineer\":[{{\"scope\":\"user\",\"installPath\":\"{}\"}}]}}}}' > \"$HOME/.claude/plugins/installed_plugins.json\"\n",
                dir.display()
            )
        });
        write_script(
            &bin,
            &format!(
                "#!/bin/sh\necho \"$*\" >> \"$HOME/claude-calls.log\"\n{swap}echo 'update failed: network' >&2\nexit {exit}\n"
            ),
        );
        bin
    }

    /// Writes an executable script under a temp name and renames it, so no
    /// writer still holds the file when it is first run.
    fn write_script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, body).unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&tmp, path).unwrap();
    }

    fn calls(home: &Path) -> String {
        fs::read_to_string(home.join("claude-calls.log")).unwrap_or_default()
    }

    fn probe_items(home: &Path) -> Vec<crate::pending::Item> {
        crate::pending::plugin_items(&crate::pending::PluginProbe {
            claude_home: home.join(".claude"),
            session_root: None,
            binary: env!("CARGO_PKG_VERSION").to_string(),
            homebrew: false,
        })
    }

    #[test]
    fn an_update_to_the_current_plugin_leaves_no_old_version_item() {
        // Arrange
        let home = scratch_dir("plugin-update-moves-forward");
        let old = plugin(&home, "0.0.1");
        let current = plugin(&home, env!("CARGO_PKG_VERSION"));
        install(&home, "playbook@amplifiedengineer", &old);
        let probe = Probe {
            home: home.clone(),
            claude: Some(fake_claude(&home, Some(&current), 0)),
        };
        let before = probe_items(&home);

        // Act
        let outcome = run_for_binary(&probe, env!("CARGO_PKG_VERSION"));
        let after = probe_items(&home);

        // Assert
        assert_eq!(before.len(), 1, "{before:?}");
        assert!(matches!(outcome, Outcome::Updated { .. }), "{outcome:?}");
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn an_old_install_for_this_project_is_not_reported_up_to_date() {
        // Arrange: the user install is current, the install for this
        // project is old, and Claude Code loads the project one here.
        let home = scratch_dir("plugin-update-project-install");
        let old = plugin(&home, "0.0.1");
        let current = plugin(&home, env!("CARGO_PKG_VERSION"));
        let cwd = std::env::current_dir().unwrap();
        fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            format!(
                r#"{{"version":2,"plugins":{{"playbook@amplifiedengineer":[{{"scope":"user","installPath":"{}"}},{{"scope":"project","projectPath":"{}","installPath":"{}"}}]}}}}"#,
                current.display(),
                cwd.display(),
                old.display()
            ),
        )
        .unwrap();
        let probe = Probe {
            home: home.clone(),
            claude: Some(fake_claude(&home, None, 0)),
        };

        // Act
        let outcome = run_for_binary(&probe, env!("CARGO_PKG_VERSION"));

        // Assert
        let Outcome::Behind { version, .. } = &outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(version.as_deref(), Some("0.0.1"));
        assert_eq!(probe_items(&home).len(), 1);
    }

    #[test]
    fn a_failed_spawn_marks_the_claim_failed() {
        // Arrange
        let root = scratch_dir("plugin-update-spawn-failed");
        assert!(claim(&root, None, "1.2.3", 100));

        // Act
        let spawned = settle_spawn(&root, "1.2.3", false, 101);

        // Assert
        assert!(!spawned);
        assert_eq!(seen(&root).as_deref(), Some("1.2.3 failed 101"));
    }

    #[test]
    fn a_started_spawn_leaves_the_running_marker() {
        // Arrange
        let root = scratch_dir("plugin-update-spawn-ok");
        assert!(claim(&root, None, "1.2.3", 100));

        // Act
        let spawned = settle_spawn(&root, "1.2.3", true, 101);

        // Assert
        assert!(spawned);
        assert_eq!(seen(&root).as_deref(), Some("1.2.3 running 100"));
    }

    #[test]
    fn a_still_older_plugin_never_reads_as_up_to_date() {
        // Arrange
        let id = "playbook@amplifiedengineer".to_string();
        let current = Outcome::Current {
            id: id.clone(),
            version: Some("0.36.0".to_string()),
        };
        let updated = Outcome::Updated {
            id,
            from: Some("0.35.0".to_string()),
            to: Some("0.36.0".to_string()),
        };

        // Act
        let lines = [
            notice_line(&current, State::Behind),
            notice_line(&updated, State::Behind),
        ];

        // Assert
        for line in lines {
            let line = line.unwrap();
            assert!(
                line.contains("still older than this playbook binary"),
                "{line}"
            );
            assert!(line.ends_with("claude plugin update playbook@amplifiedengineer"));
        }
    }

    #[test]
    fn a_current_plugin_that_reached_done_keeps_its_own_line() {
        // Arrange
        let current = Outcome::Current {
            id: "playbook@x".to_string(),
            version: Some("1.0.0".to_string()),
        };

        // Act
        let line = notice_line(&current, State::Done);

        // Assert
        assert_eq!(line, current.line());
    }

    #[test]
    fn in_flight_is_true_only_for_a_fresh_running_marker_of_this_version() {
        // Arrange
        let root = scratch_dir("plugin-update-in-flight");
        assert!(claim(&root, None, "1.2.3", 1000));
        let stale = 1000 + RUNNING_STALE.as_secs() + 1;

        // Act and Assert
        assert!(in_flight(&root, "1.2.3", 1001));
        assert!(!in_flight(&root, "1.2.4", 1001));
        assert!(!in_flight(&root, "1.2.3", stale));
    }

    #[test]
    fn it_updates_only_the_playbook_plugin_and_says_to_restart() {
        // Arrange
        let home = scratch_dir("plugin-update-ok");
        let old = plugin(&home, "0.36.0");
        let new = plugin(&home, "0.37.0");
        install(&home, "playbook@amplifiedengineer", &old);
        let claude = fake_claude(&home, Some(&new), 0);

        // Act
        let out = run_for_binary(
            &Probe {
                home: home.clone(),
                claude: Some(claude),
            },
            "0.37.0",
        );

        // Assert
        assert_eq!(
            calls(&home),
            "plugin marketplace update amplifiedengineer\nplugin update playbook@amplifiedengineer\n"
        );
        assert_eq!(
            out.line().unwrap(),
            "plugin: updated playbook@amplifiedengineer from 0.36.0 to 0.37.0. Restart Claude Code to load it"
        );
    }

    #[test]
    fn an_install_that_kept_the_old_name_is_updated_under_that_name() {
        let home = scratch_dir("plugin-update-old-name");
        let old = plugin(&home, "0.36.0");
        let new = plugin(&home, "0.37.0");
        install(&home, "playbook@pragmatic-engineer", &old);
        let claude = fake_claude(&home, Some(&new), 0);

        run(&Probe {
            home: home.clone(),
            claude: Some(claude),
        });

        assert_eq!(
            calls(&home),
            "plugin marketplace update pragmatic-engineer\nplugin update playbook@pragmatic-engineer\n"
        );
    }

    #[test]
    fn the_new_id_is_picked_first_when_both_ids_are_listed() {
        let home = scratch_dir("plugin-update-both-ids");
        let dir = plugin(&home, "0.37.0");
        fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            format!(
                r#"{{"version":2,"plugins":{{"playbook@pragmatic-engineer":[{{"installPath":"{0}"}}],"playbook@amplifiedengineer":[{{"installPath":"{0}"}}]}}}}"#,
                dir.display()
            ),
        )
        .unwrap();

        assert_eq!(
            plugin_id(&home.join(".claude")).as_deref(),
            Some("playbook@amplifiedengineer")
        );
        install(&home, "playbook@pragmatic-engineer", &dir);
        assert_eq!(
            plugin_id(&home.join(".claude")).as_deref(),
            Some("playbook@pragmatic-engineer")
        );
    }

    fn placed_prompt_after_update(edit_before: Option<&str>) -> (String, bool) {
        let home = scratch_dir(if edit_before.is_some() {
            "plugin-update-prompt-edited"
        } else {
            "plugin-update-prompt"
        });
        let old = plugin(&home, "0.36.0");
        let new = plugin(&home, "0.37.0");
        for (dir, text) in [(&old, "old rules"), (&new, "new rules")] {
            fs::create_dir_all(dir.join("prompts")).unwrap();
            fs::write(dir.join("prompts/SYSTEM_PROMPT.md"), text).unwrap();
        }
        let placed = crate::init::system_prompt::path(&home);
        fs::create_dir_all(placed.parent().unwrap()).unwrap();
        fs::write(&placed, "old rules").unwrap();
        crate::init::migrate::record_system_prompt(&home);
        if let Some(text) = edit_before {
            fs::write(&placed, text).unwrap();
        }
        install(&home, "playbook@amplifiedengineer", &old);
        let claude = fake_claude(&home, Some(&new), 0);

        run(&Probe {
            home: home.clone(),
            claude: Some(claude),
        });

        let recorded = crate::init::migrate::placed_unchanged(&home, "system-prompt", &placed);
        (fs::read_to_string(&placed).unwrap(), recorded)
    }

    #[test]
    fn an_update_refreshes_the_placed_system_prompt_from_the_new_plugin() {
        let (text, recorded) = placed_prompt_after_update(None);

        assert_eq!(text, "new rules");
        assert!(recorded, "the new copy's hash must be recorded");
    }

    #[test]
    fn an_update_leaves_an_edited_system_prompt_alone() {
        assert_eq!(placed_prompt_after_update(Some("my rules")).0, "my rules");
    }

    #[test]
    fn a_missing_cli_or_a_failed_update_names_the_command_and_does_not_fail() {
        let home = scratch_dir("plugin-update-fail");
        let dir = plugin(&home, "0.36.0");
        install(&home, "playbook@amplifiedengineer", &dir);
        let missing = run(&Probe {
            home: home.clone(),
            claude: None,
        });
        let failed = run(&Probe {
            home: home.clone(),
            claude: Some(fake_claude(&home, None, 1)),
        });
        assert!(missing
            .line()
            .unwrap()
            .ends_with("run: claude plugin update playbook@amplifiedengineer"));
        assert_eq!(
            failed.line().unwrap(),
            "plugin: the update failed (update failed: network); run: claude plugin update playbook@amplifiedengineer"
        );
    }

    /// A fake `claude` that logs its arguments, fails `plugin marketplace
    /// update` with `market_error`, and succeeds for everything else.
    fn fake_claude_failing_marketplace(home: &Path, market_error: &str) -> PathBuf {
        let bin = home.join("bin/claude");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho \"$*\" >> \"$HOME/claude-calls.log\"\ncase \"$*\" in\n*marketplace*) echo '{market_error}' >&2; exit 1;;\nesac\nexit 0\n"
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[test]
    fn a_failed_marketplace_refresh_does_not_stop_the_plugin_update() {
        // Arrange
        let home = scratch_dir("plugin-update-market-fail");
        let dir = plugin(&home, "0.36.0");
        install(&home, "playbook@amplifiedengineer", &dir);
        let claude = fake_claude_failing_marketplace(&home, "git pull failed");
        let probe = Probe {
            home: home.clone(),
            claude: Some(claude),
        };

        // Act
        let (outcome, marketplace) = run_with_marketplace(&probe, SyncOpts::default());

        // Assert
        assert!(matches!(outcome, Outcome::Current { .. }), "{outcome:?}");
        assert_eq!(marketplace.as_deref(), Some("git pull failed"));
        assert_eq!(
            calls(&home),
            "plugin marketplace update amplifiedengineer\nplugin update playbook@amplifiedengineer\n"
        );
    }

    #[test]
    fn a_plugin_older_than_the_binary_after_a_good_refresh_says_the_marketplace_lags() {
        // Arrange
        let home = scratch_dir("plugin-update-lag");
        let dir = plugin(&home, "0.36.0");
        install(&home, "playbook@amplifiedengineer", &dir);
        let probe = Probe {
            home: home.clone(),
            claude: Some(fake_claude_failing_marketplace(&home, "unused")),
        };
        fs::write(home.join("bin/claude"), "#!/bin/sh\nexit 0\n").unwrap();

        // Act
        let outcome = run_for_binary(&probe, "0.37.0");

        // Assert
        let line = outcome.line().unwrap();
        assert!(outcome.awaits_release(), "{outcome:?}");
        assert!(line.contains("marketplace has not published"), "{line}");
        assert!(!line.contains(COMMAND_PREFIX), "{line}");
    }

    #[test]
    fn a_plugin_older_than_the_binary_after_a_failed_refresh_names_it_and_keeps_the_fix() {
        // Arrange
        let home = scratch_dir("plugin-update-lag-fail");
        let dir = plugin(&home, "0.36.0");
        install(&home, "playbook@amplifiedengineer", &dir);
        let probe = Probe {
            home: home.clone(),
            claude: Some(fake_claude_failing_marketplace(&home, "git pull failed")),
        };

        // Act
        let outcome = run_for_binary(&probe, "0.37.0");

        // Assert
        assert!(!outcome.awaits_release(), "{outcome:?}");
        assert_eq!(
            outcome.line().unwrap(),
            "plugin: playbook@amplifiedengineer 0.36.0 is older than this binary (0.37.0); the marketplace refresh failed (git pull failed); run: claude plugin marketplace update amplifiedengineer && claude plugin update playbook@amplifiedengineer"
        );
    }

    #[test]
    fn a_plugin_at_the_binary_version_stays_current() {
        // Arrange
        let home = scratch_dir("plugin-update-at-binary");
        let dir = plugin(&home, "0.37.0");
        install(&home, "playbook@amplifiedengineer", &dir);
        let probe = Probe {
            home: home.clone(),
            claude: Some(fake_claude(&home, None, 0)),
        };

        // Act
        let outcome = run_for_binary(&probe, "0.37.0");

        // Assert
        assert!(matches!(outcome, Outcome::Current { .. }), "{outcome:?}");
    }

    #[test]
    fn the_marketplace_is_the_id_suffix() {
        assert_eq!(
            marketplace_of("playbook@amplifiedengineer"),
            Some("amplifiedengineer")
        );
        assert_eq!(marketplace_of("playbook"), None);
        assert_eq!(marketplace_of("playbook@"), None);
    }

    #[test]
    fn a_plugin_that_is_off_or_a_false_config_key_runs_nothing() {
        // Arrange
        let home = scratch_dir("plugin-update-off");
        let dir = plugin(&home, "0.36.0");
        install(&home, "playbook@amplifiedengineer", &dir);
        let claude = fake_claude(&home, None, 0);
        fs::write(
            home.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"playbook@amplifiedengineer":false}}"#,
        )
        .unwrap();

        // Act
        let off = run(&Probe {
            home: home.clone(),
            claude: Some(claude.clone()),
        });
        fs::write(home.join(".claude/settings.json"), "{}").unwrap();
        let root = crate::common::paths::playbook_root_from(&home);
        fs::create_dir_all(&root).unwrap();
        crate::config::store::put(
            &root,
            crate::config::write::Tier::Global,
            None,
            CONFIG_KEY,
            &serde_json::json!("off"),
        )
        .unwrap();
        let config_off = run(&Probe {
            home: home.clone(),
            claude: Some(claude),
        });

        // Assert
        let (off, config_off) = (off.line().unwrap(), config_off.line().unwrap());
        assert!(off.contains("turned off in Claude Code"), "{off}");
        assert!(config_off.contains("upgrade.plugin is off"), "{config_off}");
        assert_eq!(calls(&home), "");
    }

    #[test]
    fn an_id_with_a_control_character_is_not_used() {
        let home = scratch_dir("plugin-update-ctl");
        fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            r#"{"version":2,"plugins":{"playbook@evil\u001b[2J":[{"scope":"user","installPath":"/x"}]}}"#,
        )
        .unwrap();

        assert_eq!(plugin_id(&home.join(".claude")), None);
    }

    #[test]
    fn mode_reads_each_word() {
        // Arrange
        let words = [("auto", Mode::Auto), ("ask", Mode::Ask), ("off", Mode::Off)];

        for (word, expected) in words {
            // Act
            let mode = mode_from(Some(&serde_json::json!(word)));

            // Assert
            assert_eq!(mode, expected, "{word}");
        }
        assert_eq!(
            mode_from(None),
            Mode::Auto,
            "a missing value is the default"
        );
    }

    #[test]
    fn a_stored_true_reads_as_auto_and_false_as_off() {
        // Arrange
        let home = scratch_dir("plugin-update-bool-mode");
        let root = crate::common::paths::playbook_root_from(&home);
        fs::create_dir_all(&root).unwrap();
        let put = |value: serde_json::Value| {
            crate::config::store::put(
                &root,
                crate::config::write::Tier::Global,
                None,
                CONFIG_KEY,
                &value,
            )
            .unwrap();
        };

        // Act
        put(serde_json::json!(true));
        let on = mode(&home);
        put(serde_json::json!(false));
        let off = mode(&home);

        // Assert
        assert_eq!((on, off), (Mode::Auto, Mode::Off));
    }

    #[test]
    fn every_allowed_word_for_the_key_parses_to_a_mode() {
        // Arrange
        let words = crate::config::keys::allowed_enum_values(CONFIG_KEY).unwrap();

        for word in words {
            // Act
            let mode = parse_mode(&serde_json::json!(word));

            // Assert
            assert!(mode.is_some(), "{word}");
        }
    }

    #[test]
    fn a_stored_ask_updates_the_plugin() {
        // Arrange
        let home = scratch_dir("plugin-update-ask");
        let dir = plugin(&home, "0.36.0");
        install(&home, "playbook@amplifiedengineer", &dir);
        let claude = fake_claude(&home, None, 0);
        let root = crate::common::paths::playbook_root_from(&home);
        fs::create_dir_all(&root).unwrap();
        crate::config::store::put(
            &root,
            crate::config::write::Tier::Global,
            None,
            CONFIG_KEY,
            &serde_json::json!(MODE_ASK),
        )
        .unwrap();

        // Act
        let out = run(&Probe {
            home: home.clone(),
            claude: Some(claude),
        });

        // Assert
        assert_eq!(
            calls(&home),
            "plugin marketplace update amplifiedengineer\nplugin update playbook@amplifiedengineer\n"
        );
        assert!(!matches!(out, Outcome::Skipped(_)), "{out:?}");
    }

    #[test]
    fn an_unknown_word_reads_as_auto() {
        // Arrange
        let unknown = serde_json::json!("sometimes");

        // Act
        let mode = mode_from(Some(&unknown));

        // Assert
        assert_eq!(mode, Mode::Auto);
        assert_eq!(parse_mode(&unknown), None);
    }

    #[test]
    fn a_fork_id_that_is_off_runs_nothing() {
        // Arrange
        let home = scratch_dir("plugin-update-fork-off");
        let dir = plugin(&home, "0.36.0");
        install(&home, "playbook@my-fork", &dir);
        let claude = fake_claude(&home, None, 0);
        fs::write(
            home.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"playbook@my-fork":false}}"#,
        )
        .unwrap();

        // Act
        let out = run(&Probe {
            home: home.clone(),
            claude: Some(claude),
        });

        // Assert
        assert!(out.line().unwrap().contains("turned off"));
        assert_eq!(calls(&home), "");
    }

    #[test]
    fn the_id_comes_from_installed_plugins_and_nothing_runs_without_the_plugin() {
        let home = scratch_dir("plugin-update-id");
        fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        assert_eq!(plugin_id(&home.join(".claude")), None);
        assert_eq!(
            run(&Probe {
                home: home.clone(),
                claude: None
            }),
            Outcome::NotInstalled
        );
        let dir = plugin(&home, "0.37.0");
        install(&home, "playbook@my-fork", &dir);
        assert_eq!(
            plugin_id(&home.join(".claude")).as_deref(),
            Some("playbook@my-fork")
        );
        install(&home, "playbook@amplifiedengineer", &dir);
        assert_eq!(
            plugin_id(&home.join(".claude")).as_deref(),
            Some("playbook@amplifiedengineer")
        );
    }

    const RUN: &str = "1.2.0";
    const T0: u64 = 1_000_000;
    const H24: u64 = 24 * 60 * 60;
    const M10: u64 = 10 * 60;

    fn marker(version: &str, state: State, at: u64) -> Option<Marker> {
        Some(Marker {
            version: version.into(),
            state,
            at,
        })
    }

    fn base() -> DueInputs<'static> {
        DueInputs {
            marker: marker("1.1.0", State::Done, T0),
            running: RUN,
            installed: Some("1.1.0"),
            mode: Mode::Auto,
            skip_env: None,
            headless: false,
            ci_vendor: false,
            disabled_in_claude: false,
            plugin_id: Some("playbook@amplifiedengineer"),
        }
    }

    #[test]
    fn due_from_applies_every_rule() {
        let not_due = |why| Due::NotDue(why);
        let cases: Vec<(&str, DueInputs, u64, Due)> = vec![
            ("older marker version", base(), T0, Due::Run(Mode::Auto)),
            (
                "ask is passed through",
                DueInputs {
                    mode: Mode::Ask,
                    ..base()
                },
                T0,
                Due::Run(Mode::Ask),
            ),
            (
                "no marker",
                DueInputs {
                    marker: None,
                    ..base()
                },
                T0,
                Due::Run(Mode::Auto),
            ),
            (
                "failed at 24h plus 1s",
                DueInputs {
                    marker: marker(RUN, State::Failed, T0),
                    ..base()
                },
                T0 + H24 + 1,
                Due::Run(Mode::Auto),
            ),
            (
                "failed at 24h minus 1s",
                DueInputs {
                    marker: marker(RUN, State::Failed, T0),
                    ..base()
                },
                T0 + H24 - 1,
                not_due("failed recently"),
            ),
            (
                "failed at exactly 24h",
                DueInputs {
                    marker: marker(RUN, State::Failed, T0),
                    ..base()
                },
                T0 + H24,
                not_due("failed recently"),
            ),
            (
                "behind at 24h plus 1m",
                DueInputs {
                    marker: marker(RUN, State::Behind, T0),
                    ..base()
                },
                T0 + H24 + 60,
                Due::Run(Mode::Auto),
            ),
            (
                "behind at 24h minus 1m",
                DueInputs {
                    marker: marker(RUN, State::Behind, T0),
                    ..base()
                },
                T0 + H24 - 60,
                not_due("behind, tried recently"),
            ),
            (
                "behind at exactly 24h",
                DueInputs {
                    marker: marker(RUN, State::Behind, T0),
                    ..base()
                },
                T0 + H24,
                not_due("behind, tried recently"),
            ),
            (
                "running at 10min plus 1s",
                DueInputs {
                    marker: marker(RUN, State::Running, T0),
                    ..base()
                },
                T0 + M10 + 1,
                Due::Run(Mode::Auto),
            ),
            (
                "running at 10min minus 1s",
                DueInputs {
                    marker: marker(RUN, State::Running, T0),
                    ..base()
                },
                T0 + M10 - 1,
                not_due("another process is running it"),
            ),
            (
                "a clock set back",
                DueInputs {
                    marker: marker(RUN, State::Running, T0),
                    ..base()
                },
                T0 - 5,
                not_due("another process is running it"),
            ),
            (
                "done for this version",
                DueInputs {
                    marker: marker(RUN, State::Done, T0),
                    ..base()
                },
                T0 + 9 * H24,
                not_due("already done for this version"),
            ),
            (
                "plugin equal, marker done",
                DueInputs {
                    marker: marker(RUN, State::Done, T0),
                    installed: Some(RUN),
                    ..base()
                },
                T0,
                not_due("already recorded as current"),
            ),
            (
                "plugin equal, no record",
                DueInputs {
                    installed: Some(RUN),
                    ..base()
                },
                T0,
                Due::Current,
            ),
            (
                "plugin newer",
                DueInputs {
                    installed: Some("1.3.0"),
                    ..base()
                },
                T0,
                Due::Current,
            ),
            (
                "a prerelease plugin is older than the release",
                DueInputs {
                    installed: Some("1.2.0-rc.1"),
                    ..base()
                },
                T0,
                Due::Run(Mode::Auto),
            ),
            (
                "plugin version unknown",
                DueInputs {
                    installed: None,
                    ..base()
                },
                T0,
                not_due("the installed plugin version is unknown"),
            ),
            (
                "mode off",
                DueInputs {
                    mode: Mode::Off,
                    ..base()
                },
                T0,
                not_due("upgrade.plugin is off"),
            ),
            (
                "skip env 1",
                DueInputs {
                    skip_env: Some("1"),
                    ..base()
                },
                T0,
                not_due("the skip variable is set"),
            ),
            (
                "skip env 0 does not skip",
                DueInputs {
                    skip_env: Some("0"),
                    ..base()
                },
                T0,
                Due::Run(Mode::Auto),
            ),
            (
                "headless",
                DueInputs {
                    headless: true,
                    ..base()
                },
                T0,
                not_due("headless run"),
            ),
            (
                "CI vendor",
                DueInputs {
                    ci_vendor: true,
                    ..base()
                },
                T0,
                not_due("CI run"),
            ),
            (
                "disabled in Claude Code",
                DueInputs {
                    disabled_in_claude: true,
                    ..base()
                },
                T0,
                not_due("the plugin is turned off in Claude Code"),
            ),
            (
                "no id",
                DueInputs {
                    plugin_id: None,
                    ..base()
                },
                T0,
                not_due("the plugin is not installed"),
            ),
            (
                "off wins over current",
                DueInputs {
                    mode: Mode::Off,
                    installed: Some(RUN),
                    ..base()
                },
                T0,
                not_due("upgrade.plugin is off"),
            ),
        ];
        for (name, inputs, now, expected) in cases {
            // Act
            let got = due_from(&inputs, now);

            // Assert
            assert_eq!(got, expected, "{name}");
        }
    }

    #[test]
    fn a_marker_round_trips_and_unparsable_text_is_not_a_marker() {
        // Arrange
        let m = Marker {
            version: "1.2.0-rc.1".into(),
            state: State::Failed,
            at: 42,
        };

        // Act
        let parsed = Marker::parse(&m.format());

        // Assert
        assert_eq!(parsed, Some(m));
        for bad in [
            "",
            "1.2.0",
            "1.2.0 done",
            "1.2.0 stuck 5",
            "1.2.0 done x",
            "1.2.0 done 5 6",
        ] {
            assert_eq!(Marker::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_behind_marker_round_trips_under_its_own_word() {
        // Arrange
        let m = Marker {
            version: "1.2.0".into(),
            state: State::Behind,
            at: 42,
        };

        // Act
        let text = m.format();

        // Assert
        assert_eq!(text, "1.2.0 behind 42");
        assert_eq!(Marker::parse(&text), Some(m));
    }

    #[test]
    fn an_unparsable_marker_counts_as_another_version() {
        // Arrange
        let inputs = DueInputs {
            marker: Marker::parse("garbage"),
            ..base()
        };

        // Act
        let got = due_from(&inputs, T0);

        // Assert
        assert_eq!(got, Due::Run(Mode::Auto));
    }

    fn store(tag: &str) -> PathBuf {
        let root = crate::common::paths::playbook_root_from(&scratch_dir(tag));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn a_claim_succeeds_once_for_callers_that_read_the_same_value() {
        // Arrange
        let root = store("pu-claim");
        let seen = seen(&root);

        // Act
        let first = claim(&root, seen.as_deref(), RUN, T0);
        let second = claim(&root, seen.as_deref(), RUN, T0);

        // Assert
        assert!(first);
        assert!(!second);
        assert_eq!(super::seen(&root), Some(format!("{RUN} running {T0}")));
    }

    #[test]
    fn a_claim_moves_from_the_value_it_read() {
        // Arrange
        let root = store("pu-claim-from");
        crate::state::set(&root, MARKER_KEY, "1.1.0 done 5").unwrap();

        // Act
        let stale = claim(&root, None, RUN, T0);
        let fresh = claim(&root, Some("1.1.0 done 5"), RUN, T0);

        // Assert
        assert!(!stale);
        assert!(fresh);
    }

    fn failed() -> Outcome {
        Outcome::Failed {
            id: "p@x".into(),
            why: "timeout".into(),
            install: false,
        }
    }

    fn current() -> Outcome {
        Outcome::Current {
            id: "p@x".into(),
            version: None,
        }
    }

    fn behind_outcome(marketplace: Option<&str>) -> Outcome {
        Outcome::Behind {
            id: "playbook@amplifiedengineer".into(),
            version: Some("0.36.0".into()),
            running: "0.37.0".into(),
            marketplace: marketplace.map(String::from),
        }
    }

    fn updated() -> Outcome {
        Outcome::Updated {
            id: "p@x".into(),
            from: None,
            to: None,
        }
    }

    #[test]
    fn finish_writes_failed_or_done_by_outcome() {
        let cases: Vec<(&str, Outcome, Option<&str>, State)> = vec![
            ("failed", failed(), Some(RUN), State::Failed),
            (
                "current but still older",
                current(),
                Some("1.1.0"),
                State::Behind,
            ),
            (
                "current, version unreadable",
                current(),
                None,
                State::Behind,
            ),
            (
                "current and at the running version",
                current(),
                Some(RUN),
                State::Done,
            ),
            ("updated", updated(), Some(RUN), State::Done),
            (
                "updated but still older",
                updated(),
                Some("1.1.0"),
                State::Behind,
            ),
            (
                "behind the binary",
                behind_outcome(None),
                Some("1.1.0"),
                State::Behind,
            ),
            (
                "behind after a failed refresh",
                behind_outcome(Some("git pull failed")),
                Some("1.1.0"),
                State::Behind,
            ),
            ("not installed", Outcome::NotInstalled, None, State::Done),
            (
                "no claude on PATH",
                Outcome::NoCli {
                    id: "p@x".into(),
                    install: false,
                },
                Some("1.1.0"),
                State::Failed,
            ),
        ];
        for (name, outcome, after, expected) in cases {
            // Arrange
            let root = store("pu-finish");

            // Act
            finish(&root, &outcome, RUN, after, T0);

            // Assert
            let written = Marker::parse(&seen(&root).unwrap()).unwrap();
            assert_eq!(
                written,
                Marker {
                    version: RUN.into(),
                    state: expected,
                    at: T0
                },
                "{name}"
            );
        }
    }

    #[test]
    fn a_plugin_on_the_binary_line_ends_done_whatever_its_patch() {
        // Arrange
        let cases = [
            ("current", current(), "1.2.0"),
            ("updated", updated(), "1.2.0"),
        ];
        for (name, outcome, installed) in cases {
            // Act
            let state = end_state(&outcome, "1.2.3", Some(installed));

            // Assert
            assert_eq!(state, State::Done, "{name}");
        }
    }

    #[test]
    fn a_behind_notice_waits_for_the_marketplace_and_names_no_failure() {
        // Arrange
        let waiting = behind_outcome(None);
        let refresh_failed = behind_outcome(Some("git pull failed"));

        // Act
        let waiting_line = notice_line(&waiting, State::Behind).unwrap();
        let failed_line = notice_line(&refresh_failed, State::Behind).unwrap();

        // Assert
        assert!(
            waiting_line.contains("the marketplace has not published a newer plugin yet"),
            "{waiting_line}"
        );
        assert!(
            waiting_line.contains("tries again after 24 hours"),
            "{waiting_line}"
        );
        assert!(!waiting_line.contains("failed"), "{waiting_line}");
        assert!(!waiting_line.contains(COMMAND_PREFIX), "{waiting_line}");
        assert!(failed_line.contains(COMMAND_PREFIX), "{failed_line}");
    }

    #[test]
    fn a_plugin_one_patch_behind_the_binary_stays_current() {
        // Arrange
        let home = scratch_dir("plugin-update-patch-lag");
        let dir = plugin(&home, "0.37.0");
        install(&home, "playbook@amplifiedengineer", &dir);
        let probe = Probe {
            home: home.clone(),
            claude: Some(fake_claude(&home, None, 0)),
        };

        // Act
        let outcome = run_for_binary(&probe, "0.37.3");

        // Assert
        assert!(matches!(outcome, Outcome::Current { .. }), "{outcome:?}");
    }

    #[test]
    fn a_claude_call_that_outlives_its_timeout_fails_with_the_limit_named() {
        // Arrange
        let home = scratch_dir("plugin-update-timeout");
        let bin = home.join("bin/claude");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        write_script(&bin, "#!/bin/sh\nsleep 5\n");
        let probe = Probe {
            home: home.clone(),
            claude: Some(bin.clone()),
        };

        // Act
        let result = run_claude(&probe, &bin, &["plugin", "update"], Duration::from_secs(1));

        // Assert: a start failure reads "could not be started", so this error
        // alone proves the call began and then outlived the limit. The script
        // logs nothing because a loaded machine can spend the whole second
        // before `sh` runs its first line.
        assert_eq!(result, Err("it did not finish within 1 s".to_string()));
    }

    #[test]
    fn a_claude_that_cannot_start_says_so_instead_of_naming_a_timeout() {
        // Arrange
        let home = scratch_dir("plugin-update-no-spawn");
        let missing = home.join("bin/claude");
        let probe = Probe {
            home,
            claude: Some(missing.clone()),
        };

        // Act
        let result = run_claude(
            &probe,
            &missing,
            &["plugin", "update"],
            Duration::from_secs(1),
        );

        // Assert
        assert_eq!(result, Err("it could not be started".to_string()));
    }

    #[test]
    fn an_install_with_an_unreadable_version_stays_installed() {
        // Arrange
        let home = scratch_dir("plugin-install-no-version");
        fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        fs::write(
            home.join(".claude/plugins/known_marketplaces.json"),
            r#"{"amplifiedengineer":{}}"#,
        )
        .unwrap();
        let probe = Probe {
            home: home.clone(),
            claude: Some(fake_claude(&home, None, 0)),
        };
        let opts = SyncOpts {
            allow_install: true,
            force: true,
            ..SyncOpts::default()
        };

        // Act
        let outcome = sync_at(&probe, opts, "0.41.0", &no_env);

        // Assert
        assert_eq!(
            outcome,
            Outcome::Installed {
                id: PLUGIN_KEY.to_string(),
                version: None,
            }
        );
    }

    #[test]
    fn a_current_plugin_is_recorded_done() {
        // Arrange
        let root = store("pu-record");

        // Act
        record_done(&root, RUN, T0);

        // Assert
        assert_eq!(seen(&root), Some(format!("{RUN} done {T0}")));
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn env_with<'a>(set: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            set.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    fn set_off(home: &Path) {
        let root = crate::common::paths::playbook_root_from(home);
        fs::create_dir_all(&root).unwrap();
        crate::config::store::put(
            &root,
            crate::config::write::Tier::Global,
            None,
            CONFIG_KEY,
            &serde_json::json!("off"),
        )
        .unwrap();
    }

    #[test]
    fn setup_is_blocked_by_the_skip_variable_a_ci_vendor_and_config_off() {
        // Arrange
        let open = scratch_dir("plugin-blocked-open");
        let off = scratch_dir("plugin-blocked-off");
        set_off(&off);

        // Act
        let cases = [
            ("nothing set", blocked_by(&open, &no_env), None),
            (
                "skip variable",
                blocked_by(&open, &env_with(&[(SKIP_ENV, "1")])),
                Some("PLAYBOOK_SKIP_PLUGIN_UPDATE is set"),
            ),
            (
                "skip variable not 1",
                blocked_by(&open, &env_with(&[(SKIP_ENV, "0")])),
                None,
            ),
            (
                "CI vendor",
                blocked_by(&open, &env_with(&[("GITHUB_ACTIONS", "true")])),
                Some("a CI environment was detected"),
            ),
            (
                "plain CI",
                blocked_by(&open, &env_with(&[("CI", "true")])),
                Some("a CI environment was detected"),
            ),
            (
                "headless switch",
                blocked_by(&open, &env_with(&[("PLAYBOOK_HEADLESS", "1")])),
                Some("a CI environment was detected"),
            ),
            (
                "config off",
                blocked_by(&off, &no_env),
                Some("upgrade.plugin is off"),
            ),
        ];

        // Assert
        for (name, got, expected) in cases {
            assert_eq!(got, expected, "{name}");
        }
    }

    #[test]
    fn the_setup_default_and_sync_agree_for_every_guard() {
        // Arrange
        let open = scratch_dir("plugin-agree-open");
        let off = scratch_dir("plugin-agree-off");
        set_off(&off);
        let cases: Vec<(&Path, Vec<(&str, &str)>)> = vec![
            (&open, vec![]),
            (&open, vec![(SKIP_ENV, "1")]),
            (&open, vec![("GITHUB_ACTIONS", "true")]),
            (&open, vec![("CI", "true")]),
            (&open, vec![("PLAYBOOK_HEADLESS", "1")]),
            (&off, vec![]),
        ];
        let install = SyncOpts {
            allow_install: true,
            ..SyncOpts::default()
        };

        for (home, vars) in cases {
            let env = env_with(&vars);
            let probe = Probe {
                home: home.to_path_buf(),
                claude: None,
            };

            // Act
            let blocked = blocked_by(home, &env);
            let outcome = sync_with_env(&probe, install, &env);

            // Assert
            let skipped = blocked.map(|why| Outcome::LeftAlone(why.to_string()));
            match skipped {
                Some(expected) => assert_eq!(outcome, expected, "{vars:?}"),
                None => assert!(!matches!(outcome, Outcome::Skipped(_)), "{vars:?}"),
            }
        }
    }

    #[test]
    fn an_install_caller_is_held_back_by_the_guard_unless_forced() {
        // Arrange
        let home = scratch_dir("plugin-sync-guard");
        let bin = fake_claude(&home, None, 0);
        let probe = Probe {
            home: home.clone(),
            claude: Some(bin),
        };
        let install = SyncOpts {
            allow_install: true,
            ..SyncOpts::default()
        };
        let forced = SyncOpts {
            force: true,
            ..install
        };

        // Act
        let ci = sync_with_env(&probe, install, &env_with(&[("GITHUB_ACTIONS", "true")]));
        let skipped = sync_with_env(&probe, install, &env_with(&[(SKIP_ENV, "1")]));
        let held_calls = calls(&home);
        sync_with_env(&probe, forced, &env_with(&[("GITHUB_ACTIONS", "true")]));

        // Assert
        assert_eq!(
            ci,
            Outcome::LeftAlone("a CI environment was detected".to_string())
        );
        assert_eq!(
            skipped,
            Outcome::LeftAlone("PLAYBOOK_SKIP_PLUGIN_UPDATE is set".to_string())
        );
        assert_eq!(held_calls, "");
        assert!(calls(&home).contains("plugin install"));
    }

    #[test]
    fn config_off_skips_an_install_caller_and_force_overrides_it() {
        // Arrange
        let home = scratch_dir("plugin-sync-off");
        set_off(&home);
        let bin = fake_claude(&home, None, 0);
        let probe = Probe {
            home: home.clone(),
            claude: Some(bin),
        };
        let install = SyncOpts {
            allow_install: true,
            ..SyncOpts::default()
        };

        // Act
        let held = sync_with_env(&probe, install, &no_env);
        let forced = sync_with_env(
            &probe,
            SyncOpts {
                force: true,
                ..install
            },
            &no_env,
        );

        // Assert
        assert_eq!(
            held,
            Outcome::LeftAlone("upgrade.plugin is off".to_string())
        );
        assert!(matches!(forced, Outcome::Installed { .. }), "{forced:?}");
    }

    #[test]
    fn run_keeps_updating_only_and_ignores_the_guard() {
        // Arrange
        let home = scratch_dir("plugin-run-default");
        let probe = Probe { home, claude: None };

        // Act
        let outcome = run(&probe);

        // Assert
        assert_eq!(outcome, Outcome::NotInstalled);
    }

    #[test]
    fn an_install_reads_with_a_restart_notice_and_ends_done() {
        // Arrange
        let installed = Outcome::Installed {
            id: PLUGIN_KEY.to_string(),
            version: Some("1.2.3".to_string()),
        };

        // Act
        let line = installed.line();
        let state = end_state(&installed, "1.2.3", Some("1.0.0"));

        // Assert
        assert_eq!(
            line.as_deref(),
            Some(
                "plugin: installed playbook@amplifiedengineer 1.2.3. Restart Claude Code to load it"
            )
        );
        assert_eq!(state, State::Done);
        assert_eq!(notice_line(&installed, state), line);
    }
}
