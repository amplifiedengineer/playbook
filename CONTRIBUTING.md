# Contributing to Playbook

This project follows the standard igorjs contribution rules. Start here:

- **[.github/CONTRIBUTING-RULES.md](.github/CONTRIBUTING-RULES.md)**: DCO, CLA, commit conventions, PR process, and the code style baseline (SPDX headers, dependency policy, commit signing). These rules are shared across all igorjs repos.
- **First pull request:** sign the CLA by adding your own row to [.github/CONTRIBUTORS.md](.github/CONTRIBUTORS.md) in a commit you author. A person named in a `Co-authored-by:` trailer signs the same way. The steps are in the rules above, and [the internals note](docs/internals/18-cla-and-dco-checks.md) explains the checks.
- **[README](README.md)**: project description and install.
- **[SECURITY.md](SECURITY.md)**: vulnerability disclosure process.

## Project-Specific Notes

See the [justfile](justfile) for build and test commands (`just --list`). The recipes need `just`, `cargo-nextest`, `shellcheck` and `jq`: `brew install just cargo-nextest shellcheck jq`. `seed/Brewfile` lists only what playbook itself calls on a user's machine.

### Repository layout

This is a Cargo workspace. The skills, commands and agents live in `plugins/playbook`. The Rust code lives under `crates/`:

| Folder | Package | What it holds |
| --- | --- | --- |
| `crates/cli` | `playbook` | the `playbook` binary and library, with its tests in `crates/cli/tests` |
| `crates/plugin-checks` | `playbook-plugin-checks` | tests that read `plugins/playbook`: prose, structure and policy table |
| `crates/common` | `playbook-common` | shared helpers: paths, git, sqlite, atomic files |
| `crates/code` | `playbook-code` | the code index, search, symbols and references |
| `crates/telemetry` | `playbook-telemetry` | the Agent Budget Dashboard, with the `pbtop` binary |
| `crates/data` | `playbook-data` | shared storage (data folder, `config.db`, `memory.db`) |
| `crates/pbsync` | `pbsync` | the opt-in usage sync helper that `playbook sync` runs: the Turso SQL-over-HTTP transport and token handling |
| `crates/store` | `playbook-store` | the shim over the SQLite engine, with the busy-retry helpers |
| `crates/code-bench` | `code-bench` | dev-only benchmark for the code index: corpus, metrics and gate. Nothing in the shipped binary depends on it |
| `crates/pbdoc` | `pbdoc` | offline document to Markdown converter that ships next to `playbook` |

Run `just test` for everything, or `cargo nextest run -p playbook-code --config-file .github/nextest.toml` for one crate (nextest, because some tests share ports and plain `cargo test` runs them in one process). Doc tests run with `cargo test --workspace --doc`. Tests read the repository through `env!("PLAYBOOK_REPO_ROOT")` and the plugin through `env!("PLAYBOOK_PLUGIN_ROOT")`, both set in `.cargo/config.toml`, instead of `CARGO_MANIFEST_DIR`. Regenerate the reference docs with `UPDATE_REFERENCE=1 cargo test -p playbook --test reference_docs`. See [ADR-0029](docs/adr/0029-cargo-workspace-monorepo.md) for the layout and the release tags.

In CI (`.github/workflows/rust-ci.yml`) the `test` and `lint` jobs run side by side, so a slow lint step never delays the tests. On Linux, `test` splits nextest over two runners with `--partition hash:1/2` and `hash:2/2`, so each test runs on exactly one runner. macOS runs the whole suite. The `build-test` check waits for every `test` leg and for `lint`, and it is the required check. A run of the whole suite on your machine covers both halves.

The completion scripts are also tested in real bash, zsh and fish (`crates/cli/tests/shells.rs`). Those tests skip themselves unless `PLAYBOOK_REQUIRE_SHELLS=1` is set and the shells are installed. The `shell-tests` job in `.github/workflows/rust-ci.yml` sets it on Linux. The job is not required and does not run in a release gate. Its `EXPECTED_SHELL_TESTS` count must match the number of tests in the file.

### Faster local builds

These settings are per shell or per command, so nothing changes for other contributors. Measured once each on an M-series Mac with Rust 1.99.1 and `CARGO_INCREMENTAL=0`, building every test binary with `cargo test --workspace --no-run`:

| Setting | Clean build | Rebuild after editing `playbook-common` | Target folder |
| --- | --- | --- | --- |
| debug info on (the default) | 43 s | 20 s | 1.7 GB |
| `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` | 35 s | 18 s | 1.0 GB |
| the above and `ld64.lld` | 36 s | 16 s | 1.0 GB |

