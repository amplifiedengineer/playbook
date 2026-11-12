# Hook reference

Every hook that `playbook setup` wires into `~/.claude/settings.json`. Each command is `playbook hook <name>`. A test fails when setup wires a hook that is missing from the tables below, so this page cannot drift.

Paths below use `~/.config/playbook/` as the playbook home. The session directory is `~/.config/playbook/runtime/<session id>/`, owner-only (mode 0700). No hook reads a secret value, and no hook sends anything over the network.

## How to turn a hook off

Five ways, from narrowest to widest.

- A per-hook switch, where the last column names one. Environment switches (`POLICY_GUARD=0`; `POLICY_GUARD=1` turns the optional policy guard on) go in the `env` block of `settings.json` or your shell.
- Disable the plugin in Claude Code. That sets `enabledPlugins["playbook@amplifiedengineer"]` to `false`. The old key `playbook@pragmatic-engineer` counts too. Playbook is paused when at least one key is `false` and none is `true`. Every hook then exits with no output and no side effects, and the status line prints nothing, while the entries stay in `settings.json`. The two worktree hooks keep running, because they replace Claude Code's own worktree creation. Enable the plugin again to resume.
- Delete the hook's entry from `~/.claude/settings.json`. Setup puts it back on the next run unless you pass `--no-hooks`.
- `playbook setup --no-hooks` never adds playbook hook entries. It does not remove ones that are already there.
- `playbook uninstall` removes every entry setup wired. Add `--dry-run` to see the list first.

A hook that fails or cannot read its input allows the call. Only the hooks marked "can deny" or "can block" ever stop a tool call.

## Safety guards

| Event | Matcher | Command | What it does | Can block or change | Reads and writes | Turn off |
| :- | :- | :- | :- | :- | :- | :- |
| `PreToolUse` | `Bash` | `playbook hook rm-workspace-guard` | Stops an `rm`, `rmdir`, `unlink` or `shred` whose target is the filesystem root or your home folder, such as `rm -rf /` or `rm -rf ~`, also after `sudo`, `env` or `command`, inside a command chain, a group or a `bash -c` script. Every other path is allowed, and it does not follow `cd`, variables or `find -delete`. Runs for every Bash command. | Can deny the call with one fixed sentence. | Reads the command and `HOME`. Writes nothing. | Entry only. |
| `PreToolUse` | `Bash` | `playbook hook bg-await-guard` | Warns when a command backgrounds an install, build or typecheck that a later step needs. | Warns only. Never blocks. | Reads the command. Writes nothing. | `BG_AWAIT_GUARD=0` |
| `PreToolUse` | `Bash` | `playbook hook precommit-check` | Warns about debug leftovers, secret-shaped file names and oversized commits in the staged diff. Runs only for `git commit`. | Warns only. Never blocks. | Reads the staged diff through git. File names only for the secret check. Writes nothing. | `PRECOMMIT_CHECK=0` |
| `PreToolUse` | `Bash` | `playbook hook commit-message-sanitizer` | Removes AI attribution from commit, tag, merge and PR messages, and adds `-s` to a commit that has no sign-off. It also lints the prose of commit messages, PR text and comments against the writing-style phrase table: a warning by default, a denial when `hooks.proseLint.mode` is `block`. | Changes the command input. Blocks only in `block` prose-lint mode. | Reads the command and the `commit.signOff` and `hooks.proseLint.mode` config keys. Writes nothing. | `commit.signOff` false stops only the `-s` part. `hooks.proseLint.mode` `off` stops the lint. Otherwise entry only. |
| `PostToolUse` | `Bash` | `playbook hook commit-message-sanitizer` | Backstop for what the PreToolUse rewrite did not see. It can amend a signed commit and edit a PR. | Changes the commit or PR after the call. Never blocks. | Reads the command and the commit or PR text. Writes through git and `gh`. | Entry only. |
| `PreToolUse` | `Bash` | `playbook hook policy-guard` | Off by default (`hooks.policyGuard.enabled`, see docs/guides/08-policy-guard.md): the entry stays wired and allows every call. When on, it denies skipped git hooks or signing, a hand-run `gh pr create`, memory writes outside playbook's store, a Bash write to a Claude Code settings file (a redirect, `tee`, `sed -i`, `cp`, `mv` or an interpreter one-liner), and model-run changes to the policy guard switch, the prose lint mode or the PR rules (`config set` on `hooks.policyGuard.*`, `hooks.proseLint.*`, `pr.rules.*`, `pr.bodyMaxChars` or `pr.ticketPatterns`, `config import`, `config unset`, `config edit`, writes to the config database, a config file or the memory store (reads, copies out and deletions are allowed, since the `rm` guard stops only `/` and the home folder; `mv` and `ln` either way are writes; the denial names the rule that matched), inline `WORKTREE_BASE_DIR=`). The denial tells the model to ask the user to run it with `!`. It also denies `playbook uninstall --yes`. |
| `PreToolUse` | `Edit\|Write` | `playbook hook policy-guard` | Off by default, like the Bash entry. When on, it denies writes to Claude Code's own memory paths and to playbook's config files, and any Edit or Write on a Claude Code settings file (`settings.json` or `settings.local.json`, user or project). Settings change through the `playbook` binary only. Shows the fact format when a fact file is written to playbook's store. | Can deny the call. | Reads the target path or the command. Writes nothing. | `POLICY_GUARD=0` |

