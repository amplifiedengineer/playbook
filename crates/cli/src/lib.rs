// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Library root for the `playbook` binary. Exposes the CLI shape that
//! `main.rs` parses and dispatches on, plus the `common` helpers and the
//! `hooks` modules, one per hook.

pub mod adr;
pub mod agents;
pub mod atlassian;
pub mod brief;
pub mod cc;
pub mod ci;
pub mod command_index;
pub use playbook_code as code;
pub mod commit;
pub mod common;
pub mod completions;
pub mod config;
pub mod deliver;
pub mod deps;
#[cfg(unix)]
pub mod doc;
/// Document conversion needs process groups and signals, so on other
/// platforms the command reports that it is unavailable.
#[cfg(not(unix))]
pub mod doc {
    use std::path::Path;

    pub mod converter {
        use std::path::Path;

        pub struct Search;

        impl Search {
            pub fn process(_configured: &str) -> Self {
                Self
            }

            pub fn is_available(&self) -> bool {
                false
            }
        }

        pub fn configured_path(_home: &Path, _slug: Option<&str>) -> String {
            String::new()
        }
    }

    #[derive(Debug, Default)]
    pub struct SliceRequest {
        pub offset: Option<usize>,
        pub limit: Option<usize>,
        pub section: Option<usize>,
    }

    pub fn read(_home: &Path, _file: &Path, _json: bool, _req: &SliceRequest) -> i32 {
        eprintln!("doc read: not supported on this platform");
        1
    }

    pub fn status(_home: &Path) -> i32 {
        println!("converter: document conversion is not supported on this platform");
        1
    }
}
pub mod doctor;
pub mod effort;
pub mod eval;
pub mod fastpath;
pub mod flags_snapshot;
pub mod gate;
pub mod gc;
pub mod handoff;
pub mod heal;
pub mod hooks;
pub mod init;
pub mod json;
pub mod learn;
pub mod manifest;
pub mod memory_import;
pub mod memory_stats;
pub mod migrate_cli;
pub mod mode;
pub mod models;
pub mod notices;
pub mod parse;
pub mod pending;
pub mod plan_cleanup;
pub mod planning;
pub mod plans;
pub mod plugin_root;
pub mod plugin_update;
pub mod pr;
pub mod prose;
pub mod release_notes;
pub mod renames;
pub mod review;
pub mod routing;
pub mod sanitize;
pub mod segment;
pub mod sessions;
pub mod settings;
pub mod state;
pub mod statusline;
pub mod storage;
pub mod sync;
#[doc(hidden)]
pub mod testing;
pub mod tools_map;
pub mod trust;
pub mod ui;
pub mod uninstall;
pub mod update;
pub use playbook_telemetry as usage;
pub mod service_manager;
pub mod skills_sync;
pub mod usage_host;
pub mod watch;
pub mod worktree;

use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
pub use code::cli::CodeCommand;
use std::path::PathBuf;
pub use usage::command::UsageCommand;

/// Configure and support Claude Code from the command line.
///
/// playbook installs hooks and a session launcher, shows the Agent Budget Dashboard,
/// and helps with PRs, reviews, worktrees and session handoffs.
#[derive(Parser, Debug)]
#[command(
    name = "playbook",
    bin_name = "playbook",
    version,
    after_help = "Run `playbook <command> --help` for details on one command. `pb` is a short alias for `playbook`.\n\
Check your setup with `/playbook:doctor` inside Claude Code (`playbook doctor --help` lists its helpers).\n\
Docs: https://github.com/amplifiedengineer/playbook"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// `playbook skills` subcommands.
#[derive(Subcommand, Debug)]
pub enum SkillsCommand {
    /// Copy the playbook skills into another agent's skill directory
    ///
    /// Without `--agent`, every agent found on this machine is synced. A
    /// second run with nothing new changes nothing.
    ///
    /// Example: `playbook skills sync --agent codex`
    Sync {
        /// Sync only this agent
        #[arg(long, value_parser = ["codex"])]
        agent: Option<String>,
        /// Show what would change and write nothing
        #[arg(long)]
        dry_run: bool,
        /// Print the report lines as a JSON array
        #[arg(long)]
        json: bool,
        /// Refresh the agents already synced and store a summary for the
        /// next session (started in the background)
        #[arg(long, hide = true)]
        refresh: bool,
    },
}

/// `playbook state` subcommands.
#[derive(Subcommand, Debug)]
pub enum StateCommand {
    /// Print the state rows, sorted by key
    List {
        /// Only keys that start with this prefix
        prefix: Option<String>,
        /// Print the rows as one JSON object
        #[arg(long)]
        json: bool,
    },
}

