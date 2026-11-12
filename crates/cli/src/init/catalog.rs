// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! What `playbook setup` is made of, in words a person reads before saying yes.
//! One table of parts lives here. The plan, the questions, `--explain` and
//! the summary after a run all draw from it, so a description is written once.
//! The hooks live in one table too, in `init::wire`.

use crate::init::wire::{wired_hooks, Group, HookSpec};
use std::fmt::Write as _;

/// One part of `init`.
pub struct Part {
    /// The name used by `--explain <part>`.
    pub id: &'static str,
    /// Whether `init` asks about it. A part without a question runs every time.
    pub asked: bool,
    /// On or off when no flag, no answer and no config says otherwise.
    pub default_on: bool,
    /// The flag pair that decides it, empty when there is none.
    pub flags: &'static str,
    /// The question text. Names the files the part edits. Empty when not asked.
    pub question: &'static str,
    /// One line: what the part does.
    pub does: &'static str,
    /// The files it edits.
    pub changes: &'static str,
    /// How to undo it.
    pub undo: &'static str,
}

const SETTINGS_UNDO: &str =
    "`playbook uninstall` removes what `playbook setup` wrote. Backups of settings.json are saved next to it.";

/// Every part, in the order `init` shows and runs them.
pub const PARTS: &[Part] = &[
    Part {
        id: "plugin",
        asked: true,
        default_on: true,
        flags: "--plugin, --no-plugin",
        question: "Install or update the playbook plugin in Claude Code with the `claude` command? Claude Code keeps its plugins in ~/.claude/plugins.",
        does: "Keeps the playbook plugin in step with this binary. It refreshes the amplifiedengineer marketplace and updates the plugin, or adds the marketplace and installs the plugin when none is installed, by running `claude plugin` commands. It also turns on auto-update for the amplifiedengineer marketplace in ~/.claude/settings.json when that key is missing, and keeps an explicit false. It stays off without a terminal and without `--yes`, and with `--yes` when `upgrade.plugin` is off, `PLAYBOOK_SKIP_PLUGIN_UPDATE` is set or a CI environment is detected. `--plugin` or `--only plugin` runs it anyway. A plugin you turned off in Claude Code stays off. A failure is reported and setup still succeeds.",
        changes: "Claude Code's plugins (~/.claude/plugins, written by claude itself) and ~/.claude/settings.json (extraKnownMarketplaces.amplifiedengineer.autoUpdate)",
        undo: "Run `claude plugin uninstall playbook@amplifiedengineer` and `claude plugin marketplace remove amplifiedengineer`, and set extraKnownMarketplaces.amplifiedengineer.autoUpdate to false in ~/.claude/settings.json.",
    },
    Part {
        id: "hooks",
        asked: true,
        default_on: true,
        flags: "--hooks, --no-hooks",
        question: "Wire the playbook hooks into ~/.claude/settings.json? (see them with `playbook hooks list`)",
        does: "Adds playbook's hooks: safety guards, session helpers, memory and token hygiene checks, and a background warm-up of the usage store. Also removes hook entries whose ~/.claude/hooks script is gone, after a backup.",
        changes: "~/.claude/settings.json (the hooks block) and ~/.claude/.settings.base.json",
        undo: SETTINGS_UNDO,
    },
    Part {
        id: "statusline",
        asked: true,
        default_on: true,
        flags: "--statusline, --no-statusline",
        question: "Set the status line in ~/.claude/settings.json to `playbook statusline`?",
        does: "Shows model, git, PR and cost info at the bottom of a session.",
        changes: "~/.claude/settings.json (the statusLine key)",
        undo: SETTINGS_UNDO,
    },
    Part {
        id: "security",
        asked: false,
        default_on: false,
        flags: "--security, --no-security",
        question: "",
        does: "Adds permission rules (allow, ask, deny). Off unless you pass --security or set `security.defaults`. It is never asked about. Update control stays with you, `playbook upgrade` and Homebrew.",
        changes: "~/.claude/settings.json (the permissions block)",
        undo: "Run `playbook setup --no-security`, then delete the permissions entries from settings.json by hand. Setup never removes a value you have.",
    },
    Part {
        id: "path",
        asked: true,
        default_on: true,
        flags: "--path, --no-path",
        question: "Add the playbook binary's directory to PATH in your shell startup files (~/.zshenv for zsh, ~/.bashrc and a login file for bash)?",
        does: "Puts the playbook binary on PATH so hooks find it.",
        changes: "your shell startup files: ~/.zshenv (zsh), ~/.bashrc and a login file (bash), ~/.config/fish/conf.d/playbook.fish (fish), ~/.profile (other shells)",
        undo: "`playbook uninstall --remove-binary` removes the block, which starts with `# playbook binary`.",
    },
    Part {
        id: "system-prompt",
        asked: true,
        default_on: true,
        flags: "--system-prompt, --no-system-prompt",
        question: "Copy the playbook system prompt to ~/.config/playbook/prompts/SYSTEM_PROMPT.md?",
        does: "Copies the system prompt `pb cc`, `playbook cc` and `playbook ccd` load, and stores the answer as `systemPrompt.enabled`. A no stores false, so `pb cc` starts with Claude Code's own prompt. An existing copy is refreshed on every run, unless you edited it. A plain `claude` never loads it. `--yes` leaves it off when `systemPrompt.enabled` is false, or on an earlier install that had none.",
        changes: "~/.config/playbook/prompts/SYSTEM_PROMPT.md and ~/.config/playbook (the key `systemPrompt.enabled`)",
        undo: "Run `playbook config set systemPrompt.enabled false`. `playbook uninstall` removes the file.",
    },
    Part {
        id: "telemetry",
        asked: true,
        default_on: true,
        flags: "--telemetry, --no-telemetry",
        question: "Turn on Claude Code telemetry by writing the OpenTelemetry variables for the local usage collector into the env block of ~/.claude/settings.json, so plain claude, the IDE and the desktop app feed `playbook usage`? (skipped with one line when telemetry is on with every key in place, set up by you, or switched off by a setting `playbook setup` will not change)",
        does: "Points Claude Code at a collector on 127.0.0.1 (`playbook usage collect`). Prompt text, tool content, emails and tokens are never sent. Asked only when a yes can act: telemetry not set, switched off by a lone off switch in ~/.claude/settings.json, or on with some playbook keys missing or out of date, or with an IS_DEMO in ~/.claude/settings.json (Claude Code sends no telemetry from interactive sessions while it is set, so a yes removes it and uninstall puts it back). Otherwise setup says why in one line and asks nothing. On by default: Y is the default answer, and `--yes` counts as a yes, except that `--yes` alone never replaces an off switch you set (answer the question or run `playbook setup --only telemetry` for that). Without a terminal and without `--yes` nothing is written and setup prints the command to run. Written only on a yes, and only when you have no telemetry variable in your environment or any Claude Code settings file, except a lone off switch in ~/.claude/settings.json, which a yes replaces (uninstall puts the old value back), or when the only ones are playbook's own keys or a lone on switch, which a yes completes. Without it, `cc` still does this for its own sessions and `playbook usage` reads transcripts.",
        changes: "~/.claude/settings.json (the env block) and ~/.config/playbook/telemetry-env.json (a record of what was written)",
        undo: "`playbook uninstall` removes exactly the keys `playbook setup` wrote, unless you changed them.",
    },
    Part {
        id: "short-name",
        asked: true,
        default_on: true,
        flags: "--short-name, --no-short-name",
        question: "Add `pb` as a short name for `playbook`, as a symlink `pb -> playbook` in the playbook install folder (~/.local/bin or $PLAYBOOK_BIN_DIR)? No link is made when a `pb` command already exists.",
        does: "Makes `pb` run playbook, so `pb cc` starts a session. Passes when a `pb` on PATH already runs this binary, as after install.sh or Homebrew. Otherwise, when no other `pb` is on PATH and playbook sits in the install folder `playbook uninstall` knows, it adds the relative symlink `pb -> playbook` there. It leaves any other `pb` alone, and prints the `ln -s` command to run by hand when playbook sits outside the install folder. A yes or no is stored as `launcher.shortName`, and a stored no keeps `--yes` from adding the link.",
        changes: "the playbook install folder (the `pb` symlink) and ~/.config/playbook (the key `launcher.shortName`)",
        undo: "Delete `pb` from the install folder and run `playbook config set launcher.shortName false`. `playbook uninstall --remove-binary` also removes the link.",
    },
    Part {
        id: "completions",
        asked: true,
        default_on: true,
        flags: "--completions, --no-completions",
        question: "Install tab completion for `playbook` and `pb` in your shell? It adds small files (bash: ~/.local/share/bash-completion/completions, fish: ~/.config/fish/completions, zsh: ~/.local/share/playbook/completions.zsh) and, for zsh, one marked line in ~/.zshrc, created when missing.",
        does: "Writes a small file for the shell in $SHELL, for `playbook` and `pb`. It asks the installed binary for the real script, so an upgrade needs no rewrite and a missing binary breaks nothing. For zsh the file is a loader that does its work on the first Tab press, and setup adds one marked line to ~/.zshrc that sources it, creating the file when it is missing. Setup rewrites a loader it wrote, keeps a file it did not write, and does not touch an edited line. Asked once per run for one shell: run `playbook setup --only completions` from each shell you use. Without a terminal and without `--yes` it stays off.",
        changes: "bash: ~/.local/share/bash-completion/completions/playbook and pb; fish: ~/.config/fish/completions/playbook.fish and pb.fish; zsh: ~/.local/share/playbook/completions.zsh and one line in ~/.zshrc",
        undo: "`playbook uninstall` removes the files and the zsh line it wrote, after a backup of ~/.zshrc.",
    },
    Part {
        id: "policy-guard",
        asked: true,
        default_on: false,
        flags: "--policy-guard, --no-policy-guard",
        question: "Turn on the policy guard? It denies git commands that skip checks or signing, a hand-run `gh pr create`, memory writes outside playbook's store, Edit or Write on settings.json and settings.local.json, writes to Claude Code's own memory folders, and model-run changes to protected config keys. The choice is stored in ~/.config/playbook.",
        does: "Stores `hooks.policyGuard.enabled` true. The hook entry `playbook hook policy-guard` is always in settings.json, and it allows every call while the key is false. It is off by default, and `--yes` leaves it off. See docs/guides/08-policy-guard.md for every rule.",
        changes: "~/.config/playbook (the key `hooks.policyGuard.enabled`)",
        undo: "Run `playbook config set --global hooks.policyGuard.enabled false`. `POLICY_GUARD=0` in the environment switches it off for one session.",
    },
    Part {
        id: "attribution",
        asked: true,
        default_on: false,
        flags: "--attribution (ON), --no-attribution (OFF, the default)",
        question: "Turn attribution ON, so Claude Code adds its Co-Authored-By trailer, generated-by line and claude.ai session link to commits and pull requests? The default is OFF: playbook sets `attribution` in ~/.claude/settings.json so Claude Code adds none of them.",
        does: "ON removes the `attribution` keys playbook wrote to ~/.claude/settings.json, each key that holds exactly the off value, so Claude Code adds its attribution again; a key with your own value stays. OFF, the default, sets `attribution` to `{\"commit\": \"\", \"pr\": \"\", \"sessionUrl\": false}`: only missing keys are added, and a key that holds your own value is replaced only with `--no-attribution`. The choice is stored as `attribution.enabled`, so `--yes` and `--only attribution` keep it. A no to this question in 0.36 or 0.37 counts as ON. A project or managed settings file can still set its own value.",
        changes: "~/.claude/settings.json (the attribution key) and ~/.config/playbook (the key `attribution.enabled`)",
        undo: "Run `playbook setup --only attribution` with the other flag: `--attribution` for ON, `--no-attribution` for OFF. A backup of settings.json is saved next to it.",
    },
    Part {
        id: "codex-hooks",
        asked: true,
        default_on: false,
        flags: "--codex-hooks, --no-codex-hooks",
        question: "Register the playbook Bash guards in Codex? It adds the rm-workspace-guard and precommit-check entries to ~/.codex/hooks.json. Codex skips them until you trust them in /hooks.",
        does: "Adds two PreToolUse entries on Bash to ~/.codex/hooks.json, keeping every other entry, after a backup. It runs only when Codex is found (a ~/.codex folder or a codex program on PATH). Codex continues when a hook fails, so the guards are best effort. It is off by default, and `--yes` leaves it off.",
        changes: "~/.codex/hooks.json",
        undo: "`playbook uninstall` removes the entries it wrote, after a backup. Or delete the two `playbook hook` entries by hand.",
    },
    Part {
        id: "memory-migration",
        asked: false,
        default_on: true,
        flags: "",
        question: "",
        does: "Moves playbook's old memory folder to its new place, once.",
        changes: "~/.claude/memory moves to ~/.config/playbook/memory",
        undo: "Move the folder back by hand. Claude Code's own memory is never touched.",
    },
    Part {
        id: "trust",
        asked: false,
        default_on: true,
        flags: "",
        question: "",
        does: "Marks ~/.config/playbook as trusted so Claude Code skips its trust dialog there.",
        changes: "~/.claude.json (only when the file exists)",
        undo: "Remove the entry for ~/.config/playbook from ~/.claude.json.",
    },
    Part {
        id: "config-hash",
        asked: false,
        default_on: true,
        flags: "",
        question: "",
        does: "Removes the config-hash script older installs placed. The hash now runs inside playbook.",
        changes: "~/.config/playbook/hooks/lib (removes one file, when present)",
        undo: "Nothing to undo. The script is no longer used.",
    },
];

