// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The optional parts of `playbook setup` and how each one gets decided: a flag
//! wins, else a question in a terminal, else the default.

use crate::init::{attribution, catalog, pb_link, system_prompt};
use clap::Args;
use std::io::{self, IsTerminal};

/// The flags, each part as a `--x` and `--no-x` pair. The last one given wins.
#[derive(Args, Debug, Default, Clone, PartialEq, Eq)]
pub struct Flags {
    /// Install or update the playbook plugin in Claude Code with the `claude` command. Given by name it runs even without a terminal, with `upgrade.plugin` off or in CI
    #[arg(long, overrides_with = "no_plugin")]
    pub plugin: bool,
    /// Leave the playbook plugin as it is
    #[arg(long, overrides_with = "plugin")]
    pub no_plugin: bool,
    /// Wire every playbook hook into settings.json (guards, session and worktree hooks)
    #[arg(long, overrides_with = "no_hooks")]
    pub hooks: bool,
    /// Add no playbook hook entries, including the guards in the shared settings
    #[arg(long, overrides_with = "hooks")]
    pub no_hooks: bool,
    /// Use the playbook status line (sets statusLine.command to `playbook statusline`)
    #[arg(long, overrides_with = "no_statusline")]
    pub statusline: bool,
    /// Do not set or change statusLine in settings.json
    #[arg(long, overrides_with = "statusline")]
    pub no_statusline: bool,
    /// Also apply the security defaults (permissions rules). Off unless asked or `security.defaults` is true
    #[arg(long, overrides_with = "no_security")]
    pub security: bool,
    /// Do not apply the security defaults, even when `security.defaults` is true
    #[arg(long, overrides_with = "security")]
    pub no_security: bool,
    /// Put the playbook binary on PATH for every shell start
    #[arg(long, overrides_with = "no_path")]
    pub path: bool,
    /// Do not edit shell files for PATH
    #[arg(long, overrides_with = "path")]
    pub no_path: bool,
    /// Retired: setup no longer writes a shell rc line (ADR 0044). Accepted and ignored until 1.0
    #[arg(long, hide = true, overrides_with = "no_aliases")]
    pub aliases: bool,
    /// Retired with `--aliases`
    #[arg(long, hide = true, overrides_with = "aliases")]
    pub no_aliases: bool,
    /// Install the playbook system prompt (an existing copy is refreshed either way)
    #[arg(long, overrides_with = "no_system_prompt")]
    pub system_prompt: bool,
    /// Do not install the system prompt. It stores `systemPrompt.enabled` false so `pb cc` starts with Claude Code's own
    #[arg(long, overrides_with = "system_prompt")]
    pub no_system_prompt: bool,
    /// Answer yes to the telemetry question. Writes the OpenTelemetry variables for the local usage collector into settings.json. Given alone it means `--only telemetry`
    #[arg(long, overrides_with = "no_telemetry")]
    pub telemetry: bool,
    /// Do not write telemetry variables into settings.json
    #[arg(long, overrides_with = "telemetry")]
    pub no_telemetry: bool,
    /// Retired: playbook starts the collector itself, so no service is installed. Accepted and ignored until 1.0
    #[arg(long, hide = true, overrides_with = "no_collector_service")]
    pub collector_service: bool,
    /// Retired with `--collector-service`
    #[arg(long, hide = true, overrides_with = "collector_service")]
    pub no_collector_service: bool,
    /// Retired: setup no longer defines a `claude` shell function (ADR 0044). Accepted and ignored until 1.0
    #[arg(long, hide = true, overrides_with = "no_claude_override")]
    pub claude_override: bool,
    /// Retired with `--claude-override`
    #[arg(long, hide = true, overrides_with = "claude_override")]
    pub no_claude_override: bool,
    /// Retired: the disk feature is gone. Accepted and ignored until 1.0
    #[arg(long, hide = true, overrides_with = "no_disk_ballast")]
    pub disk_ballast: bool,
    /// Retired with `--disk-ballast`
    #[arg(long, hide = true, overrides_with = "disk_ballast")]
    pub no_disk_ballast: bool,
    /// Add `pb` as a short name for `playbook`. Makes a `pb -> playbook` symlink in the install directory, only when `pb` is free
    #[arg(long, overrides_with = "no_short_name")]
    pub short_name: bool,
    /// Do not add the `pb` short name
    #[arg(long, overrides_with = "short_name")]
    pub no_short_name: bool,
    /// Install tab completion for `playbook` and `pb` in the shell named by `$SHELL`
    #[arg(long, overrides_with = "no_completions")]
    pub completions: bool,
    /// Do not install shell completion
    #[arg(long, overrides_with = "completions")]
    pub no_completions: bool,
    /// Turn the policy guard on (`hooks.policyGuard.enabled`). It denies skipped git hooks, a hand-run gh pr create, Claude Code memory writes and settings.json edits
    #[arg(long, overrides_with = "no_policy_guard")]
    pub policy_guard: bool,
    /// Do not turn the policy guard on; it is off by default and a stored choice is not changed
    #[arg(long, overrides_with = "policy_guard")]
    pub no_policy_guard: bool,
    #[arg(
        long,
        overrides_with_all = ["no_attribution", "attribution_off", "no_attribution_off"],
        help = "Attribution ON: Claude Code adds its Co-Authored-By trailer, generated-by line and session link. Removes each `attribution` key in settings.json that holds exactly the off value playbook writes, and stores `attribution.enabled` true"
    )]
    pub attribution: bool,
    #[arg(
        long,
        overrides_with_all = ["attribution", "attribution_off", "no_attribution_off"],
        help = "Attribution OFF. Claude Code adds no trailer, generated-by line or session link. Sets `attribution` in settings.json, replacing your own values. Setup applies this when you choose nothing and have not chosen ON before"
    )]
    pub no_attribution: bool,
    /// Deprecated name for `--no-attribution`, from 0.36 and 0.37.
    #[arg(long, hide = true, overrides_with_all = ["attribution", "no_attribution", "no_attribution_off"])]
    pub attribution_off: bool,
    /// Deprecated name for `--attribution`, from 0.36 and 0.37.
    #[arg(long, hide = true, overrides_with_all = ["attribution", "no_attribution", "attribution_off"])]
    pub no_attribution_off: bool,
    /// Register the playbook Bash guards in the hooks.json of Codex (off unless asked)
    #[arg(long, overrides_with = "no_codex_hooks")]
    pub codex_hooks: bool,
    /// Do not register guards in Codex
    #[arg(long, overrides_with = "codex_hooks")]
    pub no_codex_hooks: bool,
    #[arg(
        long,
        value_name = "PART",
        value_delimiter = ',',
        num_args = 1..,
        help = "Run only these parts and ask nothing. Parts, comma separated: plugin, hooks, statusline, security, path, system-prompt, telemetry, short-name, completions, policy-guard, attribution, codex-hooks. Everything else stays untouched. `playbook setup --only telemetry` turns telemetry on, and `--only attribution` applies attribution OFF unless you chose ON"
    )]
    pub only: Vec<String>,
    /// Never ask: use the default for every part you did not answer with a flag
    #[arg(long, short = 'y', visible_alias = "defaults")]
    pub yes: bool,
    /// Print what would change for each part and change nothing
    #[arg(long, conflicts_with = "explain")]
    pub dry_run: bool,
    /// Describe a part, or every part when none is named, and change nothing
    #[arg(long, value_name = "PART", num_args = 0..=1, conflicts_with = "dry_run")]
    pub explain: Option<Option<String>>,
}