// Variant order is the order users see in `--help`; nothing else depends on it.
/// `playbook effort` subcommands.
#[derive(Subcommand, Debug)]
pub enum EffortCommand {
    /// Show one component's shipped effort, ceiling and effective level
    ///
    /// For an agent it also names the file to dispatch and every file allowed
    /// under the ceiling.
    ///
    /// `--cap <level>` adds a ceiling from the calling command, for example
    /// `review` holding its reviewer to `medium`. The lowest of the cap
    /// and the user's ceilings applies.
    ///
    /// Example: `playbook effort resolve agents reviewer --cap medium --json`
    Resolve {
        /// One of agents, commands or skills
        kind: String,
        /// Component name, such as `reviewer` or `review`
        name: String,
        /// Extra ceiling from the caller: low, medium, high, xhigh or max
        #[arg(long)]
        cap: Option<String>,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// List agents where a change to effort looks safe or needed (read only)
    ///
    /// Reads the local usage store and the quality floor table and prints, for
    /// each agent, its dispatches, reruns and overrides, the cost per effort
    /// level, and whether lowering is permitted or something needs a look.
    /// Lowering is only ever proposed down to the benchmark floor. Nothing is
    /// written: not the store, not memory, not any settings file. Cost is per
    /// model and effort bucket, shared with the main session.
    ///
    /// Example: `playbook effort suggest --range 90d --json`
    Suggest {
        /// One of 30d, 60d, 90d, month or all
        #[arg(long, default_value = "30d")]
        range: String,
        /// Print JSON
        #[arg(long)]
        json: bool,
        /// Print a proposed config patch instead (still read only)
        #[arg(long)]
        patch: bool,
    },
    /// Apply the proposed effort patch to playbook's config, after approval
    ///
    /// Builds the same patch as `suggest --patch`, shows it, and asks `y/N`.
    /// `--yes` approves without asking, and is required when not at a
    /// terminal. Writes only playbook's own config: the global tier, a backup
    /// of the previous `effort.*` keys under `~/.config/playbook/effort/`, a
    /// ledger and a change log. Never goes below an agent's quality floor,
    /// never touches the reviewer, the auditor or any agent marked costly to
    /// miss, and never exceeds a ceiling. Claude Code's settings are not touched.
    ///
    /// Example: `playbook effort apply --range 90d`
    Apply {
        /// One of 30d, 60d, 90d, month or all
        #[arg(long, default_value = "30d")]
        range: String,
        /// Approve without asking
        #[arg(long)]
        yes: bool,
    },
    /// Choose an effort for one dispatch from its diff
    ///
    /// Does nothing while `effort.auto` is `off` (the default). With `suggest`
    /// it logs what it would pick and the dispatch keeps the default. With
    /// `on` it names the `effort` to pass to the `Agent` call on the base
    /// agent, picked strictly between the agent's quality floor and every
    /// ceiling, raised (to `high` at most) for risky or large diffs and lowered
    /// only for small clean ones. An agent
    /// marked costly to miss is never lowered. Any error gives the default.
    /// Every choice is appended to a local log (`playbook effort log`).
    ///
    /// Example: `playbook effort choose reviewer --base origin/main --json`
    Choose {
        /// Agent name, such as `reviewer`
        agent: String,
        /// Read the diff shape from `git diff --numstat <ref>`
        #[arg(long)]
        base: Option<String>,
        /// Changed lines, when not using --base
        #[arg(long)]
        lines: Option<u64>,
        /// Changed files, when not using --base
        #[arg(long)]
        files: Option<u64>,
        /// Diff size in bytes
        #[arg(long)]
        bytes: Option<u64>,
        /// Touched path, repeated for several, when not using --base
        #[arg(long = "path")]
        paths: Vec<String>,
        /// Review lens, such as `security`
        #[arg(long)]
        lens: Option<String>,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Show the automatic effort choices that were logged
    ///
    /// Each row has the time (UTC), component, default and chosen level, the
    /// inputs and the reason. The log is local and append only.
    ///
    /// Example: `playbook effort log --limit 20`
    Log {
        /// How many of the latest rows to show
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Undo what `effort apply` changed, in one command
    ///
    /// Restores each key the patch changed to its earlier value, unless you
    /// edited it since. `--all` also sets every other `effort.*` key in the
    /// global tier back to its default. A backup is written first.
    ///
    /// Example: `playbook effort reset --all`
    Reset {
        /// Also reset every `effort.*` key to its default
        #[arg(long)]
        all: bool,
    },
    /// List every component with its shipped effort and effective level
    List {
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
}

/// The line the old command name prints on stderr in a terminal.
pub const INIT_DEPRECATION: &str = "playbook init is deprecated, use: playbook setup";

/// Top-level subcommand groups.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Install or repair your local Claude Code setup
    ///
    /// Safe to run again: it only touches what is missing or out of date.
    /// In a terminal it asks about each optional part (hooks, status line,
    /// PATH, the system prompt, the `pb` short name) unless you answer with a flag.
    /// Each part has `--x` and `--no-x`. With `--yes`,
    /// unanswered parts use their defaults, and every asked part is on: hooks,
    /// the status line, PATH, the system prompt and `pb`. The system prompt
    /// stays off when `systemPrompt.enabled` is false or an earlier install
    /// had none, and `pb` stays off after a stored no. Telemetry is on by default too: the
    /// question defaults to yes and `--yes` is a yes, but `--yes` alone never
    /// replaces an off switch you set. Without a terminal and without `--yes`,
    /// only PATH is on: hooks, the status line, the system prompt, `pb` and
    /// telemetry stay off unless you pass their flag.
    /// `--only telemetry` (or a bare `--telemetry`) runs just that part;
    /// `--only` takes any parts and leaves everything else alone.
    ///
    /// The security defaults (permissions rules)
    /// are never asked about and stay off unless you pass `--security` or set
    /// `security.defaults` to true. Without them, `setup` leaves any
    /// permissions you already have untouched.
    ///
    /// Before it asks anything, setup prints a plan of each part and the files
    /// it edits. `--dry-run` shows what would change on your machine and
    /// changes nothing. `--explain` describes a part, or every part, and
    /// changes nothing.
    ///
    /// Example: `playbook setup --no-hooks --system-prompt`
    Setup {
        #[command(flatten)]
        flags: init::choices::Flags,
    },
    /// Deprecated name for `playbook setup`
    ///
    /// Runs `playbook setup` with the same flags and exit codes. In a terminal
    /// it also prints one line on stderr that names `playbook setup`. Hidden
    /// from the command list and kept for the whole 0.x line (ADR-0040).
    #[command(hide = true)]
    Init {
        #[command(flatten)]
        flags: init::choices::Flags,
    },
    /// List the hooks that `playbook setup` wires into Claude Code
    Hooks {
        #[command(subcommand)]
        sub: HooksCommand,
    },
    /// Remove what playbook installed from this machine
    ///
    /// Takes out the hook entries and status line that `playbook setup` wrote
    /// into `settings.json`, the launcher block in your shell rc files, and
    /// the files playbook placed under `~/.config/playbook`. It stops and
    /// removes a collector service that an older setup installed, under the
    /// current or the 0.37.0 launchd label. Everything else
    /// in `settings.json` and the rc files stays, each changed file is backed
    /// up first, and memory, sessions and history are never touched. The
    /// `playbook` binary stays unless you pass `--remove-binary`. It changes
    /// nothing without `--yes`; `--dry-run` lists what would go.
    ///
    /// Example: `playbook uninstall --dry-run`
    Uninstall {
        /// Go ahead and remove (without it, nothing is changed)
        #[arg(long)]
        yes: bool,
        /// List what would be removed and change nothing
        #[arg(long)]
        dry_run: bool,
        /// Also remove the `playbook` binary, its backups and the PATH line the installer added
        #[arg(long)]
        remove_binary: bool,
    },
    /// Show what playbook state takes space on disk, or clean it up
    ///
    /// Lists each store of on-disk state with its size, item count, age and
    /// the action a cleanup would take. It changes nothing and takes no lock.
    /// With `--yes` it repairs interrupted moves, tombstones items past their
    /// limit and deletes tombstones past their grace period. With `--restore`
    /// it puts a tombstoned item back where it came from.
    ///
    /// Example: `playbook gc`
    Gc {
        /// Apply the cleanup
        #[arg(long, conflicts_with = "restore")]
        yes: bool,
        /// Restore a tombstoned item by id
        #[arg(long, value_name = "ID")]
        restore: Option<String>,
        /// Also clean build output in unmanaged worktrees, after a prompt (not available yet)
        #[arg(long, hide = true)]
        build_output: bool,
    },
    /// Upgrade playbook to a published release, and its plugin with it
    ///
    /// Looks up the latest release and compares it with this binary. When it
    /// is newer, shows both versions and asks you to confirm in a terminal;
    /// `--yes` skips the question. With no terminal and no `--yes`, it prints
    /// the hint and changes nothing. After you confirm, it downloads the
    /// release, checks its SHA256 sum and, when `gh` is installed, its build
    /// attestation, then swaps the binary and keeps the old one as a backup.
    /// In auto mode it refuses unless you pass `--yes`.
    ///
    /// Then it runs the plugin step of `playbook plugin install` and
    /// `playbook plugin upgrade` so the plugin matches the new binary: it adds
    /// the marketplace when it is missing, turns on `autoUpdate` for it, and
    /// installs or updates the plugin. Pass `--no-plugin` or set `upgrade.plugin`
    /// to `off` to skip that step. To change only the plugin, run
    /// `playbook plugin upgrade`.
    ///
    /// A Homebrew install is never replaced: `upgrade` checks for a newer
    /// release, says Homebrew owns the binary, and tells you to run
    /// `brew upgrade playbook`. It exits 0.
    ///
    /// `--check` and `--list` change nothing. `--check` exits 0 whether or not
    /// a newer release exists, and 1 on an error such as an unreachable
    /// release list or an unknown version. `--json` prints one object in the
    /// shared envelope and exits 0 whatever the result.
    ///
    /// Example: `playbook upgrade --check`
    #[command(alias = "update")]
    Upgrade {
        /// Version to install (default: the latest stable release)
        version: Option<String>,
        /// Only report whether an update is available
        #[arg(long)]
        check: bool,
        /// List published releases
        #[arg(long)]
        list: bool,
        /// Consider pre-releases when picking the latest
        #[arg(long)]
        pre: bool,
        /// Skip the confirmation question, and confirm the upgrade when auto
        /// mode is on
        #[arg(long)]
        yes: bool,
        /// Print one JSON object in the shared envelope, and exit 0 whatever
        /// the result
        #[arg(long)]
        json: bool,
        /// Refresh the cached release check and exit (started in the background)
        #[arg(long, hide = true)]
        refresh_check: bool,
        /// Update the plugin and store the outcome line for the next session
        /// (started in the background)
        #[arg(long, hide = true)]
        refresh_plugin: bool,
        /// Print the release notes of this binary from a release-notes.json
        /// the install script checked, record them as shown, and exit
        #[arg(long, hide = true, value_name = "FILE")]
        print_notes: Option<std::path::PathBuf>,
        /// The version the install script replaced, for --print-notes
        #[arg(long, hide = true, value_name = "VERSION", requires = "print_notes")]
        from: Option<String>,
        /// Leave the playbook plugin in Claude Code as it is. By default
        /// `upgrade` installs or updates it, unless the config key
        /// `upgrade.plugin` is `off`
        #[arg(long)]
        no_plugin: bool,
    },
    /// Manage the playbook plugin in Claude Code, apart from the binary
    ///
    /// `install` adds the marketplace and the plugin, for a playbook that
    /// did not come from Homebrew. `upgrade` refreshes the marketplace and
    /// runs `claude plugin update` for each installed playbook plugin id.
    /// Neither downloads a binary. Run `playbook upgrade` to update the
    /// binary.
    ///
    /// Example: `playbook plugin install`
    Plugin {
        #[command(subcommand)]
        sub: PluginCommand,
    },
    /// Print the installed version, same as `--version`
    Version,
    /// Print a shell completion script for bash, zsh or fish
    ///
    /// Example: `playbook completions zsh`
    Completions {
        /// Shell to print the script for
        #[arg(value_enum)]
        shell: completions::CompletionShell,
    },
    /// Install the tools a Brewfile lists, keeping any already on PATH
    ///
    /// Example: `playbook deps ensure Brewfile`
    Deps {
        #[command(subcommand)]
        sub: DepsCommand,
    },
    /// Read a PDF or Office file as text with the bundled converter
    ///
    /// Conversion is local, bounded in size, time, memory and output, and
    /// never uses OCR or a hosted service. The text is printed as untrusted
    /// document content. A long document is printed in slices, and the full
    /// text is kept in a private cache for a day.
    ///
    /// Example: `playbook doc read report.docx`
    Doc {
        #[command(subcommand)]
        sub: DocCommand,
    },
    /// Check the playbook setup, or read one fact the check uses
    ///
    /// `playbook doctor` with no subcommand runs `playbook doctor check`, the
    /// full health check the `/playbook:doctor` command uses. It only reads. The other subcommands print one fact
    /// each, such as pending migrations, plugin version or hook wiring, and
    /// help when a layer reports a miss.
    ///
    /// `playbook doctor --fix` repairs playbook's own files after an upgrade
    /// and prints the list of what still needs you, the same as
    /// `playbook migrate`. At a terminal it first asks whether to turn
    /// attribution OFF, the default, by adding the missing `attribution`
    /// keys; that yes is the only Claude Code setting it writes. A no stores
    /// attribution ON.
    ///
    /// Example: `playbook doctor`
    ///
    /// Example: `playbook doctor migrations`
    #[command(args_conflicts_with_subcommands = true, subcommand_required = false)]
    Doctor {
        /// Run the safe migrations for playbook's own files, then list what needs you
        #[arg(long)]
        fix: bool,
        #[command(subcommand)]
        sub: Option<DoctorCommand>,
    },
    /// Bring playbook's own files up to date with this binary
    ///
    /// Runs the migrations that write only playbook's own files, then records
    /// this binary as the one that ran them. The ones that run once start only
    /// after an upgrade, and the checks that repair a file run every time.
    /// A migration waits for the release named in its `since`.
    /// Nothing runs after a downgrade. It never writes Claude Code settings.
    /// It ends with the list of what still needs you, one bullet per item
    /// with why it matters and the command that fixes it: `playbook setup`
    /// for a change to Claude Code settings or a shell file, and its own fix
    /// for a report. The session start then stops repeating a report it
    /// printed.
    /// A run also updates the playbook plugin through
    /// `claude plugin update`, then says to restart Claude Code. A missing
    /// `claude` command or a failed update prints the command to run and
    /// does not change the exit code. `--no-plugin` or
    /// `playbook config set upgrade.plugin off` turns this off.
    /// `update`, the install script and `doctor --fix` run the same step.
    ///
    /// Exit codes: 0 when it finished or had nothing to do, 1 when the record
    /// could not be read or written, a migration failed, or another playbook
    /// process held the lock.
    ///
    /// Example: `playbook migrate --status`
    Migrate {
        /// List each migration with its release, scope and state, writing nothing
        #[arg(long, conflicts_with = "dry_run")]
        status: bool,
        /// Show the migrations a run would start and what needs you, writing nothing
        #[arg(long)]
        dry_run: bool,
        /// Print JSON for tools
        #[arg(long)]
        json: bool,
        /// Leave the playbook plugin in Claude Code as it is. By default a
        /// run updates it with `claude plugin update`, unless the config key
        /// `upgrade.plugin` is `off`
        #[arg(long)]
        no_plugin: bool,
    },
    /// Choose whether playbook asks questions or decides on its own
    ///
    /// `ask` is the default. `auto` lets commands pick the recommended option
    /// and keep going. The choice is stored per repo.
    ///
    /// Example: `playbook mode auto`
    Mode {
        #[command(subcommand)]
        sub: ModeCommand,
    },
    /// Read and change playbook settings in any tier
    ///
    /// Settings come from tiers, highest first: repo, org, global, then the
    /// built-in default. `get` and `list` show which tier supplied a value.
    ///
    /// Example: `playbook config set pr.review.enabled true`
    Config {
        #[command(subcommand)]
        sub: ConfigCommand,
    },
    /// Share usage between your machines, or turn that off
    ///
    /// Sync is off until you run `playbook sync enable`. It sends only the
    /// usage columns it lists to your own Turso database. Most names run the
    /// `pbsync` helper that ships next to playbook, and `playbook sync config`
    /// shares the settings that may sync. `pbsync` is a short name for the
    /// same commands.
    ///
    /// Example: `playbook sync status`
    Sync {
        #[command(subcommand)]
        sub: SyncCommand,
    },
    /// Inspect playbook's own bookkeeping (migration record, sweep markers)
    ///
    /// `list` prints the rows of the `state` table in `config.db`, sorted by
    /// key, optionally only those under a prefix. Read only: use `playbook
    /// setup` to repair and `playbook config export` to back up the config.
    ///
    /// Example: `playbook state list migrations/`
    State {
        #[command(subcommand)]
        sub: StateCommand,
    },
    /// Share the playbook skills with other agents
    ///
    /// `sync` copies the plugin's skills and commands into the folder that
    /// Codex reads, `~/.agents/skills`, as `playbook-*` folders. It never
    /// touches folders you made yourself or edited.
    ///
    /// Example: `playbook skills sync --dry-run`
    Skills {
        #[command(subcommand)]
        sub: SkillsCommand,
    },
    /// Set playbook's own ceiling on effort, or inspect one component's
    ///
    /// `playbook effort list` shows every command, skill and agent with its
    /// shipped effort and effective level. `playbook effort resolve agents
    /// reviewer` says which agent file to dispatch under the ceilings. Set a
    /// per-component ceiling with `playbook config set --global
    /// effort.agents.reviewer medium`. `playbook effort suggest` reads your
    /// usage and lists agents where a change looks safe or needed.
    ///
    /// `auto` (the default) sets no ceiling, so Claude Code's own
    /// `maxEffortLevel` decides how high effort may go. Each skill, command and
    /// agent still keeps the effort it ships with. `low`, `medium`, `high`,
    /// `xhigh` or `max` set playbook's own ceiling. The key is
    /// `effort.max`. The effective ceiling is the lower of this and Claude Code's own
    /// `maxEffortLevel`, which playbook only reads and never changes. Sessions
    /// started with `playbook cc` or `playbook ccd` get the ceiling. With no level, shows both
    /// values and the one that wins.
    ///
    /// The ceiling lowers the `effort` an orchestrator passes when it spawns an
    /// agent, the default effort the launcher adds and what the binary reports.
    /// It does not cap a command or skill that ships its own effort, a level you
    /// state for the session, or a spawn that skips the resolve step, because
    /// only Claude Code's `maxEffortLevel` setting can, and playbook never
    /// writes it.
    ///
    /// Example: `playbook effort xhigh`
    #[command(args_conflicts_with_subcommands = true)]
    Effort {
        #[command(subcommand)]
        sub: Option<EffortCommand>,
        /// One of auto, low, medium, high, xhigh or max
        level: Option<String>,
        /// Print the status as JSON (with no level)
        #[arg(long)]
        json: bool,
    },
    /// Say which model, effort and agent a kind of task goes to
    ///
    /// One routing table for every command that dispatches agents. Kinds:
    /// mechanical, classify, check, review, write, implement, design. Only
    /// `implement` reads `--tier` (low, medium or high). The answer includes
    /// whether the user must approve first (the top model, the high tier, or a
    /// task that failed twice). `routing.escalate` decides what happens then:
    /// `ask` tells you to ask the user, `auto` (default) proceeds (auto mode
    /// and `auto.budget.mainUsd` still cap the spend), `deny` returns the cheaper
    /// route. Your effort ceiling always wins.
    ///
    /// Example: `playbook route implement --tier medium --json`
    Route {
        /// One of mechanical, classify, check, review, write, implement or design
        kind: String,
        /// One of low, medium or high. Only `implement` reads it
        #[arg(long, default_value = "low")]
        tier: String,
        /// How many times this task already failed
        #[arg(long, default_value_t = 0)]
        failures: u32,
        /// Print the decision as JSON
        #[arg(long)]
        json: bool,
    },
    /// Start a Claude Code session, or manage sessions and worktrees
    ///
    /// With no subcommand, starts a session with the playbook prompt and model
    /// chain. Any other arguments go to claude as they are. The subcommands do the housekeeping: list sessions,
    /// prune state, clear caches, and create worktrees. `pb` is an alias for
    /// `playbook`, so `pb cc` works too.
    ///
    /// Example: `playbook cc --model sonnet`
    #[command(args_conflicts_with_subcommands = true)]
    Cc {
        #[command(subcommand)]
        sub: Option<CcCommand>,
        /// Arguments passed through to claude
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Start a session with permission prompts skipped
    ///
    /// The same launcher as `playbook cc`, with
    /// `--dangerously-skip-permissions` added. It replaces the old `ccd`
    /// shortcut. `pb ccd` works too.
    ///
    /// Example: `playbook ccd fresh`
    Ccd {
        /// Subcommand and arguments, passed through to claude
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show token and cost usage across your sessions
    ///
    /// With no subcommand, opens the terminal view, which opens on this week
    /// compared with last week. When stdout is not a terminal, or with
    /// `--summary`, prints a text summary of estimated cost instead. `--json`
    /// prints the same numbers as JSON for scripts. Times and days use your
    /// local time zone, shown in the output; `--utc` switches to UTC, or set
    /// `usage.timeZone` or `PLAYBOOK_TELEMETRY_TZ` to `local` or `utc`. Ranges:
    /// today, week, last-week, month, last-month, 30d, 60d, 90d, all. Weeks
    /// start on Monday unless `usage.weekStart` says otherwise. For today,
    /// week and month, `--json` adds a comparison with the previous period
    /// over the same elapsed span, and `--summary` prints it. The cost per
    /// agent and skill, tool latency and health views come from Claude Code's
    /// OpenTelemetry export when it is available, see `playbook usage collect`.
    ///
    /// Example: `playbook usage --range today`
    Usage {
        #[command(subcommand)]
        sub: Option<UsageCommand>,
        /// Print the totals, groupings and live view as JSON
        #[arg(long, conflicts_with_all = ["summary"])]
        json: bool,
        /// Print the text summary and exit (every recorded session unless `--range` is given)
        #[arg(long, conflicts_with_all = ["json"])]
        summary: bool,
        /// Date range, counted in days of the chosen time zone. One of today, week, last-week, month, last-month, 30d, 60d, 90d or all. Default week for the terminal view, 30d for `--json`, all for `--summary`
        #[arg(long, value_name = "RANGE")]
        range: Option<String>,
        /// Show only one account, by key or name. Scopes `--summary` and `--json`, and the terminal view opens on it
        #[arg(long, value_name = "KEY_OR_NAME")]
        account: Option<String>,
        /// Show times and group days in UTC. Overrides `usage.timeZone` and `PLAYBOOK_TELEMETRY_TZ`
        #[arg(long, global = true, conflicts_with = "local")]
        utc: bool,
        /// Show times and group days in your local time zone. This is the default. Overrides `usage.timeZone` and `PLAYBOOK_TELEMETRY_TZ`
        #[arg(long, global = true, conflicts_with = "utc")]
        local: bool,
    },
    /// List recent agent sessions and their state
    ///
    /// Shows the repo, branch, worktree, linked PR, state, context size and
    /// cost of each session. Read only. A session is done when the PR of its
    /// branch is merged or closed, and stale after a few days
    /// without activity. Suggestions are printed, never acted on, and a
    /// worktree with uncommitted changes is flagged, never removed.
    ///
    /// Example: `playbook sessions --json`
    Sessions {
        #[command(subcommand)]
        sub: Option<SessionsCommand>,
        /// Print the sessions as JSON
        #[arg(long)]
        json: bool,
    },
    /// Poll GitHub issues and comments and act on them by rules
    ///
    /// Rules live in `watch.rule.<id>` and `watch.repos` lists the repos. At
    /// the default level a matching event becomes a proposal that nothing
    /// acts on until you approve it. The GitHub token is read from the
    /// environment variable named by `watch.tokenEnv` (`GITHUB_TOKEN` when
    /// unset).
    ///
    /// With no verb it runs in the foreground until you stop it, polling every
    /// `watch.pollSeconds` and picking up after a laptop sleep from where it
    /// stopped. A second foreground run prints `watch already running` and
    /// exits 1.
    ///
    /// Example: `playbook watch once`
    Watch {
        #[command(subcommand)]
        sub: Option<WatchCommand>,
    },
    /// Check how well the review triage classifier sorts real PRs
    ///
    /// Each case is a PR with a known answer per review lens. Costs
    /// one `gh pr diff` fetch and one live `claude` call per case, so run it
    /// by hand or on a schedule.
    ///
    /// Example: `playbook eval triage`
    Eval {
        #[command(subcommand)]
        sub: EvalCommand,
    },
    /// Prepare and open PRs, and pick a review depth
    ///
    /// These are the mechanical steps behind `/playbook:pr-create`.
    ///
    /// Example: `playbook pr prepare --base main`
    Pr {
        #[command(subcommand)]
        sub: PrCommand,
    },
    /// Clean up git worktrees whose branches have landed
    ///
    /// Locked worktrees are kept unless the process that locked them is gone.
    ///
    /// Example: `playbook worktree sweep --dry-run`
    Worktree {
        #[command(subcommand)]
        sub: WorktreeCommand,
    },
    /// Manage the memory graph that sessions read from
    Memory {
        #[command(subcommand)]
        sub: MemoryCommand,
    },
    /// Save and show notes that carry over to the next session
    ///
    /// A handoff is a markdown note for one directory. The next session started
    /// there loads it when the SessionStart hook is installed and runs, and
    /// `/playbook:handoff-show` loads it on demand.
    Handoff {
        #[command(subcommand)]
        sub: HandoffCommand,
    },
    /// Run one Claude Code hook by name
    ///
    /// Claude Code calls this from its settings, so you rarely type it.
    /// Hidden from the command list but fully working.
    #[command(hide = true)]
    Hook {
        /// Which hook to run, matching the name Claude Code passes from its hook settings
        name: HookName,
    },
    /// Print the Claude Code status line
    ///
    /// Claude Code runs this to draw the line at the bottom of a session.
    Statusline,
    /// Create or check the shared settings template
    Settings {
        #[command(subcommand)]
        sub: SettingsCommand,
    },
    /// Check that tracked files sit in allowed top-level paths
    Manifest {
        #[command(subcommand)]
        sub: ManifestCommand,
    },
    /// Check that agent definition files follow the playbook rules
    Agents {
        #[command(subcommand)]
        sub: AgentsCommand,
    },
    /// Record and check plan verdicts used as gates
    ///
    /// Verdicts are stored per worktree, keyed by plan.
    Gate {
        #[command(subcommand)]
        sub: GateCommand,
    },
    /// Index and search the repo's source code
    ///
    /// The index is a cache kept under the playbook state directory for this
    /// worktree. Nothing is written inside the repo.
    Code {
        #[command(subcommand)]
        sub: CodeCommand,
    },
    /// Remove AI attribution lines from message text
    ///
    /// For callers that are not an agent hook, such as a git `commit-msg`
    /// hook or CI. Each subcommand rewrites FILE in place unless `--check` is
    /// given, and prints one line per removed line to stderr: its number and
    /// the shape of the attribution, never its text. Without `--check` the exit
    /// code is always 0, even when FILE cannot be read or written.
    Sanitize {
        #[command(subcommand)]
        sub: SanitizeCommand,
    },
    /// Run the repo checks that need no model (manifest, agents, settings)
    ///
    /// Prints one line per check. Exits 0 when all pass and 1 if any fails,
    /// so it is safe in CI. A skipped check passes unless `--strict` is set.
    ///
    /// Example: `playbook ci --strict`
    Ci {
        /// Print one JSON object instead of text
        #[arg(long)]
        json: bool,
        /// Also exit 1 when a check is skipped (for example in the wrong directory)
        #[arg(long)]
        strict: bool,
        /// Run in this directory instead of the current one
        #[arg(long)]
        dir: Option<String>,
    },
    /// Print the path of a playbook storage directory
    ///
    /// Prints one of this repo's storage directories. For those kinds it
    /// first moves any legacy directories from the old repo-local location.
    /// `data` and `plugin-root` skip that move. `plugin-root` prints the installed playbook plugin directory instead. It
    /// looks at `PLAYBOOK_PLUGIN_DIR`, then the `installPath` Claude Code
    /// recorded, then `CLAUDE_PLUGIN_ROOT`, then the newest plugin cache
    /// directory. When none is usable it prints one line naming each place
    /// tried and exits 1. `--create` does not apply to it.
    ///
    /// Example: `playbook path plans`
    Path {
        /// Which directory to print
        kind: PathKind,
        /// Create the directory when it does not exist
        #[arg(long)]
        create: bool,
    },
    /// Save plan checkpoints, write Work Unit briefs, clear scratch files
    Plan {
        #[command(subcommand)]
        sub: PlanCommand,
    },
    /// Append entries to the repo's `GLOSSARY.md` file
    Glossary {
        #[command(subcommand)]
        sub: GlossaryCommand,
    },
    /// List the commands a session can use for each task
    Tools {
        #[command(subcommand)]
        sub: ToolsCommand,
    },
    /// List every playbook command, what it does and who may run it
    ///
    /// Built from this binary's own command tree, so it lists only the
    /// commands the binary has. One line per command, with its subcommands. A name
    /// ending in `!` or a line marked `(user-only)` is for the user to run;
    /// `(read-only)` runs freely. Give a group to list its subcommands with
    /// their flags. Reads nothing and writes nothing.
    ///
    /// Example: `playbook commands pr`
    Commands {
        /// Command or group to list, such as `pr` or `worktree review`
        group: Vec<String>,
        /// Print a JSON array with one object per command: command, purpose, access, args, subcommands, intents
        #[arg(long)]
        json: bool,
    },
    /// Work with ADRs, such as the next free record number
    Adr {
        #[command(subcommand)]
        sub: AdrCommand,
    },
    /// Check prose against the writing-style rules
    Prose {
        #[command(subcommand)]
        sub: ProseCommand,
    },
    /// Check the Atlassian tools the `playbook:atlassian` skill uses
    Atlassian {
        #[command(subcommand)]
        sub: AtlassianCommand,
    },
    /// Gather the repo facts that `/playbook:learn` reads
    Learn {
        #[command(subcommand)]
        sub: LearnCommand,
    },
    /// Run the git steps of `/playbook:commit`
    Commit {
        #[command(subcommand)]
        sub: CommitCommand,
    },
    /// Run the mechanical steps behind the review commands
    Review {
        #[command(subcommand)]
        sub: ReviewCommand,
    },
    /// Run the git recipes `/playbook:implement` uses for a Segment
    Segment {
        #[command(subcommand)]
        sub: SegmentCommand,
    },
    /// Mark a directory as trusted in Claude Code
    ///
    /// Writes to `~/.claude.json` so Claude Code's first-launch trust prompt
    /// does not block `playbook cc` or `playbook ccd`. Always exits 0, even on failure, so a
    /// script can call it without checking.
    ///
    /// Example: `playbook trust /Users/me/project`
    Trust {
        /// Absolute path of the directory to trust
        path: String,
    },
}

impl Command {
    /// Commands that run many times a session or inside other commands:
    /// hooks, the status line, `version`, `completions`, and
    /// the hidden `json` helpers that hook scripts call. `commands` is here
    /// too: it only prints the clap tree and reads nothing under home. Steps
    /// that run before a command (the first-run heal, the update check) skip
    /// them.
    pub fn is_hot_path(&self) -> bool {
        matches!(
            self,
            Command::Hook { .. }
                | Command::Statusline
                | Command::Version
                | Command::Completions { .. }
                | Command::Commands { .. }
        )
    }

    /// The flags of `setup`, or of `init`, its deprecated name. Checks that
    /// treat the two alike use this, so neither name can be left out.
    pub fn setup_flags(&self) -> Option<&init::choices::Flags> {
        match self {
            Command::Setup { flags } | Command::Init { flags } => Some(flags),
            _ => None,
        }
    }

    /// The one stderr line a deprecated command name prints before it runs,
    /// or `None`. Only `init` has one, and only when stderr is a terminal, so
    /// scripts and the plugin, which read the output, see the same text as
    /// `setup` prints.
    pub fn deprecation_notice(&self, stderr_is_terminal: bool) -> Option<&'static str> {
        match self {
            Command::Init { .. } if stderr_is_terminal => Some(INIT_DEPRECATION),
            _ => None,
        }
    }
}

/// `playbook eval` subcommands.
#[derive(Subcommand, Debug)]
pub enum EvalCommand {
    /// Run the review triage classifier on each case
    ///
    /// Compares each result with the known answer. Needs network access, a
    /// logged-in `gh`, and a working `claude` with a live API key. Exits 0 when
    /// no lens was wrongly skipped and no case errored, and 1 otherwise. Re-run
    /// it whenever the triage prompt changes.
    ///
    /// Example: `playbook eval triage`
    Triage {
        /// Case file to run; defaults to the cases shipped in the playbook repo
        fixtures: Option<PathBuf>,
        /// Triage prompt to send; defaults to the one shipped in the playbook repo
        #[arg(long)]
        prompt: Option<PathBuf>,
    },
    /// Benchmark agent roles across models and effort levels
    ///
    /// Each case is a fixed prompt with an objective check (a planted bug
    /// found, a claim judged right, a test that passes), so no judge model is
    /// involved. It prints pass rate, cost and time per role, model and
    /// effort. It estimates the cost first and refuses to start above
    /// `--max-cost-usd`, and stops mid run when the spend reaches it. Needs a
    /// logged-in `claude`; it makes real API calls.
    ///
    /// With `--strategy`, the implementer cases run with tools in a scratch git
    /// repo, comparing one dispatch per TDD step against one per Work Unit.
    ///
    /// Example: `playbook eval bench --role fact-checker --model haiku,sonnet --effort low,medium --runs 2`
    Bench {
        /// Case file or directory; defaults to the cases shipped in the playbook repo
        #[arg(long)]
        cases: Option<PathBuf>,
        /// Only these roles (comma separated)
        #[arg(long, value_delimiter = ',')]
        role: Vec<String>,
        /// Only cases whose id starts with one of these (comma separated)
        #[arg(long, value_delimiter = ',')]
        id: Vec<String>,
        /// Models to try (aliases such as haiku, sonnet, opus)
        #[arg(long, value_delimiter = ',', default_value = "haiku")]
        model: Vec<String>,
        /// Effort levels to try
        #[arg(long, value_delimiter = ',', default_value = "medium")]
        effort: Vec<String>,
        /// Calls per case, model and effort
        #[arg(long, default_value_t = 1)]
        runs: u32,
        /// Spend cap in US dollars; the run is refused above it
        #[arg(long, default_value_t = 5.0)]
        max_cost_usd: f64,
        /// Calls in flight at once
        #[arg(long, default_value_t = 4)]
        jobs: usize,
        /// Print JSON instead of a table
        #[arg(long)]
        json: bool,
        /// List the cases and exit
        #[arg(long)]
        list: bool,
        /// Run the `tdd_repo` implementer cases with tools in a scratch git repo.
        /// Use one dispatch per TDD step (`per-step`) or one per Work Unit (`per-wu`)
        #[arg(long, value_delimiter = ',')]
        strategy: Vec<String>,
    },
}

/// `playbook plugin` subcommands.
#[derive(Subcommand, Debug)]
pub enum PluginCommand {
    /// Install the playbook plugin and its marketplace into Claude Code
    ///
    /// For a playbook that did not come from Homebrew, such as one from the
    /// install script, a container, a home server platform like Umbrel or a
    /// nix profile. Needs `claude` on PATH. Adds the `amplifiedengineer/marketplace`
    /// marketplace unless Claude Code lists it already, and leaves one listed
    /// under the old `pragmatic-engineer` name as it is. Turns on
    /// `autoUpdate` for the marketplace in `~/.claude/settings.json`, and
    /// keeps an explicit `false`. Then installs `playbook@amplifiedengineer`.
    /// Claude Code updates a plugin with its marketplace, so that one flag
    /// also keeps the plugin up to date; there is no per-plugin key.
    ///
    /// You asked for it, so `upgrade.plugin` set to `off`,
    /// `PLAYBOOK_SKIP_PLUGIN_UPDATE` and a CI environment do not stop it.
    /// Prints one line per step. A second run reports every step as done and
    /// changes nothing. Exits 1 when `claude` is not on PATH or a step fails.
    ///
    /// Restart Claude Code or run /reload-plugins after an install.
    ///
    /// Example: `playbook plugin install`
    Install {
        /// Print one JSON object in the shared envelope, and exit 0 whatever
        /// the result
        #[arg(long)]
        json: bool,
    },
    /// Upgrade only the playbook plugin and its marketplace
    ///
    /// Refreshes the marketplace, then runs `claude plugin update` for every
    /// installed playbook plugin id, `playbook@amplifiedengineer` and the old
    /// `playbook@pragmatic-engineer`. You asked for it, so `upgrade.plugin` set
    /// to `off`, `PLAYBOOK_SKIP_PLUGIN_UPDATE` and a CI environment do not
    /// stop it. Prints one line per plugin. Exits 1 when an upgrade fails,
    /// `claude` is not on PATH, or the marketplace refresh failed while the
    /// plugin lags; an up to date plugin, one the marketplace has not
    /// published yet and a missing plugin exit 0.
    ///
    /// Restart Claude Code or run /reload-plugins after an upgrade.
    ///
    /// Example: `playbook plugin upgrade`
    Upgrade {
        /// Print one JSON object in the shared envelope, and exit 0 whatever
        /// the result
        #[arg(long)]
        json: bool,
    },
}

/// `playbook mode` subcommands.
#[derive(Subcommand, Debug)]
pub enum ModeCommand {
    /// Turn auto mode on for this repo
    Auto,
    /// Turn auto mode off for this repo, so playbook asks again
    Ask,
    /// Show the current mode and where it came from
    ///
    /// Example: `playbook mode status --json --command implement --args "--auto"`
    Status {
        /// Print one JSON object instead of a line of text
        #[arg(long)]
        json: bool,
        /// Report as if a command ran with `--auto` or `--ask`, and warn when the hooks would disagree
        #[arg(long, value_enum)]
        flag: Option<ModeArg>,
        /// Add the effort ceiling for this command, such as `implement`, to the JSON
        #[arg(long, requires = "json", conflicts_with = "flag")]
        command: Option<String>,
        /// Raw arguments of a command, as one string. Its `--auto` or `--ask` sets the mode
        #[arg(
            long,
            requires = "json",
            conflicts_with = "flag",
            default_value = "",
            allow_hyphen_values = true
        )]
        args: String,
    },
}

/// A mode as typed on the command line.
#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum ModeArg {
    Ask,
    Auto,
}

/// `playbook handoff` subcommands.
#[derive(Subcommand, Debug)]
pub enum HandoffCommand {
    /// Save the handoff note read from stdin for the next session
    Save {
        /// Directory the handoff belongs to (default: the current one)
        #[arg(long)]
        dir: Option<String>,
        /// Session id the handoff is for. Only the start that follows that
        /// session's `/clear` loads it, so parallel sessions never share one
        #[arg(long)]
        session: Option<String>,
    },
    /// Print this directory's handoff without using it up
    Show {
        /// Print every waiting handoff, not just the newest
        #[arg(long)]
        all: bool,
        /// Directory to show (default: the current one)
        #[arg(long)]
        dir: Option<String>,
    },
    /// Show recent session starts and this directory's handoff counts
    Status,
    /// Print the rules and template for writing a handoff
    Template,
    /// Print the git facts for the "Where we are now" section
    Facts {
        /// Directory to read (default: the current one)
        #[arg(long)]
        dir: Option<String>,
    },
    /// Check a handoff read from stdin for section shape and dashes
    Check,
}

/// Which storage directory `playbook path` prints. All but `data` sit under
/// `~/.config/playbook/repos/<owner>/<repo>/`, the same from every worktree.
#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum PathKind {
    Plans,
    Implement,
    Worktrees,
    /// Where session handoffs for this repo are saved
    Handoff,
    /// The shared data directory holding every database (not tied to a repo)
    Data,
    /// The installed playbook plugin directory, for skills and agents that need
    /// a plugin file (not tied to a repo)
    PluginRoot,
}

