// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Orchestrates `playbook setup`: composes `merge`, `wire` and the other
//! parts into one idempotent repair, backing `Command::Init`.

use crate::init::merge;
use crate::init::migrate;
use crate::init::path;
use crate::init::system_prompt;
use crate::init::wire;
use std::fs;
use std::path::{Path, PathBuf};

/// Everything `run` needs to locate and repair a machine's Claude Code
/// configuration, resolved once by the caller (`main.rs`) so this module
/// never reads the environment itself and stays trivial to test against a
/// scratch directory.
#[derive(Clone)]
pub struct InitPaths {
    /// Whether the plugin step runs, and whether it overrides the guards.
    pub plugin: PluginRun,
    /// The installed playbook plugin (`plugin_root::resolve`), whose shipped
    /// skills init records to notice later edits. `None` when not installed.
    pub plugin_root: Option<PathBuf>,
    /// Where the shipped template, shell runtime and system prompt live.
    /// `None` when `CLAUDE_PLUGIN_ROOT` is unset and no plugin cache exists, in which case every step
    /// that needs it is skipped rather than guessing a path.
    pub self_root: Option<PathBuf>,
    /// `$HOME/.claude`, where `settings.json` and the launcher runtime live.
    pub claude_home: PathBuf,
    /// The user's home directory.
    pub home: PathBuf,
    /// The system prompt answer. `Yes` places the file and stores
    /// `systemPrompt.enabled` as true, `No` stores false, and `Unasked` only
    /// refreshes an installed copy; see `init::system_prompt`.
    pub system_prompt: system_prompt::Answer,
    /// Whether to wire the hook entries into settings.json.
    pub hooks: bool,
    /// Whether init may set or change `statusLine` in settings.json. False
    /// drops it from the merge and skips the legacy command migration.
    pub statusline: bool,
    /// Whether the settings merge also applies the security defaults
    /// (the `permissions` block). False leaves the user's value exactly as it
    /// is; see `merge_filtered`.
    pub security: bool,
    /// Whether to write the OpenTelemetry variables for the local collector
    /// into the `env` block of settings.json (never over an existing setup).
    pub telemetry: bool,
    /// Whether a lone `CLAUDE_CODE_ENABLE_TELEMETRY` off switch the user set
    /// may be replaced. True only for an answer to the question or an explicit
    /// `--telemetry`; `--yes` alone leaves a value the user set.
    pub telemetry_replace_off: bool,
    /// An `--only` run: only the parts that are on run, and the migrations,
    /// the trust entry and the old launcher notes are skipped.
    pub only: bool,
    /// What the short-name part does: skip, record a no, or link `pb`.
    pub short_name: crate::init::pb_link::Choice,
    /// Whether to turn the policy guard on (`hooks.policyGuard.enabled`).
    pub policy_guard: bool,
    /// Whether to register the Bash guards in the hooks.json of Codex.
    pub codex_hooks: bool,
    /// The shell to install completion for. `None` skips the `completions` step.
    pub completions: Option<crate::init::completion::Setup>,
    /// Where the `pb` link may go. `None` when the binary's location is unknown.
    pub pb_target: Option<crate::init::pb_link::Target>,
    /// What the attribution part does: skip, OFF (add the missing
    /// `attribution` keys, or also replace values the user set), or ON
    /// (remove the keys playbook wrote).
    pub attribution: crate::init::attribution::Choice,
    /// Where the binary lives and the shell to wire it for. `None` skips the
    /// `path` step.
    pub path_setup: Option<path::Setup>,
    /// `(repo_root, dest_base)` when init runs inside a repo with a resolvable slug.
    pub repo: Option<(PathBuf, PathBuf)>,
}

/// Whether setup runs the plugin step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginRun {
    Skip,
    /// Run it, but let `upgrade.plugin` off, CI and the skip variable stop it.
    Guarded,
    /// The user named the part with `--plugin` or `--only plugin`, so the
    /// guards do not stop it.
    Forced,
}

/// How one step of `run` landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// The step wrote a change.
    Wired,
    /// Nothing needed to change; already in the target shape.
    AlreadyCorrect,
    /// The step could not run, for a reason that is not itself a failure
    /// (missing `CLAUDE_PLUGIN_ROOT`, an unrecognised `$SHELL`).
    Skipped,
    /// The step tried and failed.
    Failed,
}

/// One step's result, ready to render as a report line.
pub struct StepReport {
    pub name: &'static str,
    pub status: StepStatus,
    pub detail: String,
}

impl StepReport {
    pub(crate) fn wired(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: StepStatus::Wired,
            detail: detail.into(),
        }
    }

    pub(crate) fn already_correct(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: StepStatus::AlreadyCorrect,
            detail: detail.into(),
        }
    }

    pub(crate) fn skipped(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: StepStatus::Skipped,
            detail: detail.into(),
        }
    }

    pub(crate) fn failed(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: StepStatus::Failed,
            detail: detail.into(),
        }
    }

    /// One terse line for a human, e.g. `hooks: wired - ...`.
    pub fn render(&self) -> String {
        let verb = match self.status {
            StepStatus::Wired => "wired",
            StepStatus::AlreadyCorrect => "ok",
            StepStatus::Skipped => "skipped",
            StepStatus::Failed => "FAILED",
        };
        format!("{}: {verb} - {}", self.name, self.detail)
    }
}

