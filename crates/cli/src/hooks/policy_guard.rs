// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Optional and off by default. It denies nothing until
//! `hooks.policyGuard.enabled` is true (or `POLICY_GUARD=1`); see
//! `docs/guides/` "The policy guard". When on, it denies these things the
//! system prompt used to ask for in prose:
//!
//! - skipping git hooks or signing (`--no-verify`, `-n` on commit,
//!   `--no-gpg-sign`, `-c commit.gpgsign=false`),
//! - a hand-run `gh pr create`, which skips the pre-flight checks the
//!   `/playbook:pr-create` skill runs (that skill calls
//!   `playbook pr create`, which this hook never sees),
//! - writing memory outside playbook's store: Claude Code's own auto memory
//!   (`~/.claude/projects/*/memory/`) or the retired `~/.claude/memory/`.
//!
//! - changing protected settings: `playbook config set` on a
//!   `hooks.proseLint.*`, `pr.rules`, `pr.rules.*`, `pr.ticketPatterns` key, `config import`,
//!   `config unset` or `config edit`,
//!   a write to the config database or a config file (a redirect into it, a
//!   writer program, an interpreter, `sed -i`, or `sqlite3` with anything but
//!   a read), or an inline `WORKTREE_BASE_DIR=` assignment. Reading the
//!   store is allowed, and each denial names the rule that matched. This
//!   holds in `ask` mode too, unlike the `auto_cost` guard, which covers
//!   `mode`, `auto.*` and `fix.*` in `auto` mode only. The inline assignment
//!   cannot reach the hook process, so denying it is defense in depth.
//!
//! - writing a Claude Code settings file (`settings.json` or
//!   `settings.local.json`, user or project). Settings change only through the
//!   `playbook` binary (`init`, `uninstall`, `config set`, `doctor`), which asks
//!   for consent, merges and marks its entries. An Edit, Write or MultiEdit on
//!   the file is denied, and so is a Bash command that writes it: a redirect,
//!   `tee`, `sed -i`, `perl -pi`, `cp`, `mv`, `install`, `rsync`, `ln`, `dd`,
//!   `rm`, `truncate` or an interpreter one-liner. The check is lexical, so
//!   heredoc and script-file interpreters, `git checkout` of the file and a
//!   path built at run time get through. Reading the file is fine. Bash is also denied when it runs
//!   `playbook uninstall --yes`. An agent that could drop the guard entries
//!   could run without them, so this holds in `ask` mode too.
//!
//! It also shows the memory fact format (`MEMORY_SCHEMA`) when a fact file is
//! written to playbook's store, so the system prompt does not carry it.
//!
//! One hook on two matchers: Bash looks at the command, Edit and Write look at
//! the target path. It fails safe: a payload it cannot read, a command the
//! reader cannot split, or anything unexpected allows the call, and it never
//! spawns a process. It denies nothing until `hooks.policyGuard.enabled` is
//! true or `POLICY_GUARD=1` is set; `POLICY_GUARD=0` turns it off. It only denies, it never
//! writes anything.
//!
//! A guardrail against an agent drifting, not a security boundary: it does not
//! expand variables, aliases or scripts, so an obfuscated spelling passes.

mod settings_writes;
mod store_writes;

pub(crate) use store_writes::writes_config_store;

use crate::common::payload::Payload;
use crate::common::shell::{
    program_index, program_name, runner_value_options, shell_script_at, simple_commands,
    skip_options, SHELLS,
};
use crate::common::{emit_pre_context, emit_pre_deny};
use std::path::{Component, Path, PathBuf};

/// Subcommands that run git hooks, so `--no-verify` matters.
const HOOKED_SUBCOMMANDS: [&str; 6] = ["commit", "push", "merge", "rebase", "cherry-pick", "am"];

/// Subcommands that sign, so `--no-gpg-sign` matters.
const SIGNING_SUBCOMMANDS: [&str; 7] = [
    "commit",
    "tag",
    "merge",
    "rebase",
    "cherry-pick",
    "revert",
    "am",
];

/// `git` options that take a separate value, skipped while finding the
/// subcommand.
const GIT_VALUE_OPTIONS: [&str; 6] = [
    "-C",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--exec-path",
    "--super-prefix",
];

/// `gh` options that take a separate value, skipped while finding `pr create`.
const GH_VALUE_OPTIONS: [&str; 2] = ["-R", "--repo"];

/// `git commit` short options that take the rest of the cluster or the next
/// word as a value, so a later `n` in the same cluster is part of that value.
const COMMIT_VALUE_SHORTS: &str = "mFCcts";

/// Programs that write to the path they are given.
const WRITER_PROGRAMS: [&str; 10] = [
    "tee", "cp", "mv", "rm", "touch", "mkdir", "ln", "install", "rsync", "dd",
];

/// The shortest prefix of a long option git still accepts, which the guard
/// treats as the option itself.
const MIN_ABBREVIATION: usize = 8;

const NO_VERIFY_REASON: &str = "Skipping git hooks (--no-verify, commit -n) is not allowed. Fix what the hook reports instead. If a hook is wrong, say so and ask the user. In an unattended run, report the hook's error text and stop.";

const NO_SIGN_REASON: &str = "Turning off commit or tag signing (--no-gpg-sign, commit.gpgsign=false) is not allowed. Sign every commit and tag. If signing fails, report the error text and stop.";

const PR_CREATE_REASON: &str = "Do not hand-run `gh pr create`. Use `playbook pr create` (it commits, pushes and opens or updates the PR from any worktree, `--dir <worktree>`), or the /playbook:pr-create skill, which writes the title and body and opens the PR. If the command itself fails, run `playbook pr create --help`, report the error text to the user, and stop; do not fall back to `gh pr create`.";