/// What `init` will do for each optional part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choices {
    /// Whether to install or update the plugin through `claude`.
    pub plugin: bool,
    pub hooks: bool,
    pub statusline: bool,
    pub security: bool,
    pub path: bool,
    pub system_prompt: bool,
    pub telemetry: bool,
    pub short_name: bool,
    pub completions: bool,
    /// Whether to turn the policy guard on. Off unless chosen.
    pub policy_guard: bool,
    /// Attribution OFF (the default), ON, or left as it is.
    pub attribution: attribution::Choice,
    /// Whether to register the Bash guards in Codex. Off unless chosen.
    pub codex_hooks: bool,
}

fn pick(on: bool, off: bool) -> Option<bool> {
    if on {
        Some(true)
    } else if off {
        Some(false)
    } else {
        None
    }
}

pub(crate) fn part_pair(flags: &Flags, id: &str) -> Option<(bool, bool)> {
    Some(match id {
        "plugin" => (flags.plugin, flags.no_plugin),
        "hooks" => (flags.hooks, flags.no_hooks),
        "statusline" => (flags.statusline, flags.no_statusline),
        "security" => (flags.security, flags.no_security),
        "path" => (flags.path, flags.no_path),
        "system-prompt" => (flags.system_prompt, flags.no_system_prompt),
        "telemetry" => (flags.telemetry, flags.no_telemetry),
        "short-name" => (flags.short_name, flags.no_short_name),
        "completions" => (flags.completions, flags.no_completions),
        "policy-guard" => (flags.policy_guard, flags.no_policy_guard),
        "attribution" => (flags.attribution, flags.no_attribution),
        "codex-hooks" => (flags.codex_hooks, flags.no_codex_hooks),
        _ => return None,
    })
}

/// The part `--collector-service` set up until playbook started the
/// collector itself.
pub const COLLECTOR_SERVICE_PART: &str = "collector-service";

/// What setup prints for the retired collector-service part.
pub const COLLECTOR_SERVICE_RETIRED: &str =
    "the collector service is no longer needed; playbook starts the collector itself";

/// Removal: the retired flags and part names below are dropped in 1.0.0. The
/// flags came from releases up to 0.38.0, and a script or an old install guide
/// may still pass them, so clap keeps accepting them until then.
///
/// The part names setup took once and now ignores: the launchers ADR 0044
/// dropped, the disk ballast and the collector service.
pub const RETIRED_PARTS: [&str; 4] = [
    "aliases",
    "claude-override",
    "disk-ballast",
    COLLECTOR_SERVICE_PART,
];

/// One stderr line per retired flag or `--only` name in use, for a terminal.
pub fn retired_notes(flags: &Flags) -> Vec<String> {
    let set_by_flag = [
        flags.aliases || flags.no_aliases,
        flags.claude_override || flags.no_claude_override,
        flags.disk_ballast || flags.no_disk_ballast,
        flags.collector_service || flags.no_collector_service,
    ];
    RETIRED_PARTS
        .iter()
        .zip(set_by_flag)
        .filter(|(id, flag)| *flag || flags.only.iter().any(|n| n == *id))
        .map(|(id, _)| match *id {
            COLLECTOR_SERVICE_PART => format!("playbook setup: {COLLECTOR_SERVICE_RETIRED}"),
            "disk-ballast" => format!(
                "playbook setup: `{id}` is retired and does nothing; the disk feature was removed"
            ),
            _ => format!(
                "playbook setup: `{id}` is retired and does nothing; `playbook cc` and `pb cc` need no shell line (ADR 0044)"
            ),
        })
        .collect()
}

/// The `--only` and `--explain` name 0.36 and 0.37 gave the attribution part.
pub const OLD_ATTRIBUTION_PART: &str = "attribution-off";

/// Maps the attribution names 0.36 and 0.37 shipped to the current ones
/// (ADR 0042): `--attribution-off` is `--no-attribution` (OFF),
/// `--no-attribution-off` is `--attribution` (ON), and the part name
/// `attribution-off` is `attribution`. Returns one stderr line per old name.
pub fn map_deprecated(flags: &mut Flags) -> Vec<String> {
    let note =
        |old: &str, new: &str| format!("playbook setup: `{old}` is deprecated, use: `{new}`");
    let mut notes = Vec::new();
    if std::mem::take(&mut flags.attribution_off) {
        flags.no_attribution = true;
        notes.push(note("--attribution-off", "--no-attribution"));
    }
    // 0.37 took `--no-attribution-off` beside an `--only` without the part
    // as a no-op, so it stays one there.
    let only_other = !flags.only.is_empty()
        && !flags
            .only
            .iter()
            .any(|n| n == OLD_ATTRIBUTION_PART || n == attribution::PART);
    if std::mem::take(&mut flags.no_attribution_off) {
        flags.attribution = !only_other;
        notes.push(note("--no-attribution-off", "--attribution"));
    }
    let explain = flags.explain.as_mut().and_then(Option::as_mut);
    let old_names = flags.only.iter_mut().chain(explain);
    let mut renamed = false;
    for name in old_names.filter(|n| *n == OLD_ATTRIBUTION_PART) {
        *name = attribution::PART.to_string();
        renamed = true;
    }
    if renamed {
        notes.push(note(OLD_ATTRIBUTION_PART, attribution::PART));
    }
    notes
}

