// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Ordered migration registry (ADR 0016, ADR 0034). Auto migrations run once
//! and are recorded; Manual ones only warn. `init` runs every entry; the
//! session hook, the first-run step, `migrate` and `doctor --fix` run only
//! the `Owned` ones (`run_owned`).

use crate::common::atomic::{acquire_dir_lock, ensure_private_dir, remove_stale_lock_dir};
use crate::common::paths::playbook_root_from;
use crate::init::run::{StepReport, StepStatus};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const LOCK_DIR: &str = "migrations.lock";
const SYSTEM_PROMPT_KEY: &str = "system-prompt";
const STATUSLINE_KEY: &str = "statusline";
const SKILL_KEY_PREFIX: &str = "skill:";
/// The `Consent` migration `init` skips when the user declines the status line.
pub const STATUSLINE_MIGRATION: &str = "statusline-rust-command";
/// The `Consent` migration `init` skips when the user declines the hooks.
pub const DEAD_HOOKS_MIGRATION: &str = "dead-hook-scripts";
/// The 0.18.0 item that rewrote legacy `source` lines to the `shell-init` line.
/// ADR 0044 made it do nothing, and `retired-launcher-rc-line` removes those
/// lines. Its registry entry is gone, because ids are slugs and a gap costs
/// nothing; a record row under it still loads and is ignored.
pub const RETIRED_SHELL_INIT_MIGRATION: &str = "shell-init-rc-line";
/// The `Consent` item 0.36.0 and 0.37.0 shipped for the attribution part,
/// retired with ADR 0042's amendment. A record that names it, by slug or as
/// `0015-attribution-off`, still loads, and the id is ignored.
pub const RETIRED_ATTRIBUTION_MIGRATION: &str = "attribution-off";

/// Slugs taken out of the registry. A record row or pending id under one
/// loads and is ignored. A retired slug is never given to a new migration,
/// because old applied rows would mark that one as already run.
pub const RETIRED_IDS: [&str; 5] = [
    RETIRED_SHELL_INIT_MIGRATION,
    RETIRED_ATTRIBUTION_MIGRATION,
    RETIRED_DISK_GUARD_MIGRATION,
    RETIRED_COLLECTOR_SERVICE_MIGRATION,
    RETIRED_COLLECTOR_LABEL_MIGRATION,
];
/// The `Consent` reminder for the collector-service part, shipped from
/// 0.36.0 as `0016-collector-service` and retired with the service.
pub const RETIRED_COLLECTOR_SERVICE_MIGRATION: &str = "collector-service";
/// The `Owned` move of the collector agent to its new launchd label,
/// shipped in 0.38.0 and retired with the service.
pub const RETIRED_COLLECTOR_LABEL_MIGRATION: &str = "collector-service-label";
/// The `Owned` removal of the collector service, now that playbook starts
/// the collector itself.
pub const COLLECTOR_REMOVAL_MIGRATION: &str = "collector-service-removed";
/// The `Owned` move of stored auto budgets off the old $5 default (ADR 0028).
pub const AUTO_BUDGET_MIGRATION: &str = "auto-budget-defaults";
/// The `Consent` removal of the retired launcher rc lines (ADR 0044). Releases
/// up to 0.36.0 wrote them, so an upgrade from one still needs this. Remove it
/// in 1.0.0 with doctor layer 3 and the launcher half of `init::rcfile`.
pub const RC_LINE_MIGRATION: &str = "retired-launcher-rc-line";
/// The `Consent` question about the system prompt now on by default (ADR 0044).
pub const PROMPT_DEFAULT_MIGRATION: &str = "system-prompt-default-on";
/// The `Consent` item 0.38.0 shipped to wire the disk guard hook, retired
/// when the disk guard was removed. A record that names it still
/// loads, and the id is ignored.
pub const RETIRED_DISK_GUARD_MIGRATION: &str = "disk-guard-hook";
/// The `Consent` removal of the retired `no-slop-guard` and `no-dash-guard`
/// hook entries setup used to write.
pub const NO_SLOP_GUARD_MIGRATION: &str = "no-slop-guard-entry";
/// The `Owned` move of a stored `update.plugin` boolean to `auto` or `off`.
pub const PLUGIN_UPDATE_MODE_MIGRATION: &str = "plugin-update-mode";
/// The `Owned` notice that the policy guard is now off by default. It changes
/// nothing and is recorded once it has run.
pub const POLICY_GUARD_NOTICE_MIGRATION: &str = "policy-guard-off-by-default";
/// The `Owned` move of stored config rows from a renamed key's old name to its new one.
pub const RENAME_CONFIG_KEYS_MIGRATION: &str = "rename-config-keys";
/// The step name of that notice, which the heal line shows in full.
pub const POLICY_GUARD_NOTICE_STEP: &str = "policy guard";
/// What the notice says (docs/guides/08-policy-guard.md).
const POLICY_GUARD_NOTICE: &str = "the policy guard is now off by default; to turn it on, run: playbook config set --global hooks.policyGuard.enabled true";
/// The `Owned` notice that the rm guard is now a short fixed rule (ADR 0045,
/// 2026-10-19 amendment). It changes nothing and is recorded once it has run.
pub const RM_GUARD_NOTICE_MIGRATION: &str = "rm-guard-simplified";
/// The `Owned` removal of the per-worktree code index files (ADR 0056). The
/// shared store replaces them.
pub const CODE_INDEX_FILES_MIGRATION: &str = "code-index-per-worktree-files";
/// The `Owned` move of loaded handoffs from the old `used` folder into the gc
/// tombstone folder.
pub const HANDOFF_USED_MIGRATION: &str = "handoff-used-to-tombstone";
/// The `Manual` report of stored values for keys that became built-in constants.
pub const FIXED_CONFIG_KEYS_MIGRATION: &str = "fixed-config-keys";
/// The step name of that notice, which the heal line shows in full.
pub const RM_GUARD_NOTICE_STEP: &str = "rm guard";
/// What the notice says (docs/guides/07-rm-guard.md).
const RM_GUARD_NOTICE: &str = "the rm guard now denies only rm of / or your home folder, and hooks.rmGuard.allowedRoots is no longer used; see docs/guides/07-rm-guard.md";

/// What a migration needs to find the state it moves.
pub struct Ctx {
    pub home: PathBuf,
    pub claude_home: PathBuf,
    /// The shipped plugin tree, when init could resolve it.
    pub self_root: Option<PathBuf>,
    /// `(repo_root, dest_base)` when the caller is inside a repo.
    pub repo: Option<(PathBuf, PathBuf)>,
    /// The stamp for any backup a migration takes. `init` passes its run's
    /// one stamp, so all of a run's backups share it.
    pub epoch: u64,
}

impl Ctx {
    /// A context for `home` alone: no plugin tree and no repo, as the session
    /// hook and `doctor --fix` run.
    pub fn for_home(home: &Path) -> Ctx {
        Ctx {
            home: home.to_path_buf(),
            claude_home: home.join(".claude"),
            self_root: None,
            repo: None,
            epoch: crate::common::time::now_epoch_secs(),
        }
    }
}

/// How many times a waiting caller tries the migration lock, 100 ms apart.
pub const LOCK_RETRIES: u32 = 50;

pub enum Kind {
    /// Runs until it succeeds once, then is recorded and never run again.
    Auto,
    /// Self-healing and idempotent: runs every init, never recorded, no lock.
    Idempotent,
    /// Never changes anything; only warns.
    Manual,
}

pub enum Outcome {
    /// Succeeded; record the id so it is not run again.
    Done(StepReport),
    /// Succeeded but is idempotent and self-healing; run again next time.
    Repeat(StepReport),
    /// Idempotent and nothing to report; run again next time.
    Quiet,
    /// Manual finding for the user.
    Warn(String),
    /// Did not complete; the id is not recorded and later migrations wait.
    Failed(StepReport),
}

/// What a migration writes, next to `Kind`, which says how often it runs.
#[derive(Clone, Copy)]
pub enum Scope {
    /// Writes only playbook's own files, or nothing. Safe to run unattended.
    Owned,
    /// Writes a file the user owns, so only `init` runs it. `check` says,
    /// without writing, what the run would change, or `None`. `command` is
    /// the init command that applies it.
    Consent {
        check: fn(&Ctx) -> Option<String>,
        command: &'static str,
    },
    /// Reports on files Claude Code owns and writes nothing. A `Manual`
    /// migration whose warning the session hook repeats until it clears. A
    /// note that should not repeat each session stays `Owned`, as `orphaned-plugin-caches` does.
    Report,
}

pub struct Migration {
    pub id: &'static str,
    /// The release that ships it, as `X.Y.Z`, or `next` until the release
    /// pull request writes the version in (ADR 0046). Write `next` only as
    /// the one line form `scripts/release-prepare.sh` replaces.
    pub since: &'static str,
    pub kind: Kind,
    pub scope: Scope,
    pub run: fn(&Ctx) -> Outcome,
}

/// What a run produced, for the caller to print.
#[derive(Default)]
pub struct Report {
    pub steps: Vec<StepReport>,
    pub warnings: Vec<String>,
    /// A migration failed, or the applied record could not be read or written.
    pub failed: bool,
    /// Another process held the migration lock, so the locked part did not run.
    pub lock_busy: bool,
    /// The deadline passed, so the later migrations did not start.
    pub timed_out: bool,
}

/// The release a migration ships in (ADR 0046).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    /// Not released yet: no build runs, checks or lists it as open.
    Next,
    /// The `X.Y.Z` release that first runs it.
    At(&'static str),
}

impl Since {
    /// The value as written in the registry: `X.Y.Z` or `next`.
    pub fn as_str(self) -> &'static str {
        match self {
            Since::Next => NEXT,
            Since::At(v) => v,
        }
    }
}

/// The `since` of a migration that waits for the next release.
pub const NEXT: &str = "next";

/// Whether `v` is a plain `MAJOR.MINOR.PATCH`, the only form a released
/// `since` takes.
fn plain_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        })
}

impl Migration {
    pub fn since(&self) -> Since {
        if self.since == NEXT {
            Since::Next
        } else {
            Since::At(self.since)
        }
    }

    /// Whether a binary at `running` ships this migration: its `since` is a
    /// plain version no higher than `running`. `next` and any other value
    /// stay unreleased, because `update::resolve::compare` reads a word as 0
    /// and would ship it everywhere.
    pub fn released_in(&self, running: &str) -> bool {
        match self.since() {
            Since::Next => false,
            Since::At(v) => {
                plain_version(v)
                    && crate::update::resolve::compare(v, running) != std::cmp::Ordering::Greater
            }
        }
    }
}

/// The registry entries this binary ships, in apply order (ADR 0046).
pub fn active() -> Vec<Migration> {
    shipped(registry(), env!("CARGO_PKG_VERSION"))
}

/// The entries of `migrations` a binary at `running` ships, in order.
pub fn shipped(migrations: Vec<Migration>, running: &str) -> Vec<Migration> {
    migrations
        .into_iter()
        .filter(|m| m.released_in(running))
        .collect()
}

/// Every migration in apply order, the unreleased ones last. Append only;
/// never reorder or rename an id. A new one takes `next` as its `since`,
/// written as the one line form `scripts/release-prepare.sh` replaces.
pub fn registry() -> Vec<Migration> {
    vec![
        Migration {
            id: "memory-store-move",
            since: "0.18.0",
            kind: Kind::Idempotent,
            scope: Scope::Owned,
            run: memory_store_move,
        },
        Migration {
            id: "gate-repo-local-move",
            since: "0.18.0",
            kind: Kind::Idempotent,
            scope: Scope::Owned,
            run: gate_repo_local_move,
        },
        Migration {
            id: "system-prompt-edited",
            since: "0.18.0",
            kind: Kind::Manual,
            scope: Scope::Owned,
            run: system_prompt_edited,
        },
        Migration {
            id: "skills-edited",
            since: "0.18.0",
            kind: Kind::Manual,
            scope: Scope::Owned,
            run: skills_edited,
        },
        Migration {
            id: "statusline-edited",
            since: "0.18.0",
            kind: Kind::Manual,
            scope: Scope::Owned,
            run: statusline_edited,
        },
        Migration {
            id: STATUSLINE_MIGRATION,
            since: "0.18.0",
            kind: Kind::Idempotent,
            scope: Scope::Consent {
                check: statusline_rust_command_pending,
                command: "playbook setup --statusline",
            },
            run: statusline_rust_command,
        },
        Migration {
            id: "remove-config-hash-script",
            since: "0.20.0",
            kind: Kind::Idempotent,
            scope: Scope::Owned,
            run: remove_config_hash_script,
        },
        Migration {
            id: "adopt-late-config-json",
            since: "0.20.0",
            kind: Kind::Idempotent,
            scope: Scope::Owned,
            run: adopt_late_config_json,
        },
        Migration {
            id: "state-files-to-sqlite",
            since: "0.20.0",
            kind: Kind::Idempotent,
            scope: Scope::Owned,
            run: state_files_to_sqlite,
        },
        Migration {
            id: DEAD_HOOKS_MIGRATION,
            since: "0.34.0",
            kind: Kind::Idempotent,
            scope: Scope::Consent {
                check: dead_hook_scripts_pending,
                command: "playbook setup --hooks",
            },
            run: dead_hook_scripts,
        },
        Migration {
            id: "other-playbook-plugins",
            since: "0.34.0",
            kind: Kind::Manual,
            scope: Scope::Report,
            run: other_playbook_plugins,
        },
        Migration {
            id: "plugin-version-range",
            since: "0.34.0",
            kind: Kind::Manual,
            scope: Scope::Report,
            run: plugin_version_range,
        },
        Migration {
            id: "orphaned-plugin-caches",
            since: "0.34.0",
            kind: Kind::Manual,
            scope: Scope::Owned,
            run: orphaned_plugin_caches,
        },
        Migration {
            id: AUTO_BUDGET_MIGRATION,
            since: "0.36.0",
            kind: Kind::Auto,
            scope: Scope::Owned,
            run: auto_budget_defaults,
        },
        Migration {
            id: RC_LINE_MIGRATION,
            since: "0.37.0",
            kind: Kind::Idempotent,
            scope: Scope::Consent {
                check: retired_rc_line_pending,
                command: "playbook setup",
            },
            run: retired_rc_line,
        },
        Migration {
            id: PROMPT_DEFAULT_MIGRATION,
            since: "0.37.0",
            kind: Kind::Idempotent,
            scope: Scope::Consent {
                check: system_prompt_default_pending,
                command: "playbook setup --only system-prompt",
            },
            run: system_prompt_default_by_setup,
        },
        Migration {
            id: NO_SLOP_GUARD_MIGRATION,
            since: "0.39.0",
            kind: Kind::Idempotent,
            scope: Scope::Consent {
                check: retired_hooks_pending,
                command: "playbook setup --hooks",
            },
            run: no_slop_guard_entry,
        },
        Migration {
            id: PLUGIN_UPDATE_MODE_MIGRATION,
            since: "0.39.0",
            kind: Kind::Auto,
            scope: Scope::Owned,
            run: plugin_update_mode,
        },
        Migration {
            id: COLLECTOR_REMOVAL_MIGRATION,
            since: "0.39.0",
            kind: Kind::Idempotent,
            scope: Scope::Owned,
            run: collector_service_removed,
        },
        Migration {
            id: POLICY_GUARD_NOTICE_MIGRATION,
            since: "0.39.0",
            kind: Kind::Auto,
            scope: Scope::Owned,
            run: policy_guard_off_notice,
        },
        Migration {
            id: RM_GUARD_NOTICE_MIGRATION,
            since: "0.39.0",
            kind: Kind::Auto,
            scope: Scope::Owned,
            run: rm_guard_simplified_notice,
        },
        Migration {
            id: CODE_INDEX_FILES_MIGRATION,
            since: "0.40.0",
            kind: Kind::Auto,
            scope: Scope::Owned,
            run: code_index_per_worktree_files,
        },
        Migration {
            id: HANDOFF_USED_MIGRATION,
            since: "0.40.0",
            kind: Kind::Idempotent,
            scope: Scope::Owned,
            run: crate::init::handoff_used::run,
        },
        Migration {
            id: FIXED_CONFIG_KEYS_MIGRATION,
            since: "next",
            kind: Kind::Manual,
            scope: Scope::Owned,
            run: fixed_config_keys,
        },
        Migration {
            id: RENAME_CONFIG_KEYS_MIGRATION,
            since: "next",
            kind: Kind::Auto,
            scope: Scope::Owned,
            run: rename_config_keys,
        },
    ]
}