impl PathKind {
    /// The exact subdirectory name under the repo folder.
    pub fn dir_name(&self) -> &'static str {
        match self {
            PathKind::Plans => "plans",
            PathKind::Implement => "implement",
            PathKind::Worktrees => "worktrees",
            PathKind::Handoff => "handoff",
            PathKind::Data => "data",
            PathKind::PluginRoot => "plugin-root",
        }
    }
}

/// `playbook settings` subcommands.
#[derive(Subcommand, Debug)]
pub enum SettingsCommand {
    /// Create the shared settings template from a live settings.json
    ///
    /// Takes the hooks and status line from the live file and keeps the
    /// permissions of the current seed. Prints the new seed.
    ///
    /// Example: `playbook settings gen ~/.claude/settings.json seed/shared-settings.json`
    Gen {
        /// Live settings.json to start from
        src: PathBuf,
        /// Current seed, whose permissions block is kept
        seed: PathBuf,
    },
    /// Check the shared settings template is valid
    ///
    /// Exits 0 when valid and 1 with a message when not.
    ///
    /// Example: `playbook settings check seed/shared-settings.json .`
    Check {
        /// Shared settings template to check
        template: PathBuf,
        /// Repo root that every hook command must resolve inside
        repo_root: PathBuf,
    },
}

