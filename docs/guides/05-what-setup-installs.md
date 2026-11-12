# What setup installs

`playbook setup` changes files on your machine. This page lists every change, what each part is for, how to say no, and how to undo it. Read it before you answer the questions `setup` asks. The per-hook detail is in the [hook reference](../reference/hooks.md).

## See it before you say yes

- In a terminal, `playbook setup` is a guided wizard. It shows one screen per part ("Step 3 of 8"), with what happens if you accept and what happens if you decline, a mark for the recommended answer, and the keys y, n, b (back), s (use the default) and q (quit). A summary and a confirm come last. Nothing is written before you confirm.
- `--yes` (or `--defaults`) skips the wizard and takes the defaults. Each part also has a `--x` and `--no-x` flag, and a flag wins over the wizard and shows in the summary as set by a flag.
- Without a terminal there are no questions. Only `PATH` is on: hooks, the status line, the system prompt, the `pb` link and telemetry stay off, and attribution is left as it is, unless you pass `--yes` or their own flag.
- `playbook setup --dry-run` prints exactly what would change for each part and changes nothing.
- `playbook setup --explain` describes every part. `playbook setup --explain <part>` describes one, for example `--explain hooks`.
- `playbook hooks list` prints every hook with its event, matcher and one line of purpose.
- After a real run, `setup` prints a summary of what changed and how to undo each part.

All of these read from the same tables in the code, and a test keeps the [hook reference](../reference/hooks.md) in line with them.

## What the wizard looks like

```text
Step 3 of 8  Path  (recommended: yes)
------------------------------------------------------------
Add the playbook binary's directory to PATH in your shell startup files?

If you accept:
  + Puts the playbook binary on PATH so hooks find it.
  + Adds a block that starts with `# playbook binary` and one `export PATH=...` line.
  + Undo: `playbook uninstall --remove-binary` removes the block.

If you decline:
  - Your shell files are not touched. Hooks may not find playbook unless it is already on PATH.

Do this? [Y/n, b back, s skip, q quit]
```

After the summary, each action prints one line, then the totals and the next steps:

```text
Applying
------------------------------------------------------------
[ok] hooks  wired the ported hooks into settings.json
[=] statusline  already up to date
[skip] short-name  playbook is not installed in ~/.local/bin, so no link was added; to add one, run: ln -s ...
[fail] trust  could not write ~/.claude.json