/// Moves every stored row of a renamed config key to its new name, in every
/// tier. Where a scope already holds the new name, that row stays.
fn rename_config_keys(ctx: &Ctx) -> Outcome {
    move_renamed_rows(ctx, crate::config::keys::renames())
}

fn move_renamed_rows(ctx: &Ctx, renames: &[crate::config::keys::KeyRename]) -> Outcome {
    const STEP: &str = "config key names";
    let root = playbook_root_from(&ctx.home);
    let mut moved = 0;
    for rename in renames {
        match crate::config::store::rename_key(&root, rename.old, rename.new) {
            Ok(n) => moved += n,
            Err(err) => return Outcome::Failed(StepReport::failed(STEP, err.to_string())),
        }
    }
    if moved == 0 {
        return Outcome::Done(StepReport::skipped(
            STEP,
            "no stored row has an old key name",
        ));
    }
    Outcome::Done(StepReport::wired(
        STEP,
        format!("moved {moved} stored config row(s) to their new key names"),
    ))
}

/// The stored values of `key` that differ from its built-in `fixed` value.
fn differing_stored_values(root: &Path, key: &str, fixed: &serde_json::Value) -> Vec<String> {
    crate::config::store::stored_values(root, key)
        .unwrap_or_default()
        .iter()
        .filter(|value| *value != fixed)
        .map(ToString::to_string)
        .collect()
}

/// A stored value for a key that is now a built-in constant is never read.
/// Names each such key once with what is stored and the built-in value.
fn fixed_config_keys(ctx: &Ctx) -> Outcome {
    let root = playbook_root_from(&ctx.home);
    let found: Vec<String> = crate::config::keys::FIXED_KEYS
        .iter()
        .filter_map(|(key, fixed)| {
            let stored = differing_stored_values(&root, key, fixed);
            (!stored.is_empty())
                .then(|| format!("{key} (stored {}, built in {fixed})", stored.join(", ")))
        })
        .collect();
    if found.is_empty() {
        return Outcome::Quiet;
    }
    Outcome::Warn(format!(
        "these config keys are now built in and their stored values are ignored: {}",
        found.join("; ")
    ))
}

/// The code index files that earlier versions kept per worktree: the
/// `<worktree-id>.db` files and `index.lock` under `data/code/<owner>/<repo>/`,
/// and the index under the worktree state folder when its schema is older
/// than this build's. A current file at that state path is the fallback store
/// of a repo with no safe data folder path, so it stays, and so does a file
/// that is not SQLite or comes from a newer build. The walk never follows a
/// symlink, so a link in the data folder cannot lead a delete outside it.
fn code_index_per_worktree_files(ctx: &Ctx) -> Outcome {
    const STEP: &str = "code index files";
    let root = playbook_root_from(&ctx.home);
    let mut doomed = per_worktree_code_files(&playbook_data::data_dir_from(&ctx.home));
    doomed.extend(old_state_indexes(&root.join("repos")));
    let mut removed = 0;
    let mut failed: Vec<String> = Vec::new();
    for file in doomed {
        for path in with_sidecars(&file) {
            match fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => failed.push(format!("{}: {e}", path.display())),
            }
        }
    }
    if let Some(first) = failed.first() {
        return Outcome::Failed(StepReport::failed(
            STEP,
            format!(
                "could not delete {} file(s), the first is {first}; the rest were deleted",
                failed.len()
            ),
        ));
    }
    if removed == 0 {
        return Outcome::Done(StepReport::skipped(
            STEP,
            "no per-worktree code index files",
        ));
    }
    Outcome::Done(StepReport::wired(
        STEP,
        format!("deleted {removed} per-worktree code index file(s); `playbook code index` builds the shared store"),
    ))
}

fn with_sidecars(file: &Path) -> Vec<PathBuf> {
    ["-wal", "-shm", ""]
        .iter()
        .map(|suffix| {
            let mut name = file.as_os_str().to_owned();
            name.push(suffix);
            PathBuf::from(name)
        })
        .collect()
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect()
}

/// `data/code/<owner>/<repo>/*.db` and the old repo-wide `index.lock`.
fn per_worktree_code_files(data: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for owner in subdirs(&data.join("code")) {
        for repo in subdirs(&owner) {
            for entry in fs::read_dir(&repo).into_iter().flatten().flatten() {
                let path = entry.path();
                let is_db = path.extension().is_some_and(|x| x == "db");
                let is_lock = path.file_name().is_some_and(|n| n == "index.lock");
                let is_file = entry.file_type().is_ok_and(|t| t.is_file());
                if is_file && (is_db || is_lock) {
                    out.push(path);
                }
            }
        }
    }
    out
}

/// `repos/<owner>/<repo>/<worktree-id>/code/index.sqlite3` files whose
/// `user_version` is below the current schema.
fn old_state_indexes(repos: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for owner in subdirs(repos) {
        for repo in subdirs(&owner) {
            for worktree in subdirs(&repo) {
                let file = worktree
                    .join("code")
                    .join(playbook_code::host::FALLBACK_FILE);
                let is_file = fs::symlink_metadata(&file).is_ok_and(|m| m.is_file());
                if is_file
                    && schema_version(&file)
                        .is_some_and(|v| v != 0 && v < playbook_code::db::SCHEMA_VERSION)
                {
                    out.push(file);
                }
            }
        }
    }
    out
}

/// `PRAGMA user_version` of a SQLite file, or `None` for any other file.
fn schema_version(file: &Path) -> Option<i64> {
    let conn = playbook_store::Connection::open_with_flags(
        file,
        playbook_store::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    conn.query_row("PRAGMA user_version", [], |r| r.get(0)).ok()
}

/// One notice for an install that has the policy guard wired and never stored
/// `hooks.policyGuard.enabled`: it had the guard on by default and now has it
/// off. It changes nothing and never turns the guard on. A fresh install, or
/// one that stored the key either way, gets nothing.
fn policy_guard_off_notice(ctx: &Ctx) -> Outcome {
    let settings = ctx.claude_home.join("settings.json");
    let wired =
        crate::doctor::field::hook_commands_for_event(&settings, "PreToolUse", &["policy-guard"])
            .iter()
            .any(|(_, n)| *n > 0);
    let stored = matches!(
        crate::config::resolve(crate::hooks::policy_guard::ENABLED_KEY, &ctx.home, None),
        Ok((_, source)) if source != crate::config::Source::Default
    );
    if !wired || stored {
        return Outcome::Done(StepReport::skipped(
            POLICY_GUARD_NOTICE_STEP,
            "the policy guard was not relying on its old default",
        ));
    }
    Outcome::Done(StepReport::wired(
        POLICY_GUARD_NOTICE_STEP,
        POLICY_GUARD_NOTICE,
    ))
}

/// One notice for an install that had the rm guard wired: it now denies only
/// `rm` of `/` or the home folder, and `hooks.rmGuard.allowedRoots` is no
/// longer read. It changes nothing. A fresh install gets nothing.
fn rm_guard_simplified_notice(ctx: &Ctx) -> Outcome {
    let settings = ctx.claude_home.join("settings.json");
    let wired = crate::doctor::field::hook_commands_for_event(
        &settings,
        "PreToolUse",
        &["rm-workspace-guard"],
    )
    .iter()
    .any(|(_, n)| *n > 0);
    if !wired {
        return Outcome::Done(StepReport::skipped(
            RM_GUARD_NOTICE_STEP,
            "the rm guard was not in use",
        ));
    }
    Outcome::Done(StepReport::wired(RM_GUARD_NOTICE_STEP, RM_GUARD_NOTICE))
}

/// Removes the retired `no-slop-guard` and `no-dash-guard` entries older
/// setups wrote. The hook is gone; the subcommand stays a no-op for one
/// release. It writes settings.json, so only `init` does it.
fn no_slop_guard_entry(ctx: &Ctx) -> Outcome {
    let settings = ctx.claude_home.join("settings.json");
    match crate::init::retired_hooks::remove(&settings, ctx.epoch) {
        Ok(0) => Outcome::Quiet,
        Ok(n) => Outcome::Repeat(StepReport::wired(
            "retired-hooks",
            format!(
                "removed {n} retired hook entries (no-slop-guard, no-dash-guard); their rules live in the prompt and skills"
            ),
        )),
        Err(err) => Outcome::Failed(StepReport::failed("retired-hooks", err.to_string())),
    }
}

fn retired_hooks_pending(ctx: &Ctx) -> Option<String> {
    crate::init::retired_hooks::pending(&ctx.claude_home.join("settings.json"))
}

/// Stops and deletes every collector service file playbook wrote: the
/// agent under the current label, the one under the 0.37.0 label and the
/// systemd unit. Then it deletes the answers the retired setup part stored
/// and starts a collector, because `bootout` stopped the one the service ran
/// and open sessions still send to it, also when a removal fails part way.
/// `Owned`: a file goes only when playbook wrote it (its comment and the
/// label or description it wrote); any other file at those paths stays and
/// is reported with the commands that remove it. The service manager runs only where
/// `service_manager::refusal` allows it. Idempotent: with no file left it
/// reads a few paths and runs nothing, so a run the heal's start budget cut
/// short finishes on the next one.
fn collector_service_removed(ctx: &Ctx) -> Outcome {
    if let Some(why) = crate::service_manager::refusal(&ctx.home) {
        return match crate::init::collector_service::installed_names(&ctx.home) {
            Some(names) => Outcome::Warn(format!(
                "the {names} is still installed; playbook did not remove it, because {why}"
            )),
            None => Outcome::Quiet,
        };
    }
    let manager = crate::service_manager::manager_for(&ctx.home);
    remove_collector_service(ctx, manager.as_ref(), &start_collector)
}

/// Asks for a collector the way a command does, with no wait, so the heal
/// that runs this migration is not held up. A headless heal asks too.
fn start_collector() {
    let _ = crate::cc::telemetry::ensure_after_removal();
}

/// [`collector_service_removed`] with the service manager and the collector
/// start passed in, so tests use a fake launchd and count the starts.
fn remove_collector_service(
    ctx: &Ctx,
    manager: &dyn crate::service_manager::Manager,
    start: &dyn Fn(),
) -> Outcome {
    use crate::init::collector_service::{foreign_files, forget_answers, remove_with};
    const STEP: &str = "collector service";
    let home = &ctx.home;
    let removed = match remove_with(home, manager) {
        Ok(removed) => removed,
        Err(why) => {
            // An earlier bootout may have stopped the collector already.
            start();
            return Outcome::Failed(StepReport::failed(
                STEP,
                format!("{why}; to try again, run: playbook migrate"),
            ));
        }
    };
    forget_answers(home);
    let notes: Vec<String> = foreign_files(home)
        .iter()
        .map(|f| format!("{}; to remove it, run: {}", f.sentence(), f.fix))
        .collect();
    let foreign = (!notes.is_empty()).then(|| notes.join("\n"));
    match (removed, foreign) {
        (Some(done), foreign) => {
            start();
            let detail = match foreign {
                Some(note) => format!("{done}; {note}"),
                None => done,
            };
            Outcome::Repeat(StepReport::wired(STEP, detail))
        }
        (None, Some(note)) => Outcome::Warn(note),
        (None, None) => Outcome::Quiet,
    }
}

/// The numbered ids the releases up to 0.37.0 shipped (ADR 0046). The slug
/// is the id without its `NNNN-` prefix. Fixed: never add to it. A record row or a pending id under an
/// old id reads as its slug, and applying one of these writes both rows, so
/// an older binary still finds its own id.
const LEGACY_IDS: [&str; 19] = [
    "0001-memory-store-move",
    "0002-gate-repo-local-move",
    "0003-system-prompt-edited",
    "0004-skills-edited",
    "0005-statusline-edited",
    "0006-statusline-rust-command",
    "0007-shell-init-rc-line",
    "0008-remove-config-hash-script",
    "0009-adopt-late-config-json",
    "0010-state-files-to-sqlite",
    "0011-dead-hook-scripts",
    "0012-other-playbook-plugins",
    "0013-plugin-version-range",
    "0014-orphaned-plugin-caches",
    "0015-attribution-off",
    "0016-collector-service",
    "0017-auto-budget-defaults",
    "0018-retired-launcher-rc-line",
    "0019-system-prompt-default-on",
];

/// The slug for `id`: itself, or the slug of an old numbered id. A numbered
/// id outside [`LEGACY_IDS`], such as one a dev build wrote, stays as it is.
pub fn slug_of(id: &str) -> &str {
    LEGACY_IDS
        .iter()
        .find(|old| **old == id)
        .map_or(id, |old| &old[LEGACY_PREFIX..])
}

/// The length of the `NNNN-` prefix of an old id.
const LEGACY_PREFIX: usize = 5;

/// The old numbered id of `slug`, when a release before ADR 0046 shipped it.
fn legacy_id(slug: &str) -> Option<&'static str> {
    LEGACY_IDS
        .iter()
        .find(|old| &old[LEGACY_PREFIX..] == slug)
        .copied()
}

/// The record `lines` with a slug row added next to each numbered applied
/// row that lacks one, or `None` when nothing is missing. The numbered rows
/// stay, so an older binary's `setup` still skips what ran.
fn converted(lines: &[String]) -> Option<Vec<String>> {
    let mut out = lines.to_vec();
    for line in lines {
        let Some(id) = line.strip_prefix("applied ") else {
            continue;
        };
        let slug = slug_of(id);
        let row = format!("applied {slug}");
        if slug != id && !out.contains(&row) {
            out.push(row);
        }
    }
    (out.len() != lines.len()).then_some(out)
}

/// Writes the slug rows [`converted`] adds. The caller holds the migration
/// lock. Running it again changes nothing.
fn convert_record(home: &Path) -> std::io::Result<()> {
    let Some(lines) = load_state(home) else {
        return Err(std::io::Error::other("could not read the migration record"));
    };
    match converted(&lines) {
        Some(lines) => write_state(home, &lines),
        None => Ok(()),
    }
}

/// An earlier install without the prompt, now that `pb cc` loads it by
/// default. A stored answer keeps it quiet (ADR 0044 decision 8).
fn system_prompt_default_pending(ctx: &Ctx) -> Option<String> {
    crate::init::system_prompt::earlier_install_without_prompt(&ctx.home, &ctx.claude_home).then(|| {
        "pb cc now loads the playbook system prompt by default and none is installed; to keep it off, run: playbook config set systemPrompt.enabled false".to_string()
    })
}

/// Setup's system-prompt part places the file and stores the answer.
fn system_prompt_default_by_setup(_: &Ctx) -> Outcome {
    Outcome::Quiet
}

/// Writable rc files that still hold a line the retired launcher wrote.
fn retired_rc_line_pending(ctx: &Ctx) -> Option<String> {
    let files = crate::init::rcfile::retired_rc_files(&ctx.home);
    (!files.is_empty()).then(|| {
        format!(
            "{} still loads the retired playbook launcher; setup removes those lines after a backup",
            join_paths(&files)
        )
    })
}

/// Removes the retired launcher lines after a backup (ADR 0044 decision 6).
fn retired_rc_line(ctx: &Ctx) -> Outcome {
    let changed = crate::init::rcfile::strip_retired_rc_lines(&ctx.home, ctx.epoch);
    let backups: Vec<PathBuf> = changed.iter().filter_map(|c| c.backup.clone()).collect();
    let failed: Vec<String> = changed
        .iter()
        .filter_map(|c| Some(format!("{}: {}", c.rc_file.display(), c.error.as_deref()?)))
        .collect();
    let done = (!backups.is_empty()).then(|| {
        format!(
            "removed the retired launcher line, backup at {}",
            join_paths(&backups)
        )
    });
    if !failed.is_empty() {
        let mut detail = format!(
            "could not remove the retired launcher line from {}",
            failed.join("; ")
        );
        if let Some(done) = done {
            detail.push_str(&format!("; {done}"));
        }
        return Outcome::Failed(StepReport::failed("rc-line", detail));
    }
    match done {
        Some(done) => Outcome::Repeat(StepReport::wired("rc-line", done)),
        None => Outcome::Quiet,
    }
}

/// The default both auto budgets had before the 2026-10-17 amendment to ADR
/// 0028, in US dollars.
const OLD_AUTO_BUDGET_USD: f64 = 5.0;