/// A part by id.
pub fn part(id: &str) -> Option<&'static Part> {
    PARTS.iter().find(|p| p.id == id)
}

/// The question for an asked part. Panics on a bad id, which a test rules out.
pub fn question(id: &str) -> &'static str {
    part(id).map_or("", |p| p.question)
}

/// Every part id, for the flag's value list.
pub fn ids() -> Vec<&'static str> {
    PARTS.iter().map(|p| p.id).collect()
}

/// The short plan printed before the first question.
pub fn plan() -> String {
    let mut out = String::from("playbook setup can do the following. You choose each part.\n\n");
    for p in PARTS.iter().filter(|p| p.asked) {
        let default = if p.default_on { "on" } else { "off" };
        let _ = writeln!(
            out,
            "  {}  ({default} by default)\n    {}\n    Edits: {}",
            p.id, p.does, p.changes
        );
    }
    let _ = writeln!(
        out,
        "\nThese run every time and are not asked about: {}.",
        PARTS
            .iter()
            .filter(|p| !p.asked)
            .map(|p| p.id)
            .collect::<Vec<_>>()
            .join(", ")
    );
    out.push_str(
        "See the details with `playbook setup --explain`, or the changes for your machine with `playbook setup --dry-run`.\n\n",
    );
    out
}

