// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Wires the 26 hooks Claude Code can invoke into `settings.json` as a bare
//! `playbook hook <name>` command: 18 functional hooks plus 7 guards.
//!
//! `GUARD_SPECS` stays a separate list from `PORTED_HOOK_SPECS` because the
//! two mirror different sources: `PORTED_HOOK_SPECS` is the 18 functional
//! hooks, while `GUARD_SPECS` is the guards
//! `settings.json` wires directly, carrying their own `if` and `timeout`
//! fields hooks.json never had. `wire` treats both lists identically: every
//! entry in either is upserted unconditionally through `upsert_hook`. Only an
//! entry whose command is the bare `playbook hook <name>` form is ours
//! (`entry_targets`), so `upsert_hook` rewrites that slot. A command of the
//! user's own, even one whose script is called `<name>.sh` or `<name>.py`, is
//! never rewritten: the bare command is added next to it.
//!
//! `wire` is mostly an upsert into whatever `settings.json` already has,
//! never a wholesale replace: a hand-added hook entry a user placed
//! alongside one of ours, or an entire event/matcher group we do not manage,
//! survives untouched. This is the same clobber risk `src/init/merge.rs`'s
//! mandatory fixture 6 pins at the settings-merge layer; `wire` pins it
//! again at the narrower hook-wiring layer, since it edits `.hooks` directly
//! rather than going through a three-way merge.
//!
//! The bare-name form (`playbook hook session-init`, not an absolute path)
//! is deliberate: Claude Code already accepts a bare command resolved on
//! PATH for a hand-written hook entry, so `playbook`, installed the same
//! way, resolves the same way. An absolute path would go stale when Homebrew
//! replaces the binary or the user moves it. The known cost is a Claude Code
//! started from a launcher or an IDE with a minimal PATH: `playbook setup` puts
//! the binary's directory on PATH for every shell start (`init/path.rs`), and
//! `playbook doctor` probes a minimal-PATH login shell to catch the case.
//! `tests/init_wire.rs` asserts only that the command is the bare form and
//! names a real hook.

use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// What a hook is for. The hook reference and `playbook hooks list` group by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Safety,
    Session,
    Usage,
    Memory,
    Hygiene,
    Worktree,
}

impl Group {
    /// Every group, in the order the reference page lists them.
    pub const ALL: [Group; 6] = [
        Group::Safety,
        Group::Session,
        Group::Usage,
        Group::Memory,
        Group::Hygiene,
        Group::Worktree,
    ];

    /// The heading the reference page uses.
    pub fn label(self) -> &'static str {
        match self {
            Group::Safety => "Safety guards",
            Group::Session => "Session lifecycle",
            Group::Usage => "Usage tracking and auto mode",
            Group::Memory => "Memory",
            Group::Hygiene => "Hygiene",
            Group::Worktree => "Worktree",
        }
    }
}

/// One hook registration: which event it fires on, the matcher grouping it
/// belongs to (`None` for events with no matcher, such as `SessionStart`), the
/// hook name to invoke, any `if` guard or `timeout` it carries, its group and
/// one plain sentence of purpose. This table is the single source for `wire`,
/// `playbook setup --explain`, `playbook hooks list` and the hook reference
/// page, whose "What it does" column a test keeps equal to `purpose`.
pub struct HookSpec {
    pub event: &'static str,
    pub matcher: Option<&'static str>,
    pub name: &'static str,
    pub group: Group,
    /// One plain sentence. No pipe characters, so it fits a table cell.
    pub purpose: &'static str,
    pub if_cond: Option<&'static str>,
    pub timeout: Option<u64>,
    /// A prior hook name this spec's install also replaces in place, so a
    /// rename never leaves an orphaned entry behind.
    pub also_replaces: Option<&'static str>,
}

/// Every hook entry `wire` writes into `settings.json`, in write order.
pub fn wired_hooks() -> impl Iterator<Item = &'static HookSpec> {
    PORTED_HOOK_SPECS.iter().chain(GUARD_SPECS)
}

