---
description: Use on first install or to repair the local setup. Wires safety guards, settings.json, the pb short name and system prompt.
allowed-tools: Bash, Read, AskUserQuestion
argument-hint: "[--only <PART>...] [--yes] [--dry-run] [--explain [<PART>]] [--plugin|--no-plugin] [--hooks|--no-hooks] [--statusline|--no-statusline] [--security|--no-security] [--path|--no-path] [--system-prompt|--no-system-prompt] [--telemetry|--no-telemetry] [--short-name|--no-short-name] [--completions|--no-completions] [--policy-guard|--no-policy-guard] [--attribution|--no-attribution] [--codex-hooks|--no-codex-hooks] [--help] [--auto] [--ask]"

model: haiku
effort: low
---

# Setup

Wire the playbook binary into Claude Code: the safety guards and hooks in settings.json, the status line, PATH, the system prompt, the `pb` short name, and telemetry for `playbook usage`. Each part is safe to run again and only changes what is missing.

This command is the `playbook setup` binary command in a Claude Code session. It takes the same flags, and its questions match the binary's in text, order and defaults. Only the binary writes files. Never edit `~/.claude/settings.json`, a shell file or the playbook config yourself: every change goes through `playbook setup`, and only after the user said yes to it.

The plugin's skills and commands work without the binary. This command, like /playbook:doctor, needs the binary because it is the binary's wiring step.

In the steps below, `playbook setup` means `playbook init` when Step 1 found an older binary (older versions: `playbook init`).

## Step 0: Read the run mode

Do this first. If `command -v playbook` finds nothing, stop with one line: "playbook is not on PATH. If you just installed it, open a new terminal and run /playbook:setup again; otherwise run `curl -fsSL https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install.sh | bash` first."

If `$ARGUMENTS` holds `--help` or `-h`, run `playbook setup --help`, or the old name with `--help` when that fails with "unrecognized subcommand" (older versions: `playbook init --help`). Print its output verbatim in a text block, with nothing before or after it, and stop. The help changes nothing.

Otherwise read the run mode for this run's arguments. Intent: run-mode. If the session's tool map (printed at session start) lists it, use that command, then read `skills/delegating-subagents/references/run-context.md` in this plugin and follow it for the warning, the both-flags exit, any other failure, and an outdated binary. Otherwise do this: if `$ARGUMENTS` holds both `--auto` and `--ask`, stop with one line saying they conflict; `--auto` or `--ask` alone sets the mode; with neither, use `PLAYBOOK_MODE` when it is `auto` or `ask`; else the mode is `ask`.

If the mode is `auto`, stop here. Print one line and nothing else: "/playbook:setup needs a person to run it, because it changes your global settings and shell files outside this repo." Do not run any later step.

In `ask` mode, behave exactly as this file describes.

## Step 1: Find the command name

```bash
playbook setup --help >/dev/null 2>&1 && echo setup || echo init
```

`setup` means the binary has the command. `init` means the binary predates the rename: use the old name with the same flags for the rest of this run (older versions: `playbook init`). The two names run the same code.

## Step 2: Read the arguments

Take `$ARGUMENTS` word by word:

- Drop `--auto` and `--ask`. Step 0 read them, and the binary does not take them.
- Replace the old name `--use-system-prompt` with `--yes --system-prompt`. Say so in one line.
- Replace the old names from 0.36 and 0.37: `--attribution-off` with `--no-attribution` (attribution OFF), `--no-attribution-off` with `--attribution` (attribution ON), and `attribution-off` inside an `--only` or `--explain` value with `attribution`. When an `--only` value names neither `attribution` nor `attribution-off`, drop `--no-attribution-off` instead, as the binary does. Say so in one line.
- Drop the retired words `--install-aliases`, `--aliases`, `--no-aliases`, `--claude-override` and `--no-claude-override`, and the names `aliases` and `claude-override` inside an `--only` value. Say in one line that they are retired and that `playbook cc` and `pb cc` need no shell line (ADR 0044).
- Drop the retired words `--disk-ballast` and `--no-disk-ballast`, and the name `disk-ballast` inside an `--only` value. Say in one line that the disk feature was removed.
- Drop the retired words `--collector-service` and `--no-collector-service`, and the name `collector-service` inside an `--only` value. Say in one line that the collector service is no longer needed, as playbook starts the collector itself.
- When an `--only` value had only retired names, stop there and run nothing.
- Every other word must be a flag from the Flags table below, or the value after `--only` or `--explain`. A value is part names from the Questions table or `security`, joined by commas. When a word is neither, stop with one line that names it and points to `/playbook:setup --help`. Ask nothing and run nothing.