/// Shown when a fact file is written in playbook's store: the format the
/// system prompt no longer carries.
pub const MEMORY_SCHEMA: &str = "Memory fact format. One fact per file, kebab-case name, `.md`. \
Frontmatter: `name`, `description` (start with \"Use when ...\"), `type` (user, feedback, project or \
reference), optional `links:` and optional `anchors:`. Body for feedback and project facts: the rule, \
then **Why:** and **How to apply:**. Edges in `links:` use bare basenames with no path or extension: \
`supersedes` (this replaces an older fact; act on the newest in a chain, treat the rest as history), \
`depends_on` (read the prerequisite first), `relates_to` (symmetric, pull the neighbour), \
`contradicts` (symmetric; if both are live, surface the conflict and do not pick silently). Store \
each edge once, on the fact that authors it. A basename that resolves in neither the fact's scope nor \
the global scope is dangling: surface it, do not fail. `anchors:` lists the repo-relative code a fact \
describes: a directory (`src/auth/`), a file (`src/auth/login.py`) or a symbol \
(`src/auth/login.py#authenticate`). Scope comes from the folder: `<owner>/<repo>/` for one repo, \
`<owner>/` for one owner's repos, the root for everything. Inside a scoped folder do not repeat the \
owner or repo in the text.";

/// The shared tail of the reasons for a protected setting.
macro_rules! protected_reason {
    ($rule:literal) => {
        concat!(
            "Matched rule: ",
            $rule,
            ". The prose lint mode and the pull request rules are the user's to change, not the model's. Ask the user to run the command themselves in the prompt with `!`, for example `! playbook config set --global hooks.proseLint.mode warn`. Reading the config store is fine. Until then, work within the current setting and say so."
        )
    };
}

const CONFIG_SET_REASON: &str =
    protected_reason!("`playbook config set`, `unset`, `import` or `edit` of a protected key");

const CONFIG_STORE_REASON: &str =
    protected_reason!("a write to playbook's config database a config file or the memory store (write a fact with the Write tool)");

const WORKTREE_ENV_REASON: &str = protected_reason!("an inline `WORKTREE_BASE_DIR=` assignment");

/// The part both settings reasons share: what to ask the user and what the
/// model may run itself.
macro_rules! settings_reason_tail {
    () => {
        " Ask the user to run `playbook setup --only hooks` (to add the hooks) or `playbook uninstall` (to remove them) themselves in the prompt with `!`. Use `playbook config set <key> <value>` for a playbook setting, or `playbook doctor check` to check the setup. For any other settings change, ask the user to make it themselves in the prompt with `!`. Reading the file is fine."
    };
}

const SETTINGS_WRITE_REASON: &str = concat!(
    "Claude Code settings files (settings.json and settings.local.json, user or project) are changed through playbook, not by writing the file.",
    settings_reason_tail!()
);

const HOOK_ENTRIES_REASON: &str = "The hook entries in settings.json are the user's to change, not the model's. They are the guards that keep this session safe. Ask the user to run `playbook setup --only hooks` (to add them) or `playbook uninstall` (to remove them) themselves in the prompt with `!`, or to edit settings.json by hand. Until then, leave the hook entries as they are and say so; in an unattended run, report this and stop.";

const SETTINGS_EDIT_REASON: &str = concat!(
    "Claude Code settings files (settings.json and settings.local.json, user or project) are changed through playbook, not with the Edit or Write tool.",
    settings_reason_tail!()
);

const MEMORY_REASON: &str = "Claude Code's own memory is off limits here. Save the fact in playbook's store instead, with the Write tool: ~/.config/playbook/memory/ for a global fact, <owner>/<repo>/ under it for a project fact.";

/// The per-user key that switches the denials on. Off unless set to true.
pub const ENABLED_KEY: &str = "hooks.policyGuard.enabled";

/// Whether the denials are on: `POLICY_GUARD=1` or `0` decides, else the
/// stored `hooks.policyGuard.enabled`, which is off by default.
pub fn enabled() -> bool {
    enabled_for(&crate::common::home_dir())
}

/// [`enabled`] for a given home, so doctor reads what the hook reads.
pub fn enabled_for(home: &Path) -> bool {
    match env_override() {
        Some(on) => on,
        None => stored_enabled(home),
    }
}

/// `POLICY_GUARD=1` or `0` from the environment; any other value is ignored.
pub fn env_override() -> Option<bool> {
    match std::env::var("POLICY_GUARD").as_deref() {
        Ok("0") => Some(false),
        Ok("1") => Some(true),
        _ => None,
    }
}

/// `hooks.policyGuard.enabled` as stored. An unreadable value counts as off.
pub fn stored_enabled(home: &Path) -> bool {
    matches!(
        crate::config::resolve_valid(ENABLED_KEY, home, None),
        Ok((serde_json::Value::Bool(true), _, _))
    )
}

pub fn run(payload: &Payload) {
    if std::env::var("POLICY_GUARD").as_deref() == Ok("0") {
        return;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let command = payload.field(".tool_input.command");
    let path = payload.field(".tool_input.file_path");
    let reason = if enabled() {
        decide(&command, &path, &home)
    } else {
        None
    };
    if let Some(reason) = reason {
        emit_pre_deny(reason);
    } else if payload.field(".tool_name") == "Write"
        && is_fact_file(&path, &crate::common::paths::memory_dir())
    {
        emit_pre_context("PreToolUse", MEMORY_SCHEMA);
    }
}

/// The reason to deny a Bash `command`, or an Edit or Write to `path`.
fn decide(command: &str, path: &str, home: &str) -> Option<&'static str> {
    if !command.is_empty() {
        check_command(command, home)
    } else if settings_path(path, home).is_some() {
        Some(SETTINGS_EDIT_REASON)
    } else {
        check_path(path, home)
    }
}

/// Whether `path` is a fact file (`.md`) inside playbook's memory store.
fn is_fact_file(path: &str, memory_dir: &Path) -> bool {
    let path = Path::new(path);
    path.is_absolute()
        && path.extension().is_some_and(|ext| ext == "md")
        && path.starts_with(memory_dir)
        && !path.components().any(|c| c == Component::ParentDir)
}