/// Every step's outcome, in run order.
pub struct InitOutcome {
    pub steps: Vec<StepReport>,
    /// Manual migration findings, for stderr.
    pub warnings: Vec<String>,
}

impl InitOutcome {
    /// Whether every step that attempted a change succeeded. A step that
    /// was deliberately skipped does not count against this.
    pub fn ok(&self) -> bool {
        !self.steps.iter().any(|s| s.status == StepStatus::Failed)
    }
}

/// Run every `init` step against `paths`. Copy steps run first; `settings`
/// only rewrites its file once its copy step is confirmed.
pub fn run(paths: &InitPaths) -> InitOutcome {
    // One stamp for every backup of this run, so a second boundary cannot split them.
    let epoch = crate::common::time::now_epoch_secs();
    // No reseed from the plugin after an install: it ships neither seed file (ADR 0034).
    let plugin_step = plugin_step(paths);
    let mut outcome = if paths.only {
        run_only(paths, epoch)
    } else {
        run_all(paths, epoch)
    };
    outcome.steps.splice(0..0, plugin_step);
    outcome
}

/// The plugin step, when it is on.
fn plugin_step(paths: &InitPaths) -> Option<StepReport> {
    use crate::plugin_update as pu;
    let force = match paths.plugin {
        PluginRun::Skip => return None,
        PluginRun::Guarded => false,
        PluginRun::Forced => true,
    };
    let opts = pu::SyncOpts {
        allow_install: true,
        force,
        progress: true,
    };
    let ensured = pu::ensure(
        &pu::Probe::for_home(&paths.home),
        opts,
        pu::Wanted::InstallOrUpdate,
        env!("CARGO_PKG_VERSION"),
    );
    pu::record_setup_run(&paths.home, &ensured.outcome);
    Some(plugin_report(&ensured.outcome, ensured.auto.as_ref()))
}

/// One report line for the outcome. A plugin that could not be installed or
/// updated is a skip with the command to run, never a failure, so setup
/// still exits 0.
fn plugin_report(
    outcome: &crate::plugin_update::Outcome,
    auto_update: Option<&crate::plugin_update::AutoUpdate>,
) -> StepReport {
    use crate::plugin_update::Outcome;
    let strip = |line: String| line.trim_start_matches("plugin: ").to_string();
    let mut detail = outcome
        .line()
        .map(strip)
        .unwrap_or_else(|| "the plugin is not installed".to_string());
    let auto_line = auto_update.and_then(|a| a.line()).map(strip);
    if let Some(line) = &auto_line {
        detail = format!("{detail}. {line}");
    }
    let set = auto_update == Some(&crate::plugin_update::AutoUpdate::Set);
    match outcome {
        Outcome::Installed { .. } | Outcome::Updated { .. } => StepReport::wired("plugin", detail),
        Outcome::Current { .. } if set => StepReport::wired("plugin", detail),
        Outcome::Current { .. } => StepReport::already_correct("plugin", detail),
        _ => StepReport::skipped("plugin", detail),
    }
}