1 done, 1 unchanged, 1 skipped, 1 failed
```

Before this wizard, the command (then called `init`) printed a flat plan and one yes or no question per part, then lines like `hooks: wired - ...`.

## Output in a terminal and in a pipe

- `uninstall`, `update` and `doctor` print marked lines (`[ok]`, `[=]`, `[skip]`, `[fail]`, `[plan]`, `[warn]`, `[info]`) with a heading, totals and next steps when output goes to a terminal. A UTF-8 terminal gets symbols instead of the bracket marks.
- Color is off when `NO_COLOR` is set, `TERM=dumb`, or output is not a terminal. Text wraps to the terminal width, between 40 and 100 columns.
- A pipe, `doctor check --json` and the `/playbook:doctor` skill keep the plain text they read before.

## Privacy

- Playbook stores everything on your machine unless you turn on [usage sync](17-sync.md). It keeps its files under `~/.config/playbook/` and the files named below.
- Playbook sends nothing anywhere unless you turn on [usage sync](17-sync.md), which is off by default and needs `playbook sync enable` and a yes. No hook, and no part of `setup`, makes a network call itself, but the `session-init` hook can start the background `claude plugin update` described below. The commands that reach the network are `playbook upgrade` (it downloads a release from GitHub), `claude plugin update`, which Claude Code runs against the plugin marketplace when `playbook upgrade`, `playbook migrate`, the first playbook command at a terminal after an upgrade or the next session start brings the plugin up to the binary's version (`playbook config set upgrade.plugin off` turns it off), the background release check that `upgrade.mode` controls (one unauthenticated request to the GitHub releases API every 24 hours, whose only identifying header is the `playbook/<version>` user agent; with `upgrade.mode` set to `auto` the same background process also downloads a newer release from GitHub and checks its attestation through the `gh` tool you are logged into; `playbook config set --global upgrade.mode off` turns it off), and the status line, which calls the `gh` tool you already have to show PR and CI state.
- No hook reads a secret value. `precommit-check` looks at file names only, to flag names that look like credentials.
- `playbook usage` reads local Claude Code history. With `usage.telemetry` on `auto`, `playbook cc` points a session at a receiver on your own machine, and `setup --telemetry` can do the same for plain `claude`, the IDE and the desktop app. See [usage telemetry privacy](../internals/04-usage-dashboard.md#telemetry-privacy).
- Claude Code's own settings are yours. Playbook edits `~/.claude/settings.json` only when you agree to it in `setup` (and the plugin part's auto-update key is among those edits), and takes a timestamped backup first.

## The parts

Each part has a flag pair. With no flag, `setup` asks in a terminal and uses the default otherwise. `--yes` accepts every default, so it turns every part but the security defaults on. It keeps the system prompt off when `systemPrompt.enabled` is false or on an earlier install that had none, and the `pb` link off after a stored no. `--aliases`, `--claude-override`, `--collector-service` and `--disk-ballast` are retired: setup accepts them and does nothing, with a note on stderr when it is a terminal.

| Part | Default | Flags | Files it edits |
| :- | :- | :- | :- |
| Plugin | on, asked | `--plugin`, `--no-plugin` | Claude Code's plugin folders (written by `claude`), and `~/.claude/settings.json` (the `extraKnownMarketplaces.amplifiedengineer.autoUpdate` key) |
| Hooks | on | `--hooks`, `--no-hooks` | `~/.claude/settings.json`, `~/.claude/.settings.base.json` |
| Status line | on | `--statusline`, `--no-statusline` | `~/.claude/settings.json` |
| Security defaults | off | `--security`, `--no-security` | `~/.claude/settings.json` |
| PATH block | on | `--path`, `--no-path` | your shell startup files |
| System prompt | on, asked | `--system-prompt`, `--no-system-prompt` | `~/.config/playbook/prompts/SYSTEM_PROMPT.md`, and `~/.config/playbook` (the `systemPrompt.enabled` key) |
| Usage telemetry | on, asked | `--telemetry`, `--no-telemetry` | `~/.claude/settings.json` (the `env` block), `~/.config/playbook/telemetry-env.json` |
| `pb` short name | on, asked | `--short-name`, `--no-short-name` | the `pb` symlink in the install folder, and `~/.config/playbook` (the `launcher.shortName` key) |
| Shell completion | on, asked | `--completions`, `--no-completions` | small files in the shell's own completion folder (zsh: `~/.local/share/playbook/completions.zsh`), and for zsh one marked line in `~/.zshrc` |
| Policy guard | off, asked | `--policy-guard`, `--no-policy-guard` | `~/.config/playbook` (the `hooks.policyGuard.enabled` key) |
| Attribution | OFF, asked | `--attribution` (ON), `--no-attribution` (OFF) | `~/.claude/settings.json` (the `attribution` key), and `~/.config/playbook` (the `attribution.enabled` key) |
| Codex hooks | off, asked, only when Codex is found | `--codex-hooks`, `--no-codex-hooks` | `~/.codex/hooks.json` (two guard entries, after a backup) |

`--settings` and `--no-settings` no longer exist. Each option that part used to carry now sits behind the hooks, status line or security part, or is not shipped.

Three steps run every time and have no flag: the memory migration, the trust step and the retired config-hash cleanup. They are described below.

### Plugin

When the `claude` command is found, setup keeps the playbook plugin in step with the binary. It runs `claude plugin marketplace update`, then updates the plugin, or adds the marketplace and installs the plugin when none is installed. It also sets `extraKnownMarketplaces.amplifiedengineer.autoUpdate` to `true` in `~/.claude/settings.json` when the key is missing, and keeps an explicit `false`. It asks in a terminal and does nothing without a terminal unless you pass `--yes`, `--plugin` or `--only plugin`. A failure is reported and setup still succeeds. To undo it, run `claude plugin uninstall playbook@amplifiedengineer` and `claude plugin marketplace remove amplifiedengineer`, and set the auto-update key to `false`. Claude Code might ask you to trust a marketplace added at user level; that has not been verified. This is the only part of setup that changes Claude Code's plugins, and `playbook uninstall` never removes them.

### Hooks

Setup adds 33 hook entries to `~/.claude/settings.json`. They run 25 different `playbook hook <name>` commands. The [hook reference](../reference/hooks.md) lists each one with its event, matcher, what it reads and writes, whether it can block a tool call, and how to turn it off.

The entries fall into six groups: safety guards, session lifecycle, usage tracking and auto mode, memory, hygiene, and worktree. Only a few hooks can stop a tool call: `rm-workspace-guard`, `policy-guard` (only once you turn it on), `preread-size-check`, and (in `auto` mode) `auto-guard` and `auto-cost`. `memory-capture` can pause a stop. `worktree-create` replaces Claude Code's own worktree creation. The rest add context or warn.

While the plugin is disabled in Claude Code, every hook and the status line pause. See the [hook reference](../reference/hooks.md#how-to-turn-a-hook-off).

Setup adds or updates its own entries only. A hook you added by hand, or an event group setup does not manage, stays as it is. Running setup twice changes nothing the second time.

### Status line and the settings merge

When the hooks, status line or security part is on, setup three-way merges the shipped `settings.shared.json` into `~/.claude/settings.json`. A key you set yourself is kept. A key you never touched takes the template value. The merge records its base in `~/.claude/.settings.base.json` so the next run can tell your edits from the template's.

The template includes:

- A `statusLine` entry that runs `playbook statusline`. It reads the session data Claude Code passes on stdin, the git state of the working directory, and PR and CI state through `gh` when `gh` is installed. It caches PR lookups under your cache directory. It writes nothing to your repo.
- The `hooks` block, which is the same guards as the Hooks part.
- The `permissions` block, only when you opt in to the security defaults.

The template ships nothing else. Earlier templates also carried display options, notification toggles and `env` entries such as telemetry opt-outs and `DISABLE_AUTOUPDATER`. No hook or launcher reads them, so they are gone. A value you already have stays as it is: setup never removes a key from your file. A fresh install gets none of them, so Claude Code keeps its own defaults and your update control stays yours (`playbook upgrade` and Homebrew).

When a merge skips a key because you changed it, setup writes a `settings-merge-skipped.<time>.json` report next to the backup. Setup keeps the five newest backups and reports.

### Security defaults

Off unless you pass `--security` or set `security.defaults` to true. When on, setup also applies the template's `permissions` block (allow, ask and deny lists). Nothing else changes, and update control (the Claude Code autoupdater) stays with you, `playbook upgrade` and Homebrew. When off, setup leaves your value exactly as it is, and never removes one you already have. To go back, run `playbook setup --no-security` and delete the permissions entries from `~/.claude/settings.json` by hand.

### PATH block

Setup adds a block that starts with `# playbook binary` to your shell startup files, so hooks find the `playbook` binary. Hook commands run in a non-interactive shell that never reads `.zshrc`, so zsh gets `~/.zshenv`, bash gets `~/.bashrc` and its login file, and fish gets its own `conf.d` file. Other shells get `~/.profile`. Open a new terminal afterward. `playbook uninstall --remove-binary` removes the block.