## Session lifecycle

| Event | Matcher | Command | What it does | Can block or change | Reads and writes | Turn off |
| :- | :- | :- | :- | :- | :- | :- |
| `SessionStart` | none | `playbook hook session-init` | Prepares the session directory, warns when a resumed session's config drifted, and adds the tool map, a handoff, an auto-learn nudge and a small memory block to the context. Also sweeps stale worktrees. | Adds context. Never blocks. | Reads the memory graph, saved handoffs and the to-learn queue. Writes the session directory (counters, start time, config hash). | `AUTO_LEARN_NUDGE=0` stops the nudge. `worktreeCleanup.enabled` false stops the sweep. Otherwise entry only. |
| `Stop` | none | `playbook hook session-clean-exit` | Records that the session ended cleanly after each turn, so the next start can spot a crash. | Never blocks. | Writes `last-clean-ts` in the session directory and may queue an auto-learn entry under `runtime/to-learn/`. | `AUTO_LEARN_NUDGE=0` stops the queue entry. |
| `SessionEnd` | none | `playbook hook session-clean-exit` | Writes the clean-exit marker when the session truly ends. | Never blocks. | Writes `clean-exit` in the session directory. Reads `edit-count`. | `AUTO_LEARN_NUDGE=0` stops the queue entry. |
| `UserPromptSubmit` | none | `playbook hook session-activity` | Notes that a turn started, for the working state in `playbook sessions`. Stores no prompt text. | Never blocks. Prints nothing. | Writes `activity.json` in the session directory: the turn state, the last waiting notice and a time. Reads no prompt text. | Entry only. |
| `Stop` | none | `playbook hook session-activity` | Notes that the turn ended. | Never blocks. Prints nothing. | Writes `activity.json` in the session directory. | Entry only. |
| `Notification` | `permission_prompt\|idle_prompt\|agent_needs_input` | `playbook hook session-activity` | Notes that the session waits for you, for the waiting state in `playbook sessions`. Stores the notice type only. | Never blocks. Prints nothing. | Writes `activity.json` in the session directory. The notice is one of the three fixed words, never the message text. | Entry only. |
| `PreCompact` | none | `playbook hook precompact-warn` | Warns you that the context is about to be compacted and logs the event. | Warns only. Never blocks. | Appends one tab-separated line to `~/.config/playbook/runtime/compactions.log` (newest 2000 lines kept): time, `session=`, `trigger=` (`auto` or `manual`), `project=` (the working directory name), `model=` (empty unless the payload has one), and `tokens=` (context size before the compaction, from the last usage record in the transcript; empty if unreadable). A repeat of the same time, session and trigger is not written twice, and a call with no session id is not logged. | Entry only. |
| `SessionEnd` | `clear` | `playbook hook handoff-on-clear` | When `handoff.onClear` is true, writes the session's handoff as `/clear` ends it, by resuming the session as a fork. Off by default. | Never blocks the clear. Prints nothing. | Reads `handoff.onClear`. Runs `claude` on Haiku with no tools for at most 50 seconds. | `playbook config set handoff.onClear false`. |