fn run_all(paths: &InitPaths, epoch: u64) -> InitOutcome {
    let self_root = paths.self_root.as_deref();

    // The skills live in the `playbook` plugin, not in the root the seed files
    // come from, so the skill-edit tracking looks for them separately.
    let skills_root = paths.plugin_root.clone();
    let ctx = migrate::Ctx {
        home: paths.home.clone(),
        claude_home: paths.claude_home.clone(),
        self_root: skills_root.clone(),
        repo: paths.repo.clone(),
        epoch,
    };
    // Before the heal, so the record it writes sees the attribution answer.
    let attribution_step = attribution_step(paths, epoch);
    // Before the migrations, so the off-by-default notice sees a key this run stores.
    let policy_guard_step = policy_guard_step(paths);
    let migrated = migrate::run_pending_for(&ctx, paths.statusline, paths.hooks);
    // Record this binary in the version record, so the session hook does not
    // run the same migrations again (ADR 0034).
    let healed = crate::heal::run(
        &ctx,
        crate::heal::Trigger::Init {
            failed: migrated.failed || migrated.lock_busy,
            statusline: paths.statusline,
        },
        crate::common::time::now_epoch_secs(),
    );
    // The line that names the release notes of this upgrade (ADR 0037), on
    // stderr.
    eprint!("{}", healed.notices.render());

    let system_prompt_step = place_system_prompt_step(
        self_root,
        &paths.home,
        paths.system_prompt == system_prompt::Answer::Yes,
    );
    if step_confirmed(&system_prompt_step) {
        migrate::record_system_prompt(&paths.home);
    }
    let system_prompt_step = store_system_prompt_answer(paths, system_prompt_step);
    if let Some(root) = &skills_root {
        migrate::record_skills(&paths.home, root);
    }

    let (settings_step, hooks_step) = settings_and_hooks_steps(paths, epoch);

    let telemetry_step = telemetry_step(paths, epoch);
    let short_name_step = short_name_step(paths);
    let completions_step = completions_step(&paths.home, paths.completions.as_ref());
    let path_step = path_step(&paths.home, paths.path_setup.as_ref());

    let trust_step = trust_config_dir_step(&paths.home);

    let mut steps = vec![
        system_prompt_step,
        settings_step,
        hooks_step,
        telemetry_step,
        attribution_step,
        short_name_step,
        completions_step,
        policy_guard_step,
        codex_hooks_step(paths, epoch),
        path_step,
    ];
    steps.extend(migrated.steps);
    steps.push(trust_step);
    steps.extend(old_launcher_steps(
        &paths.home,
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    InitOutcome {
        steps,
        warnings: migrated.warnings,
    }
}

/// An `--only` run: just the parts that are on, nothing else.
fn run_only(paths: &InitPaths, epoch: u64) -> InitOutcome {
    let mut steps = Vec::new();
    if paths.system_prompt == system_prompt::Answer::Yes {
        let step = place_system_prompt_step(paths.self_root.as_deref(), &paths.home, true);
        if step_confirmed(&step) {
            migrate::record_system_prompt(&paths.home);
        }
        steps.push(store_system_prompt_answer(paths, step));
    }
    if paths.hooks || paths.statusline || paths.security {
        let (settings_step, hooks_step) = settings_and_hooks_steps(paths, epoch);
        steps.push(settings_step);
        if paths.hooks {
            steps.push(hooks_step);
        }
    }
    if paths.telemetry {
        steps.push(telemetry_step(paths, epoch));
    }
    if paths.attribution != crate::init::attribution::Choice::Skip {
        steps.push(attribution_step(paths, epoch));
    }
    if paths.short_name != crate::init::pb_link::Choice::Skip {
        steps.push(short_name_step(paths));
    }
    if paths.completions.is_some() {
        steps.push(completions_step(&paths.home, paths.completions.as_ref()));
    }
    if paths.policy_guard {
        steps.push(policy_guard_step(paths));
    }
    if paths.codex_hooks {
        steps.push(codex_hooks_step(paths, epoch));
    }
    if paths.path_setup.is_some() {
        steps.push(path_step(&paths.home, paths.path_setup.as_ref()));
    }
    InitOutcome {
        steps,
        warnings: Vec::new(),
    }
}

/// The settings merge and the hook wiring, in the order a real run does them.
/// `init --dry-run` runs the same code against a scratch copy of the files.
pub(crate) fn settings_and_hooks_steps(paths: &InitPaths, epoch: u64) -> (StepReport, StepReport) {
    let settings_path = paths.claude_home.join("settings.json");
    let self_root = paths.self_root.as_deref();
    let settings_step = if paths.hooks || paths.statusline || paths.security {
        seed_or_merge_settings(
            self_root,
            &paths.claude_home,
            &settings_path,
            epoch,
            paths.hooks,
            paths.security,
            paths.statusline,
        )
    } else {
        StepReport::skipped(
            "settings",
            "hooks, statusline and security are all off; settings.json is not merged",
        )
    };
    let mut settings_step = settings_step;
    if !paths.security && settings_step.status != StepStatus::Failed {
        settings_step
            .detail
            .push_str("; security defaults not applied, pass --security to opt in");
    }
    if !paths.statusline && has_playbook_status_line(&settings_path) {
        settings_step.detail.push_str(
            "; status line not changed, the existing playbook status line stays (`playbook uninstall` removes it)",
        );
    }
    let hooks_step = if paths.hooks {
        wire_hooks(&settings_path, epoch)
    } else {
        StepReport::skipped("hooks", "not requested; pass --hooks to opt in")
    };
    (settings_step, hooks_step)
}

/// The attribution part, as `paths.attribution` says.
pub(crate) fn attribution_step(paths: &InitPaths, epoch: u64) -> StepReport {
    crate::init::attribution::step(
        &paths.home,
        &paths.claude_home.join("settings.json"),
        epoch,
        paths.attribution,
    )
}

/// The short-name part, as `paths.short_name` says.
pub(crate) fn short_name_step(paths: &InitPaths) -> StepReport {
    crate::init::pb_link::step(&paths.home, paths.short_name, paths.pb_target.as_ref())
}

/// The policy-guard part: stores `hooks.policyGuard.enabled` true on a yes.
/// Without one it changes nothing, so a stored choice survives a rerun.
pub(crate) fn policy_guard_step(paths: &InitPaths) -> StepReport {
    use crate::hooks::policy_guard::{stored_enabled, ENABLED_KEY};
    const PART: &str = "policy-guard";
    if !paths.policy_guard {
        let detail = match stored_enabled(&paths.home) {
            true => format!(
                "not requested; {ENABLED_KEY} stays true, set it to false to turn the guard off"
            ),
            false => "not requested; pass --policy-guard to turn it on".to_string(),
        };
        return StepReport::skipped(PART, detail);
    }
    if stored_enabled(&paths.home) {
        return StepReport::already_correct(PART, format!("{ENABLED_KEY} is already true"));
    }
    match crate::config::write::set(
        crate::config::write::Tier::Global,
        ENABLED_KEY,
        &serde_json::Value::Bool(true),
        &paths.home,
        None,
    ) {
        Ok(_) => StepReport::wired(
            PART,
            format!("stored {ENABLED_KEY} true; `playbook config set --global {ENABLED_KEY} false` turns it off"),
        ),
        Err(err) => StepReport::failed(PART, format!("could not store {ENABLED_KEY}: {err}")),
    }
}

/// Whether Codex is installed: a `~/.codex` folder or a `codex` program on PATH.
pub(crate) fn codex_found(home: &Path) -> bool {
    let path = std::env::var("PATH").unwrap_or_default();
    !crate::skills_sync::detect(home, &path).is_empty()
}

/// The codex-hooks part: registers the Bash guards in `~/.codex/hooks.json`
/// when asked and when Codex is installed. Codex skips them until the user
/// trusts them in `/hooks`.
pub(crate) fn codex_hooks_step(paths: &InitPaths, epoch: u64) -> StepReport {
    use crate::init::codex_hooks;
    const PART: &str = "codex-hooks";
    if !paths.codex_hooks {
        return StepReport::skipped(
            PART,
            "not requested; pass --codex-hooks to register the guards in Codex",
        );
    }
    if !codex_found(&paths.home) {
        return StepReport::skipped(
            PART,
            "Codex was not found (no ~/.codex folder and no codex on PATH)",
        );
    }
    let file = codex_hooks::hooks_path(&paths.home);
    match codex_hooks::wire(&file, epoch) {
        Ok(out) if !out.changed => {
            StepReport::already_correct(PART, "the guards are already in ~/.codex/hooks.json")
        }
        Ok(_) => StepReport::wired(
            PART,
            "added the guards to ~/.codex/hooks.json; open /hooks in Codex and trust them, or Codex skips them. Codex runs on when a hook fails, so they are best effort",
        ),
        Err(err) => StepReport::failed(PART, format!("could not update ~/.codex/hooks.json: {err}")),
    }
}

/// The telemetry part: writes the collector variables when asked and when the
/// user has no telemetry setup of their own.
pub(crate) fn telemetry_step(paths: &InitPaths, epoch: u64) -> StepReport {
    if !paths.telemetry {
        return StepReport::skipped("telemetry", "not requested; pass --telemetry to opt in");
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| paths.home.clone());
    super::telemetry::apply(
        &paths.home,
        &paths.claude_home,
        &cwd,
        epoch,
        super::telemetry::process_configured_beyond_settings(&paths.claude_home),
        paths.telemetry_replace_off,
    )
}

/// Puts the binary's directory on PATH for every shell start. A failure is
/// reported, never fatal: the binary still runs by absolute path.
pub(crate) fn path_step(home: &Path, setup: Option<&path::Setup>) -> StepReport {
    let Some(setup) = setup else {
        return StepReport::skipped("path", "not requested, or the binary location is unknown");
    };
    match path::ensure(home, setup) {
        Ok(out) if !out.changed.is_empty() => {
            let files: Vec<String> = out
                .changed
                .iter()
                .map(|f| f.display().to_string())
                .collect();
            StepReport::wired(
                "path",
                format!(
                    "added {} to PATH in {}. Open a new terminal to pick it up.",
                    setup.bin_dir.display(),
                    files.join(", ")
                ),
            )
        }
        Ok(out) if !out.skipped.is_empty() => {
            let (file, why) = &out.skipped[0];
            StepReport::skipped(
                "path",
                format!(
                    "{}: {why}. Add {} to PATH by hand.",
                    file.display(),
                    setup.bin_dir.display()
                ),
            )
        }
        Ok(_) => StepReport::already_correct(
            "path",
            format!(
                "{} is already on PATH for new shells",
                setup.bin_dir.display()
            ),
        ),
        Err(err) => StepReport::failed("path", err.to_string()),
    }
}

/// Installs the completion files for one shell. A failure is reported,
/// never fatal: the binary works without completion.
pub(crate) fn completions_step(
    home: &Path,
    setup: Option<&crate::init::completion::Setup>,
) -> StepReport {
    use crate::init::completion;
    let Some(setup) = setup else {
        return StepReport::skipped("completions", "not requested");
    };
    let out = match completion::ensure(home, setup.shell) {
        Ok(out) => out,
        Err(err) => return StepReport::failed("completions", err.to_string()),
    };
    let names = |files: &[std::path::PathBuf]| {
        files
            .iter()
            .map(|f| f.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let written: Vec<_> = out.created.iter().chain(&out.changed).cloned().collect();
    let mut notes: Vec<String> = out
        .skipped
        .iter()
        .map(|(file, why)| match why.as_str() {
            completion::SKIP_NOT_OURS => format!("kept {} (not ours)", file.display()),
            _ => format!("{}: skipped, {why}", file.display()),
        })
        .collect();
    notes.extend(
        out.removed
            .iter()
            .map(|file| format!("removed {} (replaced by the loader)", file.display())),
    );
    notes.extend(
        out.kept
            .iter()
            .map(|file| format!("kept {} (edited)", file.display())),
    );
    if !written.is_empty() {
        let mut detail = format!(
            "wrote {}. Open a new terminal to pick it up.",
            names(&written)
        );
        for note in &notes {
            detail.push_str(&format!(" {note}."));
        }
        return StepReport::wired("completions", detail);
    }
    if !notes.is_empty() {
        return StepReport::skipped("completions", notes.join(". "));
    }
    StepReport::already_correct("completions", "shell completion is already installed")
}

/// Marks `~/.config/playbook` as trusted in `~/.claude.json`, so a session
/// started in that folder skips Claude Code's trust dialog. Best-effort: a
/// missing file or any failure is reported as skipped, never as a failure.
fn trust_config_dir_step(home: &Path) -> StepReport {
    let claude_json = home.join(".claude.json");
    let config_dir = home.join(".config").join("playbook");
    let dir = config_dir.to_string_lossy();
    if !claude_json.exists() {
        return StepReport::skipped("trust", "no ~/.claude.json yet; nothing to update");
    }
    if crate::trust::is_trusted(&claude_json, &dir) {
        return StepReport::already_correct("trust", "config dir already trusted");
    }
    match crate::trust::write_trust_entry(&claude_json, &dir) {
        Ok(()) => StepReport::wired("trust", format!("trusted {dir}")),
        Err(err) => StepReport::skipped("trust", format!("could not update ~/.claude.json: {err}")),
    }
}

/// Whether a copy step actually landed: `Skipped` does not count, since it
/// means the destination's state is unconfirmed, not verified complete.
pub(crate) fn step_confirmed(step: &StepReport) -> bool {
    matches!(step.status, StepStatus::Wired | StepStatus::AlreadyCorrect)
}

/// Stores the system prompt answer in `systemPrompt.enabled`, which decides
/// whether `pb cc` loads the prompt (ADR 0044). A run that asked nothing
/// stores nothing.
fn store_system_prompt_answer(paths: &InitPaths, step: StepReport) -> StepReport {
    let on = match paths.system_prompt {
        system_prompt::Answer::Yes => true,
        system_prompt::Answer::No => false,
        system_prompt::Answer::Unasked => return step,
    };
    match system_prompt::record(&paths.home, on) {
        Err(err) => StepReport::failed(
            "system-prompt",
            format!("could not store {}: {err}", system_prompt::KEY),
        ),
        Ok(()) if on => step,
        Ok(()) => StepReport::skipped(
            "system-prompt",
            "you said no, so `pb cc` starts with Claude Code's own prompt; to turn it on, run: playbook setup --only system-prompt",
        ),
    }
}

/// Step 6: place `prompts/SYSTEM_PROMPT.md`, which is opt-in. See
/// `init::system_prompt` for why `init` refreshes an existing copy but never
/// installs one the user did not ask for.
fn place_system_prompt_step(self_root: Option<&Path>, home: &Path, opt_in: bool) -> StepReport {
    let Some(self_root) = self_root else {
        return StepReport::skipped(
            "system-prompt",
            "no plugin root and no embedded copy could be written",
        );
    };
    match system_prompt::place_system_prompt(self_root, home, opt_in) {
        Ok(system_prompt::Placement::Placed(dest)) => {
            StepReport::wired("system-prompt", format!("placed at {}", dest.display()))
        }
        Ok(system_prompt::Placement::AlreadyCurrent(dest)) => StepReport::already_correct(
            "system-prompt",
            format!("already up to date at {}", dest.display()),
        ),
        Ok(system_prompt::Placement::UserEdited(dest)) => StepReport::skipped(
            "system-prompt",
            format!("left your edited copy at {}", dest.display()),
        ),
        Ok(system_prompt::Placement::NotShipped(source)) => StepReport::skipped(
            "system-prompt",
            format!("not shipped at {}", source.display()),
        ),
        Ok(system_prompt::Placement::NotOptedIn) => StepReport::skipped(
            "system-prompt",
            "not installed; pass --system-prompt to opt in",
        ),
        Err(err) => StepReport::failed("system-prompt", err.to_string()),
    }
}

/// Step 1: seed a fresh `settings.json` from the shipped template, or
/// three-way-merge an existing one, always through `merge::merge`. The
/// merge's BASE lives at `claude_home/.settings.base.json` and is refreshed
/// on every run regardless of whether `settings.json` itself changed, the
/// same way `init::merge::merge` already refreshes NEWBASE_OUT unconditionally.
///
/// A missing `settings.json` is treated as a user with zero customisations:
/// it is seeded as an empty object first, so `merge::merge` loads it validly
/// and adopts every template key through its ordinary "user never touched
/// this key" branch. The retired `setup-local.sh` instead special-cased a fresh install
/// as a verbatim template copy, which is byte-for-byte faithful to the
/// template's own key order on that one run, but `merge::merge` serialises
/// its OWN output with keys in sorted order (matching the python it ports),
/// so the very next `init` run would silently reorder the file and report a
/// spurious change. Going through `merge::merge` from the first run avoids
/// that: the file is in its final, stable shape immediately.
fn seed_or_merge_settings(
    self_root: Option<&Path>,
    claude_home: &Path,
    settings_path: &Path,
    epoch: u64,
    with_hooks: bool,
    with_security: bool,
    with_statusline: bool,
) -> StepReport {
    let Some(self_root) = self_root else {
        return StepReport::skipped(
            "settings",
            "no plugin root and no embedded copy could be written",
        );
    };
    let template_path = self_root.join("settings.shared.json");
    if !template_path.is_file() {
        return StepReport::skipped(
            "settings",
            format!("no template shipped at {}", template_path.display()),
        );
    }

    if !settings_path.is_file() {
        let init_empty =
            fs::create_dir_all(claude_home).and_then(|()| fs::write(settings_path, "{}\n"));
        if let Err(err) = init_empty {
            return StepReport::failed(
                "settings",
                format!("could not initialise {}: {err}", settings_path.display()),
            );
        }
    }

    let base_path = claude_home.join(".settings.base.json");
    forget_retired_keys(&base_path, &template_path);
    if !with_hooks || !with_security || !with_statusline {
        return merge_filtered(
            claude_home,
            &template_path,
            settings_path,
            &base_path,
            epoch,
            Filters {
                hooks: with_hooks,
                security: with_security,
                statusline: with_statusline,
            },
        );
    }
    match merge::merge(&base_path, &template_path, settings_path, &base_path, None) {
        Ok(mut outcome) => match own_refresh_interval(&mut outcome) {
            Ok(()) => finish_merge(settings_path, &outcome, epoch),
            Err(err) => StepReport::failed("settings", err),
        },
        Err(merge::MergeError::Validation(err)) => StepReport::failed("settings", err.to_string()),
        Err(merge::MergeError::Io(err)) => StepReport::failed("settings", err.to_string()),
    }
}

/// Playbook owns the refresh interval of its own status line, so a value the
/// three-way merge kept as a customisation, or a missing one, is replaced.
fn own_refresh_interval(outcome: &mut merge::MergeOutcome) -> Result<(), String> {
    let mut merged: serde_json::Value =
        serde_json::from_str(&outcome.stdout).map_err(|e| e.to_string())?;
    let changed = merged
        .get_mut("statusLine")
        .is_some_and(crate::init::statusline::set_refresh);
    if changed {
        outcome.stdout = serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Drop from the recorded base every top-level key the template no longer
/// ships. The three-way merge removes a key the template dropped when the user
/// never changed it, which would take a value out of an existing user's file
/// (older templates shipped display options and `env` entries). With the key
/// gone from the base, the merge sees a key only the user has, and keeps it.
fn forget_retired_keys(base_path: &Path, template_path: &Path) {
    let read = |p: &Path| {
        fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    };
    let (Some(mut base), Some(template)) = (read(base_path), read(template_path)) else {
        return;
    };
    let (Some(base_map), Some(template_map)) = (base.as_object_mut(), template.as_object()) else {
        return;
    };
    let retired: Vec<String> = base_map
        .keys()
        .filter(|k| !template_map.contains_key(*k))
        .cloned()
        .collect();
    if retired.is_empty() {
        return;
    }
    for key in retired {
        base_map.shift_remove(&key);
    }
    if let Ok(text) = serde_json::to_string_pretty(&base) {
        let _ = fs::write(base_path, format!("{text}\n"));
    }
}

/// Whether settings.json already points `statusLine.command` at playbook.
fn has_playbook_status_line(settings_path: &Path) -> bool {
    fs::read_to_string(settings_path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.pointer("/statusLine/command")
                .and_then(|c| c.as_str().map(|c| c.contains("playbook statusline")))
        })
        .unwrap_or(false)
}

/// Which optional slices of the template a merge applies.
#[derive(Clone, Copy)]
struct Filters {
    hooks: bool,
    security: bool,
    statusline: bool,
}

/// Point the template's security slice at the user's own values, so the merge
/// sees "nothing to change" there. The slice is the whole `permissions` key:
/// the user's value is copied in when they have one, and the entry is left out
/// of the template when they do not. Whatever
/// the user has, the merge hands back unchanged, and the recorded base keeps
/// describing what the user had, so a later opt-in merges as if this run had
/// never happened.
fn neutralise_security(template: &mut serde_json::Value, user: &serde_json::Value) {
    let Some(map) = template.as_object_mut() else {
        return;
    };
    for key in crate::settings::keys::SECURITY_KEYS {
        match user.get(key) {
            Some(v) => {
                map.insert((*key).to_string(), v.clone());
            }
            None => {
                map.shift_remove(*key);
            }
        }
    }
}

/// The settings merge when part of the template is left out.
///
/// `statusline: false` (`--no-statusline`): the template's `statusLine` is
/// dropped and the user's own value (or its absence) is restored after the
/// merge, so the three-way merge cannot add, change or remove it.
///
/// `hooks: false` (`--no-hooks`): the template's `hooks` block (the
/// `PreToolUse` guards) is dropped, and whatever hooks the user's file already
/// has are kept as they are. The merge runs against a scratch base, so the
/// recorded base still describes the full template and a later run with hooks
/// on merges normally.
///
/// `security: false` (the default): `neutralise_security` leaves the
/// permissions and `DISABLE_AUTOUPDATER` entries as the user has them. Init
/// stops adding them and never strips ones that are already there.
fn merge_filtered(
    claude_home: &Path,
    template_path: &Path,
    settings_path: &Path,
    base_path: &Path,
    epoch: u64,
    filters: Filters,
) -> StepReport {
    let scratch = |name: &str| claude_home.join(format!(".settings.{name}.{epoch}.tmp"));
    let (tpl_tmp, base_tmp) = (scratch("template-filtered"), scratch("base-filtered"));
    let result = (|| -> Result<merge::MergeOutcome, String> {
        let mut template: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(template_path).map_err(|e| e.to_string())?)
                .map_err(|e| format!("{}: {e}", template_path.display()))?;
        let user: serde_json::Value = fs::read_to_string(settings_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(serde_json::Value::Null);
        if !filters.hooks {
            if let Some(map) = template.as_object_mut() {
                map.shift_remove("hooks");
            }
        }
        if !filters.statusline {
            if let Some(map) = template.as_object_mut() {
                map.shift_remove("statusLine");
            }
        }
        if !filters.security {
            neutralise_security(&mut template, &user);
        }
        fs::write(&tpl_tmp, template.to_string()).map_err(|e| e.to_string())?;
        // Without hooks the recorded base must keep describing the full
        // template, so the merge works on a scratch copy of it.
        let (base_in, base_out) = if filters.hooks && filters.statusline {
            (base_path, base_path)
        } else {
            if base_path.is_file() {
                fs::copy(base_path, &base_tmp).map_err(|e| e.to_string())?;
            }
            (base_tmp.as_path(), base_tmp.as_path())
        };
        let mut outcome = merge::merge(base_in, &tpl_tmp, settings_path, base_out, None).map_err(
            |e| match e {
                merge::MergeError::Validation(err) => err.to_string(),
                merge::MergeError::Io(err) => err.to_string(),
            },
        )?;
        if !filters.statusline {
            let mut merged: serde_json::Value =
                serde_json::from_str(&outcome.stdout).map_err(|e| e.to_string())?;
            if let Some(map) = merged.as_object_mut() {
                match user.get("statusLine") {
                    Some(line) => {
                        map.insert("statusLine".to_string(), line.clone());
                    }
                    None => {
                        map.remove("statusLine");
                    }
                }
            }
            outcome.stdout = serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?;
        }
        if filters.statusline {
            own_refresh_interval(&mut outcome)?;
        }
        if !filters.hooks {
            let mut merged: serde_json::Value =
                serde_json::from_str(&outcome.stdout).map_err(|e| e.to_string())?;
            if let Some(map) = merged.as_object_mut() {
                match user.get("hooks") {
                    Some(hooks) => {
                        map.insert("hooks".to_string(), hooks.clone());
                    }
                    None => {
                        map.remove("hooks");
                    }
                }
            }
            outcome.stdout = serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?;
        }
        Ok(outcome)
    })();
    let _ = fs::remove_file(&tpl_tmp);
    let _ = fs::remove_file(&base_tmp);
    match result {
        Ok(outcome) => finish_merge(settings_path, &outcome, epoch),
        Err(err) => StepReport::failed("settings", err),
    }
}

/// Turn a completed merge into a `StepReport`, comparing its rendered output
/// against what is already on disk before writing anything: a no-op merge
/// (the common case on a re-run) neither takes a backup nor rewrites the
/// file, mirroring `wire::wire`'s own idempotence check.
fn finish_merge(settings_path: &Path, outcome: &merge::MergeOutcome, epoch: u64) -> StepReport {
    let rendered = format!("{}\n", outcome.stdout);
    let existing = fs::read_to_string(settings_path).unwrap_or_default();
    if rendered == existing {
        return StepReport::already_correct("settings", "already matches the template");
    }
    match backup_then_write(settings_path, &rendered, &outcome.skipped, epoch) {
        Ok(()) => StepReport::wired(
            "settings",
            format!(
                "merged the template in ({} customisation(s) preserved)",
                outcome.skipped.len()
            ),
        ),
        Err(err) => StepReport::failed(
            "settings",
            format!("could not write {}: {err}", settings_path.display()),
        ),
    }
}

/// Copy `path` to a timestamped sibling before overwriting it with
/// `content`, the same safety net `init::wire::wire` gives its own
/// `settings.json` changes. The merge step and the dead hook migration
/// (`init::dead_hooks`) both write through it, with the run's one epoch.
///
/// Also writes `skipped` beside the backup, as
/// `settings-merge-skipped.<epoch>.json`, using the SAME epoch as the backup
/// so the two files one real write produces are easy to pair up by eye;
/// written only when `skipped` is non-empty, since an idempotent re-run
/// never reaches this function at all (`finish_merge` returns before calling
/// it), and a real write that withheld nothing has no report worth keeping.
/// Reuses `merge::render_skip_report` rather than re-deriving the shape, so
/// this stays byte-for-byte the same shape `merge::merge`'s own SKIP_OUT
/// would have written, without going through `merge::merge`'s `skip_out`
/// parameter itself: that parameter writes unconditionally whenever `Some`,
/// even an empty array (N3, pinned by `tests/init_merge.rs`'s
/// `n3_zero_withheld_keys_writes_empty_skip_array`), and its target path
/// would have to be decided before `merge::merge` runs, before this
/// function's epoch even exists. `outcome.skipped` is already in memory by
/// the time `finish_merge` calls this, so no second call or disk round-trip
/// is needed to get it.
///
/// Both file families this function can produce are unbounded without
/// pruning, so after writing, `prune_family` retains only the 5
/// newest-epoch files in each: the `.bak.<epoch>` backups and the
/// `settings-merge-skipped.<epoch>.json` reports. This runs only on this,
/// the real-write path; `finish_merge`'s idempotent short-circuit means an
/// idempotent re-run never prunes either family, matching the same "nothing
/// changed, nothing happens" rule the write itself already follows.
///
/// A backup already present under `epoch` is kept, so the first write of a
/// run (or of a second) leaves the pre-change copy and the later ones add none.
pub(crate) fn backup_then_write(
    path: &Path,
    content: &str,
    skipped: &[merge::SkippedEntry],
    epoch: u64,
) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    if path.is_file() {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "settings.json".to_string());
        let backup = path.with_file_name(format!("{file_name}.bak.{epoch}"));
        // The first backup of a run holds the pre-run state; keep it.
        if !backup.exists() {
            fs::copy(path, &backup)?;
        }

        if !skipped.is_empty() {
            fs::write(
                path.with_file_name(format!("settings-merge-skipped.{epoch}.json")),
                merge::render_skip_report(skipped),
            )?;
        }

        prune_family(dir, &format!("{file_name}.bak."), "");
        prune_family(dir, "settings-merge-skipped.", ".json");
    }

    fs::create_dir_all(dir)?;
    crate::common::atomic::write_atomic(path, content)
}

/// The retain-5 pruning policy for both of `backup_then_write`'s file
/// families: keep only the 5 files directly in `dir` named
/// `{prefix}<epoch>{suffix}` with the highest embedded epoch, deleting the
/// rest. Parses the epoch back out of each matching file name rather than
/// trusting file modification times, since a fabricated or copied file's
/// mtime need not agree with the epoch its own name claims, and the epoch in
/// the name is what both families are already keyed by everywhere else. A
/// name matching `{prefix}...{suffix}` whose middle segment does not parse
/// as a `u64` is left alone rather than guessed about; this only ever runs
/// against files this crate itself named, so an unparsable match is not
/// expected in practice. Best-effort like the rest of this module's writes:
/// a `read_dir` or `remove_file` failure is swallowed rather than turned
/// into a step failure, since a pruning miss leaves stale files behind
/// rather than losing data.
fn prune_family(dir: &Path, prefix: &str, suffix: &str) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(u64, PathBuf)> = read_dir
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let epoch_str = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
            let epoch: u64 = epoch_str.parse().ok()?;
            Some((epoch, entry.path()))
        })
        .collect();
    if entries.len() <= 5 {
        return;
    }
    entries.sort_unstable_by_key(|(epoch, _)| *epoch);
    let excess = entries.len() - 5;
    for (_, stale_path) in entries.into_iter().take(excess) {
        let _ = fs::remove_file(stale_path);
    }
}