/// The functional hooks, already ported to Rust, rewired here to the bare
/// `playbook hook <name>` form. Eleven were ported from the old
/// `hooks/hooks.json` (since removed), line for line (`session-clean-exit` fires on both
/// `Stop` and `SessionEnd`), while `auto-guard` and `auto-cost` never had a
/// `hooks/hooks.json` entry. Together with `GUARD_SPECS` below these are exactly
/// the 26 settings-wired `HookName` variants.
///
/// Divergence from `hooks/hooks.json`'s literal shape: that file shipped
/// `Stop`'s two hooks (`session-clean-exit`, `memory-capture`) as two
/// separate single-hook groups rather than one two-hook group. `wire` groups
/// purely by (event, matcher), so it merges them into one `Stop` group here.
/// This is a deliberate shape simplification, not a behaviour change: a
/// matcher only selects which tool invocations a hook fires on, and `Stop`
/// entries have no matcher to select by, so two groups versus one dispatches
/// identically.
const PORTED_HOOK_SPECS: &[HookSpec] = &[
    HookSpec {
        event: "SessionStart",
        matcher: None,
        name: "session-init",
        group: Group::Session,
        purpose: "Prepares the session directory, warns when a resumed session's config drifted, and adds the tool map, a handoff, an auto-learn nudge and a small memory block to the context. Also sweeps stale worktrees.",
        if_cond: None,
        timeout: None,
        also_replaces: None,
    },
    HookSpec {
        event: "PreToolUse",
        matcher: Some("Read"),
        name: "preread-edit-check",
        group: Group::Hygiene,
        purpose: "Reminds the model not to re-read a file it just edited.",
        if_cond: None,
        timeout: None,
        also_replaces: None,
    },
    HookSpec {
        event: "PreToolUse",
        matcher: Some("Read"),
        name: "preread-size-check",
        group: Group::Hygiene,
        purpose: "Denies a whole-file Read of a large file, and a repeat full Read of an unchanged file, and points to a targeted Read.",
        if_cond: None,
        timeout: None,
        also_replaces: None,
    },
    HookSpec {
        event: "PreToolUse",
        matcher: Some("Read|Grep|Glob|Edit|Write|NotebookEdit"),
        name: "search-counter",
        group: Group::Usage,
        purpose: "Counts exploration calls, names the code search rule once per session at the first Grep or Glob, and suggests the Explore subagent once, at the eighth.",
        if_cond: None,
        timeout: None,
        also_replaces: None,
    },
    HookSpec {
        event: "PreToolUse",
        matcher: Some("Edit|Write"),
        name: "memory-anchors",
        group: Group::Memory,
        purpose: "Shows the memory facts anchored to the file you are about to edit.",
        if_cond: None,
        timeout: None,
        also_replaces: None,
    },
    HookSpec {
        event: "UserPromptSubmit",
        matcher: None,
        // ADR 0008: prompt-time recall, same hook name and binary as the
        // PreToolUse entry above.
        name: "memory-anchors",
        group: Group::Memory,
        purpose: "Adds the memory facts that match your prompt or the files touched this session.",
        if_cond: None,
        timeout: None,
        also_replaces: None,
    },
    HookSpec {
        event: "PostToolUse",
        matcher: Some("Edit|Write|NotebookEdit"),
        name: "post-edit-track",
        group: Group::Usage,
        purpose: "Records which files this session edited.",
        if_cond: None,
        timeout: None,
        also_replaces: None,
    },
    HookSpec {
        event: "PostToolUse",
        matcher: Some("Edit|Write|NotebookEdit"),
        name: "rebuild-memory-graph",
        group: Group::Memory,
        purpose: "Rebuilds the memory graph after a fact file is saved. Does nothing for any other file.",
        if_cond: None,
        timeout: None,
        also_replaces: None,
    },
    HookSpec {
        event: "UserPromptSubmit",
        matcher: None,
        name: "auto-model-detect",
        group: Group::Hygiene,
        purpose: "Suggests `/playbook:plan`, `/playbook:adr` or `/playbook:implement` when your prompt looks like design or build work.",
        if_cond: None,
        timeout: None,
        also_replaces: None,
    },
    HookSpec {
        event: "PreToolUse",
        matcher: Some("AskUserQuestion"),
        name: "auto-guard",
        group: Group::Usage,
        purpose: "In `auto` mode, denies questions to you so an unattended run does not stall.",
        if_cond: None,
        timeout: Some(10),
        also_replaces: None,
    },
    HookSpec {
        event: "UserPromptSubmit",
        matcher: None,
        name: "auto-guard",
        group: Group::Usage,
        purpose: "In `auto` mode, advises a trusted permission mode once per session.",
        if_cond: None,
        timeout: Some(10),
        also_replaces: None,
    },
    HookSpec {
        event: "PreToolUse",
        matcher: None,
        name: "auto-cost",
        group: Group::Usage,
        purpose: "In `auto` mode, tracks spend per agent against `auto.budgetUsd` (main session) and `auto.agentBudgetUsd` (each subagent), and denies new work for an agent at its cap while the closing steps stay allowed. Parks every agent at `auto.sessionCeilingUsd`.",
        if_cond: None,
        timeout: Some(10),
        also_replaces: None,
    },
    HookSpec {
        event: "UserPromptSubmit",
        matcher: None,
        name: "context-hygiene",
        group: Group::Hygiene,
        purpose: "Nudges the model to save a handoff when the context grows large.",
        if_cond: None,
        timeout: Some(10),
        also_replaces: None,
    },
    HookSpec {
        event: "PreToolUse",
        matcher: Some("Read"),
        name: "doc-read-hint",
        group: Group::Hygiene,
        purpose: "When `docs.convert` is `auto` and a converter is found, suggests `playbook doc read` for PDF and Office files. It only suggests.",
        if_cond: None,
        timeout: Some(10),
        also_replaces: None,
    },
    HookSpec {
        event: "PreCompact",
        matcher: None,
        name: "precompact-warn",
        group: Group::Session,
        purpose: "Warns you that the context is about to be compacted and logs the event.",
        if_cond: None,
        timeout: None,
        also_replaces: None,
    },
    HookSpec {
        event: "Stop",
        matcher: None,
        name: "session-clean-exit",
        group: Group::Session,
        purpose: "Records that the session ended cleanly after each turn, so the next start can spot a crash.",
        if_cond: None,
        timeout: None,
        also_replaces: None,
    },
    HookSpec {
        event: "Stop",
        matcher: None,
        name: "memory-capture",
        group: Group::Memory,
        purpose: "Once per context threshold crossing, asks the model to write down durable facts before it continues.",
        if_cond: None,
        timeout: None,
        also_replaces: None,
    },
    HookSpec {
        event: "SessionEnd",
        matcher: None,
        name: "session-clean-exit",
        group: Group::Session,
        purpose: "Writes the clean-exit marker when the session truly ends.",
        if_cond: None,
        timeout: None,
        also_replaces: None,
    },
    HookSpec {
        event: "SessionStart",
        matcher: None,
        name: "usage-warm",
        group: Group::Usage,
        purpose: "Starts a short background read of new session history, so `playbook usage` opens with data. Prints nothing and waits for nothing.",
        if_cond: None,
        timeout: Some(5),
        also_replaces: None,
    },
    HookSpec {
        event: "SessionEnd",
        matcher: None,
        name: "usage-warm",
        group: Group::Usage,
        purpose: "Starts the same background read when a session ends, so the finished session is already stored.",
        if_cond: None,
        timeout: Some(5),
        also_replaces: None,
    },
    HookSpec {
        event: "SessionEnd",
        matcher: Some("clear"),
        name: "handoff-on-clear",
        group: Group::Session,
        purpose: "When `handoff.onClear` is true, writes the session's handoff as `/clear` ends it, by resuming the session as a fork. Off by default.",
        if_cond: None,
        timeout: Some(60),
        also_replaces: None,
    },
    HookSpec {
        event: "UserPromptSubmit",
        matcher: None,
        name: "session-activity",
        group: Group::Session,
        purpose: "Notes that a turn started, for the working state in `playbook sessions`. Stores no prompt text.",
        if_cond: None,
        timeout: Some(5),
        also_replaces: None,
    },
    HookSpec {
        event: "Stop",
        matcher: None,
        name: "session-activity",
        group: Group::Session,
        purpose: "Notes that the turn ended.",
        if_cond: None,
        timeout: Some(5),
        also_replaces: None,
    },
    HookSpec {
        event: "Notification",
        matcher: Some("permission_prompt|idle_prompt|agent_needs_input"),
        name: "session-activity",
        group: Group::Session,
        purpose: "Notes that the session waits for you, for the waiting state in `playbook sessions`. Stores the notice type only.",
        if_cond: None,
        timeout: Some(5),
        also_replaces: None,
    },
    HookSpec {
        event: "WorktreeCreate",
        matcher: None,
        name: "worktree-create",
        group: Group::Worktree,
        purpose: "Creates worktrees under `<repo-parent>/.worktrees/<repo>/<name>`, copies files named in `.worktreeinclude`, clones the main checkout's ignored `target` folder on macOS and keeps `origin/HEAD` at most a day old.",
        if_cond: None,
        timeout: Some(180),
        also_replaces: None,
    },
    HookSpec {
        event: "WorktreeRemove",
        matcher: None,
        name: "worktree-remove",
        group: Group::Worktree,
        purpose: "Removes a worktree only when nothing would be lost. Dirty trees and unpublished commits keep it; squash-merged branches count as published.",
        if_cond: None,
        timeout: Some(120),
        also_replaces: None,
    },
];