The rest of this file calls the kept words the arguments. A part flag is any flag in the Flags table from `--plugin` to `--no-attribution`. When you build a command, put each argument in single quotes.

## Step 3: Runs that ask nothing

These runs ask no question in a terminal either, so pass the arguments straight to the binary:

- The arguments hold `--dry-run` or `--explain`: run `playbook setup` with the arguments, print the output verbatim, and stop. Neither changes any part. When the arguments hold `--dry-run` and none of `--yes`, `-y`, `--defaults` or `--only`, add one line after the output: "This session has no terminal, so this preview shows the plugin, the hooks, the status line, the system prompt, telemetry and the pb short name off unless a flag turns them on, and leaves attribution as it is unless a flag sets it. Answering the questions with the defaults turns them on and sets attribution OFF."
- The arguments hold `--yes`, `-y`, `--defaults` or `--only`, or `--telemetry` with no other part flag (a bare `--telemetry` means `--only telemetry`): the user already chose. Go to Step 6 with the arguments as they are.

Otherwise go on to Step 4.

## Step 4: Ask the questions

`playbook setup` in a terminal asks one question per part, in the order of the Questions table below. It skips a part when the arguments hold that part's `--x` or `--no-x` flag. Do the same:

1. Print the welcome text, verbatim: "This walks through each change playbook can make to your machine. Nothing is written until you confirm at the end. playbook never changes Claude Code's memory, and it edits settings.json only for the parts you accept."
2. Run the probes in one Bash call. None of them changes any part:

   ```bash
   playbook setup --dry-run; echo '--- explain ---'; playbook setup --explain; echo '--- plugin ---'; playbook setup --dry-run --yes | grep -A1 '^plugin'; echo '--- security ---'; playbook config get security.defaults; echo '--- attribution ---'; playbook config get attribution.enabled
   ```

3. Read the plugin probe. A line `plugin (off)` means setup is blocked from touching the plugin here (`upgrade.plugin` is off, a CI environment, or `PLAYBOOK_SKIP_PLUGIN_UPDATE` is set). Print "plugin: left as it is, because setup is blocked here. To run it anyway: playbook setup --only plugin", drop the plugin question and add no plugin flag. Otherwise the plugin default is Yes.
4. Read the dry run output. A line that starts with `telemetry: already` or `telemetry: switched off` means setup would not ask about telemetry. Print that line and drop the telemetry question.
5. Read the explain output. For each part it prints a `What it does:` line.
6. Ask the remaining questions with the AskUserQuestion tool, in table order, at most four questions per call. For each question:
   - The header and the question are the Header and Question columns, verbatim.
   - There are two options, Yes and No. The default option comes first, and its label ends with " (Recommended)". For `attribution`, the default is Yes when the attribution probe printed true, because the user chose ON before.
   - The Yes description is the part's `What it does:` line from sub-step 5.
   - The No description is the If no column.
   - An answer whose label starts with Yes means the part's `--x` flag. An answer whose label starts with No means its `--no-x` flag. `--plugin` forces the plugin past `upgrade.plugin` off and CI, so pass it only for an answer the user gave, never for a dropped question or a `--yes` run.

## Step 5: Summary and confirm

Print "Summary of your choices", then one line per part in table order: the part, on or off, and "will be applied" or "left as it is", plus "(set by a flag)" when the arguments decided it. For `attribution`, print "ON: Claude Code adds its attribution" for a yes and "OFF (default): Claude Code adds no attribution" for a no, in place of on or off and the words after it. For a telemetry question dropped in Step 4, print the dry run line from sub-step 4 in place of on or off. For a plugin question dropped in Step 4, print "plugin: off, left as it is (blocked here)". Add the line `security: on, will be applied` when the arguments hold `--security`, or when `security.defaults` printed true and the arguments do not hold `--no-security`.

Build the command: `playbook setup`, then the arguments, then for each answered part its `--x` or `--no-x` flag. When Step 4 dropped the telemetry question and the arguments hold no telemetry flag, also add `--telemetry`: a terminal run keeps telemetry on in that case, and setup still leaves a setting of yours alone. Print the command in a bash block.

Ask "Apply these changes?" with the AskUserQuestion tool, header "Apply", options "Yes (Recommended)" and "No". On No, print "Cancelled. Nothing was changed." and stop.

Every asked part now has a flag, so the binary writes what the user chose even though this session has no terminal.

## Step 6: Run

Run the command, then check the tools the plugin needs and install only the missing ones:

```bash
playbook setup <the flags from Step 3 or Step 5, each in single quotes>
playbook deps ensure --builtin
```