/// Moves a stored `auto.budgetUsd` or `auto.agentBudgetUsd` that holds the
/// old default of 5 to the new default (ADR 0028, 2026-10-17 amendment). Any
/// other stored value was chosen on purpose and stays. A key that is not
/// stored already resolves to the new default, so nothing is written for it.
/// A global `auto.sessionCeilingUsd` the user set below the new default caps
/// the moved value, so the ceiling stays at or above both budgets.
fn auto_budget_defaults(ctx: &Ctx) -> Outcome {
    use crate::config::keys::{AUTO_AGENT_BUDGET_USD, AUTO_BUDGET_USD};
    let root = playbook_root_from(&ctx.home);
    let ceiling = crate::config::resolve("auto.sessionCeilingUsd", &ctx.home, None)
        .ok()
        .and_then(|(value, _)| crate::config::auto_cap_cents(&value));
    let rows = [
        ("auto.budgetUsd", AUTO_BUDGET_USD),
        ("auto.agentBudgetUsd", AUTO_AGENT_BUDGET_USD),
    ];
    let mut moved = Vec::new();
    for (key, default) in rows {
        let new_value = match ceiling {
            Some(cents) if cents < i64::from(default) * 100 => {
                serde_json::Value::from(cents as f64 / 100.0)
            }
            _ => serde_json::Value::from(default),
        };
        match crate::config::store::replace_number(&root, key, OLD_AUTO_BUDGET_USD, &new_value) {
            Ok(0) => {}
            Ok(_) => moved.push(format!(
                "{key} $5 to ${}",
                new_value.to_string().trim_end_matches(".0")
            )),
            Err(err) => return Outcome::Failed(StepReport::failed("auto budget", err.to_string())),
        }
    }
    if moved.is_empty() {
        return Outcome::Done(StepReport::skipped(
            "auto budget",
            "no stored auto budget held the old $5 default",
        ));
    }
    let capped = match ceiling {
        Some(cents) if cents < i64::from(AUTO_BUDGET_USD) * 100 => {
            " (held at your auto.sessionCeilingUsd; set it to 0 for ten times auto.budgetUsd)"
        }
        _ => "",
    };
    Outcome::Done(StepReport::wired(
        "auto budget",
        format!(
            "raised the old default: {}{capped}. Run `playbook config set <key> <usd>` to pick another cap",
            moved.join(", ")
        ),
    ))
}

/// Moves a stored `update.plugin` boolean to the word the key takes now:
/// `true` was the automatic update, `false` was off. Any word stays.
fn plugin_update_mode(ctx: &Ctx) -> Outcome {
    use serde_json::Value;
    const STEP: &str = "plugin update mode";
    const KEY: &str = "update.plugin";
    const MOVES: [(bool, &str); 2] = [(true, "auto"), (false, "off")];
    let root = playbook_root_from(&ctx.home);
    let mut moved = 0;
    for (old, new) in MOVES {
        match crate::config::store::replace_value(
            &root,
            KEY,
            &Value::Bool(old),
            &Value::String(new.to_string()),
        ) {
            Ok(n) => moved += n,
            Err(err) => return Outcome::Failed(StepReport::failed(STEP, err.to_string())),
        }
    }
    if moved == 0 {
        return Outcome::Done(StepReport::skipped(
            STEP,
            "no stored update.plugin held a true or false",
        ));
    }
    Outcome::Done(StepReport::wired(
        STEP,
        format!("moved update.plugin from true or false to auto or off in {moved} place(s)"),
    ))
}

/// Hook entries whose `~/.claude/hooks/` script is gone (ADR 0034 decision
/// 9). Removing them writes settings.json, so only `init` does it.
fn dead_hook_scripts(ctx: &Ctx) -> Outcome {
    let settings = ctx.claude_home.join("settings.json");
    match crate::init::dead_hooks::remove(&settings, &ctx.home, &ctx.claude_home, ctx.epoch) {
        Ok(removed) if removed.is_empty() => Outcome::Quiet,
        Ok(removed) => Outcome::Repeat(StepReport::wired(
            "dead-hooks",
            format!(
                "removed {} hook(s) that no longer exist: {}",
                removed.len(),
                removed.join(", ")
            ),
        )),
        Err(err) => Outcome::Failed(StepReport::failed("dead-hooks", err.to_string())),
    }
}

fn dead_hook_scripts_pending(ctx: &Ctx) -> Option<String> {
    crate::init::dead_hooks::pending(
        &ctx.claude_home.join("settings.json"),
        &ctx.home,
        &ctx.claude_home,
    )
}

/// The retired `playbook-hooks` plugin, or a `playbook` plugin from another
/// marketplace, runs next to this one. Claude Code owns the plugin list.
fn other_playbook_plugins(ctx: &Ctx) -> Outcome {
    let cwd = std::env::current_dir().unwrap_or_default();
    let keys = crate::common::plugin_state::other_playbook_plugins(&ctx.claude_home, &cwd);
    if keys.is_empty() {
        return Outcome::Quiet;
    }
    let commands: Vec<String> = keys
        .iter()
        .map(|k| format!("claude plugin uninstall {k}"))
        .collect();
    Outcome::Warn(format!(
        "installed next to {}, so hooks or skills run twice: {}. Run: {}",
        crate::common::plugin_state::installed_key(&ctx.claude_home),
        keys.join(", "),
        commands.join("; ")
    ))
}

/// The installed plugin is not in the minor line of this binary (ADR 0038
/// decision 7). Reads what Claude Code installed under `ctx.claude_home`,
/// not the process environment.
fn plugin_version_range(ctx: &Ctx) -> Outcome {
    let exe = std::env::current_exe().unwrap_or_default();
    let homebrew = crate::update::is_homebrew(&std::fs::canonicalize(&exe).unwrap_or(exe));
    crate::plugin_root::find(None, &ctx.claude_home)
        .and_then(|root| crate::plugin_root::plugin_version(&root))
        .and_then(|v| {
            crate::plugin_root::plugin_out_of_range(
                &v,
                env!("CARGO_PKG_VERSION"),
                &crate::common::plugin_state::installed_key(&ctx.claude_home),
                homebrew,
            )
        })
        .map_or(Outcome::Quiet, Outcome::Warn)
}

/// Old plugin folders Claude Code marked for removal. Playbook ignores them
/// and never deletes them. Scoped `Owned` so the note shows in `init` and
/// `doctor --fix` but the session hook does not repeat it.
fn orphaned_plugin_caches(ctx: &Ctx) -> Outcome {
    let dirs = crate::plugin_root::orphaned_cache_dirs(&ctx.claude_home);
    if dirs.is_empty() {
        return Outcome::Quiet;
    }
    Outcome::Warn(format!(
        "{} old plugin folder(s) are marked orphaned; playbook ignores them and Claude Code removes them: {}",
        dirs.len(),
        join_paths(&dirs)
    ))
}

/// A `config.json` written after the SQLite import (an older binary, or old
/// docs) would be ignored without a word. Adopt it when it agrees with the
/// store, and warn with its path when it does not. Runs on every init.
fn adopt_late_config_json(ctx: &Ctx) -> Outcome {
    let root = crate::common::paths::playbook_root_from(&ctx.home);
    match crate::config::store::adopt_late_files(&root) {
        Ok(report) if !report.conflicts.is_empty() => Outcome::Warn(
            crate::config::adopt_late_config(&ctx.home)
                .first()
                .cloned()
                .unwrap_or_default(),
        ),
        Ok(report) if !report.imported.is_empty() => Outcome::Repeat(StepReport::wired(
            "config",
            format!(
                "adopted {} late config.json file(s) into the SQLite store",
                report.imported.len()
            ),
        )),
        _ => Outcome::Quiet,
    }
}

/// The migration record and the worktree sweep markers live in the `state`
/// table now. Opening the store imports the old files, so this only makes sure
/// that happened and fails loudly when the store cannot be opened. The import
/// itself is reported by `run_pending`, which sees the files before any
/// earlier migration opens the store.
fn state_files_to_sqlite(ctx: &Ctx) -> Outcome {
    let root = crate::common::paths::playbook_root_from(&ctx.home);
    match crate::state::list(&root, "") {
        Ok(_) => Outcome::Quiet,
        Err(err) => Outcome::Failed(StepReport::failed("state", err.to_string())),
    }
}

fn memory_store_move(ctx: &Ctx) -> Outcome {
    let step = crate::init::memory_migrate::migrate_memory(&ctx.home, &ctx.claude_home);
    if step.status == StepStatus::Failed {
        Outcome::Failed(step)
    } else {
        Outcome::Repeat(step)
    }
}

fn gate_repo_local_move(ctx: &Ctx) -> Outcome {
    let Some((repo_root, dest_base)) = &ctx.repo else {
        return Outcome::Quiet;
    };
    match crate::gate::db::migrate_legacy_repo_local(repo_root, dest_base) {
        Ok(()) => Outcome::Quiet,
        Err(err) => Outcome::Failed(StepReport::failed("gate-repo-local", err)),
    }
}

/// The config hash is computed in Rust now. Remove the shell copy that older
/// installs placed under the playbook root, and its directories when they end
/// up empty.
fn remove_config_hash_script(ctx: &Ctx) -> Outcome {
    let root = crate::common::paths::playbook_root_from(&ctx.home);
    let script = root.join("hooks/lib/config-hash.sh");
    if !script.is_file() {
        return Outcome::Quiet;
    }
    if let Err(err) = std::fs::remove_file(&script) {
        return Outcome::Failed(StepReport::failed("config-hash", err.to_string()));
    }
    for dir in [root.join("hooks/lib"), root.join("hooks")] {
        let _ = std::fs::remove_dir(&dir);
    }
    Outcome::Repeat(StepReport::wired(
        "config-hash",
        "removed the placed config-hash.sh, the hash now runs inside playbook",
    ))
}

fn statusline_rust_command_pending(ctx: &Ctx) -> Option<String> {
    let script = crate::init::statusline::playbook_statusline_path(&ctx.home);
    let edited = user_edited(&ctx.home, STATUSLINE_KEY, &script);
    crate::init::statusline::legacy_command_pending(
        &ctx.claude_home.join("settings.json"),
        &ctx.home,
        edited,
    )
    .then(|| "statusLine.command in settings.json still runs the old script".to_string())
}

fn join_paths(files: &[PathBuf]) -> String {
    files
        .iter()
        .map(|f| f.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn statusline_rust_command(ctx: &Ctx) -> Outcome {
    let settings = ctx.claude_home.join("settings.json");
    let script = crate::init::statusline::playbook_statusline_path(&ctx.home);
    let edited = user_edited(&ctx.home, STATUSLINE_KEY, &script);
    match crate::init::statusline::migrate_legacy_command(&settings, &ctx.home, edited) {
        Ok(true) => Outcome::Repeat(StepReport::wired(
            "statusline-command",
            "statusLine.command now runs `playbook statusline`",
        )),
        Ok(false) => Outcome::Quiet,
        Err(err) => Outcome::Failed(StepReport::failed("statusline-command", err.to_string())),
    }
}

fn system_prompt_path(home: &Path) -> PathBuf {
    crate::init::system_prompt::path(home)
}

fn system_prompt_edited(ctx: &Ctx) -> Outcome {
    let path = system_prompt_path(&ctx.home);
    if user_edited(&ctx.home, SYSTEM_PROMPT_KEY, &path) {
        Outcome::Warn(format!(
            "{} was edited after playbook installed it, so setup leaves it in place; to take the shipped copy, delete it and run `playbook setup --system-prompt`",
            path.display()
        ))
    } else {
        Outcome::Quiet
    }
}

fn statusline_edited(ctx: &Ctx) -> Outcome {
    let path = crate::init::statusline::playbook_statusline_path(&ctx.home);
    if user_edited(&ctx.home, STATUSLINE_KEY, &path) {
        Outcome::Warn(format!(
            "{} was edited after playbook installed it, so setup leaves it in place; setup no longer ships this script, so delete it once nothing runs it",
            path.display()
        ))
    } else {
        Outcome::Quiet
    }
}

/// `(key, path)` for every shipped skill file, keyed by plugin version dir.
fn skill_files(skills_root: &Path) -> Vec<(String, PathBuf)> {
    // A dev checkout is edited on purpose; only cached plugin copies are tracked.
    if skills_root.join(".git").exists() {
        return Vec::new();
    }
    let version = skills_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Ok(entries) = fs::read_dir(skills_root.join("skills")) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path().join("SKILL.md");
            let name = e.file_name().to_string_lossy().into_owned();
            path.is_file()
                .then(|| (format!("{SKILL_KEY_PREFIX}{version}:{name}"), path))
        })
        .collect();
    files.sort();
    files
}

fn skills_edited(ctx: &Ctx) -> Outcome {
    let Some(self_root) = &ctx.self_root else {
        return Outcome::Quiet;
    };
    let lines = read_state(&ctx.home);
    let edited: Vec<String> = skill_files(self_root)
        .into_iter()
        .filter(|(key, path)| edited_in(&lines, key, path))
        .map(|(key, _)| key.rsplit(':').next().unwrap_or_default().to_string())
        .collect();
    if edited.is_empty() {
        Outcome::Quiet
    } else {
        Outcome::Warn(format!(
            "skills edited after install: {}; a plugin update replaces them, so move your edits into your own skill",
            edited.join(", ")
        ))
    }
}

/// FNV-1a 64: stable across builds, which a persisted hash needs.
pub(crate) fn content_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The migration record as the `applied <id>` and `shipped <key> <hash>` lines
/// it has always had, now read from the `state` table. `None` when the store
/// cannot be read, so a write never wipes a record it failed to load.
fn load_state(home: &Path) -> Option<Vec<String>> {
    if let Some(lines) = SNAPSHOT.with(|s| s.borrow().clone()) {
        return Some(lines);
    }
    let root = playbook_root_from(home);
    let applied = crate::state::list(&root, crate::state::MIGRATIONS_APPLIED).ok()?;
    let shipped = crate::state::list(&root, crate::state::MIGRATIONS_SHIPPED).ok()?;
    Some(record_lines(applied, shipped))
}