/// The always-on safety guards, wired identically to `PORTED_HOOK_SPECS`:
/// every entry here is upserted
/// unconditionally in bare `playbook hook <name>` form. A command of the
/// user's own is never replaced. Kept as a list separate from `PORTED_HOOK_SPECS`, rather than
/// merged into it, because the two mirror different sources (see the module
/// doc comment): these guards came from `settings.json`'s own pre-existing
/// guard entries, not from `hooks/hooks.json`.
const GUARD_SPECS: &[HookSpec] = &[
    HookSpec {
        event: "PreToolUse",
        matcher: Some("Bash"),
        name: "rm-workspace-guard",
        group: Group::Safety,
        purpose: "Stops an `rm`, `rmdir`, `unlink` or `shred` whose target is the filesystem root or your home folder, such as `rm -rf /` or `rm -rf ~`, also after `sudo`, `env` or `command`, inside a command chain, a group or a `bash -c` script. Every other path is allowed, and it does not follow `cd`, variables or `find -delete`. Runs for every Bash command.",
        if_cond: None,
        timeout: Some(10),
        also_replaces: None,
    },
    HookSpec {
        event: "PreToolUse",
        matcher: Some("Bash"),
        name: "bg-await-guard",
        group: Group::Safety,
        purpose: "Warns when a command backgrounds an install, build or typecheck that a later step needs.",
        if_cond: None,
        timeout: Some(10),
        also_replaces: None,
    },
    HookSpec {
        event: "PreToolUse",
        matcher: Some("Bash"),
        name: "precommit-check",
        group: Group::Safety,
        purpose: "Warns about debug leftovers, secret-shaped file names and oversized commits in the staged diff. Runs only for `git commit`.",
        if_cond: Some("Bash(git commit:*)"),
        timeout: Some(10),
        also_replaces: None,
    },
    HookSpec {
        event: "PreToolUse",
        matcher: Some("Bash"),
        name: "commit-message-sanitizer",
        group: Group::Safety,
        purpose: "Removes AI attribution from commit, tag, merge and PR messages, and adds `-s` to a commit that has no sign-off. It also lints the prose of commit messages, PR text and comments against the writing-style phrase table: a warning by default, a denial when `hooks.proseLint.mode` is `block`.",
        if_cond: None,
        timeout: Some(10),
        also_replaces: None,
    },
    HookSpec {
        event: "PreToolUse",
        matcher: Some("Bash"),
        name: "policy-guard",
        group: Group::Safety,
        purpose: "Off by default (`hooks.policyGuard.enabled`, see docs/guides/08-policy-guard.md): the entry stays wired and allows every call. When on, it denies skipped git hooks or signing, a hand-run `gh pr create`, memory writes outside playbook's store, a Bash write to a Claude Code settings file (a redirect, `tee`, `sed -i`, `cp`, `mv` or an interpreter one-liner), and model-run changes to the policy guard switch, the prose lint mode or the PR rules (`config set` on `hooks.policyGuard.*`, `hooks.proseLint.*`, `pr.rules.*` or `pr.ticketPatterns`, `config import`, `config unset`, `config edit`, writes to the config database, a config file or the memory store (reads, copies out and deletions are allowed, since the `rm` guard stops only `/` and the home folder; `mv` and `ln` either way are writes; the denial names the rule that matched), inline `WORKTREE_BASE_DIR=`). The denial tells the model to ask the user to run it with `!`. It also denies `playbook uninstall --yes`.",
        if_cond: None,
        timeout: Some(10),
        also_replaces: None,
    },
    HookSpec {
        // The backstop for what the PreToolUse rewrite did not see. It can
        // amend a signed commit and edit a PR, so it gets a longer timeout.
        event: "PostToolUse",
        matcher: Some("Bash"),
        name: "commit-message-sanitizer",
        group: Group::Safety,
        purpose: "Backstop for what the PreToolUse rewrite did not see. It can amend a signed commit and edit a PR.",
        if_cond: None,
        timeout: Some(60),
        also_replaces: None,
    },
    HookSpec {
        event: "PreToolUse",
        matcher: Some("Edit|Write"),
        name: "policy-guard",
        group: Group::Safety,
        purpose: "Off by default, like the Bash entry. When on, it denies writes to Claude Code's own memory paths and to playbook's config files, and any Edit or Write on a Claude Code settings file (`settings.json` or `settings.local.json`, user or project). Settings change through the `playbook` binary only. Shows the fact format when a fact file is written to playbook's store.",
        if_cond: None,
        timeout: Some(10),
        also_replaces: None,
    },
];