/// The part a `StepReport` name stands for.
fn part_for_step(step: &str) -> &str {
    match step {
        "memory" => "memory-migration",
        other => other,
    }
}

/// What to print after a real run: one line per part that changed, with how to
/// undo it, then where the backups are. Empty when nothing changed.
pub fn summary(changed_steps: &[&str], on_parts: &[&str]) -> String {
    let mut ids: Vec<&str> = Vec::new();
    for step in changed_steps {
        if *step == "settings" {
            // The settings merge carries the status line and the security defaults.
            ids.extend(
                ["statusline", "security"]
                    .iter()
                    .filter(|p| on_parts.contains(p)),
            );
        } else {
            ids.push(part_for_step(step));
        }
    }
    let parts: Vec<&Part> = ids.into_iter().filter_map(part).collect();
    if parts.is_empty() {
        return String::new();
    }
    let mut out = String::from("\nWhat changed and how to undo it:\n");
    for p in parts {
        let _ = writeln!(out, "  {}: edited {}. Undo: {}", p.id, p.changes, p.undo);
    }
    out.push_str(
        "Backups of settings.json are saved next to it as settings.json.bak.<time>. Run `playbook uninstall --dry-run` to preview a full removal.\n",
    );
    out
}

/// What one part is, for `--explain`. The hooks part also lists every hook.
pub fn explain_part(p: &Part) -> String {
    let mut out = format!(
        "{}\n  What it does: {}\n  What it edits: {}\n",
        p.id, p.does, p.changes
    );
    let default = if p.default_on { "on" } else { "off" };
    let _ = writeln!(out, "  Default: {default}");
    if !p.flags.is_empty() {
        let _ = writeln!(out, "  Flags: {}", p.flags);
    }
    let _ = writeln!(out, "  How to undo: {}", p.undo);
    if p.id == "hooks" {
        out.push_str("\n  The hooks it adds:\n");
        for line in hooks_list().lines() {
            let _ = writeln!(out, "  {line}");
        }
    }
    out
}