/// `playbook memory claude` subcommands.
#[derive(Subcommand, Debug)]
pub enum MemoryClaudeCommand {
    /// Copy notes from Claude Code's auto memory into playbook memory
    ///
    /// One way and opt in. It only reads Claude Code's memory for this
    /// project (`~/.claude/projects/<project>/memory/*.md`) and never changes,
    /// moves or deletes anything there. Each note becomes a playbook fact in
    /// this repo's project memory. A note already imported (tracked by content
    /// hash on the playbook side) and a fact that already exists are skipped,
    /// so running it again copies nothing.
    ///
    /// Example: `playbook memory claude import --dry-run`
    Import {
        /// Show what would be copied and write nothing
        #[arg(long)]
        dry_run: bool,
        /// Read this directory instead of the project's Claude Code memory
        #[arg(long)]
        from: Option<PathBuf>,
    },
}

/// `playbook memory` subcommands.
#[derive(Subcommand, Debug)]
pub enum MemoryCommand {
    /// Rebuild the memory graph from every fact on disk
    ///
    /// Memory is rebuilt on its own when a fact is saved, so you only need
    /// this after editing fact files by hand. The graph is written to
    /// `~/.config/playbook/data/memory.db`; the fact files in
    /// `~/.config/playbook/memory/` stay the source of truth, so the database
    /// is safe to delete. Prints the same summary as `playbook memory stats`.
    Rebuild {
        /// Print the summary as JSON
        #[arg(long)]
        json: bool,
    },
    /// Summarize the memory graph without changing it
    ///
    /// Prints the database path, size and last rebuild time, the fact count
    /// per scope (global, org, project), the node count, the edge count per
    /// relation, the anchor count, and the dangling edges (target missing)
    /// and anchors (path missing on disk). Anchors are checked only for the
    /// repo you run it in, when it has an `origin` remote. Read only: a
    /// missing database is reported, not built.
    ///
    /// Example: `playbook memory stats --json`
    Stats {
        /// Print the summary as JSON
        #[arg(long)]
        json: bool,
    },
    /// Print the memory graph as JSON
    ///
    /// The graph lives in `~/.config/playbook/data/memory.db`, rebuilt from
    /// the fact files. This prints the document older versions kept in
    /// `memory.graph.json`, for tools that read that shape. A missing database
    /// is rebuilt first.
    ///
    /// Example: `playbook memory export --json`
    Export {
        /// Print JSON (the only format)
        #[arg(long)]
        json: bool,
    },
    /// Work with Claude Code's auto memory
    Claude {
        #[command(subcommand)]
        sub: MemoryClaudeCommand,
    },
    /// Print the repo's slice of the memory graph as markdown
    ///
    /// Shows the facts in scope, typed edges and the anchor index. When there
    /// is nothing to show it prints one `Memory context: none (<reason>)`
    /// line and exits 0. A missing default graph is rebuilt from the fact
    /// files first.
    Context {
        /// Repo slug (`owner/name`); defaults to the origin remote's slug.
        #[arg(long)]
        repo: Option<String>,
        /// Graph JSON file; defaults to the graph in `~/.config/playbook/data/memory.db`.
        #[arg(long)]
        graph: Option<PathBuf>,
    },
}

/// `playbook manifest` subcommands.
#[derive(Subcommand, Debug)]
pub enum ManifestCommand {
    /// Check every tracked file is at an allowed top-level path
    ///
    /// Exits 0 when all pass and 1 otherwise.
    Check {
        /// Repo root to check (default: the repo you are in)
        repo_root: Option<PathBuf>,
    },
}

/// `playbook agents` subcommands.
#[derive(Subcommand, Debug)]
pub enum AgentsCommand {
    /// Check every agent definition follows the agent rules
    ///
    /// Exits 0 when all pass and 1 otherwise.
    Check {
        /// Directory of agent definitions. Defaults to `plugins/playbook/agents` in the repo, else `agents` at the repo root
        agents_dir: Option<PathBuf>,
    },
}

/// `playbook sanitize` subcommands.
#[derive(Subcommand, Debug)]
pub enum SanitizeCommand {
    /// Clean a commit message file, such as git's `.git/COMMIT_EDITMSG`
    ///
    /// Example: `playbook sanitize commit .git/COMMIT_EDITMSG`
    Commit {
        /// File holding the message
        file: PathBuf,
        /// Report what would be removed, without writing. Exits 1 if anything would be removed, and 2 if FILE cannot be read
        #[arg(long)]
        check: bool,
    },
    /// Clean a PR title or body saved to a file
    ///
    /// Example: `playbook sanitize pr /tmp/body.md`
    Pr {
        /// File holding the text
        file: PathBuf,
        /// Report what would be removed, without writing. Exits 1 if anything would be removed, and 2 if FILE cannot be read
        #[arg(long)]
        check: bool,
    },
}