/// `settings.json` (or a value nested under it) was not shaped the way
/// `wire` needs to safely upsert into it. Carries a human-readable message
/// rather than a structured payload, matching `merge::ValidationError`'s
/// shape for the same reason: a caller surfacing this to a person needs
/// words, not a variant to match on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError(pub String);

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Everything that can stop `wire` before it finishes. `Validation` is a
/// `settings.json` (or nested `.hooks` value) that is not an object where
/// one is required; failing closed here beats guessing a shape and silently
/// discarding whatever was actually there. `Io` covers a failed read, a
/// failed backup copy, or a failed atomic write.
#[derive(Debug)]
pub enum WireError {
    Validation(ValidationError),
    Io(std::io::Error),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Validation(err) => write!(f, "{err}"),
            WireError::Io(err) => write!(f, "{err}"),
        }
    }
}

impl From<ValidationError> for WireError {
    fn from(err: ValidationError) -> Self {
        WireError::Validation(err)
    }
}

impl From<std::io::Error> for WireError {
    fn from(err: std::io::Error) -> Self {
        WireError::Io(err)
    }
}

/// What `wire` did. `changed` is `false` when every registration already
/// matched the target shape byte for byte, in which case nothing was
/// written and no backup was taken; `backup_path` is `Some` only when a
/// pre-existing `settings.json` was actually overwritten.
#[derive(Debug)]
pub struct WireOutcome {
    pub changed: bool,
    pub backup_path: Option<PathBuf>,
}