The binary carries its own settings template, system prompt and Brewfile, so neither command needs a plugin file. Setup also marks `~/.config/playbook` as trusted in an existing `~/.claude.json`, so Claude Code's trust dialog never blocks that folder.

When the setup command fails, check these cases in order:

1. It fails with "unexpected argument": the installed binary is older than the plugin. Run `playbook setup --help`, remove every flag it does not list, and run the command once more. Tell the user which flags were left out and to upgrade with `curl -fsSL https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install.sh | bash`.
2. It reports the settings step skipped with "no template shipped": the binary is also older than the plugin. Give the same upgrade advice, because no guards were wired.
3. Any other non-zero exit: show the error verbatim, still run the second command, then report which setup steps failed.

If `playbook deps ensure --builtin` fails with "unrecognized subcommand" or "unexpected argument", skip it and say so.

## Step 7: Report

Report the output of both commands verbatim. Then say in one line what this run changed. `playbook setup` prints one line per step with its status (for example, `[=] statusline  already up to date` when nothing changed), then the totals and next steps.

End your report with:

"Re-running /playbook:setup is safe and only changes what is missing. Run /playbook:doctor to
verify the full status."

## Flags

The same flags as `playbook setup`, with the same text as `playbook setup --help`. A test in `crates/cli/tests/setup_command_parity.rs` keeps this table in step with the binary.

| Flag | What it does |
| --- | --- |
| `--plugin` | Install or update the playbook plugin in Claude Code with the `claude` command. Given by name it runs even without a terminal, with `upgrade.plugin` off or in CI |
| `--no-plugin` | Leave the playbook plugin as it is |
| `--hooks` | Wire every playbook hook into settings.json (guards, session and worktree hooks) |
| `--no-hooks` | Add no playbook hook entries, including the guards in the shared settings |
| `--statusline` | Use the playbook status line (sets statusLine.command to `playbook statusline`) |
| `--no-statusline` | Do not set or change statusLine in settings.json |
| `--security` | Also apply the security defaults (permissions rules). Off unless asked or `security.defaults` is true |
| `--no-security` | Do not apply the security defaults, even when `security.defaults` is true |
| `--path` | Put the playbook binary on PATH for every shell start |
| `--no-path` | Do not edit shell files for PATH |
| `--system-prompt` | Install the playbook system prompt (an existing copy is refreshed either way) |
| `--no-system-prompt` | Do not install the system prompt. It stores `systemPrompt.enabled` false so `pb cc` starts with Claude Code's own |
| `--telemetry` | Answer yes to the telemetry question. Writes the OpenTelemetry variables for the local usage collector into settings.json. Given alone it means `--only telemetry` |
| `--no-telemetry` | Do not write telemetry variables into settings.json |
| `--short-name` | Add `pb` as a short name for `playbook`. Makes a `pb -> playbook` symlink in the install directory, only when `pb` is free |
| `--no-short-name` | Do not add the `pb` short name |
| `--completions` | Install tab completion for `playbook` and `pb` in the shell named by `$SHELL` |
| `--no-completions` | Do not install shell completion |
| `--policy-guard` | Turn the policy guard on (`hooks.policyGuard.enabled`). It denies skipped git hooks, a hand-run gh pr create, Claude Code memory writes and settings.json edits |
| `--no-policy-guard` | Do not turn the policy guard on; it is off by default and a stored choice is not changed |
| `--attribution` | Attribution ON: Claude Code adds its Co-Authored-By trailer, generated-by line and session link. Removes each `attribution` key in settings.json that holds exactly the off value playbook writes, and stores `attribution.enabled` true |
| `--no-attribution` | Attribution OFF. Claude Code adds no trailer, generated-by line or session link. Sets `attribution` in settings.json, replacing your own values. Setup applies this when you choose nothing and have not chosen ON before |
| `--codex-hooks` | Register the playbook Bash guards in the hooks.json of Codex (off unless asked) |
| `--no-codex-hooks` | Do not register guards in Codex |
| `--only <PART>...` | Run only these parts and ask nothing. Parts, comma separated: plugin, hooks, statusline, security, path, system-prompt, telemetry, short-name, completions, policy-guard, attribution, codex-hooks. Everything else stays untouched. `playbook setup --only telemetry` turns telemetry on, and `--only attribution` applies attribution OFF unless you chose ON |
| `-y`, `--yes`, `--defaults` | Never ask: use the default for every part you did not answer with a flag |
| `--dry-run` | Print what would change for each part and change nothing |
| `--explain [<PART>]` | Describe a part, or every part when none is named, and change nothing |
| `-h`, `--help` | Print help (see a summary with '-h') |