/// The config keys the model may not change: the policy guard's own switch,
/// the prose lint mode and the pull request rules.
/// Matched on lowercase prefixes.
const PROTECTED_KEY_PREFIXES: [&str; 4] = [
    "hooks.policyguard",
    "hooks.proselint",
    "pr.rules.",
    "pr.ticketpatterns",
];

/// The `pr.rules` key itself, which the `pr.rules.` prefix does not cover.
const PROTECTED_PR_RULES_KEY: &str = "pr.rules";

fn is_protected_config_key(word: &str) -> bool {
    let key = word.to_lowercase();
    key == PROTECTED_PR_RULES_KEY || PROTECTED_KEY_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// The reason to deny this simple command because it changes a protected
/// setting or the config store, or `None`.
fn changes_protected_config(
    words: &[String],
    program_at: usize,
    home: &str,
) -> Option<&'static str> {
    changes_protected_config_at(words, program_at, home, 0)
}

fn changes_protected_config_at(
    words: &[String],
    program_at: usize,
    home: &str,
    depth: usize,
) -> Option<&'static str> {
    let assigns = words[..program_at].iter().any(|w| {
        w.split_once('=')
            .is_some_and(|(name, _)| name == "WORKTREE_BASE_DIR")
    });
    if assigns {
        return Some(WORKTREE_ENV_REASON);
    }
    let program = program_name(&words[program_at]);
    let rest = &words[program_at + 1..];
    // Reading the store (`cat`, `ls`, `jq`, `sqlite3` with a SELECT, a
    // `2>/dev/null`) and mentions in a message or a search are left alone.
    if store_writes::writes_config_store(words, home) {
        return Some(CONFIG_STORE_REASON);
    }
    if depth < 3 {
        if SHELLS.contains(&program.as_str()) {
            if let Some(at) = shell_script_at(rest) {
                if let Some(script) = rest.get(at) {
                    return simple_commands(script).iter().find_map(|inner| {
                        program_index(inner)
                            .and_then(|i| changes_protected_config_at(inner, i, home, depth + 1))
                    });
                }
            }
        }
        if let Some(takes_value) = runner_value_options(&program) {
            let inner = &rest[skip_options(rest, 0, takes_value)..];
            return program_index(inner)
                .and_then(|i| changes_protected_config_at(inner, i, home, depth + 1));
        }
    }
    let mut args = rest;
    match program.as_str() {
        "playbook" | "pb" => {}
        "cargo" => args = super::auto_cost::cargo_run_args(args)?,
        _ => return None,
    }
    if args
        .iter()
        .find(|w| !w.starts_with('-'))
        .map(String::as_str)
        != Some("config")
    {
        return None;
    }
    let at = args.iter().position(|w| w == "config").unwrap_or(0);
    let changes = match args[at + 1..].first().map(String::as_str) {
        // `unset` and `edit` do not exist yet. They are denied ahead of time so
        // adding them later cannot open a gap.
        Some("import" | "unset" | "edit") => true,
        // Any positional can be the key (`--org acme` takes a value in some
        // spellings), so all of them are checked.
        Some("set") => args[at + 2..]
            .iter()
            .filter(|w| !w.starts_with('-'))
            .any(|w| is_protected_config_key(w)),
        _ => false,
    };
    changes.then_some(CONFIG_SET_REASON)
}

/// Whether `word` names one of playbook's databases: the settings store (now
/// `config.db`, earlier `playbook.db`), the memory index or the data folder.
pub(crate) fn names_database(word: &str) -> bool {
    let last = word.rsplit('/').next().unwrap_or(word);
    let db_file =
        |name: &str| last == name || last == format!("{name}-wal") || last == format!("{name}-shm");
    word.contains("playbook.db")
        || db_file("config.db")
        || db_file("memory.db")
        || word
            .split('/')
            .collect::<Vec<_>>()
            .windows(2)
            .any(|w| w == ["playbook", "data"])
}

/// Whether `word` names playbook's config database or one of its config
/// files, or a glob inside the playbook folder.
fn names_config_store(word: &str, home: &str) -> bool {
    if names_database(&word.to_lowercase()) {
        return true;
    }
    if home.is_empty() {
        return false;
    }
    let root = crate::common::paths::playbook_root_from(Path::new(home));
    let expanded = expand_home(word, home);
    super::auto_cost::is_config_file(Path::new(&expanded), &root)
        || (expanded.contains(['*', '?'])
            && Path::new(&expanded).starts_with(&root)
            && matches!(
                Path::new(&expanded).components().count() - root.components().count(),
                1 | 2
            ))
}

/// Whether `word` is a path inside playbook's memory store, the facts the
/// model writes with the Write tool.
fn names_memory_store(word: &str, home: &str) -> bool {
    if home.is_empty() {
        return false;
    }
    let memory = crate::common::paths::playbook_root_from(Path::new(home)).join("memory");
    let lower = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
    lower(&lexical(Path::new(&expand_home(word, home)))).starts_with(lower(&memory))
}

/// Whether `path` is a Claude Code settings file: `settings*.json` directly
/// under a `.claude` directory. Case is ignored because the default macOS and
/// Windows filesystems ignore it.
fn is_claude_settings(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase());
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|d| d.to_string_lossy().eq_ignore_ascii_case(".claude"))
        && name.is_some_and(|n| is_settings_name(&n))
}

/// `path` as a Claude Code settings file, looking through a symlink, or
/// `None` when it is some other file.
fn settings_path(path: &str, home: &str) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let direct = lexical(Path::new(path));
    if is_claude_settings(&direct) {
        return Some(direct);
    }
    let real = match std::fs::canonicalize(&direct) {
        Ok(real) => real,
        // A file that does not exist yet: resolve the folder it goes in.
        Err(_) => std::fs::canonicalize(direct.parent()?)
            .ok()?
            .join(direct.file_name()?),
    };
    if is_claude_settings(&real) {
        return Some(real);
    }
    // A dotfiles setup links ~/.claude/settings.json to a file elsewhere, and
    // an edit of that file is an edit of the settings.
    ["settings.json", "settings.local.json"]
        .iter()
        .filter(|_| !home.is_empty())
        .any(|name| {
            std::fs::canonicalize(Path::new(home).join(".claude").join(name))
                .is_ok_and(|linked| linked == real)
        })
        .then_some(real)
}