### No shell line

Setup writes no line to your `.zshrc` or `.bashrc`. `playbook cc`, `playbook ccd`, `pb cc` and `pb ccd` start Claude Code from the binary with the playbook system prompt, the model chain and your effort ceiling. `playbook ccd` also skips permission prompts, so use it only where you would use `--dangerously-skip-permissions`. After a session in a new worktree, `playbook cc` prints the `cd` command to run. Older setups wrote a launcher line: `playbook migrate` lists it, and a full `playbook setup` removes it after a backup (the `retired-launcher-rc-line` migration). A hand-written `eval "$(playbook shell-init)"` goes too: the command is removed, and until 1.0 it only prints a line on stderr asking you to delete the rc line. See [the launcher](../internals/01-launcher-and-hooks.md).

### System prompt

`--system-prompt` (on by default; `--yes` is a yes unless the prompt was turned off or never installed, as above) copies `SYSTEM_PROMPT.md` to `~/.config/playbook/prompts/` and stores `systemPrompt.enabled` as true. Claude Code uses it only when you start it through `pb cc`, `playbook cc` or `playbook ccd`. A plain `claude` never loads it. A no, or `--no-system-prompt`, stores false, so `pb cc` starts with Claude Code's own prompt, and leaves an existing copy in place. Run setup again with a yes to turn it back on. If you already have a copy, setup refreshes it on every run, unless you edited it, in which case your copy stays. A plugin update (`playbook upgrade`, or the background update at session start) refreshes it the same way. An earlier install without the prompt gets a `playbook migrate` item (the `system-prompt-default-on` migration) until you answer.