/// Maps the deprecated names, then prints the retired and deprecated notes
/// when stderr is a terminal (ADR 0040), and the `--no-attribution-off` note
/// always.
pub fn map_and_note(flags: &mut Flags) {
    // `--no-attribution-off` now writes settings, so a script sees its note too.
    let old_no = flags.no_attribution_off;
    let deprecated = map_deprecated(flags);
    let terminal = io::stderr().is_terminal();
    let notes = retired_notes(flags).into_iter().chain(deprecated);
    for note in notes.filter(|n| terminal || (old_no && n.contains("`--no-attribution-off`"))) {
        eprintln!("{note}");
    }
}

/// Whether `--only` names only retired parts, so setup has nothing to run.
pub fn only_retired(flags: &Flags) -> bool {
    !flags.only.is_empty()
        && flags
            .only
            .iter()
            .all(|n| RETIRED_PARTS.contains(&n.as_str()))
}

/// The parts `--only` selects. A bare `--telemetry` (no other part flag and
/// no `--yes`) means `--only telemetry`, so it never runs unrelated parts.
pub fn only_parts(flags: &Flags) -> Vec<String> {
    if !flags.only.is_empty() {
        return flags.only.clone();
    }
    let others = catalog::ids()
        .iter()
        .any(|id| *id != "telemetry" && part_pair(flags, id).is_some_and(|(on, off)| on || off));
    if flags.telemetry && !flags.no_telemetry && !others && !flags.yes {
        return vec!["telemetry".to_string()];
    }
    Vec::new()
}

/// The choices for an `--only` run: the named parts on, every other part off.
/// `Ok(None)` when `--only` is not in play.
pub fn only_choices(flags: &Flags) -> Result<Option<Choices>, String> {
    let named: Vec<String> = only_parts(flags)
        .into_iter()
        .filter(|n| !RETIRED_PARTS.contains(&n.as_str()))
        .collect();
    if named.is_empty() && flags.only.is_empty() {
        return Ok(None);
    }
    let valid: Vec<&'static str> = catalog::ids()
        .into_iter()
        .filter(|id| part_pair(flags, id).is_some())
        .collect();
    if let Some(bad) = named.iter().find(|n| !valid.contains(&n.as_str())) {
        return Err(format!(
            "unknown part `{bad}`. Choose one of: {}",
            valid.join(", ")
        ));
    }
    for id in &valid {
        let chosen = named.iter().any(|n| n == id);
        let (on, off) = part_pair(flags, id).unwrap_or((false, false));
        // Both attribution flags pick a state, so either may go with `--only`.
        let (on, off, typed) = match (*id == attribution::PART, off) {
            (true, true) => (true, false, format!("--no-{id}")),
            _ => (on, off, format!("--{id}")),
        };
        if chosen && off {
            return Err(format!("--only {id} conflicts with --no-{id}"));
        }
        if !chosen && on && !flags.only.is_empty() {
            return Err(format!(
                "{typed} is not in --only {}; add {id} to --only or drop the flag",
                flags.only.join(",")
            ));
        }
    }
    let has = |id: &str| named.iter().any(|n| n == id);
    Ok(Some(Choices {
        plugin: has("plugin"),
        hooks: has("hooks"),
        statusline: has("statusline"),
        security: has("security"),
        path: has("path"),
        system_prompt: has("system-prompt"),
        telemetry: has("telemetry"),
        short_name: has("short-name"),
        completions: has("completions"),
        policy_guard: has("policy-guard"),
        attribution: match has(attribution::PART) {
            true => attribution_from_flags(flags).unwrap_or(attribution::Choice::Default),
            false => attribution::Choice::Skip,
        },
        codex_hooks: has("codex-hooks"),
    }))
}

/// Whether a lone off switch the user set may be replaced: an answer to the
/// question (a run with no `--yes`), `--only`, or an explicit `--telemetry`.
/// `--yes` alone never replaces it.
pub fn replace_off_ok(flags: &Flags, telemetry: bool, only_run: bool) -> bool {
    telemetry && (flags.telemetry || only_run || !flags.yes)
}

/// The attribution state a flag picks: `--attribution` is ON, and
/// `--no-attribution` is OFF and replaces the user's own values. `None` when
/// neither flag is given.
fn attribution_from_flags(flags: &Flags) -> Option<attribution::Choice> {
    pick(flags.attribution, flags.no_attribution).map(|on| match on {
        true => attribution::Choice::On,
        false => attribution::Choice::Off { replace: true },
    })
}

/// What the short-name part does. A no or `--no-short-name` is recorded; a
/// run that asked nothing is not. A stored no (`declined`) wins over a default
/// that no question or flag confirmed, as under `--yes`.
pub fn short_name_choice(flags: &Flags, on: bool, asked: bool, declined: bool) -> pb_link::Choice {
    match (on, flags.no_short_name || asked) {
        (true, _) if declined && !asked && !flags.short_name => pb_link::Choice::Skip,
        (true, _) => pb_link::Choice::Link,
        (false, true) => pb_link::Choice::Declined,
        (false, false) => pb_link::Choice::Skip,
    }
}

/// The system prompt answer setup stores: a yes, an explicit no from the
/// flag or the wizard, or nothing asked.
pub fn system_prompt_answer(flags: &Flags, on: bool, asked: bool) -> system_prompt::Answer {
    match (on, flags.no_system_prompt || asked) {
        (true, _) => system_prompt::Answer::Yes,
        (false, true) => system_prompt::Answer::No,
        (false, false) => system_prompt::Answer::Unasked,
    }
}

/// Whether an earlier no or an install without the prompt may keep it off:
/// a run that asks nothing and took the default (ADR 0044 decision 5).
pub fn prompt_may_be_held_off(flags: &Flags, on: bool, asked: bool, only_run: bool) -> bool {
    on && !asked && !only_run && !flags.system_prompt
}