/// Step 2: upsert the ported hooks and guards into `settings.json`. Every
/// `GUARD_SPECS` entry is wired (WU-13), so `wire` wires every hook
/// and guard unconditionally; WU-14 dropped the `placed_guards` and
/// `claude_home` parameters `wire::wire` used to take, since the gate they
/// fed had become permanently unreachable.
fn wire_hooks(settings_path: &Path, epoch: u64) -> StepReport {
    match wire::wire_at(settings_path, epoch) {
        Ok(outcome) if outcome.changed => {
            StepReport::wired("hooks", "wired the ported hooks into settings.json")
        }
        Ok(_) => StepReport::already_correct("hooks", "all hooks already wired"),
        Err(err) => StepReport::failed("hooks", err.to_string()),
    }
}

/// One skipped step per old `ccc` or `ccd` launcher found. Init only tells the
/// user what replaces it and what to remove; it never edits or deletes them.
pub(crate) fn old_launcher_steps(home: &Path, path: &std::ffi::OsStr) -> Vec<StepReport> {
    crate::doctor::launchers::find(home, path)
        .iter()
        .map(|f| StepReport::skipped("old-launcher", crate::doctor::launchers::describe(f)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;

    #[test]
    fn a_backup_under_the_same_stamp_is_kept_and_the_write_still_lands() {
        let dir = scratch_dir("init-backup-kept");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, "current").unwrap();
        let backup = dir.join("settings.json.bak.1700000000");
        fs::write(&backup, "pre-run original").unwrap();

        backup_then_write(&path, "new", &[], 1_700_000_000).unwrap();

        assert_eq!(fs::read_to_string(&backup).unwrap(), "pre-run original");
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }
}