thread_local! {
    /// The record `plan` read once without writing. While it is set, every
    /// read of the record on this thread returns it and opens no store.
    static SNAPSHOT: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

/// The record read through read-only connections, which create, import and
/// change nothing. `None` when the store cannot be read.
fn load_state_read_only(home: &Path) -> Option<Vec<String>> {
    let root = playbook_root_from(home);
    let applied = crate::state::list_read_only(&root, crate::state::MIGRATIONS_APPLIED).ok()?;
    let shipped = crate::state::list_read_only(&root, crate::state::MIGRATIONS_SHIPPED).ok()?;
    Some(record_lines(applied, shipped))
}

fn record_lines(applied: Vec<(String, String)>, shipped: Vec<(String, String)>) -> Vec<String> {
    let mut lines: Vec<String> = applied
        .into_iter()
        .map(|(k, _)| format!("applied {}", &k[crate::state::MIGRATIONS_APPLIED.len()..]))
        .collect();
    lines.extend(shipped.into_iter().map(|(k, v)| {
        format!(
            "shipped {} {v}",
            &k[crate::state::MIGRATIONS_SHIPPED.len()..]
        )
    }));
    lines
}

fn read_state(home: &Path) -> Vec<String> {
    load_state(home).unwrap_or_default()
}

/// Replaces the whole record with `lines`, in one transaction.
fn write_state(home: &Path, lines: &[String]) -> std::io::Result<()> {
    use crate::state::{MIGRATIONS_APPLIED, MIGRATIONS_SHIPPED};
    let root = playbook_root_from(home);
    crate::state::transaction(&root, |tx| {
        tx.delete_prefix("migrations/")?;
        for line in lines {
            if let Some(id) = line.strip_prefix("applied ") {
                tx.set(&format!("{MIGRATIONS_APPLIED}{id}"), "1")?;
            } else if let Some((key, hash)) = line
                .strip_prefix("shipped ")
                .and_then(|rest| rest.rsplit_once(' '))
            {
                tx.set(&format!("{MIGRATIONS_SHIPPED}{key}"), hash)?;
            }
        }
        Ok(())
    })
    .map_err(|e| std::io::Error::other(e.to_string()))
}

/// Whether `path` differs from the hash recorded when playbook placed it.
/// No record means nothing was placed by us, so never "edited".
pub fn user_edited(home: &Path, key: &str, path: &Path) -> bool {
    edited_in(&read_state(home), key, path)
}

/// Whether a hash was recorded for `key` and `path` still matches it: the
/// only proof that the file at `path` is the one playbook placed, untouched.
pub fn placed_unchanged(home: &Path, key: &str, path: &Path) -> bool {
    let prefix = format!("shipped {key} ");
    read_state(home)
        .iter()
        .find_map(|l| l.strip_prefix(&prefix).map(str::to_string))
        .zip(fs::read(path).ok())
        .is_some_and(|(recorded, bytes)| content_hash(&bytes) == recorded)
}

fn edited_in(lines: &[String], key: &str, path: &Path) -> bool {
    let prefix = format!("shipped {key} ");
    let Some(recorded) = lines
        .iter()
        .find_map(|l| l.strip_prefix(&prefix).map(str::to_string))
    else {
        return false;
    };
    fs::read(path).is_ok_and(|bytes| content_hash(&bytes) != recorded)
}

/// Records the hash of the file playbook just placed at `path`.
pub fn record_shipped(home: &Path, key: &str, path: &Path) {
    record_many(home, &[(key.to_string(), path.to_path_buf())], None);
}

/// Records several files in one locked write; `drop` clears matching stale keys first.
fn record_many(home: &Path, files: &[(String, PathBuf)], drop: Option<&dyn Fn(&str) -> bool>) {
    let hashed: Vec<(String, String)> = files
        .iter()
        .filter_map(|(key, path)| {
            let bytes = fs::read(path).ok()?;
            Some((format!("shipped {key} "), content_hash(&bytes)))
        })
        .collect();
    with_lock(home, || {
        let Some(mut lines) = load_state(home) else {
            return;
        };
        let before = lines.clone();
        if let Some(drop) = drop {
            lines.retain(|l| !l.strip_prefix("shipped ").is_some_and(drop));
        }
        for (prefix, hash) in &hashed {
            lines.retain(|l| !l.starts_with(prefix));
            lines.push(format!("{prefix}{hash}"));
        }
        let mut before_sorted = before;
        before_sorted.sort();
        lines.sort();
        if lines != before_sorted {
            let _ = write_state(home, &lines);
        }
    });
}

/// Records shipped skill files, never an edited one, and drops other versions' records.
pub fn record_skills(home: &Path, skills_root: &Path) {
    let files = skill_files(skills_root);
    let Some(version_prefix) = files
        .first()
        .and_then(|(key, _)| key.rsplit_once(':'))
        .map(|(v, _)| format!("{v}:"))
    else {
        return;
    };
    // An edited skill keeps its old record so it stays reported until resolved.
    let lines = read_state(home);
    let fresh: Vec<_> = files
        .into_iter()
        .filter(|(key, path)| !edited_in(&lines, key, path))
        .collect();
    let stale = |key: &str| key.starts_with(SKILL_KEY_PREFIX) && !key.starts_with(&version_prefix);
    record_many(home, &fresh, Some(&stale));
}

/// One item a run left for the user: a `Consent` migration's pending change or
/// a `Report` warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenItem {
    pub id: &'static str,
    pub message: String,
    /// The init command that applies a `Consent` change. `None` for a report.
    pub command: Option<&'static str>,
}

/// The open `Consent` and `Report` items, writing nothing. `only` limits the
/// checks to those ids.
pub fn open_items(ctx: &Ctx, only: Option<&[String]>) -> Vec<OpenItem> {
    open_items_in(&active(), ctx, only)
}

/// [`open_items`] over `migrations`.
fn open_items_in(migrations: &[Migration], ctx: &Ctx, only: Option<&[String]>) -> Vec<OpenItem> {
    migrations
        .iter()
        .filter(|m| only.is_none_or(|ids| ids.iter().any(|id| id == m.id)))
        .filter_map(|m| match m.scope {
            Scope::Consent { check, command } => check(ctx).map(|message| OpenItem {
                id: m.id,
                message,
                command: Some(command),
            }),
            Scope::Report => match (m.run)(ctx) {
                Outcome::Warn(message) => Some(OpenItem {
                    id: m.id,
                    message,
                    command: None,
                }),
                _ => None,
            },
            Scope::Owned => None,
        })
        .collect()
}

/// An empty report with one note when the real home is off limits to this
/// build (`common::real_home`), else `None`.
fn refused_report(ctx: &Ctx) -> Option<Report> {
    crate::common::real_home::refused(&ctx.home).then(|| Report {
        warnings: vec![format!(
            "migrations skipped: {}",
            crate::common::real_home::NOTE
        )],
        ..Report::default()
    })
}

/// Runs the `Owned` migrations that write, under the migration lock, taken
/// with `retries` tries 100 ms apart. A `Consent` migration never runs here.
/// No migration starts after `deadline`; the report then says it timed out.
pub fn run_owned(ctx: &Ctx, retries: u32, deadline: Option<Instant>) -> Report {
    let owned: Vec<Migration> = active().into_iter().filter(heal_runs).collect();
    let mut report = Report::default();
    if let Some(refused) = refused_report(ctx) {
        return refused;
    }
    let locked = with_lock_retry(&ctx.home, retries, || {
        if convert_into(&ctx.home, &mut report) {
            run_idempotent(&owned, ctx, &mut report, deadline);
            run_auto(&owned, ctx, &mut report, deadline);
        }
    });
    report.lock_busy = !locked;
    report
}

/// Runs [`convert_record`] under a lock the caller holds, and notes a
/// failure in `report`. False when the record could not be converted, so
/// nothing else runs.
fn convert_into(home: &Path, report: &mut Report) -> bool {
    match convert_record(home) {
        Ok(()) => true,
        Err(err) => {
            report.failed = true;
            report.warnings.push(format!(
                "migrations skipped: could not convert the record: {err}"
            ));
            false
        }
    }
}

/// Whether a heal runs `m`: an `Owned` migration that writes.
fn heal_runs(m: &Migration) -> bool {
    matches!(m.scope, Scope::Owned) && !matches!(m.kind, Kind::Manual)
}

/// The notes of the `Manual` migrations scoped `Owned`: files playbook placed
/// and the user may have edited, and orphaned plugin folders. `init` and
/// `doctor --fix` show them; the session hook does not repeat them.
pub fn owned_notes(ctx: &Ctx) -> Vec<String> {
    active()
        .iter()
        .filter(|m| matches!(m.kind, Kind::Manual) && matches!(m.scope, Scope::Owned))
        .filter_map(|m| match (m.run)(ctx) {
            Outcome::Warn(msg) => Some(format!("{}: {msg}", m.id)),
            _ => None,
        })
        .collect()
}

/// Where a migration stands, read without writing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// An `Auto` migration in the applied record.
    Applied,
    /// An `Auto` migration not in the applied record yet.
    Pending,
    /// An `Idempotent` `Owned` migration, which runs on every heal.
    EveryRun,
    /// A check that found nothing to do.
    Clear,
    /// A check that found something for the user, with the command that
    /// applies it when it is a `Consent` change.
    Open {
        message: String,
        command: Option<&'static str>,
    },
    /// Not checked, because the check reads the record and old state files
    /// are still waiting to be imported into it.
    Unknown,
    /// Waits for the next release, so this build neither runs nor checks it.
    Unreleased,
}

impl State {
    /// The stable token for JSON output.
    pub fn code(&self) -> &'static str {
        match self {
            State::Applied => "applied",
            State::Pending => "pending",
            State::EveryRun => "every_run",
            State::Clear => "clear",
            State::Open { .. } => "open",
            State::Unknown => "unknown",
            State::Unreleased => "unreleased",
        }
    }
}

/// One registry entry and where it stands.
pub struct Planned {
    pub id: &'static str,
    /// The release that ships it, or `next`.
    pub since: Since,
    pub kind: &'static str,
    pub scope: &'static str,
    pub state: State,
    /// Whether a heal would start it now, unless the record is newer.
    pub runs: bool,
}

/// Checks that read the record, which `plan` skips while old state files
/// wait to be imported, because the record is not complete yet.
const READS_STATE: [&str; 4] = [
    "system-prompt-edited",
    "skills-edited",
    "statusline-edited",
    STATUSLINE_MIGRATION,
];

/// Where every migration stands, writing nothing: the record is read once
/// through read-only connections, and every check sees that copy. While old
/// state files wait to be imported, the checks that read the record are
/// skipped. `None` when the record cannot be read.
pub fn plan(ctx: &Ctx) -> Option<Vec<Planned>> {
    let root = playbook_root_from(&ctx.home);
    let state_ready = crate::state::pending_legacy(&root) == 0;
    let lines = load_state_read_only(&ctx.home)?;
    let applied_ids = applied(&lines);
    SNAPSHOT.with(|s| *s.borrow_mut() = Some(lines));
    let plan = std::panic::catch_unwind(|| planned(ctx, state_ready, &applied_ids));
    SNAPSHOT.with(|s| *s.borrow_mut() = None);
    plan.ok()
}

fn planned(ctx: &Ctx, state_ready: bool, applied_ids: &[String]) -> Vec<Planned> {
    registry()
        .iter()
        .map(|m| {
            let needs_state = matches!(m.kind, Kind::Auto) || READS_STATE.contains(&m.id);
            let state = if !m.released_in(env!("CARGO_PKG_VERSION")) {
                State::Unreleased
            } else if needs_state && !state_ready {
                State::Unknown
            } else {
                match (&m.kind, m.scope) {
                    (_, Scope::Consent { check, command }) => {
                        check(ctx).map_or(State::Clear, |message| State::Open {
                            message,
                            command: Some(command),
                        })
                    }
                    (Kind::Manual, _) => match (m.run)(ctx) {
                        Outcome::Warn(message) => State::Open {
                            message,
                            command: None,
                        },
                        _ => State::Clear,
                    },
                    (Kind::Auto, _) if applied_ids.iter().any(|d| d == m.id) => State::Applied,
                    (Kind::Auto, _) => State::Pending,
                    (Kind::Idempotent, _) => State::EveryRun,
                }
            };
            Planned {
                runs: heal_runs(m) && !matches!(state, State::Applied | State::Unreleased),
                id: m.id,
                since: m.since(),
                kind: match m.kind {
                    Kind::Auto => "auto",
                    Kind::Idempotent => "idempotent",
                    Kind::Manual => "manual",
                },
                scope: match m.scope {
                    Scope::Owned => "owned",
                    Scope::Consent { .. } => "consent",
                    Scope::Report => "report",
                },
                state,
            }
        })
        .collect()
}

/// Manual findings only, for `playbook doctor`; runs nothing else. `Report`
/// items are left out, because the doctor has its own rows for them.
pub fn pending_manual(ctx: &Ctx) -> Vec<String> {
    active()
        .iter()
        .filter(|m| matches!(m.kind, Kind::Manual) && !matches!(m.scope, Scope::Report))
        .filter_map(|m| match (m.run)(ctx) {
            Outcome::Warn(msg) => Some(format!("{}: {msg}", m.id)),
            _ => None,
        })
        .collect()
}

/// Records the system prompt placed by init so later edits are detectable.
pub fn record_system_prompt(home: &Path) {
    record_shipped(home, SYSTEM_PROMPT_KEY, &system_prompt_path(home));
}

fn with_lock(home: &Path, f: impl FnOnce()) -> bool {
    with_lock_retry(home, LOCK_RETRIES, f)
}

/// A migration can copy a whole store, so the stale age is far above the default.
const MIGRATION_LOCK_STALE: Duration = Duration::from_secs(300);

fn with_lock_retry(home: &Path, retries: u32, f: impl FnOnce()) -> bool {
    let root = playbook_root_from(home);
    if ensure_private_dir(&root).is_err() {
        return false;
    }
    let lock = root.join(LOCK_DIR);
    remove_stale_lock_dir(&lock, MIGRATION_LOCK_STALE);
    if !acquire_dir_lock(&lock, retries, Duration::from_millis(100)) {
        return false;
    }
    /// Removes the lock when it goes out of scope, also when `f` panics.
    struct Release<'a>(&'a Path);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            let _ = fs::remove_dir(self.0);
        }
    }
    let _release = Release(&lock);
    f();
    true
}

/// Runs the shipped registry.
#[cfg(test)]
pub fn run_pending(ctx: &Ctx) -> Report {
    run_pending_for(ctx, true, true)
}

/// Runs the shipped registry. With `statusline` false the migration that
/// rewrites `statusLine.command` stays pending, so init leaves that key alone.
/// With `hooks` false the dead and retired hook migrations stay pending, so init leaves
/// the hook entries alone.
pub fn run_pending_for(ctx: &Ctx, statusline: bool, hooks: bool) -> Report {
    let root = playbook_root_from(&ctx.home);
    let pending = crate::state::pending_legacy(&root);
    let mut migrations = active();
    migrations.retain(|m| {
        (statusline || m.id != STATUSLINE_MIGRATION)
            && (hooks || (m.id != DEAD_HOOKS_MIGRATION && m.id != NO_SLOP_GUARD_MIGRATION))
    });
    let mut report = run_with(&migrations, ctx);
    if pending > 0 && crate::state::pending_legacy(&root) == 0 {
        report.steps.insert(
            0,
            StepReport::wired(
                "state",
                format!(
                    "imported {pending} old state file(s) into the state table, each renamed to .migrated"
                ),
            ),
        );
    }
    report
}

/// Runs `migrations` in order: Manual ones always, Auto ones once each.
pub fn run_with(migrations: &[Migration], ctx: &Ctx) -> Report {
    run_with_retries(migrations, ctx, LOCK_RETRIES)
}

fn run_with_retries(migrations: &[Migration], ctx: &Ctx, retries: u32) -> Report {
    let mut report = Report::default();
    if let Some(refused) = refused_report(ctx) {
        return refused;
    }
    for m in migrations.iter().filter(|m| matches!(m.kind, Kind::Manual)) {
        if let Outcome::Warn(msg) = (m.run)(ctx) {
            report.warnings.push(format!("{}: {msg}", m.id));
        }
    }
    run_idempotent(migrations, ctx, &mut report, None);
    let lines = read_state(&ctx.home);
    let done = applied(&lines);
    let pending = |m: &&Migration| matches!(m.kind, Kind::Auto) && !done.iter().any(|d| d == m.id);
    if converted(&lines).is_none() && !migrations.iter().any(|m| pending(&m)) {
        return report;
    }
    let locked = with_lock_retry(&ctx.home, retries, || {
        if convert_into(&ctx.home, &mut report) {
            run_auto(migrations, ctx, &mut report, None);
        }
    });
    if !locked {
        report.lock_busy = true;
        report.steps.push(StepReport::skipped(
            "migrations",
            "could not take the migration lock; will retry on the next run",
        ));
    }
    report
}

/// The slugs of the applied rows, an old numbered row read as its slug.
fn applied(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|l| l.strip_prefix("applied ").map(|id| slug_of(id).to_string()))
        .collect()
}

/// Whether `deadline` has passed, which marks `report` as timed out.
fn out_of_time(deadline: Option<Instant>, report: &mut Report) -> bool {
    report.timed_out |= deadline.is_some_and(|d| Instant::now() >= d);
    report.timed_out
}

/// Runs every `Idempotent` migration in `migrations`. A failure is reported
/// and the rest still run.
fn run_idempotent(
    migrations: &[Migration],
    ctx: &Ctx,
    report: &mut Report,
    deadline: Option<Instant>,
) {
    for m in migrations
        .iter()
        .filter(|m| matches!(m.kind, Kind::Idempotent))
    {
        if out_of_time(deadline, report) {
            return;
        }
        match (m.run)(ctx) {
            Outcome::Failed(step) => {
                report.failed = true;
                report.steps.push(step);
            }
            Outcome::Done(step) | Outcome::Repeat(step) => report.steps.push(step),
            Outcome::Warn(msg) => report.warnings.push(format!("{}: {msg}", m.id)),
            Outcome::Quiet => {}
        }
    }
}