/// `playbook pr` subcommands.
#[derive(Subcommand, Debug)]
pub enum PrCommand {
    /// Run pre-flight checks and gather what a PR draft needs
    ///
    /// Finds the branch and base, checks the repo is ready, writes the diff
    /// to a scratch file, and prints labeled lines for the PR drafter.
    ///
    /// Example: `playbook pr prepare --base main`
    Prepare {
        /// Base branch for the PR (default: the repo's default branch)
        #[arg(long)]
        base: Option<String>,
        /// Ticket id to cite (default: one found in the branch name)
        #[arg(long)]
        ticket: Option<String>,
        /// Work in this directory instead of the current one
        #[arg(long)]
        dir: Option<String>,
        /// Print one JSON report instead of labeled lines
        #[arg(long)]
        json: bool,
        /// Read the command's flags and the worktree phrase from stdin
        #[arg(long, requires = "json")]
        args_stdin: bool,
    },
    /// Commit, push and open or update a PR from any worktree
    ///
    /// Stages per flag, commits when something is staged (message from
    /// `--message` or `--message-file`), rebases onto the base, pushes, then
    /// opens or updates the PR through `gh`. Opens a draft unless the
    /// `pr.draft` setting is `false`. Refuses the default or a protected
    /// branch and a detached HEAD, never skips hooks or signing, and only
    /// force-pushes with a lease after its own rebase. Exit codes: 0
    /// delivered, 3 refused, 4 rebase conflict, 5 a git or gh call failed, 6
    /// PR closed or a required check failed, 7 wait timed out, 8 auto-merge
    /// did not queue, 9 nothing to do.
    ///
    /// Example: `playbook pr create --dir ../wt -A --message "fix(x): y" --title "fix(x): y" --ready`
    Create {
        /// Worktree to deliver from (default: the current directory)
        #[arg(long)]
        dir: Option<String>,
        /// Stage everything first (`git add -A`)
        #[arg(short = 'A', conflicts_with_all = ["update", "staged"])]
        all: bool,
        /// Stage tracked changes first (`git add -u`)
        #[arg(short = 'u', conflicts_with = "staged")]
        update: bool,
        /// Commit only what is already staged (the default)
        #[arg(long)]
        staged: bool,
        /// Commit message
        #[arg(long, conflicts_with = "message_file")]
        message: Option<String>,
        /// File with the commit message
        #[arg(long)]
        message_file: Option<String>,
        /// PR title
        #[arg(long)]
        title: Option<String>,
        /// PR body
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// File with the PR body
        #[arg(long)]
        body_file: Option<String>,
        /// Base branch (default: the open PR's base, a parent PR branch, or the default branch)
        #[arg(long)]
        base: Option<String>,
        /// Open or mark the PR ready for review
        #[arg(long, conflicts_with = "draft")]
        ready: bool,
        /// Open or return the PR as a draft
        #[arg(long)]
        draft: bool,
        /// Queue auto-merge and verify it queued
        #[arg(long)]
        auto_merge: bool,
        /// Wait until the PR merges, closes, or a required check fails
        #[arg(long)]
        wait: bool,
        /// Seconds to wait with `--wait`
        #[arg(long, default_value_t = 1800, requires = "wait")]
        timeout: u64,
        /// Print the plan and change nothing
        #[arg(long)]
        dry_run: bool,
        /// Ticket key for the branch (`none` turns the ticket rules off)
        #[arg(long)]
        ticket: Option<String>,
        /// GitHub issue number the PR handles
        #[arg(long)]
        issue: Option<String>,
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
        /// Refuse unless `--dir` is on this branch
        #[arg(long)]
        branch: Option<String>,
        /// Auto mode: never force a push; a branch that needs a force is parked
        #[arg(long)]
        auto: bool,
    },
    /// Read what the finish flow needs to know about a PR, in one call
    ///
    /// One call for the finish flow. With `--json` it prints the shared
    /// envelope (`ok`, `code`, `meaning`, `error`) around the report and
    /// exits 0 even on failure: read `ok` and `code`. A usage error still
    /// exits 2. Without `--json` it prints the same report as text and exits
    /// with the code.
    ///
    /// Example: `playbook pr status 12 --json`
    Status {
        /// PR number
        pr: u64,
        /// Print the JSON envelope and exit 0
        #[arg(long)]
        json: bool,
        /// Treat the run mode as this one, as `mode status --flag` does
        #[arg(long, value_enum)]
        flag: Option<ModeArg>,
        /// Work in this directory instead of the current one
        #[arg(long)]
        dir: Option<String>,
    },
    /// Review a PR
    Review {
        #[command(subcommand)]
        sub: PrReviewCommand,
    },
    /// Cut the skill rules a PR title and body need
    ///
    /// Reads the writing-style and engineering-standards skills from the
    /// plugin root (`playbook path plugin-root` prints it), writes four files
    /// under `/tmp`, checks each, and
    /// prints their paths. A failed check prints `ERROR: ...` and exits 1.
    /// With `--commit` it writes only the commit-message slice of writing-style.
    /// When the plugin lacks one of the files, the copies built into this
    /// binary are used and stderr says so.
    Rules {
        /// Plugin root (default: `playbook path plugin-root`, else the built-in copies)
        #[arg(long)]
        plugin_root: Option<PathBuf>,
        /// Write only the slice a commit message needs
        #[arg(long)]
        commit: bool,
    },
    /// Resolve a PR and fetch its review threads and comments
    ///
    /// Takes the command's arguments as one string (`#12 --bots --dry-run -y`).
    /// Prints the PR, head SHA, your login and the flags, and writes the
    /// threads and issue comments as JSON under `/tmp`.
    Comments {
        /// Command arguments as one string, such as `#12 --bots`
        #[arg(default_value = "")]
        args: String,
    },
    /// Post one reply on a PR, from a file
    ///
    /// `--thread ID` replies in a review thread (ID is the `databaseId` of the
    /// thread's first comment, as `pr comments` writes it); `--issue` comments
    /// on the PR itself. The body is read from `--body-file`, so
    /// quotes, backticks and newlines need no shell escaping. It refuses an
    /// empty body, an em or en dash, and AI attribution. Prints `Posted:
    /// <url>` and the body.
    ///
    /// Example: `playbook pr reply --pr 12 --thread 345 --body-file /tmp/reply.md`
    Reply {
        /// PR number
        #[arg(long)]
        pr: String,
        /// Reply in the review thread whose first comment has this `databaseId`
        #[arg(long, conflicts_with = "issue", required_unless_present = "issue")]
        thread: Option<String>,
        /// Comment on the PR itself
        #[arg(long)]
        issue: bool,
        /// File holding the reply text
        #[arg(long)]
        body_file: PathBuf,
        /// `owner/name`; defaults to the current repo
        #[arg(long)]
        repo: Option<String>,
    },
    /// Find the PRs stacked with a PR
    ///
    /// Reads the GitHub stack API, then falls back to the branch chain. Takes
    /// the arguments as one string (`#12 --json --max-prs 6 --max-lines 3000`).
    /// Prints a summary, or JSON with `--json`. `--context` writes one shared
    /// review context under `/tmp/pr-stack-<pr>`. `--digest` prints what the
    /// open PRs above the PR change, per file. `map --stack <file> --findings
    /// <file>` gives each finding to the PR whose diff holds the line and lists
    /// the findings a PR above changes under `fixed_above`.
    Stack {
        /// Command arguments as one string, such as `#12 --json`
        #[arg(default_value = "")]
        args: String,
    },
    /// Wait for a PR's required checks to finish (deprecated)
    ///
    /// Prints a `checks total=.. pending=.. fail=.. cancel=..` line per read,
    /// then `CI_VERDICT=` one of NONE, FAIL, CANCELLED, PASS or TIMEOUT.
    /// Hidden from the command list and removed in 1.0.0: use `pr merge`.
    ///
    /// Example: `playbook pr ci-wait 12 --timeout 600`
    #[command(hide = true)]
    CiWait {
        /// PR number
        pr: String,
        /// Give up after this many seconds
        #[arg(long, default_value_t = 1200)]
        timeout: u64,
        /// Seconds between reads
        #[arg(long, default_value_t = 20)]
        interval: u64,
        /// Wait on every check, not only the required ones
        #[arg(long)]
        all: bool,
        /// Wait this many seconds before the first read, so checks register
        #[arg(long, default_value_t = 0)]
        settle: u64,
    },
    /// Wait for a PR to merge or hit a gate (deprecated)
    ///
    /// Prints a status line per read (state, merge state, review, auto-merge),
    /// then `LAND_VERDICT=` one of MERGED, REVIEW_GATE, CHANGES_REQUESTED,
    /// RESTATE or TIMEOUT. Hidden from the command list and removed in 1.0.0:
    /// use `pr merge`.
    ///
    /// Example: `playbook pr land-wait 12`
    #[command(hide = true)]
    LandWait {
        /// PR number
        pr: String,
        /// Give up after this many seconds
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
        /// Seconds between reads
        #[arg(long, default_value_t = 20)]
        interval: u64,
    },
    /// Wait for checks, arm auto-merge, and wait for the merge (deprecated)
    ///
    /// Hidden from the command list and removed in 1.0.0: use `pr merge`.
    /// Prints `LAND=<verdict>` and a JSON report. Exit codes: 0 merged, 3 a gate
    /// (draft, waiting on a base PR, head moved, review required, changes
    /// requested, needs a rebase), 6 a check failed or the PR closed, 7 still
    /// waiting (call again with `--settle 0`), 8 auto-merge did not queue. Never
    /// uses `--admin` and never deletes the branch.
    ///
    /// Example: `playbook pr land 12`
    #[command(hide = true)]
    Land {
        /// PR number
        pr: String,
        /// Give up after this many seconds in total
        #[arg(long, default_value_t = 480)]
        timeout: u64,
        /// Seconds between reads
        #[arg(long, default_value_t = 20)]
        interval: u64,
        /// Seconds to wait for the first checks to appear (use 0 on a repeat call)
        #[arg(long, default_value_t = 120)]
        settle: u64,
        /// Work in this directory instead of the current one
        #[arg(long)]
        dir: Option<String>,
    },
    /// Merge a PR: wait for checks, arm auto-merge, wait for the merge
    ///
    /// Prints `MERGE=<verdict> reason=<why>` and then the report. Verdicts:
    /// MERGED, ARMED, GATE, FAILED, CLOSED, TIMEOUT, NOT_QUEUED. Exit codes: 0
    /// merged or armed, 3 a gate (draft, waiting on a base PR, head moved,
    /// review required, changes requested, needs a rebase), 5 `gh` failed, 6 the
    /// PR closed or a check failed, 7 still waiting (call again with
    /// `--settle 0`), 8 auto-merge did not queue. With `--json` it prints the
    /// envelope with `verdict`, `reason` and `next` (done, wait, rebase, fix-ci
    /// or ask) and exits 0. `--no-wait` and `--admin` skip the checks and the
    /// wait: they print `merge_rc=<code>` (or `admin_rc=`) and what `gh` said,
    /// and exit 0. Never deletes the branch.
    ///
    /// A PR stacked on another PR is gated bottom up (each unmerged PR must
    /// pass the same gate, and a stop names it as `blocked_by=#<n>`), then the
    /// whole stack is queued with the merge-async call pinned to the checked
    /// head, and the call waits until every PR has merged. `--no-wait` returns
    /// ARMED after the queue call. `--admin` is a usage error on a stack.
    ///
    /// Example: `playbook pr merge 12`
    #[command(group(ArgGroup::new("direct").args(["no_wait", "admin"]).multiple(true)))]
    Merge {
        /// PR number
        pr: String,
        /// Arm auto-merge now and return, without waiting for checks or the merge
        #[arg(long)]
        no_wait: bool,
        /// Squash with admin rights instead of arming auto-merge
        #[arg(long)]
        admin: bool,
        /// With --no-wait or --admin, refuse unless the PR head is this commit
        #[arg(long, requires = "direct")]
        match_head: Option<String>,
        /// Give up after this many seconds in total
        #[arg(long, default_value_t = 480)]
        timeout: u64,
        /// Seconds between reads
        #[arg(long, default_value_t = 20)]
        interval: u64,
        /// Seconds to wait for the first checks to appear (use 0 on a repeat call)
        #[arg(long, default_value_t = 120)]
        settle: u64,
        /// Work in this directory instead of the current one
        #[arg(long)]
        dir: Option<String>,
        /// Print the envelope with verdict, reason and next, and exit 0
        #[arg(long)]
        json: bool,
    },
    /// List a PR's checks, optionally waiting, and triage the failed jobs
    ///
    /// Prints `CHECKS=<verdict>` (PASS, NONE, FAIL, CANCELLED, PENDING, TIMEOUT
    /// or UNREADABLE), a line per check, and per failed job its run id, failed step
    /// and whether it is transient (cancelled, or a failed `Set up job` step:
    /// rerun it instead of fixing code). `--watch` polls until the checks
    /// settle or `--timeout`; with `--fail-fast` it stops at the first failure.
    /// Exit codes: 0 all passed or none exist, 5 the PR could not be read, 6 a
    /// check failed or was cancelled, 7 still pending. With `--json` it prints the envelope with
    /// `checks[]` and `failed[]` (`name`, `run_id`, `step`, `transient`) and
    /// exits 0.
    ///
    /// Example: `playbook pr checks 12 --watch --fail-fast`
    Checks {
        /// PR number
        pr: String,
        /// Wait until the checks settle or the timeout passes
        #[arg(long)]
        watch: bool,
        /// With --watch, stop at the first failed check
        #[arg(long, requires = "watch")]
        fail_fast: bool,
        /// Only the checks the base branch requires
        #[arg(long)]
        required: bool,
        /// With --watch, give up after this many seconds
        #[arg(long, default_value_t = 1200)]
        timeout: u64,
        /// With --watch, seconds between reads
        #[arg(long, default_value_t = 20)]
        interval: u64,
        /// Print the envelope with the checks and failed jobs, and exit 0
        #[arg(long)]
        json: bool,
    },
}