/// Writes every hook in `PORTED_HOOK_SPECS` and every guard in `GUARD_SPECS`
/// to its bare `playbook hook <name>` command, creating `.hooks` and any
/// event or matcher group it needs from scratch. Every entry in both lists
/// is upserted unconditionally. Only an entry carrying the `playbook hook
/// <name>` marker is ours and rewritten in place. A user's own command, even
/// one named `<name>.sh` or `<name>.py`, is left as it is and the bare command
/// is added next to it.
/// Every other key in the file, and every hook entry not managed here, is
/// left untouched. Idempotent: calling this twice in a row writes nothing
/// the second time, since the second call's freshly rendered content is
/// compared byte for byte against what is already on disk before anything
/// is written.
///
/// Backs `settings_path` up first, timestamped, whenever a change is about
/// to land; a no-op call takes no backup. This guards the same failure the
/// `hook-rename-lockstep-settings` incident recorded: a settings/hook name
/// mismatch went unnoticed for 28 hours and produced roughly 110 silent
/// errors, because there was no pre-change snapshot to recover from or diff
/// against.
pub fn wire(settings_path: &Path) -> Result<WireOutcome, WireError> {
    wire_at(settings_path, crate::common::time::now_epoch_secs())
}

/// `wire` with the backup stamped `epoch`, so a caller that already took a
/// backup in the same run lands on the same name and keeps its earlier copy.
pub fn wire_at(settings_path: &Path, epoch: u64) -> Result<WireOutcome, WireError> {
    let specs: Vec<&HookSpec> = wired_hooks().collect();
    upsert_specs(settings_path, epoch, &specs)
}

/// Upserts every spec in `specs` into the hooks of `settings_path` and writes
/// the file only when the result differs from what is on disk.
pub(super) fn upsert_specs(
    settings_path: &Path,
    epoch: u64,
    specs: &[&HookSpec],
) -> Result<WireOutcome, WireError> {
    let (mut root, original) = load_settings(settings_path)?;

    {
        let hooks_value = root
            .entry("hooks")
            .or_insert_with(|| Value::Object(Map::new()));
        let hooks = hooks_value.as_object_mut().ok_or_else(|| {
            ValidationError(format!(
                "'hooks' in {} is not a JSON object",
                settings_path.display()
            ))
        })?;
        for spec in specs {
            upsert_hook(hooks, spec, settings_path)?;
        }
    }

    let rendered = format!("{:#}\n", Value::Object(root));

    if rendered == original {
        return Ok(WireOutcome {
            changed: false,
            backup_path: None,
        });
    }

    let backup_path = if settings_path.is_file() {
        let backup = timestamped_backup_path(settings_path, epoch);
        if !backup.exists() {
            fs::copy(settings_path, &backup)?;
        }
        Some(backup)
    } else {
        None
    };

    atomic_write(settings_path, &rendered)?;

    Ok(WireOutcome {
        changed: true,
        backup_path,
    })
}