/// `path` with `.` and `..` resolved by text, as `is_claude_memory_path` does,
/// so `.claude/agents/../settings.json` is seen for what it is.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            part => out.push(part),
        }
    }
    out
}

/// The simple commands inside a shell `-c` script or behind a runner program
/// (`nohup`, `xargs`), each as a word list starting at its program.
fn wrapped_commands(words: &[String], program_at: usize) -> Vec<Vec<String>> {
    let program = program_name(&words[program_at]);
    let rest = &words[program_at + 1..];
    if SHELLS.contains(&program.as_str()) {
        let script = shell_script_at(rest).and_then(|at| rest.get(at));
        return script.map(|s| simple_commands(s)).unwrap_or_default();
    }
    if let Some(takes_value) = runner_value_options(&program) {
        let inner = &rest[skip_options(rest, 0, takes_value)..];
        return if inner.is_empty() {
            Vec::new()
        } else {
            vec![inner.to_vec()]
        };
    }
    Vec::new()
}

/// A `playbook uninstall` that goes ahead (`--yes`, no `--dry-run`), alone or
/// behind `cargo run`, a runner program or a shell `-c`.
fn uninstalls_playbook(words: &[String], program_at: usize, depth: usize) -> bool {
    if depth < 3
        && wrapped_commands(words, program_at).iter().any(|inner| {
            program_index(inner).is_some_and(|at| uninstalls_playbook(inner, at, depth + 1))
        })
    {
        return true;
    }
    let mut args = &words[program_at + 1..];
    match program_name(&words[program_at]).as_str() {
        "playbook" | "pb" => {}
        "cargo" => match super::auto_cost::cargo_run_args(args) {
            Some(after) => args = after,
            None => return false,
        },
        _ => return false,
    }
    args.iter()
        .find(|w| !w.starts_with('-'))
        .map(String::as_str)
        == Some("uninstall")
        && args.iter().any(|w| w == "--yes")
        && !args.iter().any(|w| w == "--dry-run")
}

/// Whether a lower-case file name is `settings.json` or `settings.local.json`.
fn is_settings_name(name: &str) -> bool {
    name == "settings.json" || name == "settings.local.json"
}

/// Whether `word` is a cluster of short options (one dash) that includes `flag`.
fn is_short_cluster_with(word: &str, flag: char) -> bool {
    word.starts_with('-') && !word.starts_with("--") && word.contains(flag)
}

/// The reason to deny `command`, or `None` to allow it.
fn check_command(command: &str, home: &str) -> Option<&'static str> {
    if !mentions_a_guarded_program(command) {
        return None;
    }
    let mut in_claude_dir = false;
    let mut previous: Option<Vec<String>> = None;
    for words in simple_commands(command) {
        if settings_writes::is_open_redirect_target(
            previous.as_deref(),
            &words,
            home,
            in_claude_dir,
        ) {
            return Some(SETTINGS_WRITE_REASON);
        }
        if store_writes::is_open_redirect_into_store(previous.as_deref(), &words, home) {
            return Some(CONFIG_STORE_REASON);
        }
        previous = Some(words.clone());
        let Some(at) = program_index(&words) else {
            if settings_writes::redirects_to_settings(&words, home, in_claude_dir) {
                return Some(SETTINGS_WRITE_REASON);
            }
            if store_writes::writes_config_store(&words, home) {
                return Some(CONFIG_STORE_REASON);
            }
            continue;
        };
        let args = &words[at + 1..];
        if let Some(reason) = changes_protected_config(&words, at, home) {
            return Some(reason);
        }
        if uninstalls_playbook(&words, at, 0) {
            return Some(HOOK_ENTRIES_REASON);
        }
        let found = match program_name(&words[at]).as_str() {
            "git" => check_git(args),
            "gh" => check_gh(args),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
        if settings_writes::writes_claude_settings(&words, at, home, in_claude_dir) {
            return Some(SETTINGS_WRITE_REASON);
        }
        if writes_claude_memory(&words, at, home) {
            return Some(MEMORY_REASON);
        }
        in_claude_dir |= settings_writes::enters_claude_dir(&words, at, home);
    }
    None
}

/// A cheap check that rules out nearly every Bash call before any parsing.
fn mentions_a_guarded_program(command: &str) -> bool {
    let bare: String = command
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect::<String>()
        .to_lowercase();
    [
        "git",
        "gh",
        ".claude",
        "~",
        "playbook",
        "config",
        "uninstall",
        "cargo",
        "worktree_base_dir",
    ]
    .iter()
    .any(|needle| bare.contains(needle))
}

fn check_git(args: &[String]) -> Option<&'static str> {
    let mut i = 0;
    while let Some(word) = args.get(i) {
        if word == "-c" {
            let value = args.get(i + 1)?.to_lowercase();
            if let Some((key, setting)) = value.split_once('=') {
                let off = matches!(setting.trim(), "false" | "no" | "off" | "0" | "");
                if off && (key == "commit.gpgsign" || key == "tag.gpgsign") {
                    return Some(NO_SIGN_REASON);
                }
            }
            i += 2;
        } else if GIT_VALUE_OPTIONS.contains(&word.as_str()) {
            i += 2;
        } else if word.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    let subcommand = args.get(i)?.as_str();
    let rest: Vec<&str> = args[i + 1..]
        .iter()
        .map(String::as_str)
        .take_while(|w| *w != "--")
        .collect();

    if HOOKED_SUBCOMMANDS.contains(&subcommand)
        && rest.iter().any(|w| is_long_option(w, "--no-verify"))
    {
        return Some(NO_VERIFY_REASON);
    }
    if subcommand == "commit" && rest.iter().any(|w| is_commit_short_n(w)) {
        return Some(NO_VERIFY_REASON);
    }
    if SIGNING_SUBCOMMANDS.contains(&subcommand)
        && rest.iter().any(|w| is_long_option(w, "--no-gpg-sign"))
    {
        return Some(NO_SIGN_REASON);
    }
    None
}