- **Run only the tests you need.** `just test` passes its arguments to nextest with the repo's config file, for example `just test -E 'test(held_lock)'`, and then runs the doc tests. For one crate, use the `cargo nextest run -p` command above, which builds only that crate.
- **Drop debug info.** `export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` makes builds faster and the target folder smaller. A panic message keeps its file and line, but backtraces and debuggers show only function names. Unset both when you need a debugger.
- **Keep incremental builds on locally.** CI sets `CARGO_INCREMENTAL=0` because each run builds once. On your machine, incremental builds make the edit and test loop faster.
- **Linker.** The rustup toolchain for x86_64 Linux already links with `rust-lld`. In CI, mold was slower: 44 s against 37 s to rebuild the test binaries. On macOS, `ld64.lld` from `brew install lld` saved 2 s per rebuild in one run: `RUSTFLAGS="-C link-arg=-fuse-ld=$(brew --prefix lld)/bin/ld64.lld"`. Cargo rebuilds everything whenever `RUSTFLAGS` changes, so set it the same way in every shell and editor, or in your own `~/.cargo/config.toml`.
- **Disk.** One `CARGO_TARGET_DIR` per worktree keeps parallel builds from blocking each other, and each one grows to 1 GB or more. See [Keeping target folders small](#keeping-target-folders-small).

The repository has no shared `.cargo/config.toml` build settings for speed, because a debugger or a missing linker would break them for some contributors.

### Keeping target folders small

Many worktrees building at once fill the disk: on 2026-10-19 sixteen worktrees held 1.5 to 6 GB of `target/` each, about 45 GB, and a full disk corrupted `target/debug/incremental`. `scripts/dev-disk.sh` is a development helper for contributors and their agents. It is not part of the `playbook` binary or the plugin, and no release ships it.

- **Lean build.** Run `eval "$(just dev-env)"` in the same shell call as your cargo command. It sets `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0` and a private `CARGO_TARGET_DIR` next to your worktree. It also warns when free space is below `DEV_DISK_WARN_FREE_GB` (default 15). Your normal builds are unchanged unless you run it. It turns incremental builds off on purpose, to save disk, so keep them on in the worktree you edit in all day.
- **Clean up.** `just dev-clean --dry-run` lists, and `just dev-clean` removes, the `target/` folder and the private `cargo-*` folder of every other linked worktree of this repo (not the main one, not the one you run it in) that is a cargo target folder, unchanged for `DEV_DISK_IDLE_MINUTES` (default 30), with no build holding its lock and no running process naming it. The main worktree is never touched.
- **Agents.** Tell an agent to run the `dev-env` line before each cargo command, and run `just dev-clean` yourself when disk is low. The plugin's own commands do not call the script.

Measured with `cargo test --workspace --no-run` from a clean target folder, on a loaded machine, so times are rough:

| Setting | Target folder | Time |
| --- | --- | --- |
| cargo defaults | 3.0 GB | 61 s |
| `CARGO_INCREMENTAL=0` | 1.9 GB | 68 s |
| and debug info `line-tables-only` | 1.6 GB | 54 s |
| and debug info `0` (what `dev-env` sets) | 1.1 GB | not comparable (one run hit machine contention) |
| debug info `0`, incremental on | 2.1 GB | 65 s |

A shared `CARGO_TARGET_DIR` makes parallel builds wait on cargo's build lock. `sccache` shares compiled dependencies but does not shrink each target folder, skips incremental builds, and is an extra install; if you have it, `RUSTC_WRAPPER=sccache` works alongside the settings above.

### Releases

`vX.Y.Z` releases the `playbook` binary, the plugin and `pbtop` together, with one version ([ADR-0038](docs/adr/0038-one-release-line.md)). The `release` workflow does every step ([ADR-0039](docs/adr/0039-release-workflow.md)):

```bash
gh workflow run release.yml -f bump=patch -f dry_run=true   # rehearse
gh workflow run release.yml -f bump=patch                   # or minor, major, or -f version=X.Y.Z
```

It opens the release pull request with `scripts/bump-version.sh` and the changelog section, waits for the merge queue, tags the merged commit, builds and attests the assets, publishes the release as the latest and checks it, then opens the release pull requests in the Homebrew tap and the marketplace and waits for their own `vX.Y.Z` releases. It also opens the Nix flake pull request in amplifiedengineer/nix. That is a leaf job: a red leaf job fails the run but leaves the release published. When a run fails, run it again with `-f version=X.Y.Z`: it goes on from where it stopped. [Release channels](docs/internals/06-release-channels.md) has the details and the rollback steps.

### Breaking changes before 1.0

Playbook is before 1.0, so a breaking change can ship in any release. A change is breaking when a user who upgrades has to do something, or when something they use stops working or works in a new way.

- Mark it with `!` after the type or scope in the pull request title, as in `feat(cli)!: drop the old flag`, or with a `BREAKING CHANGE:` footer at the end of the body. The title and body become the squash commit.
- Give the footer one line that says what breaks and what to do, as in `BREAKING CHANGE: the config moved; run playbook setup`. The `breaking-changes` check warns when the title is marked and the body has no footer.
- The release notes list breaking changes first ([ADR-0037](docs/adr/0037-release-notes-from-conventional-commits.md)). `scripts/release-notes.sh` builds them from the commits.
- A change to playbook's own config, its data files, the entries it writes into Claude Code settings, or the contract between the binary and the plugin keeps working after an upgrade or ships a migration that runs on first use ([ADR-0034](docs/adr/0034-self-healing-upgrades.md)).

### Adding a migration

A migration is named by a slug and the release that ships it ([ADR-0046](docs/adr/0046-migrations-by-release-version.md)).

- Append the entry to `registry()` in `crates/cli/src/init/migrate.rs` with a new slug, such as `move-old-cache`, and `since: "next",` on its own line in exactly that form. The release pull request writes the version in its place.
- Never rename or move an entry. The record and the doctor name it by its slug.
- A build skips a `next` migration, so it never runs from a dev build of `main`. Test it with the unit tests next to it in that file, or build from a scratch checkout after `scripts/release-prepare.sh <next version>`.
- [Internals: Migrations](docs/internals/07-migrations.md) explains the kinds and the scopes.

### Adding a command

Name a command by these rules; tests in `crates/cli/tests/command_names.rs` check most of them:

- Name it noun then verb, as in `playbook pr create`. Each segment is one lowercase word, and a path is at most four segments deep.
- Use a hyphen only in a flag or a hook name, not in a command name.
- Give each job one visible entry point, `playbook <noun>`. A `pb*` binary is a helper that users do not run, or a short alias of a `playbook` command. Do not add another one.
- Do not add a new bare default for a noun. Name the verb.
- A command an agent calls takes `--json`, prints the shared envelope, and exits 0 in that mode. Without `--json` it uses the shared codes table for its exit codes.
- Move a command through `COMMAND_RENAMES` in `crates/cli/src/renames.rs`. When the contract changes too, keep the old name as a hidden legacy command with its own code and output, and remove it in 1.0.0.
- In the user docs, name only primary names. Put a legacy name after a `<!-- names: legacy -->` line, or the docs test fails.

`playbook commands` lists every visible command at run time, from the clap tree. It needs one line per command in `META` in `crates/cli/src/command_index.rs`:

- `ro("path")` for a command that only reads, `free("path")` for one that changes the repo or playbook's own state, and `user("path")` for one only the user may run: it writes their home or Claude Code settings, asks for consent, runs a paid benchmark or starts a session.
- Add `.purpose("...")` when the first sentence of the doc comment is too long or too short: 6 to 12 words for a top-level command, 3 to 14 for a subcommand.
- Keep `META` sorted by path. A group needs its own line too.

A test walks the whole tree and fails when a visible command has no line, a line names a command that is gone, or a purpose has the wrong length. Hidden commands, such as a deprecated name, need no line. Then regenerate the reference page with `UPDATE_REFERENCE=1 cargo test -p playbook --test reference_docs`.

Write help text as the [CLI help text style guide](docs/reference/cli-help-style.md) says; a lint checks most of it. A PR that fixes an allowlisted item deletes its line in `crates/cli/tests/help_lint_allowlist/`.

Renaming a command is a deprecation. Keep the old name as a hidden alias that runs the same code, prints one line on stderr in a terminal that names the new command, and keeps its exit codes. Since the old name keeps working, the commit is not marked breaking. Remove the alias no earlier than the next major release, and mark that removal as breaking. The rename to `playbook setup` is the example ([ADR-0040](docs/adr/0040-rename-init-to-setup.md)).

### Adding a config key

Think twice before you add one. A setting that no one changes is a constant, so put it in the code.

- Add the key to `KNOWN_KEYS` in `crates/cli/src/config/keys.rs`, and raise `KEY_CEILING` in the same file by one. The count test fails until you do.
- Give it a row in `docs/reference/config-keys.md` that says why a user would change it.
- Keep the leaf short: one word, or one word and one capital hump, such as `maxLines`. Do not chain words like `stackMaxLines`. Put a group word in the path instead: `review.stack.maxLines`.
- Use lowercase single-word groups, and at most four segments. A few existing groups use camelCase, and `COMPOUND_GROUPS` in the same file names them. Do not add to it.
- Rename a key through `KEY_RENAMES`, so a stored value moves with it.

## Changing the plugin

The skills, commands, agents and output style live in `plugins/playbook` and ship to users through the `amplifiedengineer/marketplace` listing. See [ADR-0032](docs/adr/0032-plugin-in-the-playbook-repo.md) for how.

- Keep the folder to markdown and `plugin.json`. The `payload-guard` check fails on anything else.
- Do not bump the plugin version. The plugin ships with the next `vX.Y.Z` release, which sets one version for the binary and the plugin ([ADR-0038](docs/adr/0038-one-release-line.md)). Claude Code updates an installed plugin only when that version rises, so a plugin fix reaches users with the next release.
- When prose needs a task from the binary, name the task in plain words and list its intent in `plugins/intents.json`. Do not write a subcommand into the prose. The one exception is the `playbook:playbook-usage` skill, which points to `playbook commands` for the full list. The `intents-contract` check compares the list with `playbook tools map`, and fails when a command the tool map or the `playbook:playbook-usage` skill names is missing from `playbook commands --json`.
- A renamed command or skill keeps its old name as a deprecated stub until 1.0.0, unlike the hidden alias of a binary command above: the stub prints one line that names the new command and runs it. Mark the PR `feat(plugin):` with a `Deprecated:` footer that lists the old and new names. The PR that removes the stubs at 1.0.0, or removes a command with no replacement, is breaking (`feat(plugin)!:`) and lists the old names in a `BREAKING CHANGE:` footer. See "Names" in [Authoring commands, skills and hooks](docs/authoring/01-commands-skills-hooks.md).
- Keep a command or a `SKILL.md` within 200 lines and 20,000 bytes, and a reference file within 300 lines and 24,000 bytes. A file over its limit has a row in `crates/plugin-checks/tests/size_budget_allowlist.txt`; the list only shrinks, so a PR adds no row and raises no number. A main file holds no "Otherwise do this" block: the fallback for a missing binary step goes in `skills/delegating-subagents/references/without-binary.md`, one section per intent.
- Run `cargo test -p playbook-plugin-checks` for the prose and structure checks, and `scripts/check-plugin.sh payload` for the payload.

### Editing agents

The agent files live in `plugins/playbook/agents`. Run `playbook agents check plugins/playbook/agents` to check the files. An orchestrator sets an agent's effort per spawn with the `effort` parameter, so a new agent needs no table entry.

## Project-Specific Code Style

Unsafe code is not allowed: the workspace sets `unsafe_code = "forbid"`, which an `#[allow]` cannot lift. Use std or the safe `rustix` wrappers. There are no other project-specific code style rules beyond the baseline. See [.github/CONTRIBUTING-RULES.md](.github/CONTRIBUTING-RULES.md#code-style-baseline) for SPDX header, dependency policy, and commit signing requirements.

## Project-Specific Tests

See the [justfile](justfile) for test commands. `just check` runs the checks CI runs, apart from the cross builds and `cargo audit`.

A change under `.github/` also needs `just zizmor` to pass (`brew install zizmor`). It lints the workflows the way the `zizmor` workflow does, and fails on any finding of any severity. Ignores go in `.github/zizmor.yml`, one rule and one file each, with the reason. This is dev tooling only, not part of the `playbook` binary or the plugin.

Workflows follow a few shared rules. Every job has a name and a timeout. A newer push to a pull request cancels the older run of the same check. A draft pull request skips the heavy `rust-ci` jobs, and the macOS leg of `shell-ci` runs as a no-op on Linux. Marking the pull request ready runs the full set. The `ci-auto-rerun` workflow looks for failed runs every 10 minutes and reruns once a run that failed from an infrastructure problem. It never reruns `release`, and it skips fork runs.

The `Auto-Update PRs` workflow keeps armed PRs (auto-merge on, not draft) up to date with main one at a time. It runs on every push to main, every 15 minutes and on demand, updates at most the oldest armed PR that is behind, and does nothing while an up to date armed PR is still waiting on its checks. `scripts/pick-pr-to-update.js` holds the decision and `scripts/pick-pr-to-update.test.sh` tests it. It never force-pushes. A repo owner turns the workflow on in the Actions settings.

A test that starts the built `playbook` gives it a scratch home: use `isolated::playbook()` from `crates/cli/tests/support/isolated.rs`, or set `HOME` yourself. `test_isolation.rs` fails for a spawn that does neither. As a second net, a debug build started by cargo or nextest does not run migrations, the upgrade heal or the real `launchctl` and `systemctl` against the real home of your account, and prints a note instead. Other writes are not covered by that net, so the scratch home stays the rule. Set `PLAYBOOK_ALLOW_REAL_HOME=1` to run a dev build against your real home on purpose.