/// Removes every hook entry whose command is one of `managed`, then any group
/// or event that leaves empty, and the `hooks` key itself when nothing is left.
/// Returns how many entries went.
pub(super) fn remove_managed_entries(root: &mut Map<String, Value>, managed: &[String]) -> usize {
    let mut removed_total = 0;
    let mut drop_hooks_key = false;
    if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
        let mut touched_events = Vec::new();
        for (event, groups) in hooks.iter_mut() {
            let Some(groups) = groups.as_array_mut() else {
                continue;
            };
            let mut touched = false;
            groups.retain_mut(|group| {
                let Some(entries) = group
                    .as_object_mut()
                    .and_then(|g| g.get_mut("hooks"))
                    .and_then(Value::as_array_mut)
                else {
                    return true;
                };
                let before = entries.len();
                entries.retain(|e| !is_managed_entry(e, managed));
                let removed = before - entries.len();
                removed_total += removed;
                touched |= removed > 0;
                removed == 0 || !entries.is_empty()
            });
            if touched {
                touched_events.push(event.clone());
            }
        }
        for event in touched_events {
            if hooks
                .get(&event)
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
            {
                hooks.remove(&event);
            }
        }
        drop_hooks_key = hooks.is_empty() && removed_total > 0;
    }
    if drop_hooks_key {
        root.remove("hooks");
    }
    removed_total
}

/// What `unwire_at` found, and did unless it ran dry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnwireOutcome {
    /// Hook entries removed (or that would be, on a dry run).
    pub hooks_removed: usize,
    /// Whether the `statusLine` setting init wrote was removed too.
    pub statusline_removed: bool,
    /// The backup taken before the write; `None` on a dry run or no change.
    pub backup_path: Option<PathBuf>,
}

/// Undo `wire`: remove each hook entry whose command is exactly the bare
/// `playbook hook <name>` form `wire` writes, then any group or event that
/// leaves empty, and the `statusLine` when it is the command init placed.
/// Every other key, hook entry and group stays as the user left it. Backs the
/// file up first when something changes; `dry_run` only counts.
pub fn unwire_at(
    settings_path: &Path,
    home: &Path,
    epoch: u64,
    dry_run: bool,
) -> Result<UnwireOutcome, WireError> {
    let (mut root, original) = load_settings(settings_path)?;
    let mut outcome = UnwireOutcome {
        hooks_removed: 0,
        statusline_removed: false,
        backup_path: None,
    };

    outcome.hooks_removed = remove_managed_entries(&mut root, &managed_commands());

    let placed_statusline = root
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(Value::as_str)
        .is_some_and(|c| {
            c == crate::init::statusline::RUST_COMMAND
                || crate::init::statusline::is_legacy_command(c, home)
        });
    if placed_statusline {
        root.remove("statusLine");
        outcome.statusline_removed = true;
    }

    if outcome.hooks_removed == 0 && !outcome.statusline_removed {
        return Ok(outcome);
    }
    if dry_run {
        return Ok(outcome);
    }
    let rendered = format!("{:#}\n", Value::Object(root));
    if rendered == original {
        return Ok(outcome);
    }
    outcome.backup_path = Some(write_with_fresh_backup(settings_path, &rendered, epoch)?);
    Ok(outcome)
}

/// Backs `path` up under a name no earlier backup used, then writes `rendered`.
/// An earlier backup holds an older state worth keeping.
pub(super) fn write_with_fresh_backup(
    path: &Path,
    rendered: &str,
    epoch: u64,
) -> std::io::Result<PathBuf> {
    let mut stamp = epoch;
    let mut backup = timestamped_backup_path(path, stamp);
    while backup.exists() {
        stamp += 1;
        backup = timestamped_backup_path(path, stamp);
    }
    fs::copy(path, &backup)?;
    atomic_write(path, rendered)?;
    Ok(backup)
}

