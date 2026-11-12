# Install

> **Renamed.** The GitHub organization was `pragmatic-engineer` (and for a while `amplified-engineer`). It is now `amplifiedengineer`. The marketplace was renamed too: the plugin id `playbook@pragmatic-engineer` is now `playbook@amplifiedengineer`. An install on the old id keeps working and keeps updating until you move. New installs use the new id. Update the binary first, because releases after v0.41.0 know both ids. Homebrew users: see [Moving from the old tap name](#moving-from-the-old-tap-name). The plugin steps are in [Moving to the new marketplace name](#moving-to-the-new-marketplace-name). The other routes are in the [README](../../README.md). Your data stays in `~/.config/playbook`.

To see every change `playbook setup` makes before you agree to it, read [What setup installs](05-what-setup-installs.md) and the [hook reference](../reference/hooks.md).

<!-- names: legacy -->
Releases up to v0.34.0 have only the old name (older versions: `playbook init`). It takes the same flags, and it still works on newer releases ([ADR-0040](../adr/0040-rename-init-to-setup.md)).

The first route to try is Homebrew. Use the install script when you have no Homebrew. This page covers every route (Homebrew, Nix, the install script and the raw release assets), requirements, how the binary reaches `PATH`, upgrades, the settings merge and uninstall.

Install order:

1. Homebrew.
2. The install script (`install.sh`), when you have no Homebrew.
3. Nix.
4. The raw release assets, for Windows (experimental) or when you want to check the files yourself.
5. The marketplace plugin, for the skills only.

## Pick an install mode

| Mode | Commands, in order | You get | You lack |
|---|---|---|---|
| Skills only | `claude plugin marketplace add amplifiedengineer/marketplace`, then `claude plugin install playbook@amplifiedengineer` | Skills, commands and subagents. No hooks, no binary, no change to `settings.json` | Safety guards, status line, hooks, usage, and every step that needs the `playbook` binary |
| Binary only | Homebrew: `brew install amplifiedengineer/tap/playbook`, then `playbook setup`. Or, with no Homebrew, the install script: `curl -fsSL https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install.sh \| bash` | The `playbook` command, hooks, guards, status line, memory and usage | The skills and slash commands |
| Both (recommended) | The binary first, then `playbook setup`, which installs the plugin when `claude` is found (or the two skills commands) | Everything | Nothing |

The skills are a Claude Code plugin and work on their own. The binary is optional. Installing the binary does not install the plugin. `playbook setup` installs or updates the plugin only when you say yes (in a terminal it asks, and `--yes`, `--plugin` or `--only plugin` is the explicit yes), and it updates the plugin after an upgrade as described in [Upgrade](#upgrade). It never removes a plugin.

To add the plugin later from a binary that did not come from Homebrew (install.sh, a container, a home server platform such as Umbrel or a nix profile), run `playbook plugin install`. It needs `claude` on `PATH`, adds the `amplifiedengineer/marketplace` marketplace, turns on `autoUpdate` for it in `~/.claude/settings.json` (Claude Code updates a plugin with its marketplace, so that one flag covers the plugin), and installs `playbook@amplifiedengineer`. A second run changes nothing. To update only the plugin later, run `playbook plugin upgrade`.

When the binary is installed, `playbook setup` adds its hook entries to `~/.claude/settings.json`. In a terminal it asks first. With no terminal and no `--yes`, it leaves your Claude Code settings alone and prints the command to run. `--hooks` or `--yes` is the explicit yes. It merges with the entries you already have. An entry counts as playbook's when its command is exactly one of the `playbook hook <name>` commands `setup` writes, and `playbook uninstall` removes only those. The old `playbook-hooks` plugin is retired. If it is still installed, remove it with `claude plugin uninstall playbook-hooks@pragmatic-engineer` (or `playbook-hooks@amplifiedengineer` if your marketplace has the new name), because its hooks would run next to the ones in `settings.json`.

Which one? Choose skills only if you want to read the skills and nothing else runs on your machine. Choose both for everything else. The binary is a hard dependency of the hooks, guards, status line and usage. Only the skill text works without it. See [Skills without the binary](#skills-without-the-binary).

## Requirements

| Tool | Status | Why |
|---|---|---|
| `claude` | only for the skills | The skills are a Claude Code plugin. The binary, hooks, guards and settings do not need it. |
| `bash` or `zsh` | required | The installer runs in bash, and the `path` part edits a bash or zsh rc file. |
| `git`, `shasum` | required | Used by hooks and the installer. |
| `gh` | optional | Status line PR and CI info, and the build attestation check. |
| `agent-browser` | optional | Browser automation `/playbook:plan` uses for web-only tickets. |

## Install

### Homebrew (first choice)

```bash
brew install amplifiedengineer/tap/playbook
playbook setup
```

`playbook setup` asks before it adds the hooks to `settings.json`, and writes the `PATH` block so hook shells find the binary even when Homebrew's directory is not on their `PATH`. Homebrew ships only the binary, so `setup` first offers to install the plugin when the `claude` command is found. When no Claude Code plugin is installed, `setup` seeds the settings template and the system prompt from copies compiled into it (written to `~/.config/playbook/embedded/`). An installed plugin, even an older version, takes precedence. Homebrew on Linux works the same way: install [Homebrew](https://brew.sh) for Linux, then run the same two commands. The formula picks the static musl build for x86_64 or arm64, so it needs no system libraries. Brew installs under its own prefix (`/home/linuxbrew/.linuxbrew` by default) and playbook never asks for `sudo`. The formula also links `pbtop` to `playbook`, so `pbtop` always has the version of `playbook` and one `brew upgrade playbook` updates both. Each release installs `playbook` this way on an x86_64 Ubuntu runner after the tap is updated, checks the `pbtop` link, and fails the release run when that breaks. Homebrew installs update with `brew upgrade playbook` once the formula is trusted, and `playbook upgrade` refuses a Homebrew install and tells you to use brew.

If you installed the separate `pbtop` formula before, remove it once: `brew uninstall pbtop`, then `brew reinstall playbook`. The two formulas conflict, because both own the `pbtop` file, and Homebrew prints this command in its conflict message. See [Optional: pbtop alone](#optional-pbtop-alone).

### Homebrew tap trust

Since Homebrew 6.0.0, a tap that is not official needs explicit trust. This is Homebrew's rule, not a playbook setting.

Install with the fully qualified name. This trusts only that formula:

```bash
brew install amplifiedengineer/tap/playbook
```

The install records that trust, so a later `brew upgrade playbook` works by short name. To install by short name without using the full name first, trust the formula:

```bash
brew tap amplifiedengineer/tap
brew trust --formula amplifiedengineer/tap/playbook
brew install playbook
```

The `pbtop` link comes with the `playbook` formula, so it needs no trust of its own. The separate `pbtop` formula is for a machine without `playbook`. Trust it on its own: `brew trust --formula amplifiedengineer/tap/pbtop`.

To trust the whole tap, run `brew trust amplifiedengineer/tap`. This covers every current and future formula in the tap. It is broader, so prefer per-formula trust.

Run `brew trust` to list trusted entries. Run `brew untrust <target>` to remove trust, for example `brew untrust --formula amplifiedengineer/tap/playbook`. If Homebrew says a formula is not trusted, run the trust command it names. Homebrew stores the entries in `~/.homebrew/trust.json`, or in `$XDG_CONFIG_HOME/homebrew/trust.json` when `XDG_CONFIG_HOME` is set. See Homebrew's own page on tap trust.

### Moving from the old tap name

The GitHub organization is now `amplifiedengineer`. It was `pragmatic-engineer` and, for a while, `amplified-engineer`. GitHub redirects the old tap address today, but do not rely on that. You have two ways to move an existing Homebrew install. Both keep your data in `~/.config/playbook`.

To keep the installed formula and the tap name `pragmatic-engineer/tap`, point the tap at the new address:

```bash
git -C "$(brew --repository pragmatic-engineer/tap)" remote set-url origin https://github.com/amplifiedengineer/homebrew-tap.git
brew update
```

To use the new tap name, remove the old formula and tap, then install from the new tap. Homebrew refuses `brew untap` while a formula from that tap is installed, so uninstall first. Add `pbtop` to the first command only if you installed that formula.

```bash
brew uninstall playbook
brew untap pragmatic-engineer/tap
brew install amplifiedengineer/tap/playbook
```

The full name in the last command trusts only that formula, as in [Homebrew tap trust](#homebrew-tap-trust). `brew tap --repair` does not do this move. It repairs symlinks and a renamed default branch, not a renamed tap. Check the result with `brew tap-info amplifiedengineer/tap`.

### Moving to the new marketplace name

The Claude Code marketplace was renamed from `pragmatic-engineer` to `amplifiedengineer`. The plugin id changed from `playbook@pragmatic-engineer` to `playbook@amplifiedengineer`. An existing registration keeps its old name, and `claude plugin update playbook@pragmatic-engineer` keeps working. A new `claude plugin marketplace add amplifiedengineer/marketplace` gets the new name. The binary prefers the new id and accepts the old one with no end date.

To move, update the binary first. The release after v0.41.0 is the first that knows both ids, and older binaries know only the old one. Then run:

```bash
claude plugin uninstall playbook@pragmatic-engineer
claude plugin marketplace remove pragmatic-engineer
claude plugin marketplace add amplifiedengineer/marketplace
claude plugin install playbook@amplifiedengineer
```

`claude plugin marketplace remove` also uninstalls the plugins from that marketplace and deletes their cache and plugin data. To keep the plugin data folder, add `--keep-data` to the `uninstall` command. If the retired `playbook-hooks` plugin is still there, run `claude plugin uninstall playbook-hooks@pragmatic-engineer` as well.

### Install script

Use this when you have no Homebrew. install.sh is a bash script, so it does not run on Windows. On Windows, use [Without `curl | bash`](#without-curl--bash).

```bash
curl -fsSL https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install.sh | bash
```

It installs the `playbook` binary, the `pbdoc` document converter and the `pbsync` usage sync helper (you run it as `playbook sync`), and links `pb`, into `PLAYBOOK_BIN_DIR` (default `~/.local/bin`). When a release has no `pbdoc` or `pbsync` archive, it warns and skips that helper. Installing `pbsync` does not turn sync on, which stays off until you opt in. It checks each download after checking its SHA256, its build attestation when `gh` can check it, and a `--version` smoke test. It puts that directory on `PATH`. It does not touch Claude Code's plugins or settings.

When it replaces an older binary, it prints "playbook updated from A to B.", then the breaking changes of the new release if there are any, then a link to the full release notes. Before 1.0 a breaking change can ship in any release, so read them.

Then it asks "Run playbook setup now?". Only a typed `y` or `yes` counts, so a bare Enter is a no. On yes it asks about the system prompt that `pb cc` loads (Enter means yes; with `--yes` setup decides, so an earlier no stays), then runs `playbook setup --yes`, which turns every part on: the hook entries, the status line and the telemetry variables (a collector on this machine) in `settings.json`, the system prompt, the `pb` link and the rest. A no to the prompt question passes `--no-system-prompt`. On no, or with no terminal and no `--yes`, it skips `setup` and prints the command to run later, with `--system-prompt` or `--no-system-prompt` if you passed one. Open a new terminal afterward.

Pass `--yes` to say yes to every question, including `playbook setup`. Flags go after `-s --` when piping:

| Flag | Effect |
|---|---|
| `--yes`, `-y` | Say yes to every question, including running `playbook setup`. |
| `--no-init` | Install the binary only. Do not ask about `playbook setup`. |
| `--system-prompt` | Install the system prompt without asking. |
| `--no-system-prompt` | Keep `pb cc` on Claude Code's own prompt, without asking. |
| `--binary-only` | Still offers `playbook setup`, but keeps the system prompt off (passes `--no-system-prompt`). |
| `--ref <tag>` | Release tag, same as `PLAYBOOK_REF`. |

`--skip-plugin`, `--no-setup`, `--skip-deps`, `--skip-shell` and `--aliases` are accepted and ignored, so older commands keep working. `--aliases` prints a warning, because `playbook cc` and `pb cc` need no shell line (ADR 0044). `--skills-only` is gone and exits with the plugin commands. The other `setup` options, such as `--short-name`, belong to `playbook setup`; run it yourself to pass them, and install.sh also links `pb` to `playbook` when the name is free.

Pin a version with `PLAYBOOK_REF`:

```bash
curl -fsSL https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install.sh | PLAYBOOK_REF=v0.43.0 bash
```

A release tag installs that release. A branch or commit is refused, because it has no published binary.

### Nix

```bash
nix run github:amplifiedengineer/nix#playbook -- --version
nix profile install github:amplifiedengineer/nix#playbook
playbook setup
```

The flake lives in [amplifiedengineer/nix](https://github.com/amplifiedengineer/nix). It fetches the release binary for `x86_64-linux`, `aarch64-linux`, `x86_64-darwin` or `aarch64-darwin` and checks it against a fixed hash. It has two outputs, `playbook` and `pbtop`, so `nix run github:amplifiedengineer/nix#pbtop` starts the dashboard. After each release a pull request in that repository moves `flake.nix` to the new version, so `main` is one release behind for a short while. A tag does not help here, because the flake has no release tags. To pin a version, use the merge commit of the `chore(release): nix flake X.Y.Z` pull request, for example `github:amplifiedengineer/nix/<commit>#playbook`. To roll back, pin the commit of the earlier flake pull request. Each release runs the flake on Linux and macOS after the pull request merges. A Nix install is read-only. The automatic update shows a notice that the install directory is not writable, and `playbook upgrade` stops with "cannot write to" the store path. Update with `nix profile upgrade playbook` instead. Your config is not touched.

### Optional: pbtop alone

You do not need this if you have `playbook`. `playbook usage` runs the same code, because playbook embeds the telemetry crate. The Homebrew formula of `playbook` also links `pbtop` to `playbook`, and the Nix flake has a `pbtop` output. Install `pbtop`, the Agent Budget Dashboard, only on a machine that has no `playbook`. From release 0.35.0 each `vX.Y.Z` release has `pbtop-X.Y.Z-<target>.tar.gz`, which holds `pbtop` alone, and there are two ways to install it:

```bash
brew install amplifiedengineer/tap/pbtop
curl -fsSL https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install-pbtop.sh | sh
```

The Homebrew route needs tap trust. The full name trusts only `pbtop`. See [Homebrew tap trust](#homebrew-tap-trust).

The `pbtop` formula conflicts with the `playbook` formula, because `playbook` owns the `pbtop` file. Homebrew refuses to install one while the other is installed, and the message names the command to run. If you add `playbook` later, run `brew uninstall pbtop` first. If you have both formulas from before the change, run `brew uninstall pbtop` once, then `brew reinstall playbook`. A machine with only the `pbtop` formula keeps using it and updates it with `brew upgrade pbtop`. Your usage data stays where it is.

A `pbtop` from another source can fall behind `playbook`, for example the install script or an older install next to a newer `playbook`. `playbook doctor check` then prints a WARN row with the version, the path and the command that fixes it. The first command after an upgrade by Homebrew, the install script or a manual download adds the same hint under the "playbook updated from A to B" line, once. When a Claude Code session starts first, its hook takes that line and only `playbook doctor check` shows the hint. The check runs `pbtop --version`, and only in those two places: never in a hook or the status line.

`install-pbtop.sh` needs `bash` and `curl`. It downloads `install.sh` from `main` and runs it in its pbtop mode, which checks the archive like the `playbook` binary (its line in `SHA256SUMS`, the build attestation when `gh` can check it, and `pbtop --version`), installs `pbtop` into `PLAYBOOK_BIN_DIR` (default `~/.local/bin`) and puts that folder on `PATH`. It runs nothing else. To pin a release, set `PLAYBOOK_REF` for `sh`, as in `curl -fsSL .../install-pbtop.sh | PLAYBOOK_REF=v0.43.0 sh`. On a release before 0.35.0 it stops, because those have no `pbtop` archive. The old `install.sh --pbtop` still works for one more release and points to `install-pbtop.sh`.

Then run `pbtop`. The [pbtop page](../pbtop.md) covers the flags, the views, the keys and where the data lives. The [crate README](../../crates/telemetry/README.md) covers `cargo` builds.

### Without `curl | bash`

1. From the [latest release](https://github.com/amplifiedengineer/playbook/releases/latest), download the asset for your platform and `SHA256SUMS`.

   | Platform | Asset |
   |---|---|
   | macOS, Apple silicon | `playbook-<version>-aarch64-apple-darwin` |
   | macOS, Intel | `playbook-<version>-x86_64-apple-darwin` |
   | Linux, x86_64 | `playbook-<version>-x86_64-unknown-linux-musl` |
   | Linux, arm64 | `playbook-<version>-aarch64-unknown-linux-musl` |
   | Windows (experimental) | `playbook-<version>-x86_64-pc-windows-msvc.exe` |

2. Verify it. `SHA256SUMS` is not signed, so it is only a corruption check. The attestation shows where the binary was built.

   ```bash
   grep "  <asset>$" SHA256SUMS | shasum -a 256 -c -
   gh attestation verify <asset> --repo amplifiedengineer/playbook
   ```

   `install.sh` and `playbook upgrade` run the attestation check when `gh` is available. They warn when `gh` is missing or the release predates attestations, and abort on a real mismatch. Set `PLAYBOOK_REQUIRE_ATTESTATION=1` to abort on a missing check too.

3. Put it on `PATH`:

   ```bash
   mkdir -p ~/.local/bin
   mv <asset> ~/.local/bin/playbook
   chmod 0755 ~/.local/bin/playbook
   ```

4. Wire the local config:

   ```bash
   playbook setup
   ```

   In a terminal, `setup` asks about hooks, settings, the status line, `PATH`, the system prompt and the `pb` short name. The status line question reads "Use the playbook status line?" and has `--statusline` and `--no-statusline`. Answering no leaves `statusLine` in `settings.json` as it is, and an existing playbook status line stays until `playbook uninstall`. Each part has `--x` and `--no-x` flags. With `--yes`, hooks, settings, the status line, `PATH`, the system prompt and the `pb` link are on. `--yes` keeps the system prompt off when `systemPrompt.enabled` is false or on an earlier install that had none, and keeps the `pb` link off after a stored no. Without a terminal and without `--yes`, the hooks, the status line, the system prompt, the `pb` link and telemetry stay off unless you pass their flag. Every step is safe to repeat.

5. For the skills, run `playbook setup`, or the two plugin commands from [Pick an install mode](#pick-an-install-mode) when `claude` is not on `PATH` during setup. Then run `/playbook:doctor`, or `playbook doctor check` without the skills.

### Windows (experimental)

The Windows route is the raw `.exe` release asset. There is no installer, package or bucket. Download `playbook-<version>-x86_64-pc-windows-msvc.exe` and `SHA256SUMS`, check the file as in the steps above, rename it to `playbook.exe` and put it in a folder on your `PATH`. Windows is an experimental CLI ([ADR 0053](../adr/0053-windows-experimental-cli.md)). CI checks on Windows that `playbook --version` and `playbook doctor check` run, and reports on `setup`, `uninstall` and the hooks. Not supported:

- PATH wiring and PowerShell profile wiring by `setup`, and the `pb` short name.
- `playbook upgrade`. It is refused on Windows, so download the new `.exe` yourself.
- `pbdoc` and the `doc` command, `pbsync`, and `pbtop`.
- Backslash paths in the `rm` guard.
- Whether Claude Code on Windows finds `playbook hook <name>` from `settings.json` is not checked.

The binary has no Authenticode signature. The attestation and `SHA256SUMS` are the checks.

## Upgrade

| Install | Command |
|---|---|
| Homebrew | `brew upgrade playbook` |
| Install script | `playbook upgrade`, or run the installer again (`--check` to look, `--list` for releases, `--pre` for pre-releases, `--yes` to skip the question, and to confirm in auto mode, `--json` for a report) |
| Nix | `nix profile upgrade playbook` for a profile install, or `nix flake update` where the flake is an input |

<!-- names: legacy -->
`playbook update` is an older name for `playbook upgrade`, and still works.

To update only the Claude Code plugin and its marketplace, run `playbook plugin upgrade`, then restart Claude Code or run /reload-plugins.

`playbook upgrade` shows both versions and asks you to confirm in a terminal. With no terminal and no `--yes` it prints the hint and changes nothing. After you confirm, it verifies SHA256SUMS and the attestation, keeps the old binary as `playbook.<version>.bak` (the newest three are kept), leaves a Homebrew install alone (it checks for a newer release, says Homebrew owns the binary and tells you to run `brew upgrade playbook`, and exits 0), and warns when another `playbook` earlier on `PATH` hides the new one. The binary and the plugin share one version line ([ADR 0038](../adr/0038-one-release-line.md)), and they work together while they are on the same MAJOR.MINOR. The plugin follows the binary on its own. Before every update, playbook runs `claude plugin marketplace update` for the plugin's marketplace, so Claude Code sees the newest release. If that refresh fails, the update still runs and the result line says the refresh failed. After the binary, `playbook upgrade` updates the plugin with `claude plugin update playbook@amplifiedengineer` (or `playbook@pragmatic-engineer` on an install that has not moved) and says to restart Claude Code, and `playbook migrate` does the same. After a `brew upgrade` or an automatic update, the first playbook command you run at a terminal does it, and so does the next Claude Code session, which starts it in the background and never waits for it. Each binary version is updated once. A failed attempt, or one that leaves the plugin on the old version, is tried again after 24 hours. Without the `claude` command, or when the update fails, playbook prints that command for you to run, and `playbook doctor check` warns until the plugin is on the binary's minor line. The automatic step after an upgrade is skipped in CI, in headless runs, when stderr is not a terminal (the session hook is not affected by that rule), when the plugin is turned off in Claude Code, and when `PLAYBOOK_SKIP_PLUGIN_UPDATE=1` is set. `playbook upgrade` and `playbook migrate` still update in those runs unless `upgrade.plugin` is `off`. `--no-plugin` skips the update for one run of either command. `playbook config set upgrade.plugin ask` makes the first command at a terminal ask before it updates, and `playbook config set upgrade.plugin off` turns it off, so you run `claude plugin update playbook@amplifiedengineer` yourself (`playbook@pragmatic-engineer` on an install that has not moved). When something needs you after an upgrade, the next session lists it, one bullet per item with why it matters and the command that fixes it. It shows each item once, then at most once a day. A list that `playbook migrate` printed in your terminal counts as shown: a setup item comes back after a day and a report does not come back. `playbook migrate --status` shows the list again. See [What needs you](../internals/07-migrations.md#what-needs-you). The automatic step only updates a plugin that is already installed. It never installs one and never writes `settings.json`. Installing is a choice you make in `playbook setup` (the `plugin` part, `--plugin`, `--no-plugin`, `--only plugin`): setup refreshes the marketplace, then updates the plugin, or adds the marketplace and installs the plugin when none is installed. It also sets `extraKnownMarketplaces.amplifiedengineer.autoUpdate` to `true` in `~/.claude/settings.json` when that key is missing, so Claude Code keeps the marketplace fresh. An explicit `false` stays, and a plugin on an old marketplace name is left alone (see [Moving to the new marketplace name](#moving-to-the-new-marketplace-name)). Claude Code might show a trust prompt for a marketplace added this way at user level. That has not been verified, and the plugin still installs and updates through the `claude plugin` commands without it. A binary pinned with `PLAYBOOK_REF` gets the same warning once the plugin moves to a newer minor line. To roll back, move a backup over the binary:

```bash
mv -f ~/.local/bin/playbook.0.19.0.bak ~/.local/bin/playbook
```

### Update notices and automatic updates

Playbook looks for a newer release on its own, the way Claude Code does. The `upgrade.mode` setting picks what happens:

| `upgrade.mode` | What happens |
|---|---|
| `notify` (default) | A command run in a terminal prints one line on stderr when a newer release exists: `playbook X.Y.Z is available (you have A.B.C). Run: playbook upgrade`. Homebrew installs see `brew upgrade playbook`. On an install that can update itself, the line also names `playbook config set --global upgrade.mode auto`. The line shows at most once in 24 hours for each version. |
| `auto` | Playbook installs the release in the background, with the same checks as `playbook upgrade`, and runs the new binary's migrations. The command you ran finishes on the old binary. The next command you run in a terminal prints "playbook updated from A.B.C to X.Y.Z. Release notes: <url>". |
| `off` | No check runs. |

Turn on automatic updates with:

```bash
playbook config set --global upgrade.mode auto
```

`upgrade.mode` and `upgrade.plugin` are settings for each user, so `config set` writes them to the global tier even without `--global` and refuses `--org`.

How the check works:

- A command reads the last result from `~/.config/playbook/data/config.db`. When that result is older than 24 hours, the command starts a background process that asks the GitHub releases API for the newest `vX.Y.Z` release. No command waits on the network. The request is unauthenticated, and its only identifying header is the `playbook/<version>` user agent. GitHub sees your IP address, as with any request. In `auto` mode the background process also downloads the release and checks its attestation through `gh`, the same requests `playbook upgrade` makes. Offline, rate limited or behind a proxy, the check stays silent and runs again after the next interval.
- The check does not run for hooks, the status line, `version`, `json`, `update`, the background `usage warm` and `usage collect` runs, any run with `--json`, or when a CI variable or `PLAYBOOK_HEADLESS` is set. The notice needs a terminal on stderr. An automatic update does not.
- `auto` falls back to the notice, with the reason, in these cases: a Homebrew install; a platform `playbook upgrade` does not support, such as Windows; a development or release candidate build; a release less than an hour old, or one whose publish time is unknown; an install directory you cannot write; another update in progress; a release whose notes list a breaking change, follow a release this binary has not seen, or cannot be read; and a build attestation that cannot be checked, for example when `gh` is missing. A failed automatic update prints one warning with the fix. When an attempt never runs, for example on an offline machine, the notice shows after 15 minutes.
- `playbook upgrade --check` and `playbook doctor check` show the mode, the last check and what it found.

### Coming from the playbook-hooks plugin

Older releases delivered the hooks in a second plugin, `playbook-hooks`. It came from the old marketplace, so its id keeps the old name. The hooks now live in `~/.claude/settings.json`. After you upgrade the binary, run these two commands once:

```bash
claude plugin uninstall playbook-hooks@pragmatic-engineer
playbook setup --yes
```

`playbook setup` adds the hook entries and asks first when you run it in a terminal. Leaving the old plugin installed makes every hook run twice, and `playbook doctor check` points it out.

### Coming from the shell launcher

Older setups added a line to `~/.zshrc` or `~/.bashrc` that runs `eval "$(playbook shell-init)"`, and some users wrote it by hand. The `shell-init` command is removed. Start sessions with `pb cc` or `pb ccd`, or `playbook cc` and `playbook ccd`. Delete the line from your rc file, or run `playbook setup`, which removes it after a backup (the `retired-launcher-rc-line` migration). `playbook doctor check` warns while the line is there. Until 1.0, an rc file that still runs the line prints one line on stderr when a shell starts and nothing else, so the shell still starts. Release 1.0.0 removes that notice, the `retired-launcher-rc-line` migration and the doctor warning, so remove the line before you move to 1.0.0.

See [release channels](../internals/06-release-channels.md) for how a release reaches each channel.

## How the binary gets on PATH

Every route ends with `playbook` on `PATH` for each shell start.

| Shell | File that gets the `# playbook binary` block |
|---|---|
| zsh | `~/.zshenv` (read by every zsh, including non-interactive ones) |
| bash | `~/.bashrc` and the login file (`~/.bash_profile`, `~/.bash_login` or `~/.profile`, whichever exists first) |
| fish | `~/.config/fish/conf.d/playbook.fish` |
| other | `~/.profile` |

`install.sh` and `playbook setup` write the same block and skip a file that already names the directory.

Hooks do not read these files. Measured on Claude Code 2.1.293 (2026-10-09), a hook runs in a non-interactive `bash -c` with exactly the `PATH` of the process that started Claude Code. So:

- **Started from a terminal:** the inherited `PATH` has the block's directory. Hooks find `playbook`.
- **Started with `playbook cc` or `playbook ccd`:** the launcher adds the binary's directory when it is missing.
- **Started from a desktop app or IDE:** hooks find `playbook` only if its directory is already on that `PATH`. Use `playbook cc`, or install with `PLAYBOOK_BIN_DIR` set to a directory that environment has.

## Setup flags

`playbook setup` has one flag pair per part: `--hooks`, `--statusline`, `--security`, `--path`, `--system-prompt`, `--telemetry`, `--short-name`, `--completions`, `--policy-guard`, `--attribution` and `--codex-hooks`, each with a `--no-` form. For attribution, `--attribution` is ON (Claude Code adds its trailer and session link) and `--no-attribution` is OFF, the default. `--aliases` and `--claude-override` are retired: setup accepts them, prints a note and does nothing. With `--yes` or in a terminal, every part but `--security`, `--policy-guard` and `--codex-hooks` is on by default, and attribution is OFF by default. `--yes` keeps the system prompt off when `systemPrompt.enabled` is false or on an earlier install that had none, and keeps the `pb` link off after a stored no. Without a terminal and without `--yes`, hooks, the status line, the system prompt, telemetry, the collector service and the `pb` link stay off, and attribution is left as it is, unless their flag is passed. `--only <part>[,<part>]` runs just the named parts. For example, `playbook setup --only telemetry` turns telemetry on and touches nothing else. A bare `--telemetry` (no `--yes` and no other part flag) means the same. The permissions rules (`--security`) stay off unless you pass the flag or set `security.defaults`.


## Find a command

`playbook commands` prints one line per command with its subcommands, what it does and who may run it. `(read-only)` commands only read. A `(user-only)` command, or a name ending in `!`, changes your home or Claude Code settings, needs your consent, runs a paid benchmark or starts a session, so a Claude session asks you to type it with `!` instead of running it. `playbook commands <group>`, such as `playbook commands pr`, lists the subcommands of one group with their flags, and `--json` prints the same for scripts. The list comes from the binary itself, so it names only the commands the binary has. Inside Claude Code, the `playbook:playbook-usage` skill points a session to this list. Every flag is in the [CLI and config reference](../reference/cli-and-config.md).

`playbook --help` groups the commands by audience. The first list is for you. A second list, "Agent commands", holds the commands that skills and agents call. When you run a command without a required argument or subcommand, it prints that command's full help to stderr and exits with code 2. A missing argument adds one `error:` line that names it.

## Shell completions

`playbook completions bash`, `playbook completions zsh` and `playbook completions fish` print a completion script on stdout. Homebrew ships the completion files. After a Nix install or `install.sh`, run `playbook setup` and answer yes to the completions question, or run `playbook setup --only completions` from the shell you use. Setup writes a small file for the shell in `$SHELL`, which asks the installed binary for the script, so an upgrade needs no rewrite. For zsh that file is a loader at `~/.local/share/playbook/completions.zsh` that does its work on the first Tab press, and setup also adds one marked line to `~/.zshrc` that sources it, creating that file when it is missing. `playbook uninstall` removes the loader and the line after a backup of `~/.zshrc`. It keeps the backup, so delete `~/.zshrc.bak-<stamp>` by hand when you no longer need it. Without a terminal and without `--yes`, setup skips this part. To load the script yourself instead, run `source <(playbook completions bash)` from your own shell setup. The script also completes `pb`, but only when `pb` on your `PATH` is the same file as `playbook`.

## Skills without the binary

With the skills plugin alone, some commands call the `playbook` binary. They fall back to plain git, gh and file steps where they can. No hook runs, so the guards and status line are not active, and `/playbook:doctor` reports the binary and the hook entries as missing. Install the binary and run `playbook setup` to add them.

## Use playbook's skills in Codex

```bash
playbook skills sync --dry-run   # list what would be written, change nothing
playbook skills sync             # write it
```

The command copies the playbook skills and commands into `~/.agents/skills/playbook-<name>/`, the user skill folder Codex reads. Each command becomes a skill, so `/playbook:plan` is `$playbook-plan` in Codex. It needs Codex on the machine: a `~/.codex` folder or `codex` on your `PATH`.

It never touches a folder it did not write, or one you edited. Those are left as they are and listed in the output. When the plugin updates, playbook adds, updates and removes its own folders to match, and only for an agent you already synced. A folder you deleted is not written again. To opt out, do not run the command. `playbook uninstall` removes the folders that `skills sync` wrote and you did not edit.

Codex has subagents, but playbook does not render its agents for Codex, because Codex cannot limit a subagent's tools. It also has no `AskUserQuestion` tool. When a command step needs an agent or a question, the Codex agent does the step itself or asks in chat. See the [Codex support matrix](../reference/codex-support.md). A rendered skill over 8,000 bytes is written as a short stub that tells the agent to read `references/playbook-body.md`. Whether a running Codex follows that line has not been verified. See [Codex skills](../internals/15-codex-skills.md).

## Settings merge

`playbook setup` merges the shipped template into your `settings.json` and never overwrites it. The template carries three things, each behind its own part: the `hooks` block, the `statusLine` and, only when you opt in, the `permissions` block. It ships no display options, notification toggles or `env` entries, because no hook or launcher reads them. Keys you customised stay as you set them. Keys an older template shipped (`cleanupPeriodDays`, `outputStyle`, `env` and the like) stay in your file too, and setup never removes them.

The retired `--settings` and `--no-settings` flags are gone. Use `--hooks`, `--statusline` and `--security` and their `--no-` forms instead.

It tracks a baseline in `~/.claude/.settings.base.json` and compares it with the new template and your live file. After each run, `$CLAUDE_HOME/settings-merge-skipped.<epoch>.json` lists every key where your value won, as `{"key":"...", "template_had":..., "yours":...}`. Review it and adopt template values by hand if you want them.

The security defaults, the `permissions` block, are opt-in. A plain `playbook setup` does not add them and never removes ones you have. Update control (the Claude Code autoupdater) is left to you, `playbook upgrade` and Homebrew. Pass `--security` or set `security.defaults` to true to merge them. `permissions` is one top-level key. If you customised it, the whole block counts as contested and the template's version is withheld, so your rules win. See [Security defaults](13-security-defaults.md).

If a run is interrupted between writing `settings.json` and the baseline, delete `.settings.base.json`. The next run treats the missing baseline as empty and only adds keys.

## Disabling the plugin

Disabling `playbook@amplifiedengineer` (or `playbook@pragmatic-engineer` on an install that has not moved) in Claude Code pauses playbook. Every hook, including the safety guards, exits as a silent success and the status line prints nothing, while the entries `playbook setup` wrote stay in `settings.json`. Enable the plugin again to resume. Playbook is paused when at least one of the two keys is `false` and none is `true`. With no entry (a binary-only install), a `true` key, or a `settings.json` that cannot be read, playbook runs as usual. To run hooks anyway, set `PLAYBOOK_IGNORE_PLUGIN_DISABLED=1` in the environment Claude Code starts in. See [Launcher and hooks](../internals/01-launcher-and-hooks.md#pausing-when-the-plugin-is-disabled).

## Uninstall

```bash
playbook uninstall --dry-run   # list what would go, change nothing
playbook uninstall --yes       # remove it
```

It removes only what playbook placed: the hook entries whose command is exactly one `playbook hook <name>` command that `setup` writes and status line in `settings.json`, the launcher block in your rc files, the completion files and the zsh completion line, and the files under `~/.config/playbook` that playbook copied (such as the system prompt). Without `--yes` it lists the plan and exits with status 1.

Everything else stays: the rest of `settings.json` and your rc files, `.settings.base.json`, `backups/`, memory, the [data folder](../internals/11-data-folder.md) with every database, and all runtime state. Each changed file is backed up first (`settings.json.bak.<epoch>`). A status line script or system prompt you edited by hand is kept. If `settings.json` cannot be edited, such as invalid JSON, the placed files and the binary stay too.

At the end it lists the manual steps that still apply, because playbook does not remove Claude Code's plugins, even one that `playbook setup` installed for you: `claude plugin uninstall playbook@amplifiedengineer` (or `playbook@pragmatic-engineer`, whichever is installed) while it is installed, `claude plugin uninstall playbook-hooks@pragmatic-engineer` if the retired hooks plugin is still there, and `claude plugin marketplace remove amplifiedengineer` (or `pragmatic-engineer`, whichever is listed) while the marketplace is listed. If `setup` turned on auto-update for the marketplace, set `extraKnownMarketplaces.amplifiedengineer.autoUpdate` to `false` in `~/.claude/settings.json`, or remove it.

`--remove-binary` also removes the binary, its `.bak` backups and the `PATH` block the installer added.

## Notes

Edits to `settings.json` or hooks take effect in a fresh session only. Run `playbook cc fresh` or plain `claude` after changing them. `playbook cc` warns when a resumed session runs on stale config.

## See also

- [Launcher and hooks](../internals/01-launcher-and-hooks.md)
- [The system prompt](../internals/12-system-prompt.md)
- [Docs index](../README.md)