`--telemetry` (on by default; `--yes` is a yes) writes the OpenTelemetry variables that point Claude Code at a collector on 127.0.0.1 into the `env` block of `~/.claude/settings.json`, so plain `claude`, the IDE and the desktop app feed `playbook usage` too. Setup first reads whether telemetry is on, off or not set, from your environment and every Claude Code settings file (user, project, local, managed). When it is already on, set up by you, or switched off by something setup will not change, setup says so in one line and asks nothing. When it is not set, or switched off by a lone off switch in `~/.claude/settings.json`, setup suggests turning it on as a yes or no question, and nothing is written without a yes. It writes nothing when your environment or any Claude Code settings file already names a telemetry variable, with one exception: a yes replaces an off switch that stands alone in `~/.claude/settings.json`. Any other setting you made, including values beside an off switch or in another file, is never overridden. Plain `--yes` never replaces an off switch you set; answer the question or run `playbook setup --only telemetry` for that. `DISABLE_TELEMETRY` and `DO_NOT_TRACK` are shown and left as they are, because they only turn off Anthropic's own analytics. Prompt text, tool content, emails and tokens are not sent, and the account id stays out of the metrics. The collector keeps one port (47318 unless that was taken on the first run). When settings point at it, the `session-init` hook and most playbook commands start it, and it keeps running until you run `playbook usage stop` or the machine restarts. Restart Claude Code after setup. `playbook uninstall` removes exactly the keys setup wrote, unless you changed them, and puts back an off switch that a yes replaced. Setup no longer installs a launchd agent or systemd unit for the collector. The `collector-service-removed` migration removes one that an earlier setup installed. See [telemetry by default](../internals/04-usage-dashboard.md#telemetry-by-default) and [ADR 0048](../adr/0048-collector-daemon-started-by-commands.md).

### The `pb` short name