/// What a read-only look at `settings.json` found about the hooks `wire` owns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookAudit {
    /// Names of hooks `wire` writes that lack an entry under one of their
    /// events, each name once.
    pub missing: Vec<&'static str>,
    /// `playbook hook <name>` entries whose name `wire` no longer writes.
    pub stale: Vec<String>,
    /// How many of the entries `wire` owns are present. A hook wired under two
    /// events has two entries.
    pub present: usize,
}

/// Compare `settings_path` with what `wire` would write, without changing it.
/// A missing, unreadable or malformed file reports every hook as missing.
pub fn audit(settings_path: &Path) -> HookAudit {
    let specs: Vec<&HookSpec> = wired_hooks().collect();
    audit_specs(settings_path, &specs)
}

/// [`audit`] against an explicit spec list.
pub(super) fn audit_specs(settings_path: &Path, specs: &[&HookSpec]) -> HookAudit {
    let root = fs::read_to_string(settings_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let hooks = root.as_ref().and_then(|r| r.get("hooks"));
    let mut out = HookAudit::default();
    for spec in specs.iter().copied() {
        let want = bare_command(spec.name);
        let found = hooks
            .and_then(|h| h.get(spec.event))
            .and_then(Value::as_array)
            .is_some_and(|groups| {
                groups.iter().any(|g| {
                    group_matcher(g) == spec.matcher
                        && g.get("hooks").and_then(Value::as_array).is_some_and(|es| {
                            es.iter().any(|e| {
                                e.get("command").and_then(Value::as_str) == Some(want.as_str())
                            })
                        })
                })
            });
        if found {
            out.present += 1;
        } else if !out.missing.contains(&spec.name) {
            out.missing.push(spec.name);
        }
    }
    let known: Vec<String> = specs.iter().map(|s| bare_command(s.name)).collect();
    if let Some(events) = hooks.and_then(Value::as_object) {
        for groups in events.values().filter_map(Value::as_array) {
            for entry in groups
                .iter()
                .filter_map(|g| g.get("hooks").and_then(Value::as_array))
                .flatten()
            {
                let cmd = entry.get("command").and_then(Value::as_str).unwrap_or("");
                if cmd.starts_with(HOOK_COMMAND_PREFIX) && !is_managed_entry(entry, &known) {
                    out.stale.push(cmd.to_string());
                }
            }
        }
    }
    out.stale.sort();
    out.stale.dedup();
    out
}

/// The command of every entry `wire` writes, which marks an entry as ours.
fn managed_commands() -> Vec<String> {
    wired_hooks().map(|spec| bare_command(spec.name)).collect()
}

/// Whether `entry` is a command hook running exactly one of `managed`.
fn is_managed_entry(entry: &Value, managed: &[String]) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|cmd| managed.iter().any(|m| m == cmd))
}

/// Reads `path` as a JSON object, returning it alongside the exact bytes
/// read so `wire` can compare its own re-serialization against them to
/// decide whether anything actually changed. A missing file starts from an
/// empty object and an empty baseline, matching a fresh `playbook setup` run
/// before any `settings.json` exists yet.
pub(super) fn load_settings(path: &Path) -> Result<(Map<String, Value>, String), WireError> {
    if !path.is_file() {
        return Ok((Map::new(), String::new()));
    }
    let original = fs::read_to_string(path)?;
    match serde_json::from_str::<Value>(&original) {
        Ok(Value::Object(map)) => Ok((map, original)),
        Ok(_) => Err(ValidationError(format!("{} is not a JSON object", path.display())).into()),
        Err(err) => {
            Err(ValidationError(format!("{} is not valid JSON: {err}", path.display())).into())
        }
    }
}