## Usage tracking and auto mode

The usage store is filled by `playbook usage` itself, by the `usage-warm` hook in the background, and from telemetry by `playbook usage collect`. See [usage telemetry privacy](../internals/04-usage-dashboard.md#telemetry-privacy). These hooks keep per-session counters and enforce the spend cap in `auto` mode.

| Event | Matcher | Command | What it does | Can block or change | Reads and writes | Turn off |
| :- | :- | :- | :- | :- | :- | :- |
| `PostToolUse` | `Edit\|Write\|NotebookEdit` | `playbook hook post-edit-track` | Records which files this session edited. | Never blocks. Prints nothing. | Appends the path and a time to `edits.jsonl` and bumps `edit-count` in the session directory. | Entry only. |
| `PreToolUse` | `Read\|Grep\|Glob\|Edit\|Write\|NotebookEdit` | `playbook hook search-counter` | Counts exploration calls, names the code search rule once per session at the first Grep or Glob, and suggests the Explore subagent once, at the eighth. | Adds context at most twice per session (the code hint and the Explore nudge). Never blocks. | Reads and writes `search-count` in the session directory. Creates `code-hint` in the session directory. | Entry only. |
| `PreToolUse` | `AskUserQuestion` | `playbook hook auto-guard` | In `auto` mode, denies questions to you so an unattended run does not stall. | Can deny the call, in `auto` mode only. | Reads the `mode` setting. Writes a once-only marker in the session directory. | `playbook mode ask` (the default). |
| `UserPromptSubmit` | none | `playbook hook auto-guard` | In `auto` mode, advises a trusted permission mode once per session. | Adds context. Never blocks. | Reads the `mode` setting. | `playbook mode ask` (the default). |
| `PreToolUse` | none | `playbook hook auto-cost` | In `auto` mode, tracks spend per agent against `auto.budgetUsd` (main session) and `auto.agentBudgetUsd` (each subagent), and denies new work for an agent at its cap while the closing steps stay allowed. Parks every agent at `auto.sessionCeilingUsd`. | Can deny the call, in `auto` mode only. | Reads the session transcripts and status line telemetry. Writes `auto-cost.json` in the session directory. | `playbook mode ask` (the default). |
| `SessionStart` | none | `playbook hook usage-warm` | Starts a short background read of new session history, so `playbook usage` opens with data. Prints nothing and waits for nothing. | Never blocks. Prints nothing. | Starts a detached, low priority `playbook usage warm`, which reads session transcripts the way `playbook usage` does and writes only the usage store plus `auto-ingest.lock` and `auto-ingest.stamp` in `~/.config/playbook`. It stores no transcript text beyond what the usage store already keeps. Skipped when the last run finished less than `usage.autoIngestMinutes` ago. | `playbook config set usage.autoIngest false`. |
| `SessionEnd` | none | `playbook hook usage-warm` | Starts the same background read when a session ends, so the finished session is already stored. | Never blocks. Prints nothing. | Same as above. | `playbook config set usage.autoIngest false`. |

## Memory

| Event | Matcher | Command | What it does | Can block or change | Reads and writes | Turn off |
| :- | :- | :- | :- | :- | :- | :- |
| `PreToolUse` | `Edit\|Write` | `playbook hook memory-anchors` | Shows the memory facts anchored to the file you are about to edit. | Adds context. Never blocks. | Reads `data/memory.db`. Writes an anchor index file in the session directory. | Entry only. |
| `UserPromptSubmit` | none | `playbook hook memory-anchors` | Adds the memory facts that match your prompt or the files touched this session. | Adds context. When the prompt has searchable words and matches no fact, or only weak ones, it adds one hint line of at most 400 characters (5 per session) that names the terms and says to grep the repo's fact folder. Never blocks. | Same as above. Writes a hint counter in the session directory. | Entry only. |
| `PostToolUse` | `Edit\|Write\|NotebookEdit` | `playbook hook rebuild-memory-graph` | Rebuilds the memory graph after a fact file is saved. Does nothing for any other file. | Never blocks. Prints nothing. | Reads the memory tree. Writes `data/memory.db` in one transaction. | Entry only. |
| `Stop` | none | `playbook hook memory-capture` | Once per context threshold crossing, asks the model to write down durable facts before it continues. | Can block the stop (up to two re-blocks), then releases. | Reads `edits.jsonl` and the memory graph. Writes `capture-attempts` in the session directory. Also runs the one-time memory move described in [What setup installs](../guides/05-what-setup-installs.md#memory-migration). | Entry only. |

## Hygiene

| Event | Matcher | Command | What it does | Can block or change | Reads and writes | Turn off |
| :- | :- | :- | :- | :- | :- | :- |
| `PreToolUse` | `Read` | `playbook hook preread-edit-check` | Reminds the model not to re-read a file it just edited. | Adds context. Never blocks. | Reads `edits.jsonl` in the session directory. | Entry only. |
| `PreToolUse` | `Read` | `playbook hook preread-size-check` | Denies a whole-file Read of a large file, and a repeat full Read of an unchanged file, and points to a targeted Read. | Can deny the call. | Reads file size and modification time. Writes `read-state.jsonl` in the session directory. | Entry only. |
| `PreToolUse` | `Read` | `playbook hook doc-read-hint` | When `docs.convert` is `auto` and a converter is found, suggests `playbook doc read` for PDF and Office files. It only suggests. | Adds context. Never blocks and never changes the call. | Reads config. Looks for a converter set in `docs.anydocPath` first, then the bundled `pbdoc` next to `playbook`, then `anydoc` on PATH. Writes nothing. | `playbook config set docs.convert off` |
| `UserPromptSubmit` | none | `playbook hook auto-model-detect` | Suggests `/playbook:plan`, `/playbook:adr` or `/playbook:implement` when your prompt looks like design or build work. | Adds context. Never blocks. | Reads your prompt text. Writes a once-only marker in the session directory. | Entry only. Silent in headless runs. |
| `UserPromptSubmit` | none | `playbook hook context-hygiene` | Nudges the model to save a handoff when the context grows large. | Adds context. Never blocks. | Reads the tail of the session transcript. Writes `hygiene-level` in the session directory. | `playbook config set hygiene.enabled false` |

## Worktree

`playbook setup` writes these two into `settings.json` like every other hook.

| Event | Matcher | Command | What it does | Can block or change | Reads and writes | Turn off |
| :- | :- | :- | :- | :- | :- | :- |
| `WorktreeCreate` | none | `playbook hook worktree-create` | Creates worktrees under `<repo-parent>/.worktrees/<repo>/<name>`, copies files named in `.worktreeinclude`, clones the main checkout's ignored `target` folder on macOS and keeps `origin/HEAD` at most a day old. | Replaces Claude Code's default worktree creation. | Runs git in your repo. Writes the new worktree, a copy-on-write clone of `target` and a refresh stamp inside the git directory. | Entry only. |
| `WorktreeRemove` | none | `playbook hook worktree-remove` | Removes a worktree only when nothing would be lost. Dirty trees and unpublished commits keep it; squash-merged branches count as published. | Never forces a removal. | Runs git in your repo. | Entry only. |