/// Runs each `Auto` migration not yet applied, in order, and records it. The
/// caller holds the migration lock. A failure stops the later ones.
fn run_auto(migrations: &[Migration], ctx: &Ctx, report: &mut Report, deadline: Option<Instant>) {
    for m in migrations.iter().filter(|m| matches!(m.kind, Kind::Auto)) {
        let Some(mut lines) = load_state(&ctx.home) else {
            report.failed = true;
            report
                .warnings
                .push("migrations skipped: could not read the migration record".to_string());
            break;
        };
        if applied(&lines).iter().any(|d| d == m.id) {
            continue;
        }
        if out_of_time(deadline, report) {
            return;
        }
        match (m.run)(ctx) {
            Outcome::Done(step) => {
                lines.push(format!("applied {}", m.id));
                if let Some(old) = legacy_id(m.id) {
                    lines.push(format!("applied {old}"));
                }
                if let Err(err) = write_state(&ctx.home, &lines) {
                    report.failed = true;
                    report
                        .warnings
                        .push(format!("{}: could not record: {err}", m.id));
                    break;
                }
                report.steps.push(step);
            }
            Outcome::Repeat(step) => report.steps.push(step),
            Outcome::Quiet => {}
            Outcome::Warn(msg) => report.warnings.push(format!("{}: {msg}", m.id)),
            Outcome::Failed(step) => {
                report.failed = true;
                report.steps.push(step);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    static CALLS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    static SLOW_RUNS: AtomicUsize = AtomicUsize::new(0);

    fn home(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("pb-migrate-{tag}-{}", crate::testing::run_id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ctx(home: &Path) -> Ctx {
        Ctx::for_home(home)
    }

    fn done(name: &'static str) -> Outcome {
        Outcome::Done(StepReport::wired(name, "ok"))
    }

    fn auto(id: &'static str, run: fn(&Ctx) -> Outcome) -> Migration {
        Migration {
            id,
            since: "0.1.0",
            kind: Kind::Auto,
            scope: Scope::Owned,
            run,
        }
    }

    fn order_a(_: &Ctx) -> Outcome {
        CALLS.lock().unwrap().push("a");
        done("a")
    }
    fn order_b(_: &Ctx) -> Outcome {
        CALLS.lock().unwrap().push("b");
        done("b")
    }

    /// Succeeds without touching `CALLS`, for tests that run beside the order test.
    fn runs_quietly(_: &Ctx) -> Outcome {
        done("quiet")
    }

    #[test]
    fn runs_in_registry_order_then_never_again() {
        let h = home("order");
        CALLS.lock().unwrap().clear();
        let ms = [auto("a", order_a), auto("b", order_b)];
        let first = run_with(&ms, &ctx(&h));
        let second = run_with(&ms, &ctx(&h));
        assert_eq!(*CALLS.lock().unwrap(), vec!["a", "b"]);
        assert_eq!(first.steps.len(), 2);
        assert!(second.steps.is_empty());
    }

    fn rows(home: &Path) -> Vec<String> {
        let root = playbook_root_from(home);
        let mut rows: Vec<String> = crate::state::list(&root, "migrations/")
            .unwrap()
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        rows.sort();
        rows
    }

    /// The rows without the index cleanup. Its `since` is `next` until a
    /// release fills it in, so whether it is recorded depends on that.
    fn rows_without_the_index_cleanup(home: &Path) -> Vec<String> {
        let mut rows = rows(home);
        rows.retain(|r| !r.contains(CODE_INDEX_FILES_MIGRATION));
        rows
    }

    /// The budget is stored and read under its old name, the way an older
    /// binary left it: the value migration runs before the key rename.
    const OLD_BUDGET_KEY: &str = "auto.budgetUsd";

    fn set_budget(home: &Path, usd: f64) {
        crate::config::store::put(
            &playbook_root_from(home),
            crate::config::write::Tier::Global,
            None,
            OLD_BUDGET_KEY,
            &serde_json::Value::from(usd),
        )
        .unwrap();
    }

    fn budget(home: &Path) -> String {
        crate::config::store::lookup(&playbook_root_from(home), OLD_BUDGET_KEY, None)
            .unwrap()
            .global
            .unwrap()
            .to_string()
    }

    #[test]
    fn a_record_from_0_37_converts_to_slugs_and_keeps_its_numbered_rows() {
        // Arrange: the rows a 0.37.0 heal leaves, with the budget migration
        // applied and a $5 budget the user set again on purpose afterwards.
        let h = home("convert");
        write_state(
            &h,
            &[
                "applied 0017-auto-budget-defaults".to_string(),
                "applied 0020-collector-service-label".to_string(),
                "shipped system-prompt abc".to_string(),
            ],
        )
        .unwrap();
        set_budget(&h, 5.0);

        // Act
        let first = run_owned(&ctx(&h), 0, None);
        let after_first = rows_without_the_index_cleanup(&h);
        let second = run_owned(&ctx(&h), 0, None);

        // Assert
        assert!(!first.failed && !second.failed, "{:?}", first.warnings);
        assert_eq!(
            after_first,
            [
                "migrations/applied/0017-auto-budget-defaults=1",
                "migrations/applied/0020-collector-service-label=1",
                "migrations/applied/auto-budget-defaults=1",
                "migrations/applied/plugin-update-mode=1",
                "migrations/applied/policy-guard-off-by-default=1",
                "migrations/applied/rm-guard-simplified=1",
                "migrations/shipped/system-prompt=abc",
            ]
        );
        assert_eq!(
            rows_without_the_index_cleanup(&h),
            after_first,
            "the conversion runs once"
        );
        assert_eq!(budget(&h), "5.0", "an applied migration does not run again");
    }

    #[test]
    fn a_record_that_names_the_retired_attribution_item_still_loads() {
        // Arrange: a 0.37.0 record with the retired item under both ids, and
        // settings with no attribution key, which its check used to report.
        let h = home("retired-attribution");
        let old = "applied 0015-attribution-off".to_string();
        let slug = format!("applied {RETIRED_ATTRIBUTION_MIGRATION}");
        write_state(&h, &[old, slug]).unwrap();
        fs::create_dir_all(h.join(".claude")).unwrap();
        fs::write(h.join(".claude/settings.json"), r#"{"model":"x"}"#).unwrap();

        // Act
        let report = run_owned(&ctx(&h), 0, None);
        let plan = plan(&ctx(&h)).expect("the record reads");
        let open = open_items(&ctx(&h), None);

        // Assert
        assert!(!report.failed, "{:?}", report.warnings);
        assert!(registry().iter().all(|m| !RETIRED_IDS.contains(&m.id)));
        assert!(plan.iter().all(|p| p.id != RETIRED_ATTRIBUTION_MIGRATION));
        assert!(open.iter().all(|i| i.id != RETIRED_ATTRIBUTION_MIGRATION));
        let rows = rows(&h);
        assert!(rows.contains(&"migrations/applied/0015-attribution-off=1".to_string()));
        assert!(rows.contains(&"migrations/applied/attribution-off=1".to_string()));
    }

    #[test]
    fn a_record_from_0_18_with_the_removed_shell_init_row_loads_and_ignores_it() {
        // Arrange: the row a pre-ADR 0044 install holds for the removed item.
        let h = home("shell-init-row");
        write_state(&h, &["applied 0007-shell-init-rc-line".to_string()]).unwrap();

        // Act
        let report = run_owned(&ctx(&h), 0, None);
        let plan = plan(&ctx(&h)).expect("the record reads");
        let open = open_items(&ctx(&h), None);

        // Assert
        assert!(!report.failed, "{:?}", report.warnings);
        assert!(registry()
            .iter()
            .all(|m| m.id != RETIRED_SHELL_INIT_MIGRATION));
        assert!(plan.iter().all(|p| p.id != RETIRED_SHELL_INIT_MIGRATION));
        assert!(open.iter().all(|i| i.id != RETIRED_SHELL_INIT_MIGRATION));
        assert!(rows(&h).contains(&"migrations/applied/0007-shell-init-rc-line=1".to_string()));
    }

    #[test]
    fn applying_a_migration_with_an_old_number_writes_both_rows() {
        // Arrange: a home from 0.35 that never ran the budget migration.
        let h = home("both-rows");
        set_budget(&h, 5.0);

        // Act
        let report = run_owned(&ctx(&h), 0, None);

        // Assert
        assert!(!report.failed, "{:?}", report.warnings);
        assert_ne!(budget(&h), "5.0");
        let rows = rows(&h);
        assert!(rows.contains(&"migrations/applied/auto-budget-defaults=1".to_string()));
        assert!(
            rows.contains(&"migrations/applied/0017-auto-budget-defaults=1".to_string()),
            "an older binary looks for its own id: {rows:?}"
        );
    }

    #[test]
    fn setup_converts_the_record_too() {
        // Arrange: only a numbered row, so no Auto migration is pending.
        let h = home("convert-setup");
        write_state(&h, &["applied 0017-auto-budget-defaults".to_string()]).unwrap();

        // Act
        let report = run_pending(&ctx(&h));

        // Assert
        assert!(!report.failed && !report.lock_busy, "{:?}", report.warnings);
        let rows = rows(&h);
        assert!(rows.contains(&"migrations/applied/auto-budget-defaults=1".to_string()));
        assert!(rows.contains(&"migrations/applied/0017-auto-budget-defaults=1".to_string()));
    }

    #[test]
    fn setup_says_so_when_the_lock_stops_the_conversion() {
        // Arrange: a numbered row to convert and a held lock.
        let h = home("convert-busy");
        write_state(&h, &["applied 0017-auto-budget-defaults".to_string()]).unwrap();
        fs::create_dir_all(playbook_root_from(&h).join(LOCK_DIR)).unwrap();

        // Act
        let report = run_with_retries(&[], &ctx(&h), 0);

        // Assert
        assert!(report.lock_busy);
        assert!(report
            .steps
            .iter()
            .any(|s| s.detail.contains("migration lock")));
        assert_eq!(rows(&h), ["migrations/applied/0017-auto-budget-defaults=1"]);
    }

    #[test]
    fn an_unreadable_record_stops_the_heal_before_anything_runs() {
        // Arrange: a store that is not a database.
        let h = home("convert-unreadable");
        let data = playbook_root_from(&h).join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("config.db"), "not a database").unwrap();

        // Act
        let report = run_owned(&ctx(&h), 0, None);

        // Assert
        assert!(report.failed);
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("could not convert the record")),
            "{:?}",
            report.warnings
        );
        assert!(
            report.steps.is_empty(),
            "nothing ran: {}",
            report.steps.len()
        );
    }

    #[test]
    fn an_old_state_file_is_imported_then_converted() {
        // Arrange: a 0.18 home that still has migrations.state.
        let h = home("convert-legacy");
        let root = playbook_root_from(&h);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("migrations.state"),
            "applied 0001-memory-store-move\napplied 0017-auto-budget-defaults\n",
        )
        .unwrap();

        // Act
        let report = run_owned(&ctx(&h), 0, None);

        // Assert
        assert!(!report.failed, "{:?}", report.warnings);
        let rows = rows(&h);
        for row in [
            "migrations/applied/0001-memory-store-move=1",
            "migrations/applied/memory-store-move=1",
            "migrations/applied/0017-auto-budget-defaults=1",
            "migrations/applied/auto-budget-defaults=1",
        ] {
            assert!(rows.contains(&row.to_string()), "{row}: {rows:?}");
        }
    }

    #[test]
    fn only_the_released_numbers_map_to_slugs() {
        // Arrange: (stored id, slug it reads as)
        let cases = [
            ("0015-attribution-off", "attribution-off"),
            ("0019-system-prompt-default-on", "system-prompt-default-on"),
            ("attribution-off", "attribution-off"),
            // Never released with a number, so a dev build's row stays as is.
            (
                "0020-collector-service-label",
                "0020-collector-service-label",
            ),
            ("0099-unknown", "0099-unknown"),
        ];

        for (stored, want) in cases {
            // Act
            let got = slug_of(stored);

            // Assert
            assert_eq!(got, want, "{stored}");
        }
        let slugs: Vec<&str> = registry().iter().map(|m| m.id).collect();
        for (i, old) in LEGACY_IDS.iter().enumerate() {
            let slug = slug_of(old);
            assert_eq!(
                *old,
                format!("{:04}-{slug}", i + 1),
                "the table keeps release order"
            );
            assert!(
                slugs.contains(&slug) || RETIRED_IDS.contains(&slug),
                "{old} names no migration"
            );
            assert_eq!(legacy_id(slug), Some(*old));
        }
        assert!(slugs
            .iter()
            .all(|id| !id.starts_with(|c: char| c.is_ascii_digit())));
    }

    #[test]
    fn the_prompt_default_item_is_open_for_an_earlier_install_until_an_answer_is_stored() {
        // Arrange
        let h = home("prompt-default");
        let fresh = home("prompt-default-fresh");
        settings_with(&h, "playbook statusline");

        // Act
        let open = open_items(&ctx(&h), None);
        let on_fresh = open_items(&ctx(&fresh), None);
        crate::config::write::set(
            crate::config::write::Tier::Global,
            "systemPrompt.enabled",
            &serde_json::Value::Bool(false),
            &h,
            None,
        )
        .unwrap();
        let after_no = open_items(&ctx(&h), None);

        // Assert
        let item = |items: &[OpenItem]| {
            items
                .iter()
                .find(|i| i.id == PROMPT_DEFAULT_MIGRATION)
                .cloned()
        };
        let got = item(&open).expect("open for an earlier install");
        assert_eq!(got.command, Some("playbook setup --only system-prompt"));
        assert!(got
            .message
            .contains("config set systemPrompt.enabled false"));
        assert!(item(&on_fresh).is_none());
        assert!(item(&after_no).is_none());
    }

    fn settings_with(h: &Path, command: &str) -> PathBuf {
        let path = h.join(".claude").join("settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = format!(
            r#"{{"model":"x","statusLine":{{"type":"command","command":{command:?},"refreshInterval":30}}}}"#
        );
        fs::write(&path, body).unwrap();
        path
    }

    fn command_of(path: &Path) -> String {
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        v["statusLine"]["command"].as_str().unwrap().to_string()
    }

    #[test]
    fn the_placed_config_hash_script_is_removed_once() {
        let h = home("cfg-hash");
        let lib = crate::common::paths::playbook_root_from(&h).join("hooks/lib");
        fs::create_dir_all(&lib).unwrap();
        fs::write(lib.join("config-hash.sh"), "config_hash() { :; }\n").unwrap();
        assert!(matches!(
            remove_config_hash_script(&ctx(&h)),
            Outcome::Repeat(_)
        ));
        assert!(!lib.join("config-hash.sh").exists());
        assert!(!lib.exists(), "empty lib dir goes too");
        assert!(matches!(
            remove_config_hash_script(&ctx(&h)),
            Outcome::Quiet
        ));
    }

    #[test]
    fn statusline_migration_rewrites_the_playbook_script_command() {
        let h = home("sl-old");
        let path = settings_with(&h, "bash $HOME/.config/playbook/statusline.sh");
        let out = statusline_rust_command(&ctx(&h));
        assert!(matches!(out, Outcome::Repeat(_)));
        assert_eq!(command_of(&path), "playbook statusline");
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        // The rewrite repairs the object, so the old 30 second interval comes
        // down to the playbook one; other keys stay.
        assert_eq!(
            v["statusLine"]["refreshInterval"],
            crate::init::statusline::REFRESH_INTERVAL_SECS
        );
        assert_eq!(v["model"], "x");
    }

    #[test]
    fn statusline_migration_rewrites_an_absolute_home_path_too() {
        let h = home("sl-abs");
        let cmd = format!("bash {}/.config/playbook/statusline.sh", h.display());
        let path = settings_with(&h, &cmd);
        statusline_rust_command(&ctx(&h));
        assert_eq!(command_of(&path), "playbook statusline");
    }

    #[test]
    fn statusline_migration_rewrites_a_tilde_path() {
        let h = home("sl-tilde");
        let path = settings_with(&h, "bash ~/.config/playbook/statusline.sh");
        statusline_rust_command(&ctx(&h));
        assert_eq!(command_of(&path), "playbook statusline");
    }

    #[test]
    fn statusline_migration_keeps_a_symlinked_settings_file_a_symlink() {
        let h = home("sl-link");
        let real = h.join("dotfiles").join("settings.json");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        let link = settings_with(&h, "bash $HOME/.config/playbook/statusline.sh");
        fs::rename(&link, &real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        statusline_rust_command(&ctx(&h));
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(command_of(&real), "playbook statusline");
    }

    #[test]
    fn statusline_migration_leaves_a_custom_command_alone() {
        let h = home("sl-custom");
        for cmd in [
            "bash $HOME/my-statusline.sh",
            "bash $HOME/.config/playbook/statusline.sh --extra",
            "sh $HOME/.config/playbook/statusline.sh",
            "my-tool render",
        ] {
            let path = settings_with(&h, cmd);
            let out = statusline_rust_command(&ctx(&h));
            assert!(matches!(out, Outcome::Quiet));
            assert_eq!(command_of(&path), cmd);
        }
    }

    #[test]
    fn statusline_migration_is_idempotent() {
        let h = home("sl-idem");
        let path = settings_with(&h, "bash $HOME/.config/playbook/statusline.sh");
        statusline_rust_command(&ctx(&h));
        let first = fs::read_to_string(&path).unwrap();
        let out = statusline_rust_command(&ctx(&h));
        assert!(matches!(out, Outcome::Quiet));
        assert_eq!(fs::read_to_string(&path).unwrap(), first);
    }

    #[test]
    fn statusline_migration_ignores_missing_or_invalid_settings() {
        let h = home("sl-none");
        assert!(matches!(statusline_rust_command(&ctx(&h)), Outcome::Quiet));
        let path = h.join(".claude").join("settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not json").unwrap();
        assert!(matches!(statusline_rust_command(&ctx(&h)), Outcome::Quiet));
        assert_eq!(fs::read_to_string(&path).unwrap(), "not json");
    }

    fn fails(_: &Ctx) -> Outcome {
        Outcome::Failed(StepReport::failed("boom", "nope"))
    }
    fn never(_: &Ctx) -> Outcome {
        panic!("must not run after a failure");
    }

    #[test]
    fn a_failure_is_not_recorded_and_stops_later_migrations() {
        let h = home("fail");
        let ms = [auto("f", fails), auto("n", never)];
        let report = run_with(&ms, &ctx(&h));
        assert_eq!(report.steps.len(), 1);
        assert_eq!(report.steps[0].status, StepStatus::Failed);
        assert!(report.failed);
        assert!(!read_state(&h).iter().any(|l| l == "applied f"));
    }

    #[test]
    fn a_held_lock_marks_the_report_busy_when_an_auto_migration_waits() {
        // Arrange
        let h = home("busy");
        fs::create_dir_all(playbook_root_from(&h).join(LOCK_DIR)).unwrap();

        // Act
        let report = run_with_retries(&[auto("n", never)], &ctx(&h), 0);

        // Assert
        assert!(report.lock_busy);
        assert!(!report.failed);
    }

    /// How long `sleepy` runs, past the deadline the test gives it. The
    /// margin keeps a loaded machine from reaching the first check late.
    const DEADLINE: Duration = Duration::from_secs(2);

    fn sleepy(_: &Ctx) -> Outcome {
        std::thread::sleep(DEADLINE + Duration::from_millis(100));
        done("sleepy")
    }

    #[test]
    fn a_deadline_lets_the_running_migration_finish_and_starts_no_other() {
        // Arrange
        let h = home("deadline");
        let ms = [auto("s", sleepy), auto("n", never)];
        let mut report = Report::default();
        let deadline = Instant::now() + DEADLINE;

        // Act
        let locked = with_lock_retry(&h, 0, || {
            run_auto(&ms, &ctx(&h), &mut report, Some(deadline))
        });

        // Assert
        assert!(locked && report.timed_out && !report.failed);
        let lines = read_state(&h);
        assert!(lines.iter().any(|l| l == "applied s"), "{lines:?}");
        assert!(!lines.iter().any(|l| l == "applied n"), "{lines:?}");
    }

    #[test]
    fn a_panic_under_the_lock_still_releases_it() {
        // Arrange
        let h = home("unwind");

        // Act
        let caught = std::panic::catch_unwind(|| with_lock_retry(&h, 0, || panic!("boom")));

        // Assert
        assert!(caught.is_err());
        assert!(!playbook_root_from(&h).join(LOCK_DIR).exists());
    }

    fn slow(_: &Ctx) -> Outcome {
        SLOW_RUNS.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(150));
        done("slow")
    }

    #[test]
    fn concurrent_runners_apply_a_migration_once() {
        let h = Arc::new(home("lock"));
        SLOW_RUNS.store(0, Ordering::SeqCst);
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let h = Arc::clone(&h);
                std::thread::spawn(move || {
                    run_with(&[auto("slow", slow)], &ctx(&h));
                })
            })
            .collect();
        for t in handles {
            t.join().unwrap();
        }
        assert_eq!(SLOW_RUNS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn warns_for_an_edited_file_and_not_for_an_untouched_one() {
        let h = home("edit");
        let prompt = system_prompt_path(&h);
        fs::create_dir_all(prompt.parent().unwrap()).unwrap();
        fs::write(&prompt, "shipped text").unwrap();
        record_system_prompt(&h);
        let clean = run_with(&registry(), &ctx(&h));
        assert!(clean.warnings.is_empty());

        fs::write(&prompt, "my edits").unwrap();
        let dirty = run_with(&registry(), &ctx(&h));
        assert_eq!(dirty.warnings.len(), 1);
        assert!(dirty.warnings[0].contains("edited"));
        let notes = owned_notes(&ctx(&h));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].starts_with("system-prompt-edited: "));
    }

    #[test]
    fn an_unrecorded_file_is_never_reported_as_edited() {
        let h = home("norecord");
        let prompt = system_prompt_path(&h);
        fs::create_dir_all(prompt.parent().unwrap()).unwrap();
        fs::write(&prompt, "anything").unwrap();
        assert!(!user_edited(&h, SYSTEM_PROMPT_KEY, &prompt));
    }

    static HELD_RUNS: AtomicUsize = AtomicUsize::new(0);
    fn held_probe(_: &Ctx) -> Outcome {
        HELD_RUNS.fetch_add(1, Ordering::SeqCst);
        done("held")
    }

    #[test]
    fn a_held_lock_skips_auto_migrations_with_a_warning() {
        let h = home("held");
        fs::create_dir_all(playbook_root_from(&h).join(LOCK_DIR)).unwrap();
        let report = run_with_retries(&[auto("held-a", held_probe)], &ctx(&h), 2);
        assert_eq!(HELD_RUNS.load(Ordering::SeqCst), 0);
        assert!(report.steps[0].detail.contains("lock"));
    }

    fn repeats(_: &Ctx) -> Outcome {
        Outcome::Repeat(StepReport::wired("rep", "ok"))
    }

    #[test]
    fn a_repeat_migration_is_never_recorded() {
        let h = home("repeat");
        let ms = [Migration {
            id: "r",
            since: "0.1.0",
            kind: Kind::Idempotent,
            scope: Scope::Owned,
            run: repeats,
        }];
        run_with(&ms, &ctx(&h));
        let second = run_with(&ms, &ctx(&h));
        assert_eq!(second.steps.len(), 1);
        assert!(!read_state(&h).iter().any(|l| l.starts_with("applied")));
    }

    fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push((p.clone(), fs::read(&p).unwrap_or_default()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn owned_and_report_migrations_write_nothing_outside_the_playbook_folder() {
        // Arrange: user files that the Consent migrations would change, and
        // Claude Code files the Report and orphan checks read.
        let h = home("scope-guard");
        let script = crate::init::statusline::playbook_statusline_path(&h);
        let settings = h.join(".claude/settings.json");
        fs::create_dir_all(h.join(".claude")).unwrap();
        fs::write(
            &settings,
            format!(
                r#"{{"statusLine":{{"type":"command","command":"bash {}"}},"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"~/.claude/hooks/gone.sh"}}]}}]}}}}"#,
                script.display()
            ),
        )
        .unwrap();
        let cache = h.join(".claude/plugins/cache/amplifiedengineer/playbook");
        fs::create_dir_all(cache.join("0.24.0")).unwrap();
        fs::write(cache.join("0.24.0/.orphaned_at"), "1").unwrap();
        fs::write(
            h.join(".claude/plugins/installed_plugins.json"),
            r#"{"version":2,"plugins":{"playbook-hooks@pragmatic-engineer":[{"scope":"user"}]}}"#,
        )
        .unwrap();
        fs::write(h.join(".zshrc"), "source ~/.claude/shell/zsh/cc.zsh\n").unwrap();
        fs::create_dir_all(h.join("project")).unwrap();
        fs::write(h.join("project/notes.md"), "mine").unwrap();
        let outside = |h: &Path| -> Vec<(PathBuf, Vec<u8>)> {
            snapshot(h)
                .into_iter()
                .filter(|(p, _)| !p.starts_with(playbook_root_from(h)))
                .collect()
        };
        let before = outside(&h);

        // Act
        let run = run_owned(&ctx(&h), 0, None);
        let notes = owned_notes(&ctx(&h));
        let open = open_items(&ctx(&h), None);

        // Assert
        assert!(!run.lock_busy && !run.failed);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("orphaned-plugin-caches: "));
        assert_eq!(outside(&h), before);
        let command = |id: &str| open.iter().find(|i| i.id == id).and_then(|i| i.command);
        assert_eq!(
            command("statusline-rust-command"),
            Some("playbook setup --statusline")
        );
        assert!(
            open.iter().all(|i| i.id != RETIRED_SHELL_INIT_MIGRATION),
            "retired by ADR 0044"
        );
        assert_eq!(command(RC_LINE_MIGRATION), Some("playbook setup"));
        assert_eq!(
            command(DEAD_HOOKS_MIGRATION),
            Some("playbook setup --hooks")
        );
        assert!(open
            .iter()
            .any(|i| i.id == "other-playbook-plugins" && i.command.is_none()));
    }

    #[test]
    fn the_policy_guard_notice_shows_once_and_only_where_the_key_was_never_stored() {
        let h = home("policy-guard-notice");
        let ctx = ctx(&h);
        let shown = |o: Outcome| match o {
            Outcome::Done(step) => step.status == crate::init::run::StepStatus::Wired,
            _ => panic!("the notice is an Auto migration that finishes"),
        };
        assert!(!shown(policy_guard_off_notice(&ctx)), "fresh install");
        let settings = ctx.claude_home.join("settings.json");
        let doc = serde_json::json!({"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
            {"type": "command", "command": "playbook hook policy-guard"}]}]}});
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, doc.to_string()).unwrap();
        let ms = [Migration {
            id: POLICY_GUARD_NOTICE_MIGRATION,
            since: "0.1.0",
            kind: Kind::Auto,
            scope: Scope::Owned,
            run: policy_guard_off_notice,
        }];
        let (mut first, mut second) = (Report::default(), Report::default());
        run_auto(&ms, &ctx, &mut first, None);
        run_auto(&ms, &ctx, &mut second, None);
        assert_eq!(first.steps.len(), 1, "shown on the first run");
        assert!(first.steps[0]
            .detail
            .contains("playbook config set --global hooks.policyGuard.enabled true"));
        assert!(second.steps.is_empty(), "recorded, so not shown again");
        assert!(
            !crate::hooks::policy_guard::stored_enabled(&h),
            "the notice never turns the guard on"
        );
    }

    #[test]
    fn the_policy_guard_notice_is_silent_when_the_key_is_stored_either_way() {
        for value in [true, false] {
            let h = home(&format!("policy-guard-stored-{value}"));
            let ctx = ctx(&h);
            let settings = ctx.claude_home.join("settings.json");
            let doc = serde_json::json!({"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": "playbook hook policy-guard"}]}]}});
            std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
            std::fs::write(&settings, doc.to_string()).unwrap();
            crate::config::write::set(
                crate::config::write::Tier::Global,
                crate::hooks::policy_guard::ENABLED_KEY,
                &serde_json::Value::Bool(value),
                &h,
                None,
            )
            .unwrap();
            let Outcome::Done(step) = policy_guard_off_notice(&ctx) else {
                panic!("the notice finishes");
            };
            assert!(
                step.status != crate::init::run::StepStatus::Wired,
                "stored as {value}"
            );
        }
    }

    #[test]
    fn the_no_slop_guard_entry_is_removed_once_and_other_entries_stay() {
        // Arrange
        let h = home("no-slop-entry");
        let ctx = ctx(&h);
        let settings = ctx.claude_home.join("settings.json");
        std::fs::create_dir_all(&ctx.claude_home).unwrap();
        let doc = serde_json::json!({"hooks": {"PreToolUse": [
            {"matcher": "Edit|Write", "hooks": [
                {"type": "command", "command": "playbook hook no-slop-guard"},
                {"type": "command", "command": "playbook hook policy-guard"}]}]}});
        std::fs::write(&settings, doc.to_string()).unwrap();

        // Act
        let first = no_slop_guard_entry(&ctx);
        let second = no_slop_guard_entry(&ctx);

        // Assert
        assert!(matches!(first, Outcome::Repeat(_)));
        assert!(matches!(second, Outcome::Quiet));
        let text = std::fs::read_to_string(&settings).unwrap();
        assert!(!text.contains("no-slop-guard") && text.contains("policy-guard"));
    }

    #[test]
    fn the_rm_guard_notice_shows_once_and_only_where_the_guard_was_wired() {
        let h = home("rm-guard-notice");
        let ctx = ctx(&h);
        let step = |o: Outcome| match o {
            Outcome::Done(step) => step,
            _ => panic!("the notice is an Auto migration that finishes"),
        };
        let fresh = step(rm_guard_simplified_notice(&ctx));
        assert!(
            fresh.status != crate::init::run::StepStatus::Wired,
            "fresh install"
        );
        let settings = ctx.claude_home.join("settings.json");
        let doc = serde_json::json!({"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
            {"type": "command", "command": "playbook hook rm-workspace-guard"}]}]}});
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, doc.to_string()).unwrap();
        let ms = [Migration {
            id: RM_GUARD_NOTICE_MIGRATION,
            since: "0.1.0",
            kind: Kind::Auto,
            scope: Scope::Owned,
            run: rm_guard_simplified_notice,
        }];
        let (mut first, mut second) = (Report::default(), Report::default());
        run_auto(&ms, &ctx, &mut first, None);
        run_auto(&ms, &ctx, &mut second, None);
        assert_eq!(first.steps.len(), 1, "shown on the first run");
        assert!(first.steps[0].detail.contains("hooks.rmGuard.allowedRoots"));
        assert!(second.steps.is_empty(), "recorded, so not shown again");
    }

    #[test]
    fn the_statusline_check_matches_the_rewrite_and_skips_an_edited_script() {
        let h = home("statusline-pending");
        let script = crate::init::statusline::playbook_statusline_path(&h);
        let path = settings_with(&h, &format!("bash {}", script.display()));
        let pending = |edited| crate::init::statusline::legacy_command_pending(&path, &h, edited);
        assert!(pending(false));
        assert!(!pending(true));
        settings_with(&h, "my-own-statusline");
        assert!(!pending(false));
    }

    #[test]
    fn every_migration_is_safe_and_idempotent_on_a_fresh_machine() {
        // Arrange: an empty home, so every file a migration reads is missing.
        let h = home("fresh-all");

        // Act
        let first = run_pending(&ctx(&h));
        let after_first = snapshot(&h);
        let second = run_pending(&ctx(&h));

        // Assert
        for report in [&first, &second] {
            assert!(!report.failed && !report.lock_busy);
            assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        }
        assert!(snapshot(&h)
            .iter()
            .all(|(p, _)| p.starts_with(playbook_root_from(&h))));
        assert_eq!(snapshot(&h), after_first);
        assert!(open_items(&ctx(&h), None).is_empty());
    }

    fn with_since(id: &'static str, since: &'static str) -> Migration {
        Migration {
            since,
            ..auto(id, never)
        }
    }

    #[test]
    fn a_migration_is_released_only_in_builds_at_or_above_its_since() {
        // Arrange: (since, running build, released)
        let cases = [
            ("0.36.0", "0.36.0", true),
            ("0.36.0", "0.37.0", true),
            ("0.36.0", "1.0.0", true),
            ("0.37.0", "0.36.0", false),
            ("0.37.0", "0.37.1", true),
            ("0.37.1", "0.37.0", false),
            ("0.10.0", "0.9.0", false),
            ("0.9.0", "0.10.0", true),
            // A dev build reports the last release, so `next` waits.
            (NEXT, "0.37.0", false),
            (NEXT, "999.0.0", false),
            (NEXT, "0.0.0", false),
            // A typo stays unreleased, where `compare` would read it as 0.
            ("Next", "0.37.0", false),
            ("0.37", "0.37.0", false),
            ("v0.36.0", "0.37.0", false),
        ];

        for (since, running, want) in cases {
            // Act
            let got = with_since("m", since).released_in(running);

            // Assert
            assert_eq!(got, want, "since {since} in {running}");
        }
    }

    #[test]
    fn a_build_runs_and_lists_only_the_migrations_it_ships() {
        // Arrange
        let ids = |running: &str| -> Vec<&'static str> {
            shipped(registry(), running).iter().map(|m| m.id).collect()
        };

        // Act
        let at_034 = ids("0.34.0");
        let at_037 = ids("0.37.0");
        let now = ids(env!("CARGO_PKG_VERSION"));

        // Assert
        assert_eq!(at_034.len(), 13, "{at_034:?}");
        assert!(!at_034.contains(&AUTO_BUDGET_MIGRATION));
        assert!(at_037.contains(&PROMPT_DEFAULT_MIGRATION));
        let unreleased: Vec<&str> = registry()
            .iter()
            .filter(|m| m.since() == Since::Next)
            .map(|m| m.id)
            .collect();
        assert!(unreleased.iter().all(|id| !now.contains(id)));
        assert_eq!(now.len() + unreleased.len(), registry().len());
    }

    fn always_open(_: &Ctx) -> Option<String> {
        Some("open".into())
    }

    #[test]
    fn a_next_entry_never_runs_or_opens_where_a_released_one_does() {
        // Arrange: each kind and scope twice, once released and once next.
        // The next ones panic if they run.
        let consent = Scope::Consent {
            check: always_open,
            command: "playbook setup",
        };
        let entry = |id, since, kind, scope, run| Migration {
            id,
            since,
            kind,
            scope,
            run,
        };
        let list = vec![
            entry("auto", "0.1.0", Kind::Auto, Scope::Owned, runs_quietly),
            entry("consent", "0.1.0", Kind::Idempotent, consent, repeats),
            entry("auto-next", NEXT, Kind::Auto, Scope::Owned, never),
            entry("idem-next", NEXT, Kind::Idempotent, Scope::Owned, never),
            entry("warn-next", NEXT, Kind::Manual, Scope::Report, never),
            entry("consent-next", NEXT, Kind::Idempotent, consent, never),
        ];
        let h = home("next-gate");

        // Act
        let ships = shipped(list, "0.37.0");
        let report = run_with(&ships, &ctx(&h));
        let open = open_items_in(&ships, &ctx(&h), None);

        // Assert
        let ids: Vec<&str> = ships.iter().map(|m| m.id).collect();
        assert_eq!(ids, ["auto", "consent"]);
        assert!(!report.failed, "{:?}", report.warnings);
        assert_eq!(open.iter().map(|i| i.id).collect::<Vec<_>>(), ["consent"]);
    }

    #[test]
    fn an_unreleased_migration_neither_runs_nor_shows_as_open() {
        // Arrange: a home where the collector label migration would act if it ran.
        let h = home("unreleased");
        let unreleased: Vec<&str> = registry()
            .iter()
            .filter(|m| m.since() == Since::Next)
            .map(|m| m.id)
            .collect();

        // Act
        let open = open_items(&ctx(&h), None);
        let plan = plan(&ctx(&h)).expect("a plan");

        // Assert
        assert!(open.iter().all(|i| !unreleased.contains(&i.id)));
        for p in plan.iter().filter(|p| unreleased.contains(&p.id)) {
            assert_eq!(p.state, State::Unreleased, "{}", p.id);
            assert!(!p.runs, "{}", p.id);
        }
    }

    #[test]
    fn every_migration_names_a_release_in_order_and_a_unique_id() {
        // Arrange
        let all = registry();
        let running = env!("CARGO_PKG_VERSION");

        // Act
        let mut ids: Vec<&str> = all.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        ids.dedup();

        // Assert
        assert_eq!(ids.len(), all.len(), "an id is used twice");
        let mut last: Option<Since> = None;
        for m in &all {
            let since = m.since();
            if let Since::At(v) = since {
                assert!(plain_version(v), "{}: since {v} is not X.Y.Z", m.id);
                assert!(
                    m.released_in(running),
                    "{}: since {v} is above this build {running}; use next",
                    m.id
                );
            }
            match (last, since) {
                (Some(Since::Next), Since::At(_)) => {
                    panic!("{}: a released migration after a next one", m.id)
                }
                (Some(Since::At(a)), Since::At(b)) => assert!(
                    crate::update::resolve::compare(a, b) != std::cmp::Ordering::Greater,
                    "{}: since {b} is below the one before it, {a}",
                    m.id
                ),
                _ => {}
            }
            last = Some(since);
        }
    }

    #[test]
    fn each_next_is_written_in_the_form_the_release_fills_in() {
        // Arrange: scripts/release-prepare.sh replaces exactly this line.
        let source = include_str!("migrate.rs");
        let form = format!("            since: \"{NEXT}\",");
        let loose = format!("since: \"{NEXT}\"");

        // Act
        let written = source.lines().filter(|l| l.contains(&loose)).count();
        let exact = source.lines().filter(|l| *l == form).count();
        let entries = registry()
            .iter()
            .filter(|m| m.since() == Since::Next)
            .count();

        // Assert
        assert_eq!(written, exact, "a next in another form stops the release");
        assert_eq!(exact, entries);
    }

    /// A scratch home with the agent playbook 0.37.0 wrote.
    fn old_agent_home(tag: &str) -> PathBuf {
        let h = home(tag);
        let file = crate::usage::service::old_file(&h);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let log = crate::usage::service::log_path(&h);
        fs::write(
            &file,
            crate::usage::service::render_old(Path::new("/opt/homebrew/bin/playbook"), &log),
        )
        .unwrap();
        h
    }

    /// [`old_agent_home`] with the agent under the current label and the
    /// systemd unit too, every file a collector service ever left.
    fn service_home(tag: &str) -> PathBuf {
        use crate::usage::service::{log_path, render, Kind};
        let h = old_agent_home(tag);
        for kind in [Kind::Launchd, Kind::Systemd] {
            let file = kind.file(&h);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(
                &file,
                render(kind, Path::new("/opt/homebrew/bin/playbook"), &log_path(&h)),
            )
            .unwrap();
        }
        h
    }

    /// Every service file [`service_home`] writes.
    fn service_files(h: &Path) -> [PathBuf; 3] {
        use crate::usage::service::{old_file, Kind};
        [Kind::Launchd.file(h), old_file(h), Kind::Systemd.file(h)]
    }

    #[test]
    fn removal_stops_and_deletes_both_agents_and_the_unit_and_names_them() {
        // Arrange
        use crate::service_manager::fake::Launchd;
        use crate::usage::service::{LABEL, OLD_LABEL, UNIT};
        let h = service_home("remove-all");
        let fake = Launchd::with(&[LABEL, OLD_LABEL]);

        // Act
        let out = remove_collector_service(&ctx(&h), &fake, &|| {});

        // Assert
        let Outcome::Repeat(step) = out else {
            panic!("a removal is reported and runs again next time");
        };
        assert_eq!(step.status, StepStatus::Wired);
        for name in [LABEL, OLD_LABEL, UNIT] {
            assert!(step.detail.contains(name), "{name}: {}", step.detail);
        }
        assert!(service_files(&h).iter().all(|f| !f.exists()));
        assert!(fake.loaded.borrow().is_empty(), "{:?}", fake.calls.borrow());
    }

    #[test]
    fn removal_leaves_an_old_label_file_playbook_did_not_write_and_names_the_commands() {
        // Arrange
        use crate::service_manager::fake::Launchd;
        let h = home("remove-foreign");
        let file = crate::usage::service::old_file(&h);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "<plist/>").unwrap();
        let starts = std::cell::Cell::new(0);

        // Act
        let out = remove_collector_service(&ctx(&h), &Launchd::default(), &|| {
            starts.set(starts.get() + 1)
        });

        // Assert
        let Outcome::Warn(msg) = out else {
            panic!("a file playbook did not write is only reported");
        };
        assert!(
            msg.contains("launchctl bootout gui/") && msg.contains("rm "),
            "{msg}"
        );
        assert!(file.is_file());
        assert_eq!(starts.get(), 0, "nothing was stopped, so nothing starts");
    }

    #[test]
    fn removal_in_a_home_that_is_not_the_account_home_touches_nothing() {
        // Arrange
        let h = service_home("remove-refused");
        let root = playbook_root_from(&h);
        crate::state::set(&root, crate::init::collector_service::DECLINED_KEY, "1").unwrap();

        // Act
        let out = collector_service_removed(&ctx(&h));

        // Assert
        let Outcome::Warn(msg) = out else {
            panic!("a refused removal is reported");
        };
        assert!(
            msg.contains("did not remove it") && msg.contains("HOME is not the home"),
            "{msg}"
        );
        assert!(service_files(&h).iter().all(|f| f.is_file()));
        assert!(crate::init::collector_service::declined(&h));
    }

    #[test]
    fn removal_deletes_the_answers_the_setup_part_stored() {
        // Arrange
        use crate::init::collector_service::{DECLINED_KEY, REMINDED_KEY};
        use crate::service_manager::fake::Launchd;
        let h = service_home("remove-keys");
        let root = playbook_root_from(&h);
        for key in [DECLINED_KEY, REMINDED_KEY] {
            crate::state::set(&root, key, "1").unwrap();
        }

        // Act
        remove_collector_service(&ctx(&h), &Launchd::default(), &|| {});

        // Assert
        for key in [DECLINED_KEY, REMINDED_KEY] {
            let rows = crate::state::list_read_only(&root, key).unwrap();
            assert!(rows.is_empty(), "{key}: {rows:?}");
        }
    }

    #[test]
    fn removal_starts_a_collector_once_and_a_second_run_finds_nothing() {
        // Arrange: the heal can cut a run short, so the next one reruns it.
        use crate::service_manager::fake::Launchd;
        use crate::usage::service::{LABEL, OLD_LABEL};
        let h = service_home("remove-twice");
        let fake = Launchd::with(&[LABEL, OLD_LABEL]);
        let starts = std::cell::Cell::new(0);
        let start = || starts.set(starts.get() + 1);

        // Act
        let first = remove_collector_service(&ctx(&h), &fake, &start);
        let calls_after_first = fake.calls.borrow().len();
        let second = remove_collector_service(&ctx(&h), &fake, &start);

        // Assert
        assert!(matches!(first, Outcome::Repeat(_)));
        assert!(matches!(second, Outcome::Quiet));
        assert_eq!(starts.get(), 1);
        assert_eq!(
            fake.calls.borrow().len(),
            calls_after_first,
            "the second run runs nothing"
        );
        assert!(service_files(&h).iter().all(|f| !f.exists()));
    }

    /// The number 0.37.0 gave the retired service reminder.
    const RETIRED_COLLECTOR_SERVICE_LEGACY_ID: &str = "0016-collector-service";

    #[test]
    fn removal_that_fails_part_way_still_starts_a_collector() {
        // Arrange: a unit file in a folder that cannot be changed, so its
        // delete fails after the launchd agent was booted out.
        use crate::service_manager::fake::Launchd;
        use crate::usage::service::{Kind, LABEL};
        use std::os::unix::fs::PermissionsExt;
        // Root ignores folder permissions, so the delete cannot be made to fail.
        if std::process::Command::new("id")
            .arg("-u")
            .output()
            .is_ok_and(|o| o.stdout == b"0\n")
        {
            return;
        }
        let h = service_home("remove-fails");
        let unit = Kind::Systemd.file(&h);
        let folder = unit.parent().unwrap().to_path_buf();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
        let fake = Launchd::with(&[LABEL]);
        let starts = std::cell::Cell::new(0);

        // Act
        let out = remove_collector_service(&ctx(&h), &fake, &|| starts.set(starts.get() + 1));

        // Assert
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        let Outcome::Failed(step) = out else {
            panic!("a failed delete is reported as a failure");
        };
        assert!(step.detail.contains("playbook migrate"), "{}", step.detail);
        assert!(!fake.has(LABEL), "{:?}", fake.calls.borrow());
        assert_eq!(starts.get(), 1, "the stopped collector is started again");
    }

    #[test]
    fn removal_keeps_a_foreign_unit_next_to_playbooks_own_agent_and_names_the_commands() {
        // Arrange: playbook's agent, and a unit that is not text.
        use crate::service_manager::fake::Launchd;
        use crate::usage::service::{log_path, render, Kind, LABEL};
        let h = home("remove-foreign-unit");
        let agent = Kind::Launchd.file(&h);
        fs::create_dir_all(agent.parent().unwrap()).unwrap();
        fs::write(
            &agent,
            render(Kind::Launchd, Path::new("/p"), &log_path(&h)),
        )
        .unwrap();
        let unit = Kind::Systemd.file(&h);
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        fs::write(&unit, [0xff_u8, 0xfe, 0x00]).unwrap();
        let fake = Launchd::with(&[LABEL]);

        // Act
        let out = remove_collector_service(&ctx(&h), &fake, &|| {});

        // Assert
        let Outcome::Repeat(step) = out else {
            panic!("a removal is reported and runs again next time");
        };
        assert!(
            step.detail.contains("was not written by playbook"),
            "{}",
            step.detail
        );
        assert!(step.detail.contains("disable --now"), "{}", step.detail);
        assert!(unit.is_file() && !agent.exists());
    }

    #[test]
    fn removal_with_a_foreign_old_label_file_removes_the_rest_and_keeps_it() {
        // Arrange: every service file, with an old-label agent playbook did
        // not write.
        use crate::service_manager::fake::Launchd;
        use crate::usage::service::{old_file, Kind, LABEL};
        let h = service_home("remove-mixed");
        let foreign = old_file(&h);
        fs::write(&foreign, "<plist/>").unwrap();
        let fake = Launchd::with(&[LABEL]);
        let starts = std::cell::Cell::new(0);

        // Act
        let out = remove_collector_service(&ctx(&h), &fake, &|| starts.set(starts.get() + 1));

        // Assert
        let Outcome::Repeat(step) = out else {
            panic!("a removal is reported and runs again next time");
        };
        assert!(
            step.detail.contains("launchctl bootout gui/") && step.detail.contains("rm "),
            "{}",
            step.detail
        );
        assert_eq!(starts.get(), 1);
        assert!(foreign.is_file());
        assert!(!Kind::Launchd.file(&h).exists() && !Kind::Systemd.file(&h).exists());
    }

    #[test]
    fn a_record_with_rows_under_both_retired_collector_slugs_still_loads() {
        // Arrange: a 0.38.0 record with both retired items applied, the
        // service reminder under its 0.37.0 number too.
        let h = home("retired-collector");
        let retired = [
            RETIRED_COLLECTOR_SERVICE_MIGRATION,
            RETIRED_COLLECTOR_LABEL_MIGRATION,
        ];
        let mut rows: Vec<String> = retired.iter().map(|id| format!("applied {id}")).collect();
        rows.push(format!("applied {RETIRED_COLLECTOR_SERVICE_LEGACY_ID}"));
        write_state(&h, &rows).unwrap();

        // Act
        let report = run_owned(&ctx(&h), 0, None);
        let plan = plan(&ctx(&h)).expect("the record reads");
        let open = open_items(&ctx(&h), None);

        // Assert
        assert!(!report.failed, "{:?}", report.warnings);
        assert!(registry().iter().all(|m| !RETIRED_IDS.contains(&m.id)));
        for id in retired {
            assert!(plan.iter().all(|p| p.id != id), "{id} is planned");
            assert!(open.iter().all(|i| i.id != id), "{id} is open");
        }
    }

    fn sqlite_file(path: &Path, user_version: i64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = playbook_store::Connection::open(path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE t(x); PRAGMA user_version = {user_version};"
        ))
        .unwrap();
    }

    fn plain_file(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    fn state_index(h: &Path, worktree: &str) -> PathBuf {
        playbook_root_from(h)
            .join("repos/o/r")
            .join(worktree)
            .join("code/index.sqlite3")
    }

    #[test]
    fn the_per_worktree_code_files_go_and_the_store_and_gate_files_stay() {
        // Arrange
        let h = home("code-index-files");
        let data = playbook_root_from(&h).join("data");
        let code = data.join("code/o/r");
        for name in ["wt.db", "wt.db-wal", "wt.db-shm", "index.lock"] {
            plain_file(&code.join(name));
        }
        for name in ["store-v4.sqlite3", "store-v4.lock", "wt.lock"] {
            plain_file(&code.join(name));
        }
        plain_file(&data.join("gate/o/r/wt.db"));
        let old = state_index(&h, "wt");
        sqlite_file(&old, playbook_code::db::SCHEMA_VERSION - 2);
        plain_file(&PathBuf::from(format!("{}-wal", old.display())));
        plain_file(&PathBuf::from(format!("{}-shm", old.display())));

        // Act
        let outcome = code_index_per_worktree_files(&ctx(&h));

        // Assert
        let Outcome::Done(step) = outcome else {
            panic!("the migration finishes");
        };
        assert_eq!(step.status, crate::init::run::StepStatus::Wired);
        for gone in ["wt.db", "wt.db-wal", "wt.db-shm", "index.lock"] {
            assert!(!code.join(gone).exists(), "{gone}");
        }
        assert!(!old.exists());
        assert!(!PathBuf::from(format!("{}-wal", old.display())).exists());
        assert!(!PathBuf::from(format!("{}-shm", old.display())).exists());
        for kept in ["store-v4.sqlite3", "store-v4.lock", "wt.lock"] {
            assert!(code.join(kept).exists(), "{kept}");
        }
        assert!(data.join("gate/o/r/wt.db").exists());
    }

    #[test]
    fn an_older_schema_at_the_state_path_goes_and_a_current_or_newer_one_stays() {
        // Arrange
        let h = home("code-index-state-versions");
        let current = playbook_code::db::SCHEMA_VERSION;
        sqlite_file(&state_index(&h, "older-1"), current - 1);
        sqlite_file(&state_index(&h, "older-2"), current - 2);
        sqlite_file(&state_index(&h, "current"), current);
        sqlite_file(&state_index(&h, "newer"), current + 1);
        let junk = state_index(&h, "not-sqlite");
        fs::create_dir_all(junk.parent().unwrap()).unwrap();
        fs::write(&junk, vec![b'x'; 4096]).unwrap();

        // Act
        let outcome = code_index_per_worktree_files(&ctx(&h));

        // Assert
        let Outcome::Done(step) = outcome else {
            panic!("the migration finishes");
        };
        assert_eq!(step.status, crate::init::run::StepStatus::Wired);
        assert!(!state_index(&h, "older-1").exists());
        assert!(!state_index(&h, "older-2").exists());
        for kept in ["current", "newer", "not-sqlite"] {
            assert!(state_index(&h, kept).exists(), "{kept}");
        }
    }

    #[test]
    fn the_code_index_migration_runs_once_and_a_home_without_code_files_reports_nothing() {
        // Arrange
        let h = home("code-index-once");
        let ctx = ctx(&h);
        plain_file(&playbook_root_from(&h).join("data/code/o/r/wt.db"));
        let ms = [Migration {
            id: CODE_INDEX_FILES_MIGRATION,
            since: "0.1.0",
            kind: Kind::Auto,
            scope: Scope::Owned,
            run: code_index_per_worktree_files,
        }];
        let (mut first, mut second) = (Report::default(), Report::default());

        // Act
        run_auto(&ms, &ctx, &mut first, None);
        run_auto(&ms, &ctx, &mut second, None);
        let empty = code_index_per_worktree_files(&super::Ctx::for_home(&home("code-index-none")));

        // Assert
        assert_eq!(first.steps.len(), 1);
        assert!(second.steps.is_empty(), "recorded, so not run again");
        let Outcome::Done(step) = empty else {
            panic!("the migration finishes");
        };
        assert_eq!(step.status, crate::init::run::StepStatus::Skipped);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_folder_or_file_is_never_followed_or_deleted() {
        // Arrange: an owner folder and a repo file that link outside the data folder.
        use std::os::unix::fs::symlink;
        let h = home("code-index-symlinks");
        let data = playbook_root_from(&h).join("data");
        let outside = home("code-index-symlinks-outside");
        plain_file(&outside.join("r/app.db"));
        plain_file(&outside.join("lone.db"));
        fs::create_dir_all(data.join("code/linked-repo-parent/r")).unwrap();
        fs::create_dir_all(data.join("code")).unwrap();
        symlink(&outside, data.join("code/o")).unwrap();
        symlink(
            outside.join("lone.db"),
            data.join("code/linked-repo-parent/r/wt.db"),
        )
        .unwrap();
        let state = playbook_root_from(&h).join("repos/o/r");
        fs::create_dir_all(&state).unwrap();
        symlink(&outside, state.join("wt")).unwrap();

        // Act
        let outcome = code_index_per_worktree_files(&ctx(&h));

        // Assert
        assert!(matches!(outcome, Outcome::Done(_)));
        assert!(outside.join("r/app.db").exists());
        assert!(outside.join("lone.db").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_delete_that_fails_reports_it_and_still_removes_the_other_files() {
        // Arrange: one old file in a folder that cannot be changed.
        use std::os::unix::fs::PermissionsExt;
        // Root ignores folder permissions, so the delete cannot be made to fail.
        if std::process::Command::new("id")
            .arg("-u")
            .output()
            .is_ok_and(|o| o.stdout == b"0\n")
        {
            return;
        }
        let h = home("code-index-delete-fails");
        let data = playbook_root_from(&h).join("data");
        let locked = data.join("code/o/locked");
        let open = data.join("code/o/open");
        plain_file(&locked.join("wt.db"));
        plain_file(&open.join("wt.db"));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();

        // Act
        let outcome = code_index_per_worktree_files(&ctx(&h));

        // Assert
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let Outcome::Failed(step) = outcome else {
            panic!("a failed delete is reported as a failure");
        };
        assert!(step.detail.contains("wt.db"), "{}", step.detail);
        assert!(locked.join("wt.db").exists());
        assert!(!open.join("wt.db").exists());
    }

    #[test]
    fn a_state_index_with_no_version_yet_stays() {
        // Arrange: a store another process is still creating reads version 0.
        let h = home("code-index-state-v0");
        sqlite_file(&state_index(&h, "fresh"), 0);

        // Act
        let outcome = code_index_per_worktree_files(&ctx(&h));

        // Assert
        assert!(matches!(outcome, Outcome::Done(_)));
        assert!(state_index(&h, "fresh").exists());
    }

    /// A stored value that differs from the fixed default, of the same type.
    fn differing(default: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        match default {
            Value::Bool(b) => Value::Bool(!b),
            Value::String(s) => Value::String(format!("{s}-changed")),
            Value::Number(n) if n.is_f64() => serde_json::json!(n.as_f64().unwrap() + 1.0),
            other => serde_json::json!(other.as_i64().unwrap() + 1),
        }
    }

    #[test]
    fn every_fixed_key_is_ignored_refused_skipped_and_reported_once() {
        use crate::config::{keys, store, write, ConfigError};
        for (key, default) in keys::FIXED_KEYS.iter() {
            // Arrange
            let h = home(&format!("fixed-{key}"));
            let root = playbook_root_from(&h);
            let stored = differing(default);
            store::put(&root, write::Tier::Global, None, key, &stored).unwrap();
            store::put(&root, write::Tier::Repo, Some("o/r"), key, &stored).unwrap();

            // Act
            let resolved = crate::config::resolve(key, &h, Some("o/r"));
            let set = write::set(write::Tier::Global, key, &stored, &h, None);
            let imported = store::import(&root, &serde_json::json!({"global": {*key: stored}}));
            let out = fixed_config_keys(&ctx(&h));

            // Assert
            assert!(matches!(resolved, Err(ConfigError::UnknownKey(_))), "{key}");
            assert!(set.is_err(), "{key}");
            assert_eq!(imported.unwrap(), 0, "{key}");
            let Outcome::Warn(msg) = out else {
                panic!("{key}: a stored fixed key is reported");
            };
            assert_eq!(msg.matches(key).count(), 1, "{key}: {msg}");
            assert!(msg.contains(&stored.to_string()), "{key}: {msg}");
        }
    }

    #[test]
    fn a_fixed_key_stored_at_its_default_or_not_at_all_is_not_reported() {
        use crate::config::{keys, store, write};
        // Arrange
        let h = home("fixed-quiet");
        let root = playbook_root_from(&h);
        let (key, default) = &keys::FIXED_KEYS[0];
        let quiet_before = fixed_config_keys(&ctx(&h));
        store::put(&root, write::Tier::Global, None, key, default).unwrap();

        // Act
        let quiet_at_default = fixed_config_keys(&ctx(&h));

        // Assert
        assert!(matches!(quiet_before, Outcome::Quiet));
        assert!(matches!(quiet_at_default, Outcome::Quiet));
    }

    #[test]
    fn fixed_config_keys_is_a_manual_migration_that_waits_for_the_next_release() {
        // Arrange
        let entry = registry()
            .into_iter()
            .find(|m| m.id == FIXED_CONFIG_KEYS_MIGRATION)
            .expect("registered");

        // Act
        let since = entry.since();

        // Assert
        assert!(matches!(entry.kind, Kind::Manual));
        assert_eq!(since, Since::Next);
    }

    /// The two stored names of a rename, as booleans (the test table's
    /// rows are booleans).
    fn flag(key: &str) -> bool {
        crate::config::keys::default_value(key)
            .and_then(|v| v.as_bool())
            .expect("a boolean key")
    }

    #[test]
    fn rename_config_keys_moves_rows_in_every_tier_and_is_a_no_op_the_second_time() {
        use crate::config::{keys, store, write};
        for row in keys::renames() {
            // Arrange
            let h = home("rename-keys-moves");
            let root = playbook_root_from(&h);
            let stored = serde_json::json!(!flag(row.new));
            store::put(&root, write::Tier::Global, None, row.old, &stored).unwrap();
            store::put(&root, write::Tier::Org, Some("o/r"), row.old, &stored).unwrap();
            store::put(&root, write::Tier::Repo, Some("o/r"), row.old, &stored).unwrap();

            // Act
            let first = rename_config_keys(&ctx(&h));
            let second = rename_config_keys(&ctx(&h));

            // Assert
            let found = store::lookup(&root, row.new, Some("o/r")).unwrap();
            assert_eq!(found.global, Some(stored.clone()), "{}", row.new);
            assert_eq!(found.org, Some(stored.clone()), "{}", row.new);
            assert_eq!(found.repo, Some(stored), "{}", row.new);
            assert!(store::stored_values(&root, row.old).unwrap().is_empty());
            let Outcome::Done(moved) = first else {
                panic!("{}: the first run is recorded", row.old);
            };
            assert!(moved.detail.contains("3"), "{}", moved.detail);
            let Outcome::Done(nothing) = second else {
                panic!("{}: the second run is recorded", row.old);
            };
            assert!(nothing.detail.contains("no stored"), "{}", nothing.detail);
        }
    }

    #[test]
    fn rename_config_keys_keeps_the_new_row_when_both_exist() {
        use crate::config::{keys, store, write};
        for row in keys::renames() {
            // Arrange
            let h = home("rename-keys-both");
            let root = playbook_root_from(&h);
            let (old, new) = (
                serde_json::json!(flag(row.new)),
                serde_json::json!(!flag(row.new)),
            );
            store::put(&root, write::Tier::Repo, Some("o/r"), row.old, &old).unwrap();
            store::put(&root, write::Tier::Repo, Some("o/r"), row.new, &new).unwrap();

            // Act
            let outcome = rename_config_keys(&ctx(&h));

            // Assert
            assert!(matches!(outcome, Outcome::Done(_)));
            let found = store::lookup(&root, row.new, Some("o/r")).unwrap();
            assert_eq!(found.repo, Some(new), "{}", row.new);
            assert!(store::stored_values(&root, row.old).unwrap().is_empty());
        }
    }

    #[test]
    fn rename_config_keys_is_an_unattended_migration_that_waits_for_the_next_release() {
        // Arrange
        let entry = registry()
            .into_iter()
            .find(|m| m.id == RENAME_CONFIG_KEYS_MIGRATION)
            .expect("registered");

        // Act
        let since = entry.since();

        // Assert
        assert!(matches!(entry.kind, Kind::Auto));
        assert!(matches!(entry.scope, Scope::Owned));
        assert_eq!(since, Since::Next);
    }

    #[test]
    fn the_real_budget_and_effort_renames_move_global_and_repo_rows_once() {
        use crate::config::keys::KEY_RENAMES;
        use crate::config::{store, write};
        let rows = [
            ("auto.budgetUsd", "auto.budget.mainUsd", 12),
            ("auto.agentBudgetUsd", "auto.budget.agentUsd", 6),
            ("auto.sessionCeilingUsd", "auto.budget.ceilingUsd", 900),
        ];
        for (old, new, usd) in rows {
            // Arrange
            let h = home("rename-real-budget");
            let root = playbook_root_from(&h);
            let global = serde_json::json!(usd);
            let repo = serde_json::json!(usd + 1);
            store::put(&root, write::Tier::Global, None, old, &global).unwrap();
            store::put(&root, write::Tier::Repo, Some("o/r"), old, &repo).unwrap();

            // Act
            let first = move_renamed_rows(&ctx(&h), KEY_RENAMES);
            let second = move_renamed_rows(&ctx(&h), KEY_RENAMES);

            // Assert
            let found = store::lookup(&root, new, Some("o/r")).unwrap();
            assert_eq!(found.global, Some(global), "{new}");
            assert_eq!(found.repo, Some(repo), "{new}");
            assert!(store::stored_values(&root, old).unwrap().is_empty());
            assert!(matches!(first, Outcome::Done(ref r) if r.detail.contains('2')));
            assert!(matches!(second, Outcome::Done(ref r) if r.detail.contains("no stored")));
        }
    }

    #[test]
    fn a_stored_old_default_budget_is_raised_first_and_then_lands_under_the_new_name() {
        use crate::config::keys::KEY_RENAMES;
        use crate::config::store;
        // Arrange: the budget move runs before the rename, so it sees the old name
        let h = home("rename-after-budget");
        set_budget(&h, 5.0);

        // Act
        let raised = auto_budget_defaults(&ctx(&h));
        let renamed = move_renamed_rows(&ctx(&h), KEY_RENAMES);

        // Assert
        assert!(matches!(raised, Outcome::Done(_)));
        assert!(matches!(renamed, Outcome::Done(_)));
        let found = store::lookup(&playbook_root_from(&h), "auto.budget.mainUsd", None).unwrap();
        assert_eq!(
            found.global,
            Some(serde_json::json!(crate::config::keys::AUTO_BUDGET_USD))
        );
    }

    #[test]
    fn the_real_worktree_watch_and_upgrade_renames_move_every_tier_once() {
        use crate::config::keys::KEY_RENAMES;
        use crate::config::{store, write};
        let rows = [
            ("worktreeCleanup.enabled", "worktree.cleanup.enabled"),
            ("watch.maxCatchupHours", "watch.catchup.maxHours"),
            ("watch.limits.actionsPerDay", "watch.limits.dailyActions"),
            ("watch.limits.usdPerDay", "watch.limits.dailyUsd"),
            ("update.mode", "upgrade.mode"),
            ("update.plugin", "upgrade.plugin"),
        ];
        let tiers = [
            (write::Tier::Global, None, "global"),
            (write::Tier::Org, Some("acme/app"), "org"),
            (write::Tier::Repo, Some("acme/app"), "repo"),
        ];
        for (old, new) in rows {
            // Arrange
            let h = home("rename-real-worktree-watch");
            let root = playbook_root_from(&h);
            for (tier, scope, label) in tiers {
                store::put(&root, tier, scope, old, &serde_json::json!(label)).unwrap();
            }

            // Act
            let first = move_renamed_rows(&ctx(&h), KEY_RENAMES);
            let second = move_renamed_rows(&ctx(&h), KEY_RENAMES);

            // Assert
            let found = store::lookup(&root, new, Some("acme/app")).unwrap();
            assert_eq!(found.global, Some(serde_json::json!("global")), "{new}");
            assert_eq!(found.org, Some(serde_json::json!("org")), "{new}");
            assert_eq!(found.repo, Some(serde_json::json!("repo")), "{new}");
            assert!(store::stored_values(&root, old).unwrap().is_empty());
            assert!(matches!(first, Outcome::Done(ref r) if r.detail.contains('3')));
            assert!(matches!(second, Outcome::Done(ref r) if r.detail.contains("no stored")));
        }
    }

    #[test]
    fn a_legacy_plugin_boolean_becomes_a_word_and_then_lands_under_the_new_name() {
        use crate::config::keys::KEY_RENAMES;
        use crate::config::{store, write};
        // Arrange: the boolean move runs before the rename, so it sees the old name
        let h = home("plugin-bool-then-rename");
        let root = playbook_root_from(&h);
        store::put(
            &root,
            write::Tier::Global,
            None,
            "update.plugin",
            &serde_json::json!(false),
        )
        .unwrap();
        store::put(
            &root,
            write::Tier::Org,
            Some("acme/app"),
            "update.plugin",
            &serde_json::json!(false),
        )
        .unwrap();
        store::put(
            &root,
            write::Tier::Repo,
            Some("acme/app"),
            "update.plugin",
            &serde_json::json!(false),
        )
        .unwrap();

        // Act
        let moved = plugin_update_mode(&ctx(&h));
        let renamed = move_renamed_rows(&ctx(&h), KEY_RENAMES);

        // Assert
        assert!(matches!(moved, Outcome::Done(_)));
        assert!(matches!(renamed, Outcome::Done(_)));
        let found = store::lookup(&root, "upgrade.plugin", Some("acme/app")).unwrap();
        let off = Some(serde_json::json!("off"));
        assert_eq!(
            (found.global, found.org, found.repo),
            (off.clone(), off.clone(), off)
        );
    }
}