/// `--explain` with no part, or with one part id.
pub fn explain(which: Option<&str>) -> Result<String, String> {
    match which {
        None => Ok(PARTS
            .iter()
            .map(explain_part)
            .collect::<Vec<_>>()
            .join("\n")),
        Some(id) => part(id)
            .map(explain_part)
            .ok_or_else(|| format!("unknown part `{id}`. Choose one of: {}", ids().join(", "))),
    }
}

fn matcher_label(spec: &HookSpec) -> &'static str {
    spec.matcher.unwrap_or("any")
}

/// Every hook, grouped by purpose: event, matcher, name and one line of purpose.
pub fn hooks_list() -> String {
    let mut out = String::new();
    for group in Group::ALL {
        let _ = writeln!(out, "{}", group.label());
        for h in wired_hooks().filter(|h| h.group == group) {
            let _ = writeln!(
                out,
                "  {} [{}] {}: {}",
                h.event,
                matcher_label(h),
                h.name,
                h.purpose
            );
        }
        out.push('\n');
    }
    out.push_str("Details: docs/reference/hooks.md in the playbook repo.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_asked_part_has_a_question_that_names_a_file() {
        for p in PARTS.iter().filter(|p| p.asked) {
            assert!(!p.question.is_empty(), "{} has no question", p.id);
            assert!(
                p.question.contains("~/") || p.question.contains("`playbook"),
                "{} question names no file: {}",
                p.id,
                p.question
            );
        }
    }

    #[test]
    fn a_part_that_is_not_asked_has_no_question() {
        for p in PARTS.iter().filter(|p| !p.asked) {
            assert!(p.question.is_empty(), "{}", p.id);
        }
    }

    #[test]
    fn the_part_ids_are_unique() {
        let mut ids = ids();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PARTS.len());
    }

    #[test]
    fn explain_names_every_part_when_none_is_given() {
        let text = explain(None).unwrap();
        for p in PARTS {
            assert!(
                text.contains(&format!("{}\n  What it does", p.id)),
                "{}",
                p.id
            );
        }
    }

    #[test]
    fn explain_of_an_unknown_part_lists_the_known_ones() {
        let err = explain(Some("nope")).unwrap_err();
        assert!(err.contains("hooks") && err.contains("path"), "{err}");
    }

    #[test]
    fn the_hooks_list_names_every_wired_hook() {
        let text = hooks_list();
        for h in wired_hooks() {
            assert!(
                text.contains(h.name) && text.contains(h.purpose),
                "{}",
                h.name
            );
        }
    }

    #[test]
    fn the_plan_names_every_asked_part_with_the_files_it_edits() {
        let text = plan();
        for p in PARTS.iter().filter(|p| p.asked) {
            assert!(text.contains(p.id) && text.contains(p.does), "{}", p.id);
            assert!(text.contains(&format!("Edits: {}", p.changes)), "{}", p.id);
        }
        assert!(text.contains("--dry-run") && text.contains("--explain"));
    }

    #[test]
    fn the_summary_says_how_to_undo_each_changed_part() {
        let text = summary(&["hooks", "path"], &[]);
        assert!(
            text.contains("hooks: edited ~/.claude/settings.json"),
            "{text}"
        );
        assert!(
            text.contains("path: edited your shell startup files"),
            "{text}"
        );
        assert!(text.contains("playbook uninstall"), "{text}");
        assert!(text.contains("settings.json.bak"), "{text}");
    }

    #[test]
    fn the_settings_step_is_reported_as_the_parts_that_use_it() {
        let text = summary(&["settings"], &["statusline", "path"]);
        assert!(text.contains("statusline: edited"), "{text}");
        assert!(!text.contains("security:"), "{text}");
    }

    #[test]
    fn there_is_no_part_for_the_retired_settings_flag() {
        assert!(part("settings").is_none());
    }

    #[test]
    fn the_summary_is_empty_when_nothing_changed() {
        assert_eq!(summary(&[], &[]), "");
        assert_eq!(summary(&["unknown-step"], &[]), "");
        assert_eq!(summary(&["settings"], &[]), "");
    }

    #[test]
    fn no_text_in_the_catalog_uses_a_dash() {
        let all = format!("{}{}{}", plan(), explain(None).unwrap(), hooks_list());
        assert!(!all.contains('\u{2014}') && !all.contains('\u{2013}'));
    }
}