/// What an unattended run found about the parts it leaves off.
pub struct LeftOff {
    /// Setup already printed why telemetry is not asked about.
    pub telemetry_skipped: bool,
    /// `pb cc` would start without the playbook prompt.
    pub prompt_missing: bool,
    /// No `pb` command is on PATH.
    pub pb_missing: bool,
    /// The user chose attribution ON, which `--yes` keeps.
    pub attribution_on: bool,
}

/// The stderr lines for a run with no terminal and no `--yes`, one per
/// part it left off that a flag did not decide.
pub fn unattended_lines(flags: &Flags, choices: &Choices, left: &LeftOff) -> Vec<String> {
    let undecided = |on: bool, yes: bool, no: bool| !on && !yes && !no;
    let mut lines = Vec::new();
    let only = |what: &str, part: &str| {
        format!("setup: no terminal and no --yes, so {what}. To add it, run: playbook setup --only {part}")
    };
    if undecided(choices.plugin, flags.plugin, flags.no_plugin) {
        lines.push(only("the plugin was not installed or updated", "plugin"));
    }
    if undecided(choices.telemetry, flags.telemetry, flags.no_telemetry) && !left.telemetry_skipped
    {
        lines.push(only("telemetry was not turned on", "telemetry"));
    }
    if undecided(
        choices.system_prompt,
        flags.system_prompt,
        flags.no_system_prompt,
    ) && left.prompt_missing
    {
        lines.push(only("the system prompt was not added", "system-prompt"));
    }
    if undecided(choices.short_name, flags.short_name, flags.no_short_name) && left.pb_missing {
        lines.push(only("the pb link was not added", "short-name"));
    }
    let left_off: Vec<&str> = [
        (
            "hooks",
            undecided(choices.hooks, flags.hooks, flags.no_hooks),
        ),
        (
            "status line",
            undecided(choices.statusline, flags.statusline, flags.no_statusline),
        ),
        (
            "attribution off",
            choices.attribution == attribution::Choice::Skip
                && !flags.attribution
                && !flags.no_attribution
                && !left.attribution_on,
        ),
    ]
    .into_iter()
    .filter_map(|(name, off)| off.then_some(name))
    .collect();
    if let Some((last, rest)) = left_off.split_last() {
        let list = if rest.is_empty() {
            (*last).to_string()
        } else {
            format!("{} and {last}", rest.join(", "))
        };
        lines.push(format!(
            "setup: no terminal and no --yes, so these were left out of your Claude Code \
             settings: {list}. To add them, run: playbook setup --yes"
        ));
    }
    lines
}

/// True when there is no terminal to ask on and the user did not pass `--yes`.
pub fn unattended(flags: &Flags, terminal: bool) -> bool {
    !terminal && !flags.yes
}

/// Decide every part. `ask(question, default)` is called only for a part with
/// no flag, and only when `interactive` is true. `security_config` is the
/// resolved `security.defaults` key: the default for the security part, which
/// is never asked about, so a plain run stays off unless that key says on.
pub fn resolve(
    flags: &Flags,
    interactive: bool,
    security_config: bool,
    ask: &mut dyn FnMut(&str, bool) -> bool,
) -> Choices {
    let settings_default = !unattended(flags, interactive);
    resolve_with(
        flags,
        interactive,
        settings_default,
        settings_default,
        security_config,
        ask,
    )
}

/// The default answer for the plugin part: the settings default, and off
/// while [`crate::plugin_update::setup_blocked`] names a reason. The same
/// guard `sync` applies, so the part is never offered on what `sync` refuses.
pub fn plugin_default(settings_default: bool, blocked: Option<&str>) -> bool {
    settings_default && blocked.is_none()
}

/// Like `resolve`, with the default for the parts that change Claude Code's
/// settings given by the caller, and `plugin_default` for the plugin part. A
/// dry run in a terminal does not ask but still previews the answers a real
/// run would give.
pub fn resolve_with(
    flags: &Flags,
    interactive: bool,
    settings_default: bool,
    plugin_default: bool,
    security_config: bool,
    ask: &mut dyn FnMut(&str, bool) -> bool,
) -> Choices {
    let mut decide = |on: bool, off: bool, question: &str, default: bool| {
        pick(on, off).unwrap_or_else(|| {
            if interactive && !flags.yes {
                ask(question, default)
            } else {
                default
            }
        })
    };
    // Hooks and the status line are entries in Claude Code's settings. With
    // nobody to ask and no --yes, they stay off unless a flag turns them on.
    let choices = Choices {
        plugin: decide(
            flags.plugin,
            flags.no_plugin,
            catalog::question("plugin"),
            plugin_default,
        ),
        hooks: decide(
            flags.hooks,
            flags.no_hooks,
            catalog::question("hooks"),
            settings_default,
        ),
        statusline: decide(
            flags.statusline,
            flags.no_statusline,
            catalog::question("statusline"),
            settings_default,
        ),
        security: pick(flags.security, flags.no_security).unwrap_or(security_config),
        path: decide(flags.path, flags.no_path, catalog::question("path"), true),
        system_prompt: decide(
            flags.system_prompt,
            flags.no_system_prompt,
            catalog::question("system-prompt"),
            settings_default,
        ),
        telemetry: decide(
            flags.telemetry,
            flags.no_telemetry,
            catalog::question("telemetry"),
            settings_default,
        ),
        short_name: decide(
            flags.short_name,
            flags.no_short_name,
            catalog::question("short-name"),
            settings_default,
        ),
        completions: decide(
            flags.completions,
            flags.no_completions,
            catalog::question("completions"),
            settings_default,
        ),
        // It denies tool calls, so it stays off unless a flag or an answer
        // says yes, `--yes` included.
        policy_guard: decide(
            flags.policy_guard,
            flags.no_policy_guard,
            catalog::question("policy-guard"),
            false,
        ),
        attribution: attribution::Choice::Skip,
        // It writes a file outside Claude Code and needs a trust step in Codex,
        // so it stays off unless a flag or an answer says yes, `--yes` included.
        codex_hooks: decide(
            flags.codex_hooks,
            flags.no_codex_hooks,
            catalog::question("codex-hooks"),
            false,
        ),
    };
    // The question asks for ON, so a no, or Enter, is OFF that keeps the
    // user's own values.
    let attribution = attribution_from_flags(flags).unwrap_or_else(|| {
        if interactive && !flags.yes {
            match ask(catalog::question(attribution::PART), false) {
                true => attribution::Choice::On,
                false => attribution::Choice::Off { replace: false },
            }
        } else if settings_default {
            attribution::Choice::Default
        } else {
            attribution::Choice::Skip
        }
    });
    Choices {
        attribution,
        ..choices
    }
}