/// `playbook plan` subcommands.
#[derive(Subcommand, Debug)]
pub enum PlanCommand {
    /// List the plans and ADR blueprints `/playbook:implement` can run
    ///
    /// Prints `path<TAB>[status]<TAB>title` per file, or `NO_PLANS`.
    List {
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Print a topic's checkpoint path, or save the checkpoint
    ///
    /// Without `--write` it creates the plans directory and prints the path, then
    /// `found` or `not-found`. With `--write` it replaces the checkpoint with
    /// stdin under a directory lock. A failed save prints a one-line notice and
    /// still exits 0, so planning continues without resume safety.
    ///
    /// Example: `playbook plan checkpoint my-topic`
    Checkpoint {
        /// Kebab-case topic slug, or the ticket id in lower case
        slug: String,
        /// Replace the checkpoint with stdin
        #[arg(long)]
        write: bool,
    },
    /// Remove the scratch files a finished plan leaves in the plans directory
    ///
    /// Removes exactly `<slug>.checkpoint.md`, `<slug>.checkpoint.md.lock` and
    /// `<slug>.gate-source*.md` from `playbook path plans`, never the final plan
    /// or its quality report. Prints the removed file names, one per line. Safe
    /// to repeat: nothing to remove exits 0. Exit 1 for a bad slug or flags, 2
    /// when a symlink leaves the directory or a removal fails (nothing removed
    /// after a refusal). `--stale` lists leftovers of every slug that already
    /// has a final plan and deletes them only with `--yes`.
    ///
    /// Example: `playbook plan cleanup my-topic --dry-run`
    Cleanup {
        /// Kebab-case topic slug
        #[arg(required_unless_present = "stale", conflicts_with = "stale")]
        slug: Option<String>,
        /// Only list what would be removed
        #[arg(long)]
        dry_run: bool,
        /// Print the result as JSON
        #[arg(long)]
        json: bool,
        /// Clear leftovers for every slug that already has a final plan; dry run unless `--yes`
        #[arg(long)]
        stale: bool,
        /// Only clear files at least this many days old. Needs `--stale`. By default a plan being re-planned today is left alone, because files under one day old are kept
        #[arg(long, default_value_t = 1, requires = "stale")]
        older_than: u64,
        /// With `--stale`, actually delete
        #[arg(long, requires = "stale")]
        yes: bool,
    },
    /// Write the brief file for each Work Unit
    ///
    /// `/playbook:implement` gives these files to its subagents. Each `<OUT>/<wu-id>.brief.md` holds a short header (plan, worktree,
    /// scoped verify), the Work Unit's own section of the plan verbatim, and a
    /// "Relevant memory" list of the facts anchored to its files or matching
    /// its title. With `--facts-tokens` above 0 it adds a `Callers`
    /// block of use sites from the code index, cut to that many tokens. Every id is checked first: a missing one writes nothing and
    /// exits 1. Prints the paths written, one per line.
    ///
    /// Example: `playbook plan brief docs/plan.md WU-1 WU-2 --out /tmp/briefs --verify 'cargo test {tests}'`
    Brief {
        /// Plan or blueprint file
        plan: PathBuf,
        /// Work Unit ids, such as `WU-1`
        #[arg(required = true)]
        wus: Vec<String>,
        /// Directory to write the briefs into
        #[arg(long)]
        out: PathBuf,
        /// One worktree path for every brief
        #[arg(long, conflicts_with = "worktree_base")]
        worktree: Option<String>,
        /// Directory holding one worktree per Work Unit, named by its lower case id
        #[arg(long)]
        worktree_base: Option<String>,
        /// Scoped verify command; `{tests}` and `{files}` become the Work Unit's paths
        #[arg(long, allow_hyphen_values = true)]
        verify: Option<String>,
        /// Repo slug (`owner/name`) for the memory scope; defaults to the origin remote's
        #[arg(long)]
        repo: Option<String>,
        /// Graph JSON file; defaults to the graph in `~/.config/playbook/data/memory.db`
        #[arg(long)]
        graph: Option<PathBuf>,
        /// Most tokens of code facts to add to each brief; 0 adds none
        #[arg(long, value_name = "N", default_value_t = 0)]
        facts_tokens: usize,
    },
}

/// `playbook glossary` subcommands.
#[derive(Subcommand, Debug)]
pub enum GlossaryCommand {
    /// Append one entry line to `GLOSSARY.md` at the repo root
    ///
    /// Example: `playbook glossary add "WU: Work Unit"`
    Add {
        /// Entry text to append
        #[arg(allow_hyphen_values = true)]
        entry: String,
    },
}

/// `playbook prose` subcommands.
#[derive(Subcommand, Debug)]
pub enum ProseCommand {
    /// Lint files for banned phrases, structures and em or en dashes
    ///
    /// Prints `path:line: finding` for each problem. Fenced code, inline
    /// code, URLs, file paths and quoted lines are skipped. Exits 0 unless
    /// `--strict` is given and a hard finding exists, or a file cannot be
    /// read. With `GITHUB_ACTIONS=true` it prints annotations.
    ///
    /// Example: `playbook prose check README.md`
    Check {
        /// Files to check
        #[arg(required = true)]
        files: Vec<std::path::PathBuf>,
        /// Exit 1 when a hard finding exists
        #[arg(long)]
        strict: bool,
    },
}

/// `playbook adr` subcommands.
#[derive(Subcommand, Debug)]
pub enum AdrCommand {
    /// Print the next ADR number and today's date
    ///
    /// Creates `docs/adr` in the repo if it is missing. Prints
    /// `Next number: NNNN   Date: YYYY-MM-DD`.
    Next,
}

/// `playbook atlassian` subcommands.
#[derive(Subcommand, Debug)]
pub enum AtlassianCommand {
    /// Check the `acli` version and whether `twg` is installed
    ///
    /// Prints one line for `acli` (`ok`, `too old` with the
    /// `brew upgrade atlassian/acli/acli` hint, `version unreadable` or
    /// `missing`) and one for `twg`. Exits 0 when `acli` is usable and 1
    /// otherwise, so the skill can fall back to the next route. Whether the
    /// Atlassian MCP tools are connected is left to the skill.
    Check,
}

/// `playbook learn` subcommands.
#[derive(Subcommand, Debug)]
pub enum LearnCommand {
    /// Name the repo and memory store, probe the tools, and tally JIRA keys
    Preflight,
    /// Count the git history or the repo layout
    ///
    /// Feeds the `/playbook:learn` collectors. `git` prints contributors, churn hotspots, commit types, remote branch
    /// prefixes, recent tags and commits per month. `structure` prints the
    /// top level, languages, entry points, build manifests with their scripts
    /// and targets, test and lint config, Dockerfiles, CI, infrastructure as
    /// code, migrations, model files and `scripts/`. Both print one JSON
    /// object, read only through `git`, with every count capped.
    ///
    /// Example: `playbook learn collect git structure`
    Collect {
        /// Which to collect: `git`, `structure`, or both
        #[arg(required = true, value_parser = ["git", "structure"])]
        roles: Vec<String>,
    },
}

/// `playbook commit` subcommands.
#[derive(Subcommand, Debug)]
pub enum CommitCommand {
    /// Stage, format, and print what a commit message is drafted from
    ///
    /// Refuses the repo's default branch. Prints `NO_STAGED_CHANGES` when there
    /// is nothing to commit, else `BRANCH=`, the changed files and the diff.
    ///
    /// Example: `playbook commit prepare -A`
    Prepare {
        /// Stage everything first (`git add -A`)
        #[arg(short = 'A')]
        all: bool,
        /// Stage tracked changes first (`git add -u`)
        #[arg(short = 'u')]
        update: bool,
        /// Replace the last commit
        #[arg(short = 'a')]
        amend: bool,
    },
    /// Deprecated name for `playbook commit run -A`
    ///
    /// Stages with `git add -A`, drafts a message unless `-m` is given and
    /// commits like `commit run -A`. With `--json --args-stdin` the staging
    /// comes from `-A` or `-u` in the text on stdin. Hidden from the command
    /// list and kept until 1.0.0.
    #[command(hide = true)]
    All {
        /// Commit message (default: drafted from the staged files)
        #[arg(short = 'm', long)]
        message: Option<String>,
        /// Auto mode: never force a push
        #[arg(long)]
        auto: bool,
        /// Leave the sign-off trailer off
        #[arg(long)]
        no_signoff: bool,
        /// Print one JSON report that always exits 0, the outcome in `verdict`
        #[arg(long)]
        json: bool,
        /// Read the flags and the worktree phrase from stdin; stages per `-A`/`-u`
        #[arg(long, requires = "json")]
        args_stdin: bool,
    },
    /// Commit, rebase when behind, and push
    ///
    /// Reads the message from stdin, or from `--message-file`. With `-A`,
    /// `-m` or `--json` it drafts the message from the staged files with no
    /// model instead, unless `-m` gives one, and refuses the default branch.
    /// Adds a sign-off per `commit.signOff` and signs when `user.signingkey`
    /// is set. A rejected push is never forced. Exits 1 on any failure.
    ///
    /// Example: `playbook commit run --message-file /tmp/msg.txt`
    Run {
        /// File with the commit message (default: read stdin)
        #[arg(long, conflicts_with_all = ["message", "all", "json"])]
        message_file: Option<PathBuf>,
        /// Stage everything first (`git add -A`) and draft the message
        #[arg(short = 'A')]
        all: bool,
        /// Commit message (default: drafted from the staged files)
        #[arg(short = 'm', long)]
        message: Option<String>,
        /// Print one JSON report that always exits 0, the outcome in `verdict`
        #[arg(long)]
        json: bool,
        /// Read the flags and the worktree phrase from stdin; stages per `-A`/`-u`
        #[arg(long, requires = "json")]
        args_stdin: bool,
        /// Replace the last commit
        #[arg(short = 'a')]
        amend: bool,
        /// Auto mode: never force a push
        #[arg(long)]
        auto: bool,
        /// Leave the sign-off trailer off
        #[arg(long)]
        no_signoff: bool,
        /// Skip the check that the commit type fits the files. A docs-only
        /// change is not `feat`, and a lock-file bump is not `fix`
        #[arg(long)]
        no_type_check: bool,
        /// Skip the prose check. It flags a dash, a curly quote, a banned word
        /// or a contrast phrase in the message
        #[arg(long)]
        no_prose_check: bool,
    },
}

/// `playbook review` subcommands.
#[derive(Subcommand, Debug)]
pub enum ReviewCommand {
    /// Resolve the PR, report-only or posting, and in-place or worktree
    ///
    /// Takes the raw arguments of the review command as one string. Prints
    /// `KEY=value` lines: PR, REPO, PR_NUMBER, HEAD_SHA, AUTHOR, SELF_REVIEW,
    /// SELF_MODE, MODE, WT and REVIEW_JSON. On a problem it prints
    /// `error: ...` and exits 1.
    ///
    /// Example: `playbook review prepare deep "#4265 --self"`
    Prepare {
        /// Which review: `quick` or `deep`
        #[arg(value_parser = ["quick", "deep"])]
        kind: String,
        /// Review command arguments as one string, such as `#12 --self`
        #[arg(default_value = "")]
        args: String,
        /// Accepted for compatibility; auto mode does not skip the post question
        #[arg(long)]
        auto: bool,
    },
    /// Recommend a quick or a deep review from the diff
    ///
    /// Reads the changed files from git (the branch against the default
    /// branch, no network) or from `gh` when a PR number is given, and applies
    /// a fixed rule list: size, files, packages touched, risky paths (auth,
    /// crypto, secrets, CI, release, hooks, guards, migrations, dependencies,
    /// the command surface, plugin commands, skills and agents over the small
    /// cut), a security label, the test ratio, and changes
    /// that cannot alter behavior (docs, lock files, renames, tests). Prints
    /// `recommendation=quick|deep`, `confidence=high|low`, up to four
    /// `reason:` lines, a `signals:` line, a `scope=` line and, with
    /// `--scope stack`, a `stack:` line. Low confidence means no rule
    /// was decisive; the answer then leans quick for your own change (a local
    /// branch always is) and deep otherwise. Docs and lock file lines do not
    /// count toward size, and files outside crates, packages, apps, services,
    /// libs and plugins count together as one `root` package.
    ///
    /// `--scope stack` measures every open PR in the PR's stack together:
    /// merged PRs are left out, closed PRs are skipped, and a PR outside a
    /// stack is measured alone. A stack with more than 6 open PRs
    /// is answered deep without reading their files.
    ///
    /// Hidden from the command list and removed in 1.0.0: use `pr review triage`.
    ///
    /// Example: `playbook review suggest 4265 --json`
    #[command(hide = true)]
    Suggest {
        /// PR number (default: the current branch against the default branch)
        pr: Option<String>,
        /// What to measure: this PR alone, or every open PR in its stack
        #[arg(long, value_enum, default_value = "this-pr")]
        scope: review::suggest::Scope,
        /// Base branch when no PR number is given. Defaults to the repo's default
        /// branch; pass the parent branch for a stacked change
        #[arg(long)]
        base: Option<String>,
        /// Work in this directory instead of the current one
        #[arg(long)]
        dir: Option<String>,
        /// Print JSON instead of text lines
        #[arg(long)]
        json: bool,
    },
    /// Run a project's own type check, lint and tests in a directory
    ///
    /// Prints their output. A failing check is output, not an error.
    ///
    /// Example: `playbook review checks /tmp/review-wt`
    Checks {
        /// Review worktree to run in
        dir: PathBuf,
    },
    /// Measure a diff and say which review path it takes
    ///
    /// Counts the changed lines of `BASE...HEAD` and prints
    /// `lines=N files=M path=single|swarm`. A diff of at most 150 changed
    /// lines takes the `single` path, one reviewer holding every lens; a
    /// larger one, or any run with `--all-lenses`, takes `swarm`.
    ///
    /// Example: `playbook review size --base main`
    Size {
        /// Ref the diff starts from
        #[arg(long)]
        base: String,
        /// Ref the diff ends at
        #[arg(long, default_value = "HEAD")]
        head: String,
        /// Always take the swarm path
        #[arg(long)]
        all_lenses: bool,
        /// Repo directory to measure in. Defaults to the current directory
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Work with the lens tier map that review triage returns
    Triage {
        #[command(subcommand)]
        sub: ReviewTriageCommand,
    },
}

/// `playbook review triage` subcommands.
#[derive(Subcommand, Debug)]
pub enum ReviewTriageCommand {
    /// Complete a review-triage tier map so no lens is left unclassified
    ///
    /// Reads the triage reply (a JSON object of `{lens: {tier, reason}}`) from
    /// `--tiers FILE`, or `-` for stdin, and prints the full map as JSON plus a
    /// `Triage: lens=tier, ...` line. A missing or unusable reply, a lens the
    /// reply left out, and a tier other than `skip`, `cheap-check` or
    /// `full-lens` all become `full-lens`.
    ///
    /// Example: `playbook review triage merge --lenses security,logic --tiers triage.json`
    Merge {
        /// Every lens that was sent to triage, comma separated
        #[arg(long, value_delimiter = ',', required = true)]
        lenses: Vec<String>,
        /// File holding the triage reply, or `-` for stdin; omit when triage returned nothing
        #[arg(long)]
        tiers: Option<String>,
        /// Fix a lens's tier whatever triage said, as `lens=tier:reason` (repeatable)
        #[arg(long = "force")]
        forced: Vec<String>,
    },
}

/// `playbook pr review` subcommands.
#[derive(Subcommand, Debug)]
pub enum PrReviewCommand {
    /// Choose a quick or deep self-review for a PR
    ///
    /// An empty diff or one over 60000 bytes is `deep`. Then the diff rules
    /// run (size, risky paths, docs-only and similar, no model): at `high`
    /// confidence their answer is final and prints `source=rules` with a
    /// `signal:` line per reason. Otherwise a small model is asked three
    /// times, and anything but a unanimous `quick` becomes `deep`
    /// (`source=model`). `--model` skips the rules. A rules error prints a
    /// `note:` line and falls through to the model.
    ///
    /// Example: `playbook pr review triage --pr 12 --scope stack`
    Triage {
        /// PR number (default: the current branch against its base)
        #[arg(long)]
        pr: Option<u64>,
        /// Base branch when no PR number is given (default: the repo's default branch)
        #[arg(long)]
        base: Option<String>,
        /// Work in this directory instead of the current one
        #[arg(long)]
        dir: Option<String>,
        /// What the rules measure: this PR alone, or every open PR in its stack
        #[arg(long, value_enum, default_value = "this-pr")]
        scope: review::suggest::Scope,
        /// Skip the diff rules and ask the model only
        #[arg(long)]
        model: bool,
    },
    /// Create a pending review on a PR from a findings file
    ///
    /// The findings file is JSON: `commit_id`, then `comments`, each with
    /// `path`, `line`, `side` and `body`, plus `start_line` and `start_side`
    /// for a range. A review `body` is optional; leave it out to frame the
    /// review at submit time. Creates the review pending, checks GitHub
    /// reports it `PENDING` (code 5 otherwise) and prints `id=`, `state=` and
    /// `url=`. With `--json` it prints the envelope and exits 0.
    ///
    /// Example: `playbook pr review post 12 --findings /tmp/findings.json`
    Post {
        /// PR number
        pr: String,
        /// Findings file to post as the pending review
        #[arg(long)]
        findings: PathBuf,
        /// Print the envelope with id, state and url, and exit 0
        #[arg(long)]
        json: bool,
    },
    /// Submit a pending review as a comment, an approval or a change request
    ///
    /// Sends the verdict for review `ID` and checks it left `PENDING` (code 5
    /// otherwise). An approve with no `--body` says `LGTM`. Prints `id=`,
    /// `state=` and `url=`. With `--json` it prints the envelope and exits 0.
    ///
    /// Example: `playbook pr review submit 12 99 --event request-changes`
    Submit {
        /// PR number
        pr: String,
        /// Review id, as `pr review post` printed it
        id: String,
        /// How to submit the review
        #[arg(long, value_enum)]
        event: pr::review::submit::Event,
        /// Text for the review
        #[arg(long)]
        body: Option<String>,
        /// Print the envelope with id, state and url, and exit 0
        #[arg(long)]
        json: bool,
    },
}

/// `playbook gate` subcommands.
#[derive(Subcommand, Debug)]
pub enum GateCommand {
    /// Save a phase verdict for a plan
    ///
    /// Reads a phase agent's output, finds its `VERDICT:` line, and stores it.
    ///
    /// Example: `playbook gate record my-plan plan adversarial out.md`
    Record {
        /// Plan slug the phase belongs to
        plan_slug: String,
        /// Command that produced this verdict
        command: String,
        /// Which phase this verdict is for
        phase: String,
        /// File with the phase agent's output, or "-" to read stdin
        input: String,
        /// File with the source content the verdict covers. Used to spot a stale verdict later.
        /// Defaults to the phase's snapshot, `<plans>/<slug>.gate-source.<phase>.md`
        #[arg(long)]
        source: Option<String>,
        /// Store a FAIL report as STUCK. Use it when the revise and re-run loop stopped
        /// because the blocking findings made no progress. Refused for any other verdict
        #[arg(long)]
        stuck: bool,
    },
    /// Freeze the plan draft for each gate phase about to run
    ///
    /// Copies `<plans>/<slug>.gate-source.md`, the draft as it stands, to
    /// `<plans>/<slug>.gate-source.<phase>.md` for every phase named, so each
    /// phase records its verdict against what it actually reviewed. Prints the
    /// snapshot paths. Fails, copying nothing, when the draft file is missing.
    ///
    /// Example: `playbook gate snapshot my-plan adversarial test-review`
    Snapshot {
        /// Plan slug
        plan_slug: String,
        /// Phase names to snapshot for
        #[arg(required = true)]
        phases: Vec<String>,
    },
    /// Check that recorded phase verdicts still pass
    ///
    /// Exits 0 only if every named phase is PASS or WARN and its source is
    /// unchanged. Exits 1 on any other verdict, a stale one, or no phases.
    ///
    /// Example: `playbook gate check my-plan plan adversarial test-review`
    Check {
        /// Plan slug to check
        plan_slug: String,
        /// Command running this check
        command: String,
        /// Phase names to check
        phases: Vec<String>,
        /// File with the current source content, compared against what each verdict saw.
        /// Defaults to the shared draft, `<plans>/<slug>.gate-source.md`
        #[arg(long)]
        source: Option<String>,
        /// Print one JSON object instead of text (exit codes stay the same)
        #[arg(long)]
        json: bool,
    },
}

/// `playbook config` subcommands.
#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Show a setting's value and which tier it came from
    ///
    /// Example: `playbook config get pr.review.enabled`
    Get {
        /// Dotted setting name, such as `pr.review.enabled`
        key: String,
    },
    /// Change a setting
    ///
    /// Writes to the repo tier unless you pass `--org` or `--global`. The
    /// per-user `update.*` keys go to the global tier, and `--org` is
    /// refused for them.
    ///
    /// Example: `playbook config set pr.review.enabled true`
    Set {
        /// Dotted setting name, such as `pr.review.enabled`
        key: String,
        /// New value; it is read as the type the setting expects
        value: String,
        /// Write to the org tier instead of the repo tier
        #[arg(long)]
        org: bool,
        /// Write to the global tier instead of the repo tier
        #[arg(long)]
        global: bool,
    },
    /// List every setting with its value and source tier
    List,
    /// Print every stored setting as one JSON document
    ///
    /// Settings live in a database, not hand-editable files. Export gives you
    /// a readable copy, and `config import` loads one back.
    ///
    /// Example: `playbook config export > playbook-config.json`
    Export,
    /// Load settings from a JSON document made by `config export`
    ///
    /// Every key and value is checked first, and nothing is stored if any is
    /// invalid. Use `-` to read standard input.
    ///
    /// Example: `playbook config import playbook-config.json`
    Import {
        /// JSON file to read, or `-` for standard input
        file: String,
    },
}

/// `playbook sync` subcommands. Each name that `pbsync` handles passes its
/// words and flags on untouched, `--help` included.
#[derive(Subcommand, Debug)]
pub enum SyncCommand {
    /// Turn sync on for the accounts you name
    ///
    /// Shows what will be sent and asks before it turns sync on. Pass `--url`
    /// for your Turso database, `--account` for each account to sync, and
    /// `--yes` to skip the question.
    ///
    /// Example: `playbook sync enable --url https://db.example.turso.io --account me`
    #[command(disable_help_flag = true)]
    Enable {
        /// Flags for the helper, such as `--url`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Turn sync off and keep the local data
    ///
    /// Example: `playbook sync disable`
    #[command(disable_help_flag = true)]
    Disable {
        /// Flags for the helper
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show whether sync is on and its local state
    ///
    /// Makes no network call.
    ///
    /// Example: `playbook sync status`
    #[command(disable_help_flag = true)]
    Status {
        /// Flags for the helper
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Send new usage rows to the shared database
    ///
    /// Example: `playbook sync push --force`
    #[command(disable_help_flag = true)]
    Push {
        /// Flags for the helper, such as `--force`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Copy the usage rows other machines sent
    ///
    /// Example: `playbook sync pull`
    #[command(disable_help_flag = true)]
    Pull {
        /// Flags for the helper, such as `--force`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Push, then pull, then share the settings that may sync
    ///
    /// The old `pbsync sync`.
    ///
    /// Example: `playbook sync run`
    #[command(disable_help_flag = true)]
    Run {
        /// Flags for the helper, such as `--force`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Remove this machine's rows from the shared database
    ///
    /// Clears the local copies and turns sync off. `--all` removes every
    /// machine's rows and asks first.
    ///
    /// Example: `playbook sync purge --yes`
    #[command(disable_help_flag = true)]
    Purge {
        /// Flags for the helper, such as `--all`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Share the settings that may sync, or apply what other machines sent
    ///
    /// `push` sends your shared global values and `pull` writes a proposal
    /// that changes nothing. `apply` shows the proposal next to your values
    /// and applies it when you answer yes.
    ///
    /// Example: `playbook sync config apply`
    #[command(
        args_conflicts_with_subcommands = true,
        disable_help_flag = true,
        disable_help_subcommand = true
    )]
    Config {
        #[command(subcommand)]
        sub: Option<SyncConfigCommand>,
        /// Words for the helper when no subcommand matches
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// `playbook sync config` subcommands.
#[derive(Subcommand, Debug)]
pub enum SyncConfigCommand {
    /// Send the shared global setting values
    ///
    /// Refreshes the key list and values files first.
    ///
    /// Example: `playbook sync config push`
    #[command(disable_help_flag = true)]
    Push {
        /// Flags for the helper
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Write the settings other machines sent as a proposal
    ///
    /// Changes no setting. Run `playbook sync config apply` to review it.
    ///
    /// Example: `playbook sync config pull`
    #[command(disable_help_flag = true)]
    Pull {
        /// Flags for the helper
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Apply the settings that `playbook sync config pull` proposed
    ///
    /// Shows each proposed value next to your current global value and asks
    /// before it stores anything. Only keys that may sync are accepted, and
    /// each value is checked like `config set`. A project value still wins
    /// over the global one. Nothing changes unless you answer yes or pass
    /// `--yes`.
    ///
    /// Example: `playbook sync config apply --yes`
    Apply {
        /// Apply without asking
        #[arg(long)]
        yes: bool,
        /// Print help
        #[arg(short, long, action = clap::ArgAction::Help)]
        help: Option<bool>,
    },
    /// Refresh the key list and values files that `pbsync` reads
    ///
    /// Writes `sync-keys.json` and `config-values.json` in the data directory
    /// that holds `usage.db`. They hold only the keys allowed to sync between
    /// your machines and their global values. `config set --global` refreshes
    /// them for a shared key, so you rarely need this.
    ///
    /// Example: `playbook sync config refresh`
    #[command(hide = true)]
    Refresh {
        /// Print help
        #[arg(short, long, action = clap::ArgAction::Help)]
        help: Option<bool>,
    },
}

/// `playbook doctor` subcommands.
#[derive(Subcommand, Debug)]
pub enum DoctorCommand {
    /// Check the seven playbook layers and print a status table
    ///
    /// Reads your settings, shell files and PATH, and lists a PASS, INFO, WARN or
    /// FAIL row for each layer, then the review config, stale worktrees and
    /// pending migration notices. Always exits 0 when it could run the checks.
    Check {
        /// Print the rows as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show the model tiers, any overrides and the fallback chain in use
    ///
    /// Also says whether your own fallback replaces playbook's, and when the
    /// effort in force makes the chain start at Haiku.
    Models {
        /// Print the report as JSON
        #[arg(long)]
        json: bool,
    },
    /// List the manual migration findings that need you
    ///
    /// Prints one line per finding, or nothing when none is pending.
    Migrations,
}

/// `playbook doc` subcommands.
#[derive(Subcommand, Debug)]
pub enum DocCommand {
    /// Print a document as Markdown, framed as untrusted content
    ///
    /// Supports pdf, docx, doc, odt, rtf, epub, pptx, ppt, odp, xlsx, ods
    /// and csv. A scanned PDF with no text layer reports that it needs OCR.
    ///
    /// Example: `playbook doc read report.pdf --section 2`
    Read {
        /// Document to read
        file: PathBuf,
        /// Print one JSON object instead of the framed text
        #[arg(long)]
        json: bool,
        /// Character offset to start the slice at (default 0)
        #[arg(long)]
        offset: Option<usize>,
        /// Most characters to print, capped at the display size
        #[arg(long)]
        limit: Option<usize>,
        /// Print one section by its number from the section index
        #[arg(long)]
        section: Option<usize>,
    },
    /// Say which document converter is in use, or that none was found
    Status,
}

/// `playbook tools` subcommands.
#[derive(Subcommand, Debug)]
pub enum ToolsCommand {
    /// Print the map from each task to the command that does it. The session
    /// start hook injects this text, so skills can ask for a task by name.
    Map {
        /// Print `[{"intent", "command"}]` as JSON
        #[arg(long, conflicts_with = "check")]
        json: bool,
        /// Exit 1 when the map costs more than its token budget
        #[arg(long)]
        check: bool,
    },
}

/// `playbook deps` subcommands.
#[derive(Subcommand, Debug)]
pub enum DepsCommand {
    /// Install missing Brewfile tools with Homebrew
    ///
    /// Leaves each tool already on PATH alone, and reads the `brew` and `tap`
    /// lines of the Brewfile.
    ///
    /// Example: `playbook deps ensure --builtin`
    Ensure {
        /// Brewfile to read, `Brewfile` in the current directory by default
        #[arg(conflicts_with = "builtin")]
        brewfile: Option<PathBuf>,
        /// Read the Brewfile built into this binary instead of a file
        #[arg(long)]
        builtin: bool,
    },
}

/// `playbook sessions` subcommands.
#[derive(Subcommand, Debug)]
pub enum SessionsCommand {
    /// List recent sessions (the default)
    List {
        /// Print the sessions as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show what a sweep would close for each finished session
    ///
    /// Judges each finished session's worktree with the same guards as
    /// `playbook worktree remove`: landed, clean, unlocked and not in use. A
    /// stale session is listed but not swept. Only the dry run exists: it
    /// removes nothing, stops no process and closes no tab or pane.
    ///
    /// Example: `playbook sessions sweep --dry-run`
    Sweep {
        /// Report only and change nothing (required)
        #[arg(long, required = true)]
        dry_run: bool,
    },
}

/// `playbook watch` subcommands.
#[derive(Subcommand, Debug)]
pub enum WatchCommand {
    /// Poll every repo once and run the items you approved
    ///
    /// A repo watched for the first time starts from now, so turning watch
    /// mode on never replays old issues. Prints `watch already running` and
    /// exits when another watch process holds the lock. With `--dry-run` it
    /// shows the rules and what a poll would propose, and changes nothing.
    ///
    /// Example: `playbook watch once --since 2026-11-01T00:00:00Z`
    Once {
        /// Replay events from this UTC time, for example 2026-11-01T00:00:00Z
        #[arg(long)]
        since: Option<String>,
        /// Show the rules and what a poll would propose, and change nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Carry out a proposal
    ///
    /// Runs the action now. When another watch process is running it only
    /// marks the item and that process carries it out.
    ///
    /// Example: `playbook watch approve 3`
    Approve {
        /// Proposal number printed by `watch once`
        id: i64,
    },
    /// Drop a proposal without acting on it
    ///
    /// Example: `playbook watch reject 3`
    Reject {
        /// Proposal number printed by `watch once`
        id: i64,
    },
    /// Show whether watch runs, the queue, the limits and the dead list
    ///
    /// Example: `playbook watch status --json`
    Status {
        /// Print the status as JSON
        #[arg(long)]
        json: bool,
    },
    /// Stop starting new actions
    ///
    /// An action that is already running finishes. Proposals still queue.
    ///
    /// Example: `playbook watch pause`
    Pause,
    /// Start new actions again after a pause
    ///
    /// Example: `playbook watch resume`
    Resume,
    /// Put a dead item back in the queue
    ///
    /// Example: `playbook watch retry 3`
    Retry {
        /// Number of the dead item, shown by `watch status`
        id: i64,
    },
    /// Print the audit log, oldest first
    ///
    /// Example: `playbook watch log --rule triage --outcome done`
    Log {
        /// Only records of this rule
        #[arg(long)]
        rule: Option<String>,
        /// Only records with this outcome, such as `done` or `retry`
        #[arg(long)]
        outcome: Option<String>,
        /// Print each record as a JSON line
        #[arg(long)]
        json: bool,
    },
    /// Install or remove the timer that runs `watch once`
    Service {
        #[command(subcommand)]
        sub: WatchServiceCommand,
    },
}

/// `playbook watch service` subcommands.
#[derive(Subcommand, Debug)]
pub enum WatchServiceCommand {
    /// Install a timer that runs `watch once` every 60 seconds
    ///
    /// A launchd agent on macOS and a systemd user timer on Linux. No process
    /// stays running between polls. The timer finds the GitHub token through
    /// `gh auth token`, so run `gh auth login` first.
    ///
    /// Example: `playbook watch service install`
    Install,
    /// Remove the timer that `service install` made
    ///
    /// The audit log stays unless you ask for it to go.
    ///
    /// Example: `playbook watch service uninstall --delete-log`
    Uninstall {
        /// Also delete the audit log
        #[arg(long)]
        delete_log: bool,
    },
}

/// `playbook worktree` subcommands.
#[derive(Subcommand, Debug)]
pub enum WorktreeCommand {
    /// Remove every landed worktree that is not in use
    ///
    /// Scans all worktrees of this repo, whichever tool made them. Keeps
    /// locked ones unless the locking process is dead.
    Sweep {
        /// Show what would be removed without removing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove one worktree if it has landed and is not in use
    ///
    /// Example: `playbook worktree remove ../.worktrees/my-repo/my-branch`
    Remove {
        /// Worktree path, as shown by `git worktree list`
        path: PathBuf,
    },
    /// Create or remove the locked review worktrees
    ///
    /// `/playbook:review --quick` and `/playbook:review --deep` run in them.
    Review {
        #[command(subcommand)]
        sub: ReviewWorktreeCommand,
    },
}

/// `playbook worktree review` subcommands.
#[derive(Subcommand, Debug)]
pub enum ReviewWorktreeCommand {
    /// Add a locked worktree for a PR head and print its path
    ///
    /// Fetches the PR head, adds a locked detached worktree for it, and
    /// prints its absolute path on stdout. Warnings go to stderr.
    ///
    /// Example: `playbook worktree review setup 12 0123456789abcdef0123456789abcdef01234567`
    Setup {
        /// PR number
        pr: String,
        /// Full head commit sha the review was resolved against
        head_sha: String,
    },
    /// Remove a review worktree, even a dirty one
    ///
    /// Always exits 0, and skips any path outside the review worktrees.
    ///
    /// Example: `playbook worktree review teardown .git/review-worktrees/12-0123456`
    Teardown {
        /// Worktree path printed by `setup`
        path: PathBuf,
    },
}

// Kebab-case on the CLI, so a typo in the hook settings fails loudly.
/// Every hook Claude Code can run.
#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum HookName {
    SessionInit,
    PrereadEditCheck,
    PrereadSizeCheck,
    SearchCounter,
    MemoryAnchors,
    PostEditTrack,
    RebuildMemoryGraph,
    AutoModelDetect,
    PrecompactWarn,
    SessionCleanExit,
    MemoryCapture,
    RmWorkspaceGuard,
    BgAwaitGuard,
    /// Retired: kept as a no-op for one release so old settings entries still run.
    #[value(hide = true)]
    NoSlopGuard,
    CommitMessageSanitizer,
    PolicyGuard,
    PrecommitCheck,
    AutoGuard,
    AutoCost,
    WorktreeCreate,
    WorktreeRemove,
    ContextHygiene,
    DocReadHint,
    UsageWarm,
    HandoffOnClear,
    SessionActivity,
    /// Retired with the disk guard. A `settings.json` entry written by
    /// 0.38.0 still runs it until `playbook setup --hooks` removes the
    /// entry, so it exits 0 and does nothing.
    // Keep this variant while RETIRED_HOOKS in init/dead_hooks.rs lists the
    // entry; without it clap rejects the stale command with a non-zero exit
    // on every call. A plain comment, because doc text becomes help text.
    #[value(hide = true)]
    DiskGuard,
    /// Kept so a playbook-hooks plugin cached on disk from an older release
    /// still exits 0 on SessionStart. Does nothing. Remove it once those
    /// caches have aged out.
    #[value(hide = true)]
    MigrationCheck,
}

/// `playbook hooks` subcommands.
#[derive(Subcommand, Debug)]
pub enum HooksCommand {
    /// Print every hook with its event, matcher and what it does
    ///
    /// Lists the hooks setup adds to settings.json, grouped by purpose.
    /// Changes nothing.
    List,
}

/// `playbook cc cache` subcommands.
#[derive(Subcommand, Debug)]
pub enum CcCacheCommand {
    /// Clear caches that would freeze old settings into a session
    Clear,
}

/// `playbook cc` subcommands.
#[derive(Subcommand, Debug)]
pub enum CcCommand {
    /// Clean up and resume a session
    ///
    /// Clones the session named for this directory with its settings overrides
    /// stripped, then resumes the clone in `claude`, so settings, plugins and
    /// hooks reload from the current config. Starts a fresh session when none
    /// matches. The same as `playbook cc --clean`.
    Clean {
        /// Arguments passed through to claude
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Start a fresh session
    ///
    /// Starts a new `claude` session without resuming an earlier one, with the
    /// current settings applied. The same as `playbook cc --fresh`.
    Fresh {
        /// Arguments passed through to claude
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Resume a session as it was
    ///
    /// Resumes a session in `claude` as it was, with no fork and its settings
    /// overrides kept. Starts a fresh session when none matches. The same as
    /// `playbook cc --raw`.
    Raw {
        /// Session id to resume (default: the most recent), then arguments for claude
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List sessions for the current project
    #[command(alias = "ls")]
    List,
    /// Remove stale runtime state
    Prune,
    /// Work with the caches that would freeze old settings into a session
    Cache {
        #[command(subcommand)]
        sub: CcCacheCommand,
    },
    /// Run the launcher (`playbook cc` and `playbook ccd` call it)
    Launch {
        /// Pass `--dangerously-skip-permissions` to claude (the `playbook ccd` form)
        #[arg(long)]
        skip_permissions: bool,
        /// Subcommand and arguments, passed through to claude
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Background half of the worktree launch, spawned by the launcher
    #[command(hide = true)]
    Housekeep {
        /// Path of the repo root
        #[arg(long)]
        repo_root: String,
        /// Path of the worktree to tidy
        #[arg(long)]
        worktree: String,
        /// Branch of the worktree
        #[arg(long)]
        branch: String,
        /// Skip pushing the branch
        #[arg(long)]
        no_push: bool,
    },
    /// Create a git worktree for a branch and print its path
    ///
    /// Prints only the path, so a shell can run `cd "$(playbook cc worktree my-branch)"`.
    ///
    /// Example: `playbook cc worktree my-branch`
    #[command(alias = "new")]
    Worktree {
        /// Branch to create the worktree for
        branch: String,
        /// Directory (relative to the repo root) holding the `.env` to copy in
        env_base: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn completions_is_a_hot_path_command() {
        // Arrange
        let cli = Cli::try_parse_from(["playbook", "completions", "bash"]).unwrap();

        // Act
        let hot = cli.command.is_hot_path();

        // Assert
        assert!(hot);
    }

    #[test]
    fn version_flag_prints_cargo_toml_version() {
        // Arrange
        let expected = env!("CARGO_PKG_VERSION");

        // Act
        let result = Cli::command().try_get_matches_from(["playbook", "--version"]);

        // Assert
        let err =
            result.expect_err("--version should short-circuit parsing with a version message");
        assert!(err.to_string().contains(expected));
    }

    #[test]
    fn hook_help_lists_every_hook_name() {
        // Arrange
        let names = [
            "session-init",
            "preread-edit-check",
            "preread-size-check",
            "search-counter",
            "memory-anchors",
            "post-edit-track",
            "rebuild-memory-graph",
            "auto-model-detect",
            "auto-guard",
            "auto-cost",
            "precompact-warn",
            "session-clean-exit",
            "memory-capture",
            "context-hygiene",
            "doc-read-hint",
            "usage-warm",
            "handoff-on-clear",
            "session-activity",
            "rm-workspace-guard",
            "bg-await-guard",
            "precommit-check",
            "commit-message-sanitizer",
            "policy-guard",
        ];

        // Act
        let result = Cli::command().try_get_matches_from(["playbook", "hook", "--help"]);

        // Assert
        let err = result.expect_err("--help should short-circuit parsing with a help message");
        let help = err.to_string();
        for name in names {
            assert!(help.contains(name), "hook --help is missing '{name}'");
        }
        assert!(
            !help.contains("no-slop-guard"),
            "retired hook shows in help"
        );
    }

    #[test]
    fn path_help_lists_all_three_kinds() {
        // Arrange
        let kinds = ["plans", "implement", "worktrees"];

        // Act
        let result = Cli::command().try_get_matches_from(["playbook", "path", "--help"]);

        // Assert
        let err = result.expect_err("--help should short-circuit parsing with a help message");
        let help = err.to_string();
        for kind in kinds {
            assert!(help.contains(kind), "path --help is missing '{kind}'");
        }
    }

    #[test]
    fn path_kind_dir_name_matches_each_variant() {
        // Arrange, Act, Assert
        assert_eq!(PathKind::Plans.dir_name(), "plans");
        assert_eq!(PathKind::Implement.dir_name(), "implement");
        assert_eq!(PathKind::Worktrees.dir_name(), "worktrees");
    }

    #[test]
    fn cc_subcommands_parse_including_aliases() {
        // Arrange, Act
        let list = Cli::command().try_get_matches_from(["playbook", "cc", "list"]);
        let ls = Cli::command().try_get_matches_from(["playbook", "cc", "ls"]);
        let worktree =
            Cli::command().try_get_matches_from(["playbook", "cc", "worktree", "my-branch"]);
        let worktree_with_env_base =
            Cli::command().try_get_matches_from(["playbook", "cc", "worktree", "my-branch", "env"]);
        let new = Cli::command().try_get_matches_from(["playbook", "cc", "new", "my-branch"]);
        let raw_no_sid = Cli::command().try_get_matches_from(["playbook", "cc", "raw"]);
        let raw_with_sid =
            Cli::command().try_get_matches_from(["playbook", "cc", "raw", "sid-123"]);
        let default = Cli::command().try_get_matches_from(["playbook", "cc"]);

        // Assert
        assert!(list.is_ok(), "cc list should parse");
        assert!(ls.is_ok(), "cc ls alias should parse");
        assert!(worktree.is_ok(), "cc worktree BRANCH should parse");
        assert!(
            worktree_with_env_base.is_ok(),
            "cc worktree BRANCH ENV_BASE should parse"
        );
        assert!(new.is_ok(), "cc new alias should parse");
        assert!(raw_no_sid.is_ok(), "cc raw with no sid should parse");
        assert!(raw_with_sid.is_ok(), "cc raw SID should parse");
        assert!(default.is_ok(), "cc with no subcommand should parse");
    }

    fn help_texts(cmd: &clap::Command, path: &str, out: &mut Vec<(String, String)>) {
        let here = format!("{path} {}", cmd.get_name());
        for text in [
            cmd.get_about(),
            cmd.get_long_about(),
            cmd.get_before_help(),
            cmd.get_before_long_help(),
            cmd.get_after_help(),
            cmd.get_after_long_help(),
        ]
        .into_iter()
        .flatten()
        {
            out.push((here.clone(), text.to_string()));
        }
        for arg in cmd.get_arguments() {
            for text in [arg.get_help(), arg.get_long_help()].into_iter().flatten() {
                out.push((format!("{here} --{}", arg.get_id()), text.to_string()));
            }
            for value in arg.get_possible_values() {
                if let Some(text) = value.get_help() {
                    out.push((format!("{here} {}", value.get_name()), text.to_string()));
                }
            }
        }
        for sub in cmd.get_subcommands() {
            help_texts(sub, &here, out);
        }
    }

    #[test]
    fn user_facing_help_never_names_source_files_or_rust_paths() {
        // Arrange
        let mut texts = Vec::new();

        // Act
        help_texts(&Cli::command(), "", &mut texts);

        // Assert
        assert!(texts.len() > 100, "the walk found too little help text");
        for (place, text) in texts {
            for banned in ["src/", "shell/", "::", ".rs", ".sh", ".py"] {
                assert!(
                    !text.contains(banned),
                    "help for `{place}` leaks `{banned}`: {text}"
                );
            }
            assert!(
                !text.contains('\u{2014}') && !text.contains('\u{2013}'),
                "help for `{place}` has a dash: {text}"
            );
        }
    }

    #[test]
    fn code_index_parses_with_its_flags() {
        // Arrange, Act
        let plain = Cli::try_parse_from(["playbook", "code", "index"]);
        let flags = Cli::try_parse_from(["playbook", "code", "index", "--json", "--rebuild"]);

        // Assert
        assert!(plain.is_ok());
        let Ok(Cli {
            command:
                Command::Code {
                    sub: CodeCommand::Index { json, rebuild },
                },
            ..
        }) = flags
        else {
            panic!("code index --json --rebuild did not parse as Code Index");
        };
        assert!(json && rebuild);
        assert!(Cli::try_parse_from(["playbook", "code", "--help"]).is_err());
    }

    #[test]
    fn code_search_and_symbol_limits_run_from_one_to_two_hundred() {
        // Arrange
        let parse = |limit: &str| {
            Cli::try_parse_from([
                "playbook", "code", "search", "--text", "x", "--limit", limit,
            ])
        };

        // Act, Assert
        assert!(Cli::try_parse_from(["playbook", "code", "search"]).is_err());
        assert!(Cli::try_parse_from(["playbook", "code", "search", "x"]).is_ok());
        assert!(Cli::try_parse_from(["playbook", "code", "search", "--text", "x"]).is_ok());
        assert!(Cli::try_parse_from(["playbook", "code", "symbol", "x"]).is_ok());
        assert!(Cli::try_parse_from(["playbook", "code", "symbol", "x", "--limit", "0"]).is_err());
        assert!(parse("0").is_err());
        assert!(parse("1").is_ok());
        assert!(parse("200").is_ok());
        assert!(parse("201").is_err());
    }

    #[test]
    fn hook_and_init_are_hidden_but_hook_still_parses() {
        // Arrange
        let cmd = Cli::command();

        // Act
        let hidden: Vec<_> = cmd
            .get_subcommands()
            .filter(|c| c.is_hide_set())
            .map(|c| c.get_name().to_string())
            .collect();

        // Assert
        let mut hidden = hidden;
        hidden.sort();
        assert_eq!(hidden, ["hook", "init"]);
        assert!(Cli::command()
            .try_get_matches_from(["playbook", "hook", "session-init"])
            .is_ok());
    }
    #[test]
    fn init_parses_to_the_same_flags_as_setup() {
        // Arrange
        let args = [
            "--no-hooks",
            "--short-name",
            "--only",
            "telemetry,path",
            "--dry-run",
        ];
        let parse = |name: &str| {
            let mut all = vec!["playbook", name];
            all.extend_from_slice(&args);
            Cli::try_parse_from(all).unwrap().command
        };

        // Act
        let (Command::Setup { flags: setup }, Command::Init { flags: init }) =
            (parse("setup"), parse("init"))
        else {
            panic!("setup and init should parse to their own variants");
        };

        // Assert
        assert_eq!(setup, init);
    }

    #[test]
    fn only_init_in_a_terminal_gets_the_deprecation_line() {
        // Arrange
        let parse = |args: &[&str]| {
            let mut all = vec!["playbook"];
            all.extend_from_slice(args);
            Cli::try_parse_from(all).unwrap().command
        };
        let init = parse(&["init", "--yes"]);
        let setup = parse(&["setup", "--yes"]);

        // Act, Assert
        assert_eq!(
            init.deprecation_notice(true),
            Some("playbook init is deprecated, use: playbook setup")
        );
        assert_eq!(init.deprecation_notice(false), None);
        assert_eq!(setup.deprecation_notice(true), None);
        assert_eq!(parse(&["plan", "list"]).deprecation_notice(true), None);
    }

    #[test]
    fn top_level_help_lists_setup_and_not_init() {
        // Arrange
        let cmd = Cli::command();

        // Act
        let setup = cmd.find_subcommand("setup");
        let init = cmd.find_subcommand("init");

        // Assert
        assert!(setup.is_some_and(|c| !c.is_hide_set()));
        assert!(init.is_some_and(clap::Command::is_hide_set));
    }
}

/// How a plan's Segments are delivered.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum SegmentTopology {
    Stacked,
    Independent,
    Single,
}

/// Naming arguments of `segment branch`.
#[derive(Debug, clap::Args)]
pub struct SegmentNaming {
    /// Conventional commit type for the branch name, such as feat
    #[arg(long = "type")]
    pub kind: String,
    /// Plan slug
    #[arg(long)]
    pub plan_slug: String,
    /// Segment number
    #[arg(long)]
    pub n: u32,
    /// Segment title, kebab-cased and truncated into the branch name
    #[arg(long)]
    pub title: String,
    /// Default branch of the repo
    #[arg(long, default_value = "main")]
    pub default_branch: String,
    /// Repo directory to run in. Defaults to the current directory
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

/// `playbook segment` subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum SegmentCommand {
    /// Put HEAD on the branch a Segment is built on
    ///
    /// Names the branch `<type>/<plan-slug>-s<N>-<title>` and creates it from
    /// the right base: the default branch for independent Segments, the
    /// previous Segment's branch for stacked ones (fetched when it is not
    /// local), and always the fetched `origin/<default>` under `--land`. The
    /// single topology creates nothing and reports the current branch and tip.
    /// Prints `branch=<name> base=<sha> base_ref=<ref>`. Exits 1 when the
    /// branch already exists.
    ///
    /// Example: `playbook segment branch --type feat --plan-slug my-plan --n 2 --title "Parser core" --topology stacked --prev-branch feat/my-plan-s1-schema`
    Branch {
        #[command(flatten)]
        naming: SegmentNaming,
        /// How the plan's Segments are delivered
        #[arg(long, value_enum)]
        topology: SegmentTopology,
        /// Previous Segment's branch, for a stacked Segment past the first
        #[arg(long)]
        prev_branch: Option<String>,
        /// Branch off the fetched origin default branch, at the land boundary
        #[arg(long)]
        land: bool,
    },
    /// Measure a Segment against its base
    ///
    /// Prints `lines=N files=M` for `BASE...HEAD`.
    ///
    /// Example: `playbook segment size --base main`
    Size {
        /// Base ref of the Segment
        #[arg(long)]
        base: String,
        /// Repo directory to run in. Defaults to the current directory
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}