/// Whether `word` is `name` or an abbreviation git accepts for it.
fn is_long_option(word: &str, name: &str) -> bool {
    word == name
        || (word.len() >= MIN_ABBREVIATION && word.starts_with("--") && name.starts_with(word))
}

/// A short cluster for `git commit` that holds `n` (`--no-verify`) before any
/// letter that takes a value, such as `-n` or `-anm`.
fn is_commit_short_n(word: &str) -> bool {
    let Some(cluster) = word.strip_prefix('-') else {
        return false;
    };
    if cluster.is_empty() || !cluster.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    for letter in cluster.chars() {
        if COMMIT_VALUE_SHORTS.contains(letter) {
            return false;
        }
        if letter == 'n' {
            return true;
        }
    }
    false
}

fn check_gh(args: &[String]) -> Option<&'static str> {
    let mut positional = Vec::new();
    let mut i = 0;
    while let Some(word) = args.get(i) {
        if GH_VALUE_OPTIONS.contains(&word.as_str()) {
            i += 2;
        } else if word.starts_with('-') {
            i += 1;
        } else {
            positional.push(word.as_str());
            i += 1;
            if positional.len() == 2 {
                break;
            }
        }
    }
    (positional == ["pr", "create"]).then_some(PR_CREATE_REASON)
}

/// Whether this simple command writes to a Claude Code memory path: it names
/// one and either redirects output or runs a program that writes.
fn writes_claude_memory(words: &[String], program_at: usize, home: &str) -> bool {
    if !words.iter().any(|w| is_claude_memory_path(w, home)) {
        return false;
    }
    let program = program_name(&words[program_at]);
    let in_place = matches!(program.as_str(), "sed" | "perl")
        && words[program_at + 1..]
            .iter()
            .any(|w| w.starts_with("-i") || w == "--in-place");
    WRITER_PROGRAMS.contains(&program.as_str()) || in_place || words.iter().any(|w| redirects(w))
}

/// A redirection that writes: `>`, `>>`, `2>`, `>file`, but not `<` or `>&`.
fn redirects(word: &str) -> bool {
    let operator = word.trim_start_matches(|c: char| c.is_ascii_digit());
    operator.starts_with('>') && !operator.starts_with(">&")
}

/// The reason to deny a write to `path`, or `None` to allow it.
fn check_path(path: &str, home: &str) -> Option<&'static str> {
    if !home.is_empty()
        && super::auto_cost::is_config_file(
            Path::new(path),
            &crate::common::paths::playbook_root_from(Path::new(home)),
        )
    {
        return Some(CONFIG_STORE_REASON);
    }
    is_claude_memory_path(path, home).then_some(MEMORY_REASON)
}