This command also reads `--auto` and `--ask` (the run mode, Step 0), the old name `--use-system-prompt`, which it turns into the flags it stood for, and the retired launcher words (Step 2), which it drops. It passes none of these to the binary.

## Questions

The questions `playbook setup` asks in a terminal, in order, with defaults. A test keeps this table in step with the binary.

| Order | Part | Header | Question | Default | If no |
| --- | --- | --- | --- | --- | --- |
| 1 | `plugin` | Plugin | Install or update the playbook plugin in Claude Code with the `claude` command? Claude Code keeps its plugins in ~/.claude/plugins. | Yes | The plugin is left as it is. Install or update it later with `playbook setup --only plugin`. |
| 2 | `hooks` | Hooks | Wire the playbook hooks into ~/.claude/settings.json? (see them with `playbook hooks list`) | Yes | No hooks are added. The safety guards, session helpers and usage warm-up do not run. |
| 3 | `statusline` | Status line | Set the status line in ~/.claude/settings.json to `playbook statusline`? | Yes | Your status line stays as it is. |
| 4 | `path` | PATH | Add the playbook binary's directory to PATH in your shell startup files (~/.zshenv for zsh, ~/.bashrc and a login file for bash)? | Yes | Your shell files are not touched. Hooks may not find playbook unless it is already on PATH. |
| 5 | `system-prompt` | Prompt | Copy the playbook system prompt to ~/.config/playbook/prompts/SYSTEM_PROMPT.md? | Yes | No file is copied, and `systemPrompt.enabled` is set to false, so `pb cc` starts with Claude Code's own prompt. |
| 6 | `telemetry` | Telemetry | Turn on Claude Code telemetry by writing the OpenTelemetry variables for the local usage collector into the env block of ~/.claude/settings.json, so plain claude, the IDE and the desktop app feed `playbook usage`? (skipped with one line when telemetry is on with every key in place, set up by you, or switched off by a setting `playbook setup` will not change) | Yes | No variables are written. `playbook usage` reads transcripts instead. |
| 7 | `short-name` | Short name | Add `pb` as a short name for `playbook`, as a symlink `pb -> playbook` in the playbook install folder (~/.local/bin or $PLAYBOOK_BIN_DIR)? No link is made when a `pb` command already exists. | Yes | No `pb` link is added, and an existing one stays. Use `playbook` in full where `pb` is missing. |
| 8 | `completions` | Completions | Install tab completion for `playbook` and `pb` in your shell? It adds small files (bash: ~/.local/share/bash-completion/completions, fish: ~/.config/fish/completions, zsh: ~/.local/share/playbook/completions.zsh) and, for zsh, one marked line in ~/.zshrc, created when missing. | Yes | No completion files are written, and your shell files are not touched. Tab does not complete `playbook` or `pb` until you run `playbook setup --only completions`. |
| 9 | `policy-guard` | Policy guard | Turn on the policy guard? It denies git commands that skip checks or signing, a hand-run `gh pr create`, memory writes outside playbook's store, Edit or Write on settings.json and settings.local.json, writes to Claude Code's own memory folders, and model-run changes to protected config keys. The choice is stored in ~/.config/playbook. | No | The policy guard setting is not changed. It is off by default, and its hook entry allows every call while off. A guard you turned on stays on until `hooks.policyGuard.enabled` is set to false. Turn it on later with `playbook config set --global hooks.policyGuard.enabled true`. |
| 10 | `attribution` | Attribution | Turn attribution ON, so Claude Code adds its Co-Authored-By trailer, generated-by line and claude.ai session link to commits and pull requests? The default is OFF: playbook sets `attribution` in ~/.claude/settings.json so Claude Code adds none of them. | No | Attribution is OFF, the default: playbook adds the missing `attribution` keys to ~/.claude/settings.json and keeps your own values, so Claude Code adds no trailer, generated-by line or session link. |
| 11 | `codex-hooks` | Codex hooks | Register the playbook Bash guards in Codex? It adds the rm-workspace-guard and precommit-check entries to ~/.codex/hooks.json. Codex skips them until you trust them in /hooks. | No | ~/.codex/hooks.json is not touched. Entries from an earlier run stay until `playbook uninstall`, and without them the playbook guards do not run in Codex. Register them later with `playbook setup --only codex-hooks`. |

The plugin question is dropped, with its default shown as No in the summary, when the plugin probe prints `plugin (off)` (Step 4, sub-step 3).

The security part is never asked about, in a terminal or here. It runs only with `--security` or when `security.defaults` is true.