`--short-name` asks whether `pb` should be a short name for `playbook`. `install.sh` and the Homebrew formula already make a `pb` symlink, so on those installs the part only checks it. Otherwise, when no other `pb` is on your PATH and playbook sits in the install folder (`$PLAYBOOK_BIN_DIR`, default `~/.local/bin`), setup adds the relative symlink `pb -> playbook` there, the same link `install.sh` makes. It never replaces a `pb` that runs something else, and when playbook lives anywhere else it prints the `ln -s` command to run by hand. The answer is stored as `launcher.shortName`, and a stored no keeps `--yes` from adding the link. `playbook uninstall --remove-binary` removes the link. See [the launcher](../internals/01-launcher-and-hooks.md#the-pb-short-name).

`--completions` installs tab completion for the shell in `$SHELL`. Bash and fish get a small stub for `playbook` and for `pb` in their own completion folders. Zsh has no such folder, so setup writes a loader at `~/.local/share/playbook/completions.zsh` and adds one line to `~/.zshrc`, marked with the trailing comment `# playbook completions`, that sources it when the loader is readable. The loader runs `playbook completions zsh` on the first Tab press, so shell startup costs nothing, and it prints nothing when `playbook` is missing. Setup rewrites a loader it wrote, keeps a loader or stub it did not write, and does not touch a marked line that you edited. An earlier version wrote a stub at `~/.local/share/zsh/site-functions/_playbook` and a three-line block in `~/.zshrc`. Setup removes that stub and replaces the block with the one line, after a backup `~/.zshrc.bak-<stamp>`. `playbook uninstall` removes the loader and every exact line and old block after a backup `~/.zshrc.bak-<stamp>`, and keeps that backup.

### Policy guard

`--policy-guard` (off by default; `--yes` leaves it off) stores `hooks.policyGuard.enabled` true. The `policy-guard` hook entries are always wired with the other hooks and allow every call while the key is false, so a no changes nothing and the guard can be switched later with `playbook config set --global hooks.policyGuard.enabled true` or `false`. A run without the flag never turns the guard off. [The policy guard](08-policy-guard.md) lists every rule it applies when on, the `POLICY_GUARD` switch and the known pitfalls.

### Attribution

Attribution is what Claude Code adds to commits and pull requests: a `Co-Authored-By` trailer, a generated-by line, and a `Claude-Session` trailer or claude.ai link. The part has two states.

- OFF, the default (`--no-attribution`; a no to the question gives it too, and so does `--yes` unless you chose ON before): setup sets `attribution` to `{"commit": "", "pr": "", "sessionUrl": false}` in `~/.claude/settings.json`, so Claude Code adds none of it. Setup adds only the keys that are missing. A key that holds your own value stays, unless you pass `--no-attribution`.
- ON (`--attribution`, or a yes to the question): setup removes each key that holds exactly that off value, so Claude Code adds its attribution again. Any other value stays.

The choice is stored as `attribution.enabled`, so `--yes` and `--only attribution` keep an ON. A no to the old question in 0.36 or 0.37 counts as ON. `playbook doctor` shows a pass for OFF with every key set or for ON by your choice, a warning with `playbook setup --only attribution` when OFF is the choice and a key is missing, or when ON is the choice and a key still holds the off value, and info for a value of your own. `playbook doctor --fix` asks before it adds a missing key, and a no stores ON. A project or managed settings file can still set its own value. The old flags `--attribution-off` (now `--no-attribution`) and `--no-attribution-off` (now `--attribution`) and the part name `attribution-off` still work and print a note. See [ADR 0042](../adr/0042-attribution-off-in-setup.md).

### Memory migration

Early installs kept playbook's memory in `~/.claude/memory`. On the first run, setup moves that tree to `~/.config/playbook/memory` and renames `graph.json` to `memory.graph.json`. The next tool that reads the graph folds that file into `~/.config/playbook/data/memory.db`, where the graph lives now, and renames it `memory.graph.json.migrated`. The move runs once and a `.migration-complete` marker records it. This is playbook's own store. Claude Code's auto memory under `~/.claude/projects/` is never touched. The `session-init` and `memory-capture` hooks run the same check, so a store left behind by a partial move heals itself.

### Trust step

If `~/.claude.json` exists, setup marks `~/.config/playbook` as trusted there, so a session started in that folder skips Claude Code's trust dialog. This is the one edit setup makes to `~/.claude.json`. If the file is missing or cannot be written, setup skips the step and reports it.

### Config-hash cleanup

Older installs placed `~/.config/playbook/hooks/lib/config-hash.sh`. The config hash now runs inside `playbook`, so setup removes that script once. The hash itself is a fingerprint of your Claude Code config files that `session-init` stores in the session directory, to warn when a resumed session's config has changed. It is never sent anywhere.

## Codex skills (not part of setup)

`playbook setup` never writes to `~/.agents/skills`. `playbook skills sync` is a separate command you run on purpose. It writes one `playbook-<name>/` folder per skill and per command, as real copies, and records the plugin version and a hash of each file it wrote. It skips a folder it did not write or one you edited. After the first run, a plugin update brings the synced set up to date for that agent: it adds new skills, updates unedited folders, and removes folders for skills the plugin dropped. A folder you deleted is not written again. `playbook uninstall` removes a recorded folder when it still matches the record, and keeps one you edited. See [Codex skills](../internals/15-codex-skills.md).

## Undo

- `playbook uninstall` removes the hook entries and status line setup wrote, the launcher block, the files setup placed under `~/.config/playbook`, the completion files and the zsh line, a collector service file that an older setup installed, and the Codex skill folders that `skills sync` wrote and you did not edit. Run it with `--dry-run` first to see the list. Add `--remove-binary` to remove the binary and its PATH block too. It does not stop a running usage collector, so run `playbook usage stop` first.
- Memory, your own settings, sessions, history and earlier backups are never removed.
- Backups of `settings.json` are timestamped files next to it, named `settings.json.bak.<time>`.
- Any single part can be turned off by running `setup` again with its `--no-` flag, then removing the entries by hand. Setup never deletes a value you have.