/// Whether `path` is inside Claude Code's auto memory
/// (`~/.claude/projects/<project>/memory`) or the retired `~/.claude/memory`.
/// `~`, `$HOME` and `${HOME}` count as `home`, and `.` and `..` segments are
/// resolved lexically so `x/../memory` cannot hide it.
fn is_claude_memory_path(path: &str, home: &str) -> bool {
    if home.is_empty() {
        return false;
    }
    let expanded = expand_home(path, home);
    if !expanded.starts_with('/') {
        return false;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in expanded.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let home_parts: Vec<&str> = home.split('/').filter(|p| !p.is_empty()).collect();
    let Some(rest) = parts.strip_prefix(home_parts.as_slice()) else {
        return false;
    };
    matches!(
        rest,
        [".claude", "memory", ..] | [".claude", "projects", _, "memory", ..]
    )
}

fn expand_home(path: &str, home: &str) -> String {
    for prefix in ["~", "$HOME", "${HOME}"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            if rest.is_empty() || rest.starts_with('/') {
                return format!("{}{rest}", PathBuf::from(home).display());
            }
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/Users/me";

    #[test]
    fn changes_to_protected_config_are_denied() {
        for command in [
            "playbook config set --org hooks.policyguard.enabled true",
            "playbook config set hooks.proseLint.mode off",
            "pb config set hooks.proseLint.mode off",
            "nohup pb config set pr.rules.template off",
            "playbook config set --global pr.rules.prose off",
            "playbook config set --org acme pr.ticketPatterns '[\"X-1\"]'",
            "playbook config unset pr.rules.length",
            "playbook config set pr.rules off",
            "playbook config set --global PR.Rules warn",
            "/usr/local/bin/playbook config import cfg.json",
            "playbook config edit",
            "cargo run -q -- config set pr.rules.prose off",
            "cd x && playbook config set pr.rules.prose off",
            "ksh -c 'playbook config set pr.rules.prose off'",
            "doas -u root playbook config set pr.rules.prose off",
            "doas sh -c 'playbook config set pr.rules.prose off'",
            "env playbook config set pr.rules.prose off",
            "sqlite3 ~/.config/playbook/playbook.db 'update x'",
            "WORKTREE_BASE_DIR=/tmp/x rm -rf /tmp/x/y",
            "env WORKTREE_BASE_DIR=/Users/me playbook worktree sweep",
            "nohup playbook config set --global pr.rules.prose off",
            "xargs playbook config set pr.rules.prose off",
            "bash -c 'playbook config set pr.rules.prose off'",
            "sh -c \"cd x && playbook config import f.json\"",
            "playbook config set --org acme pr.rules.prose off",
            "printf x > ~/.config/playbook/config.json",
            "echo '{}' | tee $HOME/.config/playbook/orgs/acme/config.json",
            "python3 -c 'x' ~/.config/playbook/playbook.db",
            "cp evil.db ~/.config/playbook/playbook.db",
            "sed -i s/a/b/ ~/.config/playbook/*.db",
            "sqlite3 ~/.config/playbook/data/config.db 'update x'",
            "cp evil.db ~/.config/playbook/data/memory.db",
            "cd ~/.config/playbook/data && sqlite3 config.db 'update x'",
            "sed -i s/a/b/ ~/.config/playbook/d*/c*.db",
        ] {
            let expected = if command.contains("WORKTREE_BASE_DIR=") {
                WORKTREE_ENV_REASON
            } else if command.contains(" config ") {
                CONFIG_SET_REASON
            } else {
                CONFIG_STORE_REASON
            };
            assert_eq!(deny(command), Some(expected), "{command}");
        }
    }

    #[test]
    fn ordinary_config_use_and_prose_are_left_alone() {
        for command in [
            "playbook config get hooks.policyGuard.enabled",
            "playbook config get hooks.proseLint.mode",
            "playbook config set --global pr.draft false",
            "playbook config set pr.rulesX true",
            "playbook config set disk.blockFreeGb 0",
            "playbook config get pr.rules.prose",
            "echo \"playbook config set hooks.proseLint.mode off\"",
            "playbook config list",
            "playbook config export",
            "playbook config set --global commit.signOff true",
            "echo \"playbook config set pr.rules.prose off\"",
            "git commit -m 'document hooks.policyGuard.enabled'",
            "cargo test --test hooks_guards",
            "cargo test -- config import",
            "git commit -m 'handle playbook.db lock'",
            "rg -n playbook.db src/",
            "cat ~/.config/playbook/config.json",
            "playbook config set --global commit.signOff true",
            "xargs echo playbook",
            "cd /work/.worktrees/playbook/data-config && cargo test",
            "git commit -m 'move the settings to config.db'",
            "cat src/config.db.ts",
            "cp a /work/.worktrees/playbook/data-config/b",
        ] {
            assert_eq!(deny(command), None, "{command}");
        }
    }

    /// Reading the config store, the memory store or a settings file is never
    /// a write, however the command is spelled.
    #[test]
    fn reads_of_the_config_store_are_left_alone() {
        for command in [
            "sqlite3 ~/.config/playbook/data/config.db \"select * from settings\"",
            "sqlite3 ~/.config/playbook/data/config.db .tables 2>/dev/null",
            "sqlite3 -readonly ~/.config/playbook/data/config.db 'select 1' 2>&1 | head",
            "sqlite3 ~/.config/playbook/data/memory.db \"select count(*) from facts\"",
            "sqlite3 -header ~/.config/playbook/data/config.db 'pragma table_info(settings)'",
            "cd ~/.config/playbook/data && sqlite3 config.db .tables",
            "cat ~/.config/playbook/config.json 2>/dev/null",
            "jq . ~/.config/playbook/config.json",
            "jq . ~/.config/playbook/config.json > /tmp/pb-config.json",
            "ls -la ~/.config/playbook/data > /private/tmp/out",
            "ls ~/.config/playbook/data/ 2>/dev/null | head",
            "grep x ~/.config/playbook/*/config.db 2>/dev/null",
            "sed -n 1,5p ~/.config/playbook/config.json",
            "grep -n hooks ~/.claude/settings.json",
            "playbook migrate --status 2>/dev/null",
            "rg \"playbook config set pr.rules\" docs/",
        ] {
            assert_eq!(deny(command), None, "{command}");
        }
    }

    #[test]
    fn a_write_to_the_config_store_names_its_rule() {
        for (command, reason) in [
            (
                "sqlite3 ~/.config/playbook/data/config.db \"update settings set v=1\"",
                CONFIG_STORE_REASON,
            ),
            (
                "sqlite3 ~/.config/playbook/data/config.db 'select 1; delete from settings'",
                CONFIG_STORE_REASON,
            ),
            (
                "sqlite3 ~/.config/playbook/data/config.db \"select writefile('x', 1)\"",
                CONFIG_STORE_REASON,
            ),
            (
                "sqlite3 ~/.config/playbook/data/config.db < x.sql",
                CONFIG_STORE_REASON,
            ),
            (
                "sqlite3 ~/.config/playbook/data/config.db .read x.sql",
                CONFIG_STORE_REASON,
            ),
            (
                "jq . a.json > ~/.config/playbook/config.json",
                CONFIG_STORE_REASON,
            ),
            (
                "echo x 2>~/.config/playbook/config.json",
                CONFIG_STORE_REASON,
            ),
            ("playbook config set pr.rules.prose off", CONFIG_SET_REASON),
            ("WORKTREE_BASE_DIR=/ ls", WORKTREE_ENV_REASON),
        ] {
            assert_eq!(deny(command), Some(reason), "{command}");
        }
        assert!(CONFIG_STORE_REASON.starts_with("Matched rule: a write"));
        for command in [
            "sqlite3 -cmd \"delete from settings\" ~/.config/playbook/data/config.db \"select 1\"",
            "sqlite3 --init x.sql ~/.config/playbook/data/config.db \"select 1\"",
            "sqlite3 ~/.config/playbook/data/config.db 'pragma writable_schema=1'",
            "Q='1; delete from settings'; sqlite3 ~/.config/playbook/data/config.db \"select $Q\"",
            "sed --in-place s/a/b/ ~/.config/playbook/config.json",
            "sed -Ei s/a/b/ ~/.config/playbook/config.json",
            "sed -n 'w /Users/me/.config/playbook/data/config.db' /tmp/evil.db",
            "echo x >| ~/.config/playbook/config.json",
            "echo x &> ~/.config/playbook/config.json",
            "echo x 1<> ~/.config/playbook/config.json",
        ] {
            assert_eq!(deny(command), Some(CONFIG_STORE_REASON), "{command}");
        }
    }

    #[test]
    fn the_edit_tools_may_not_touch_a_settings_file() {
        let user = format!("{HOME}/.claude/settings.json");
        for path in [
            user.as_str(),
            "/work/repo/.claude/settings.json",
            "/work/repo/.claude/settings.local.json",
            "/work/repo/.claude/agents/../settings.json",
            "/work/repo/.CLAUDE/Settings.json",
        ] {
            let reason = decide("", path, HOME);

            assert_eq!(reason, Some(SETTINGS_EDIT_REASON), "{path}");
        }
    }

    #[test]
    fn other_files_under_claude_stay_editable() {
        for path in [
            "/work/repo/.claude/agents/x.md",
            "/work/repo/.claude/commands/y.md",
            "/work/repo/CLAUDE.md",
            "/work/notes/settings.json",
        ] {
            assert_eq!(decide("", path, HOME), None, "{path}");
        }
    }

    #[test]
    fn a_bash_write_to_a_settings_file_gets_the_settings_reason() {
        for command in [
            "echo '{}' > ~/.claude/settings.json",
            "sed -i s/a/b/ \"$HOME/.claude/settings.json\"",
            "cd repo && tee .claude/settings.local.json < x.json",
            "bash -c 'cp /tmp/x.json ~/.claude/settings.json'",
        ] {
            assert_eq!(deny(command), Some(SETTINGS_WRITE_REASON), "{command}");
        }
    }

    #[test]
    fn writes_to_the_config_files_are_denied() {
        let root = format!("{HOME}/.config/playbook");
        for path in [
            format!("{root}/playbook.db"),
            format!("{root}/config.json"),
            format!("{root}/orgs/acme/config.json"),
            format!("{root}/x/../playbook.db"),
        ] {
            assert_eq!(check_path(&path, HOME), Some(CONFIG_STORE_REASON), "{path}");
        }
        assert_eq!(check_path(&format!("{root}/memory/a.md"), HOME), None);
    }

    /// A scratch `.claude` folder holding `body` as settings.json.
    fn settings_with(tag: &str) -> PathBuf {
        let dir = crate::common::test_support::scratch_dir(tag).join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{}").unwrap();
        path
    }

    #[test]
    fn a_path_with_dots_or_a_missing_file_still_reaches_the_settings_check() {
        let dir = crate::common::test_support::scratch_dir("settings-dots");
        std::fs::create_dir_all(dir.join(".claude/agents")).unwrap();
        let sneaky = format!("{}/.claude/agents/../settings.local.json", dir.display());

        assert!(settings_path(&sneaky, "").is_some());
        assert_eq!(lexical(Path::new("/a/b/../c/./d")), PathBuf::from("/a/c/d"));
    }

    #[test]
    fn settings_files_are_recognised_whatever_the_case_or_the_link() {
        assert!(is_claude_settings(Path::new(
            "/Users/me/.claude/settings.json"
        )));
        assert!(is_claude_settings(Path::new(
            "/Users/me/.CLAUDE/Settings.local.json"
        )));
        assert!(!is_claude_settings(Path::new("/work/notes/settings.json")));
        assert!(settings_path("", "").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_settings_file_is_followed_both_ways() {
        let real = settings_with("hook-link");
        let link = real.parent().unwrap().parent().unwrap().join("link.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(settings_path(&link.to_string_lossy(), "").is_some());

        // A dotfiles setup: ~/.claude/settings.json links to a file elsewhere.
        let home = crate::common::test_support::scratch_dir("dotfiles-home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let dotfile = home.join("dotfiles-claude.json");
        std::fs::write(&dotfile, "{}").unwrap();
        std::os::unix::fs::symlink(&dotfile, home.join(".claude/settings.json")).unwrap();
        let home = home.to_string_lossy().into_owned();
        assert!(settings_path(&dotfile.to_string_lossy(), &home).is_some());
        assert!(settings_path(&dotfile.to_string_lossy(), "").is_none());
    }

    #[test]
    fn bash_cannot_uninstall_playbook() {
        for command in [
            "playbook uninstall --yes",
            "cd x && playbook uninstall --yes --remove-binary",
            "nohup playbook uninstall --yes",
            "xargs playbook uninstall --yes",
            "bash -c 'playbook uninstall --yes'",
            "cargo run -q -- uninstall --yes",
            "/usr/local/bin/playbook uninstall --yes",
            "pb uninstall --yes",
            "nohup pb uninstall --yes",
            "doas playbook uninstall --yes",
        ] {
            assert_eq!(deny(command), Some(HOOK_ENTRIES_REASON), "{command}");
        }
        for command in [
            "playbook uninstall",
            "playbook uninstall --dry-run",
            "playbook uninstall --yes --dry-run",
            "playbook setup",
            "playbook init",
            "cargo run -q -- doctor",
        ] {
            assert_eq!(deny(command), None, "{command}");
        }
    }

    #[test]
    fn a_shell_with_option_flags_before_c_is_still_unwrapped() {
        assert_eq!(
            deny("bash --norc -c 'playbook uninstall --yes'"),
            Some(HOOK_ENTRIES_REASON)
        );
        assert_eq!(deny("bash --norc -c 'playbook doctor'"), None);
    }

    fn deny(command: &str) -> Option<&'static str> {
        check_command(command, HOME)
    }

    #[test]
    fn no_verify_is_denied_on_every_hooked_subcommand() {
        // Arrange
        let commands = [
            "git commit --no-verify -m x",
            "git commit -m x --no-verify",
            "git push --no-verify origin main",
            "git -C /repo commit --no-verify",
            "git commit --no-ver -m x",
            "cd x && git rebase --no-verify main",
            "FOO=1 git commit --no-verify",
        ];

        // Act / Assert
        for command in commands {
            assert_eq!(deny(command), Some(NO_VERIFY_REASON), "{command}");
        }
    }

    #[test]
    fn commit_short_n_is_denied_but_a_message_value_is_not() {
        // Arrange / Act / Assert
        assert_eq!(deny("git commit -n -m x"), Some(NO_VERIFY_REASON));
        assert_eq!(deny("git commit -anm x"), Some(NO_VERIFY_REASON));
        assert_eq!(deny("git commit -m n"), None);
        assert_eq!(deny("git commit -mn"), None);
        assert_eq!(deny("git commit -m \"-n fix\""), None);
        assert_eq!(
            deny("git push -n origin main"),
            None,
            "-n is --dry-run on push"
        );
    }

    #[test]
    fn signing_off_is_denied() {
        // Arrange / Act / Assert
        assert_eq!(deny("git commit --no-gpg-sign -m x"), Some(NO_SIGN_REASON));
        assert_eq!(deny("git tag --no-gpg-sign v1"), Some(NO_SIGN_REASON));
        assert_eq!(
            deny("git -c commit.gpgsign=false commit -m x"),
            Some(NO_SIGN_REASON)
        );
        assert_eq!(deny("git commit -S -s -m x"), None);
        assert_eq!(deny("git commit --no-signoff -m x"), None);
    }

    #[test]
    fn hand_run_gh_pr_create_is_denied() {
        // Arrange / Act / Assert
        assert_eq!(
            deny("gh pr create --title t --body b"),
            Some(PR_CREATE_REASON)
        );
        assert_eq!(deny("gh -R a/b pr create"), Some(PR_CREATE_REASON));
        assert_eq!(deny("gh pr create"), Some(PR_CREATE_REASON));
        assert_eq!(deny("gh pr view 12"), None);
        assert_eq!(deny("gh pr merge 12 --auto"), None);
        assert_eq!(deny("gh issue create"), None);
        assert_eq!(
            deny("playbook pr create --title t --body-file f"),
            None,
            "the skill's own command stays allowed"
        );
    }

    #[test]
    fn an_ordinary_command_passes() {
        // Arrange
        let commands = [
            "ls -la",
            "git status",
            "git commit -S -s -m 'fix: x'",
            "git log --oneline",
            "echo \"git commit --no-verify\"",
            "cat ~/.claude/projects/x/memory/a.md",
            "grep -r foo ~/.claude/memory",
        ];

        // Act / Assert
        for command in commands {
            assert_eq!(deny(command), None, "{command}");
        }
    }

    #[test]
    fn a_shell_write_to_claude_memory_is_denied_and_a_read_is_not() {
        // Arrange / Act / Assert
        assert_eq!(
            deny("echo x > ~/.claude/projects/-Users-me-repo/memory/a.md"),
            Some(MEMORY_REASON)
        );
        assert_eq!(
            deny("cat note.md >> $HOME/.claude/memory/a.md"),
            Some(MEMORY_REASON)
        );
        assert_eq!(
            deny("cp a.md /Users/me/.claude/projects/p/memory/a.md"),
            Some(MEMORY_REASON)
        );
        assert_eq!(
            deny("sed -i s/a/b/ ~/.claude/projects/p/memory/a.md"),
            Some(MEMORY_REASON)
        );
        assert_eq!(deny("ls ~/.claude/projects/p/memory"), None);
        // A fact goes into playbook's store with the Write tool, which shows
        // the fact format, not with a shell copy.
        assert_eq!(
            deny("cp a.md ~/.config/playbook/memory/a.md"),
            Some(CONFIG_STORE_REASON)
        );
        assert_eq!(deny("cat ~/.config/playbook/memory/a.md"), None);
    }

    #[test]
    fn file_tools_are_denied_inside_claude_memory_only() {
        // Arrange / Act / Assert
        let inside = [
            "/Users/me/.claude/projects/-Users-me-repo/memory/MEMORY.md",
            "/Users/me/.claude/projects/p/memory/sub/a.md",
            "/Users/me/.claude/memory/a.md",
            "~/.claude/projects/p/memory/a.md",
            "/Users/me/.claude/projects/p/x/../memory/a.md",
        ];
        for path in inside {
            assert_eq!(check_path(path, HOME), Some(MEMORY_REASON), "{path}");
        }
        let outside = [
            "/Users/me/.config/playbook/memory/a.md",
            "/Users/me/.claude/projects/p/session.jsonl",
            "/Users/me/.claude/settings.json",
            "/Users/me/.claude/CLAUDE.md",
            "/Users/me/repo/memory/a.md",
            "/Users/other/.claude/memory/a.md",
            "relative/memory/a.md",
            "",
        ];
        for path in outside {
            assert_eq!(check_path(path, HOME), None, "{path}");
        }
    }

    #[test]
    fn only_md_files_inside_the_store_are_fact_files() {
        // Arrange
        let store = Path::new("/Users/me/.config/playbook/memory");

        // Act / Assert
        assert!(is_fact_file(
            "/Users/me/.config/playbook/memory/a.md",
            store
        ));
        assert!(is_fact_file(
            "/Users/me/.config/playbook/memory/o/r/a.md",
            store
        ));
        assert!(!is_fact_file(
            "/Users/me/.config/playbook/memory/memory.graph.json",
            store
        ));
        assert!(!is_fact_file(
            "/Users/me/.config/playbook/memory/../x/a.md",
            store
        ));
        assert!(!is_fact_file("/Users/me/repo/a.md", store));
        assert!(!is_fact_file("memory/a.md", store));
    }

    #[test]
    fn the_schema_names_every_edge_type_and_the_anchor_forms() {
        // Arrange / Act / Assert
        for needle in [
            "supersedes",
            "depends_on",
            "relates_to",
            "contradicts",
            "anchors:",
            "#authenticate",
            "Why:",
        ] {
            assert!(MEMORY_SCHEMA.contains(needle), "{needle}");
        }
        assert!(MEMORY_SCHEMA.chars().count() < 1700);
    }

    #[test]
    fn an_empty_home_never_denies_a_path() {
        // Arrange / Act / Assert
        assert_eq!(check_path("/.claude/memory/a.md", ""), None);
    }

    #[test]
    fn garbage_input_is_allowed() {
        // Arrange / Act / Assert
        for command in [
            "",
            "git",
            "git -c",
            "gh",
            "'unterminated git commit --no-ver",
            ")(",
        ] {
            let _ = deny(command);
        }
        assert_eq!(deny("git -c"), None);
        assert_eq!(deny("gh"), None);
    }
}