/// Ensures `hooks[spec.event]` contains one group whose matcher equals
/// `spec.matcher`, and that group's `hooks` array contains exactly one entry
/// for `spec.name`, rewritten to its canonical form (see `bare_command`).
/// Every other entry already in that group, and every other group on the
/// same event, is left exactly as found: this upserts into the existing
/// structure, it never replaces it wholesale.
fn upsert_hook(
    hooks: &mut Map<String, Value>,
    spec: &HookSpec,
    settings_path: &Path,
) -> Result<(), WireError> {
    let groups = hooks
        .entry(spec.event)
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| {
            ValidationError(format!(
                "'hooks.{}' in {} is not a JSON array",
                spec.event,
                settings_path.display()
            ))
        })?;

    let existing_group_idx = groups
        .iter()
        .position(|group| group_matcher(group) == spec.matcher);

    let group_map = match existing_group_idx {
        Some(idx) => groups[idx].as_object_mut().ok_or_else(|| {
            ValidationError(format!(
                "a 'hooks.{}' entry in {} is not a JSON object",
                spec.event,
                settings_path.display()
            ))
        })?,
        None => {
            let mut new_group = Map::new();
            if let Some(matcher) = spec.matcher {
                new_group.insert("matcher".to_string(), Value::String(matcher.to_string()));
            }
            new_group.insert("hooks".to_string(), Value::Array(Vec::new()));
            let at = groups.len();
            groups.push(Value::Object(new_group));
            match &mut groups[at] {
                Value::Object(map) => map,
                _ => unreachable!("the group just pushed is an object"),
            }
        }
    };

    let hooks_array = group_map
        .entry("hooks")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| {
            ValidationError(format!(
                "a 'hooks.{}' group's 'hooks' field in {} is not a JSON array",
                spec.event,
                settings_path.display()
            ))
        })?;

    let entry = canonical_entry(spec);
    let existing = hooks_array.iter().position(|h| {
        entry_targets(h, spec.name) || spec.also_replaces.is_some_and(|old| entry_targets(h, old))
    });
    match existing {
        Some(idx) => hooks_array[idx] = entry,
        None => hooks_array.push(entry),
    }
    Ok(())
}

/// The `matcher` field of a `.hooks.<event>` array entry, or `None` when the
/// entry has no `matcher` key at all (hooks.json's shape for events that
/// never take one, such as `SessionStart`) or is not an object.
fn group_matcher(group: &Value) -> Option<&str> {
    group.as_object()?.get("matcher")?.as_str()
}

/// Whether `entry`'s `command` is the bare `playbook hook <name>` command
/// `wire` writes. That marker is what makes an entry ours. A user's own
/// command is never ours, whatever its script is called. Used both to find
/// the entry to rewrite and, on a second `wire` call, to recognise that it is
/// already in its target form.
fn entry_targets(entry: &Value, name: &str) -> bool {
    entry
        .as_object()
        .and_then(|e| e.get("command"))
        .and_then(Value::as_str)
        .is_some_and(|cmd| cmd == bare_command(name))
}

/// What every command `wire` writes starts with. The policy guard reads it to
/// tell the entries `init` owns from the user's own.
pub const HOOK_COMMAND_PREFIX: &str = "playbook hook ";

/// The bare command `wire` writes for the hook named `name`: no path,
/// resolved on `PATH` the same way a hand-written `rtk hook claude` entry is.
pub(super) fn bare_command(name: &str) -> String {
    format!("{HOOK_COMMAND_PREFIX}{name}")
}

/// The exact JSON object `wire` writes for one `HookSpec`: `type` and
/// `command` always, `if` and `timeout` only when the spec carries them, in
/// that fixed key order every time, so re-running `wire` against its own
/// prior output reproduces the identical object rather than merely an
/// equivalent one. `command` is always the bare binary form.
fn canonical_entry(spec: &HookSpec) -> Value {
    let mut entry = Map::new();
    entry.insert("type".to_string(), Value::String("command".to_string()));
    entry.insert(
        "command".to_string(),
        Value::String(bare_command(spec.name)),
    );
    if let Some(if_cond) = spec.if_cond {
        entry.insert("if".to_string(), Value::String(if_cond.to_string()));
    }
    if let Some(timeout) = spec.timeout {
        entry.insert("timeout".to_string(), Value::Number(timeout.into()));
    }
    Value::Object(entry)
}

/// `settings.json.bak.<unix-seconds>` beside the original, so a bad wiring
/// run is always recoverable, multiple runs never collide on the same
/// backup name, and the timestamp itself shows how stale a given backup is.
fn timestamped_backup_path(settings_path: &Path, epoch_secs: u64) -> PathBuf {
    let file_name = settings_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.json".to_string());
    settings_path.with_file_name(format!("{file_name}.bak.{epoch_secs}"))
}

/// Writes `content` to `path` via a sibling temp file plus rename, so a
/// reader of `path` never observes a half-written `settings.json`. Mirrors
/// `merge::atomic_write`'s approach exactly, but is not shared with it:
/// promoting `src/init/merge.rs`'s private helper to `pub(crate)` for one
/// caller is not worth it. Duplicating a five-line function is cheaper.
fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    // A symlinked settings file (stow, chezmoi) stays a symlink: only its target is replaced.
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    crate::common::atomic::write_atomic(&target, content)
}