/// Whether both ends are a terminal, so a question can be shown and answered.
pub fn stdio_is_interactive() -> bool {
    crate::plugin_update::assumed_tty(true)
        || (io::stdin().is_terminal() && io::stdout().is_terminal())
}

/// A line for stderr when a person at a terminal is not asked because output
/// is piped, so the defaults that apply are not a silent surprise. `None`
/// when the run is interactive, scripted with `--yes`, or a dry run.
pub fn defaults_notice(stdin_tty: bool, stdout_tty: bool, flags: &Flags) -> Option<&'static str> {
    let undecided = [
        (flags.plugin, flags.no_plugin),
        (flags.hooks, flags.no_hooks),
        (flags.statusline, flags.no_statusline),
        (flags.path, flags.no_path),
        (flags.system_prompt, flags.no_system_prompt),
        (flags.telemetry, flags.no_telemetry),
        (flags.short_name, flags.no_short_name),
        (flags.completions, flags.no_completions),
        (flags.policy_guard, flags.no_policy_guard),
        (flags.attribution, flags.no_attribution),
    ]
    .iter()
    .any(|&(on, off)| pick(on, off).is_none());
    (undecided && stdin_tty && !stdout_tty && !flags.yes && !flags.dry_run).then_some(
        "setup: output is not a terminal, so no questions were asked; the plugin, hooks, status line, \
         system-prompt, telemetry, short-name, completions and policy-guard parts stay off, attribution is left as it is and the other defaults apply; pass --yes to accept all defaults or the \
         --x/--no-x flags (see --explain) to choose",
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn piped_output_at_a_terminal_gets_a_defaults_notice() {
        let flags = Flags::default();

        assert!(defaults_notice(true, false, &flags).is_some());
        assert!(defaults_notice(true, true, &flags).is_none());
        assert!(defaults_notice(false, false, &flags).is_none());
        let yes = Flags {
            yes: true,
            ..Flags::default()
        };
        assert!(defaults_notice(true, false, &yes).is_none());
        let all_set = Flags {
            no_plugin: true,
            hooks: true,
            no_statusline: true,
            path: true,
            system_prompt: true,
            no_telemetry: true,
            no_short_name: true,
            no_completions: true,
            no_policy_guard: true,
            no_attribution: true,
            ..Flags::default()
        };
        assert!(defaults_notice(true, false, &all_set).is_none());
        let dry = Flags {
            dry_run: true,
            ..Flags::default()
        };
        assert!(defaults_notice(true, false, &dry).is_none());
    }

    use super::*;

    fn never(_: &str, _: bool) -> bool {
        panic!("asked when it should not have");
    }

    #[test]
    fn with_yes_and_no_terminal_the_settings_parts_are_on() {
        let yes = Flags {
            yes: true,
            ..Flags::default()
        };
        let c = resolve(&yes, false, false, &mut never);
        assert!(c.hooks && c.statusline);
    }

    #[test]
    fn an_explicit_hooks_flag_is_consent_without_a_terminal() {
        let flags = Flags {
            hooks: true,
            ..Flags::default()
        };
        let c = resolve(&flags, false, false, &mut never);
        assert!(c.hooks && !c.statusline);
    }

    #[test]
    fn without_a_terminal_every_part_uses_its_default() {
        let c = resolve(&Flags::default(), false, false, &mut never);
        assert_eq!(
            c,
            Choices {
                plugin: false,
                hooks: false,
                statusline: false,
                security: false,
                path: true,
                system_prompt: false,
                telemetry: false,
                short_name: false,
                completions: false,
                policy_guard: false,
                attribution: attribution::Choice::Skip,
                codex_hooks: false
            }
        );
    }

    #[test]
    fn a_flag_wins_and_is_never_asked() {
        let flags = Flags {
            no_hooks: true,
            path: true,
            yes: false,
            ..Flags::default()
        };
        let mut asked = Vec::new();
        let c = resolve(&flags, true, false, &mut |q, d| {
            asked.push(q.to_string());
            d
        });
        assert!(!c.hooks);
        assert!(c.path);
        assert_eq!(asked.len(), 9, "{asked:?}");
        assert!(asked
            .iter()
            .all(|q| q != catalog::question("hooks") && q != catalog::question("path")));
    }

    #[test]
    fn the_policy_guard_stays_off_unless_a_flag_or_an_answer_says_yes() {
        let yes = Flags {
            yes: true,
            ..Flags::default()
        };
        assert!(!resolve(&yes, false, true, &mut never).policy_guard);
        assert!(!resolve(&yes, true, true, &mut never).policy_guard);
        let flag = Flags {
            yes: true,
            policy_guard: true,
            ..Flags::default()
        };
        assert!(resolve(&flag, false, true, &mut never).policy_guard);
        let answered = resolve(&Flags::default(), true, true, &mut |q, d| {
            q == catalog::question("policy-guard") || d
        });
        assert!(answered.policy_guard);
        let only = Flags {
            only: vec!["policy-guard".into()],
            ..Flags::default()
        };
        let c = only_choices(&only).unwrap().unwrap();
        assert!(c.policy_guard && !c.hooks);
    }

    #[test]
    fn a_short_name_no_is_recorded_only_when_said_and_a_stored_no_wins_over_a_default() {
        use pb_link::Choice::*;
        let flags = |on, off| Flags {
            short_name: on,
            no_short_name: off,
            ..Flags::default()
        };
        // (--short-name, --no-short-name, on, asked, stored no) -> choice
        for (on_flag, off_flag, on, asked, declined, want) in [
            (false, false, true, false, false, Link),
            (false, false, true, false, true, Skip),
            (true, false, true, false, true, Link),
            (false, false, true, true, true, Link),
            (false, false, false, true, false, Declined),
            (false, true, false, false, false, Declined),
            (false, false, false, false, false, Skip),
        ] {
            let got = short_name_choice(&flags(on_flag, off_flag), on, asked, declined);
            assert_eq!(got, want, "{on_flag} {off_flag} {on} {asked} {declined}");
        }
    }

    #[test]
    fn attribution_is_off_under_yes_and_a_flag_or_an_answer_picks_on_or_off() {
        use attribution::Choice::*;
        let flags = |yes, on, off| Flags {
            yes,
            attribution: on,
            no_attribution: off,
            ..Flags::default()
        };
        let q = catalog::question(attribution::PART);
        // (--yes, --attribution, --no-attribution, terminal, answer) -> choice
        for (yes, on_flag, off_flag, terminal, answer, want) in [
            (true, false, false, false, false, Default),
            (true, false, false, true, false, Default),
            (true, true, false, false, false, On),
            (true, false, true, false, false, Off { replace: true }),
            (false, false, false, false, false, Skip),
            (false, true, false, false, false, On),
            (false, false, true, false, false, Off { replace: true }),
            (false, false, false, true, true, On),
            (false, false, false, true, false, Off { replace: false }),
        ] {
            let got = resolve(
                &flags(yes, on_flag, off_flag),
                terminal,
                false,
                &mut |asked, d| {
                    if asked == q {
                        assert!(!d, "the default answer is OFF");
                        answer
                    } else {
                        d
                    }
                },
            )
            .attribution;
            assert_eq!(got, want, "{yes} {on_flag} {off_flag} {terminal} {answer}");
        }
    }

    #[test]
    fn only_attribution_takes_either_flag_and_refuses_one_outside_only() {
        use attribution::Choice::*;
        let only = |on, off| Flags {
            only: vec!["attribution".into()],
            attribution: on,
            no_attribution: off,
            ..Flags::default()
        };
        let outside = Flags {
            only: vec!["hooks".into()],
            no_attribution: true,
            ..Flags::default()
        };
        let run = |f: &Flags| only_choices(f).unwrap().unwrap().attribution;

        assert_eq!(run(&only(false, false)), Default);
        assert_eq!(run(&only(true, false)), On);
        assert_eq!(run(&only(false, true)), Off { replace: true });
        assert_eq!(
            run(&Flags {
                only: vec!["hooks".into()],
                ..Flags::default()
            }),
            Skip
        );
        let err = only_choices(&outside).unwrap_err();
        assert!(
            err.contains("--no-attribution is not in --only hooks"),
            "{err}"
        );
    }

    #[test]
    fn the_old_attribution_names_map_to_the_new_ones_with_a_note() {
        let mut off = Flags {
            attribution_off: true,
            only: vec!["attribution-off".into()],
            ..Flags::default()
        };
        let mut on = Flags {
            no_attribution_off: true,
            explain: Some(Some("attribution-off".into())),
            ..Flags::default()
        };
        let off_notes = map_deprecated(&mut off);
        let on_notes = map_deprecated(&mut on);

        assert!(off.no_attribution && !off.attribution && !off.attribution_off);
        assert_eq!(off.only, ["attribution"]);
        assert_eq!(
            off_notes,
            [
                "playbook setup: `--attribution-off` is deprecated, use: `--no-attribution`",
                "playbook setup: `attribution-off` is deprecated, use: `attribution`",
            ]
        );
        assert!(on.attribution && !on.no_attribution && !on.no_attribution_off);
        assert_eq!(on.explain, Some(Some("attribution".to_string())));
        assert_eq!(on_notes.len(), 2, "{on_notes:?}");
        assert!(on_notes[0].contains("use: `--attribution`"));
        assert!(map_deprecated(&mut Flags::default()).is_empty());
        let mut other = Flags {
            no_attribution_off: true,
            only: vec!["hooks".into()],
            ..Flags::default()
        };
        map_deprecated(&mut other);
        assert!(
            !other.attribution,
            "a no-op beside --only hooks, as in 0.37"
        );
        assert!(only_choices(&other).is_ok());
    }

    #[test]
    fn clap_hides_the_old_attribution_flags_and_the_last_flag_wins() {
        use clap::{CommandFactory, Parser};
        #[derive(Parser)]
        struct Setup {
            #[command(flatten)]
            flags: Flags,
        }
        let parse = |args: &[&str]| {
            let mut flags = Setup::try_parse_from(args).unwrap().flags;
            map_deprecated(&mut flags);
            (flags.attribution, flags.no_attribution)
        };

        let help = Setup::command().render_long_help().to_string();

        assert_eq!(
            parse(&["s", "--attribution-off", "--attribution"]),
            (true, false)
        );
        assert_eq!(
            parse(&["s", "--attribution", "--attribution-off"]),
            (false, true)
        );
        assert_eq!(parse(&["s", "--no-attribution-off"]), (true, false));
        assert!(help.contains("--no-attribution"));
        assert!(!help.contains("attribution-off"), "{help}");
    }

    #[test]
    fn a_terminal_asks_and_the_answer_decides() {
        let c = resolve(&Flags::default(), true, false, &mut |q, _| {
            q.contains("hooks")
        });
        assert!(c.hooks);
        assert!(!c.statusline && !c.path && !c.system_prompt);
    }

    #[test]
    fn yes_skips_every_question_and_turns_every_asked_part_on() {
        let flags = Flags {
            yes: true,
            ..Flags::default()
        };
        let c = resolve(&flags, true, false, &mut never);
        assert_eq!(
            c,
            Choices {
                plugin: true,
                hooks: true,
                statusline: true,
                security: false,
                path: true,
                system_prompt: true,
                telemetry: true,
                short_name: true,
                completions: true,
                policy_guard: false,
                attribution: attribution::Choice::Default,
                codex_hooks: false
            }
        );
    }

    #[test]
    fn the_statusline_question_comes_after_hooks_and_before_path() {
        let mut order = Vec::new();
        resolve(&Flags::default(), true, false, &mut |q, d| {
            order.push(q.to_string());
            d
        });
        let pos = |id: &str| {
            order
                .iter()
                .position(|q| q == catalog::question(id))
                .unwrap()
        };
        assert!(pos("hooks") < pos("statusline"));
        assert!(pos("statusline") < pos("path"));
        assert!(order.iter().any(|q| q == catalog::question("statusline")));
    }

    #[test]
    fn the_statusline_flags_decide_without_a_question() {
        let on = Flags {
            statusline: true,
            ..Flags::default()
        };
        let off = Flags {
            no_statusline: true,
            ..Flags::default()
        };
        assert!(
            resolve(&on, true, false, &mut |q, d| {
                assert!(q != catalog::question("statusline"));
                d
            })
            .statusline
        );
        assert!(!resolve(&off, false, false, &mut never).statusline);
    }

    #[test]
    fn security_is_off_unless_a_flag_or_the_config_key_turns_it_on() {
        let none = Flags::default();
        assert!(!resolve(&none, false, false, &mut never).security);
        assert!(resolve(&none, false, true, &mut never).security);
        let on = Flags {
            security: true,
            ..Flags::default()
        };
        assert!(resolve(&on, false, false, &mut never).security);
        let off = Flags {
            no_security: true,
            ..Flags::default()
        };
        assert!(!resolve(&off, false, true, &mut never).security);
    }

    #[test]
    fn yes_turns_telemetry_on_and_no_terminal_without_yes_leaves_it_off() {
        let yes = Flags {
            yes: true,
            ..Flags::default()
        };

        let with_yes = resolve(&yes, false, false, &mut never);
        let without = resolve(&Flags::default(), false, false, &mut never);

        assert!(with_yes.telemetry);
        assert!(!without.telemetry);
    }

    #[test]
    fn only_runs_the_named_parts_and_nothing_else() {
        let flags = Flags {
            only: vec!["telemetry".into(), "hooks".into()],
            ..Flags::default()
        };

        let c = only_choices(&flags).unwrap().unwrap();

        assert_eq!(
            c,
            Choices {
                plugin: false,
                hooks: true,
                statusline: false,
                security: false,
                path: false,
                system_prompt: false,
                telemetry: true,
                short_name: false,
                completions: false,
                policy_guard: false,
                attribution: attribution::Choice::Skip,
                codex_hooks: false
            }
        );
    }

    #[test]
    fn the_retired_parts_are_accepted_ignored_and_noted() {
        let flags = Flags {
            only: vec!["aliases".into()],
            claude_override: true,
            ..Flags::default()
        };

        let c = only_choices(&flags).unwrap().unwrap();
        let notes = retired_notes(&flags);

        assert!(!c.hooks && !c.path && !c.system_prompt && !c.short_name);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("`aliases` is retired") && notes[0].contains("ADR 0044"));
        assert!(notes[1].contains("`claude-override` is retired"));
        assert!(retired_notes(&Flags::default()).is_empty());
        let no_flag = Flags {
            no_aliases: true,
            ..Flags::default()
        };
        assert_eq!(retired_notes(&no_flag).len(), 1);
        assert!(resolve(&no_flag, false, false, &mut never).path);
        assert!(only_retired(&flags));
        assert!(!only_retired(&Flags {
            only: vec!["aliases".into(), "hooks".into()],
            ..Flags::default()
        }));
    }

    #[test]
    fn the_retired_disk_ballast_flags_and_part_are_accepted_and_noted() {
        for flags in [
            Flags {
                no_disk_ballast: true,
                ..Flags::default()
            },
            Flags {
                only: vec!["disk-ballast".into()],
                ..Flags::default()
            },
        ] {
            assert!(only_choices(&flags).is_ok(), "{flags:?}");
            let notes = retired_notes(&flags);
            assert_eq!(notes.len(), 1, "{notes:?}");
            assert!(notes[0].contains("`disk-ballast` is retired"));
        }
    }

    #[test]
    fn the_collector_service_flags_print_the_notice_and_change_nothing() {
        let notice = format!("playbook setup: {COLLECTOR_SERVICE_RETIRED}");
        let runs = [
            Flags {
                collector_service: true,
                ..Flags::default()
            },
            Flags {
                no_collector_service: true,
                ..Flags::default()
            },
            Flags {
                only: vec!["collector-service".into()],
                ..Flags::default()
            },
        ];

        assert!(!catalog::ids().contains(&COLLECTOR_SERVICE_PART));
        for flags in &runs {
            assert_eq!(retired_notes(flags), vec![notice.clone()], "{flags:?}");
            assert_eq!(part_pair(flags, COLLECTOR_SERVICE_PART), None);
        }
        let only = only_choices(&runs[2]).unwrap().unwrap();
        assert_eq!(only, all_off());
        assert!(only_retired(&runs[2]));
    }

    #[test]
    fn a_retired_collector_flag_beside_only_is_no_conflict() {
        let flags = Flags {
            only: vec!["hooks".into()],
            collector_service: true,
            ..Flags::default()
        };

        let c = only_choices(&flags).unwrap().unwrap();

        assert!(c.hooks && !c.path);
    }

    #[test]
    fn a_part_flag_outside_a_retired_only_names_what_was_typed() {
        let flags = Flags {
            only: vec!["aliases".into()],
            hooks: true,
            ..Flags::default()
        };

        let err = only_choices(&flags).unwrap_err();

        assert!(err.contains("--hooks is not in --only aliases;"), "{err}");
    }

    #[test]
    fn an_unknown_only_part_lists_the_valid_names() {
        let flags = Flags {
            only: vec!["nope".into()],
            ..Flags::default()
        };

        let err = only_choices(&flags).unwrap_err();

        assert!(
            err.contains("unknown part `nope`") && err.contains("telemetry"),
            "{err}"
        );
    }

    #[test]
    fn only_conflicts_with_the_no_flag_of_a_named_part_and_with_a_flag_for_an_unnamed_one() {
        let named = Flags {
            only: vec!["telemetry".into()],
            no_telemetry: true,
            ..Flags::default()
        };
        let unnamed = Flags {
            only: vec!["telemetry".into()],
            hooks: true,
            ..Flags::default()
        };

        let a = only_choices(&named).unwrap_err();
        let b = only_choices(&unnamed).unwrap_err();

        assert!(a.contains("conflicts with --no-telemetry"), "{a}");
        assert!(b.contains("--hooks is not in --only"), "{b}");
    }

    #[test]
    fn a_bare_telemetry_flag_means_only_telemetry_but_not_beside_other_flags() {
        let bare = Flags {
            telemetry: true,
            ..Flags::default()
        };
        let beside = Flags {
            telemetry: true,
            hooks: true,
            ..Flags::default()
        };
        let with_yes = Flags {
            telemetry: true,
            yes: true,
            ..Flags::default()
        };

        assert_eq!(only_parts(&bare), vec!["telemetry".to_string()]);
        assert!(only_parts(&beside).is_empty());
        assert!(only_parts(&with_yes).is_empty());
    }

    #[test]
    fn replacing_an_off_switch_needs_an_answer_an_explicit_flag_or_only_but_not_yes_alone() {
        let plain = Flags::default();
        let yes = Flags {
            yes: true,
            ..Flags::default()
        };
        let yes_flag = Flags {
            yes: true,
            telemetry: true,
            ..Flags::default()
        };

        assert!(replace_off_ok(&plain, true, false));
        assert!(!replace_off_ok(&yes, true, false));
        assert!(replace_off_ok(&yes_flag, true, false));
        assert!(replace_off_ok(&yes, true, true));
        assert!(!replace_off_ok(&plain, false, false));
    }

    fn all_off() -> Choices {
        Choices {
            plugin: false,
            hooks: false,
            statusline: false,
            security: false,
            path: false,
            system_prompt: false,
            telemetry: false,
            short_name: false,
            completions: false,
            policy_guard: false,
            attribution: attribution::Choice::Skip,
            codex_hooks: false,
        }
    }

    #[test]
    fn the_prompt_answer_is_yes_when_on_no_when_declined_and_unasked_otherwise() {
        let plain = Flags::default();
        let declined = Flags {
            no_system_prompt: true,
            ..Flags::default()
        };

        assert_eq!(
            system_prompt_answer(&plain, true, false),
            system_prompt::Answer::Yes
        );
        assert_eq!(
            system_prompt_answer(&declined, false, false),
            system_prompt::Answer::No
        );
        assert_eq!(
            system_prompt_answer(&plain, false, true),
            system_prompt::Answer::No
        );
        assert_eq!(
            system_prompt_answer(&plain, false, false),
            system_prompt::Answer::Unasked
        );
    }

    #[test]
    fn an_unattended_run_names_each_undecided_part_it_left_off() {
        let left = LeftOff {
            telemetry_skipped: false,
            prompt_missing: true,
            pb_missing: true,
            attribution_on: false,
        };

        let lines = unattended_lines(&Flags::default(), &all_off(), &left);

        assert_eq!(lines.len(), 5, "{lines:?}");
        assert!(lines[0].contains("--only plugin"), "{lines:?}");
        assert!(lines[1].contains("--only telemetry"), "{lines:?}");
        assert!(lines[2].contains("--only system-prompt"), "{lines:?}");
        assert!(lines[3].contains("--only short-name"), "{lines:?}");
        assert!(
            lines[4].contains("hooks, status line and attribution off"),
            "{lines:?}"
        );
    }

    #[test]
    fn an_unattended_run_stays_quiet_about_parts_a_flag_or_the_disk_decided() {
        let flags = Flags {
            no_plugin: true,
            no_telemetry: true,
            no_hooks: true,
            no_statusline: true,
            ..Flags::default()
        };
        let left = LeftOff {
            telemetry_skipped: false,
            prompt_missing: false,
            pb_missing: false,
            attribution_on: true,
        };

        assert!(unattended_lines(&flags, &all_off(), &left).is_empty());
    }

    #[test]
    fn plugin_part_truth_table() {
        struct Row {
            name: &'static str,
            flags: Flags,
            terminal: bool,
            blocked: Option<&'static str>,
            expected: bool,
        }
        let yes = Flags {
            yes: true,
            ..Flags::default()
        };
        let rows = [
            Row {
                name: "terminal default is yes",
                flags: Flags::default(),
                terminal: true,
                blocked: None,
                expected: true,
            },
            Row {
                name: "--yes is yes",
                flags: yes.clone(),
                terminal: false,
                blocked: None,
                expected: true,
            },
            Row {
                name: "no terminal is no",
                flags: Flags::default(),
                terminal: false,
                blocked: None,
                expected: false,
            },
            Row {
                name: "--no-plugin is no",
                flags: Flags {
                    no_plugin: true,
                    yes: true,
                    ..Flags::default()
                },
                terminal: true,
                blocked: None,
                expected: false,
            },
            Row {
                name: "--plugin without a terminal is yes",
                flags: Flags {
                    plugin: true,
                    ..Flags::default()
                },
                terminal: false,
                blocked: None,
                expected: true,
            },
            Row {
                name: "upgrade.plugin off defaults to no",
                flags: Flags::default(),
                terminal: true,
                blocked: Some("upgrade.plugin is off"),
                expected: false,
            },
            Row {
                name: "a CI environment defaults to no even with --yes",
                flags: yes,
                terminal: true,
                blocked: Some("a CI environment was detected"),
                expected: false,
            },
            Row {
                name: "--plugin overrides a block",
                flags: Flags {
                    plugin: true,
                    ..Flags::default()
                },
                terminal: false,
                blocked: Some("upgrade.plugin is off"),
                expected: true,
            },
        ];
        for row in rows {
            // Arrange
            let settings_default = !unattended(&row.flags, row.terminal);
            let plugin_default = plugin_default(settings_default, row.blocked);

            // Act
            let c = resolve_with(
                &row.flags,
                false,
                settings_default,
                plugin_default,
                false,
                &mut never,
            );

            // Assert
            assert_eq!(c.plugin, row.expected, "{}", row.name);
        }
    }

    #[test]
    fn only_plugin_selects_just_the_plugin_part() {
        // Arrange
        let flags = Flags {
            only: vec!["plugin".to_string()],
            ..Flags::default()
        };

        // Act
        let c = only_choices(&flags).unwrap().unwrap();

        // Assert
        assert!(c.plugin && !c.hooks && !c.path);
    }
}
