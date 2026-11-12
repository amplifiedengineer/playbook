# Launcher and hooks

`playbook cc` is the entry point for every session. It wraps `claude` with a system prompt, a model fallback chain, an effort ceiling, transcript retention and config-drift detection. Hooks add guards, nudges and state tracking around the session. Both are subcommands of the one `playbook` binary.

## The `playbook cc` and `playbook ccd` launcher

`playbook cc` and `playbook ccd` run `playbook cc launch` (`crates/cli/src/cc/launch.rs`) straight from the binary, and so do `pb cc` and `pb ccd`. No shell line is needed ([ADR 0044](../adr/0044-drop-the-shell-launcher.md)). `playbook ccd` is `playbook cc` with `--dangerously-skip-permissions` added. Nothing else differs.

A child process cannot change its parent's directory. After a session in a worktree that `playbook cc launch worktree <branch>` created or entered, the launcher prints the `cd` command to run, quoted when the path needs it. The `playbook shell-init` command is removed. Until 1.0, a run of it prints one line on stderr that says to delete the rc line, prints nothing on stdout and exits 0, so an old rc line does not break shell startup. The `retired-launcher-rc-line` migration removes the lines older setups wrote, after a backup, when you run `playbook setup`.

On every launch the launcher:

- Trusts `$PWD` (the equivalent of `playbook trust`), so Claude Code's trust dialog never blocks. Best effort.
- Passes `--system-prompt-file ~/.config/playbook/prompts/SYSTEM_PROMPT.md` when it is installed and `systemPrompt.enabled` is true, the default. With the key false it adds no prompt flag and passes your own prompt flags through, append flags included.
- Passes the model fallback chain with `--fallback-model`, and exports `ANTHROPIC_DEFAULT_<ALIAS>_MODEL` for any `models.*` override. See [Model tiers](02-model-routing-and-memory.md#model-tiers-and-the-55-rule).
- Passes no `--settings`. Your effort ceiling lowers the default `--effort` the launcher adds and the effort an orchestrator passes when it spawns an agent. It moves `CLAUDE_CODE_EFFORT_LEVEL` to `--effort`, so the variable cannot raise a playbook skill, command or agent above its own effort. See [Effort policy](02-model-routing-and-memory.md#effort-policy).
- Removes the throwaway `playbook-variants-<pid>` folders that earlier releases left in the temp dir (`crates/cli/src/cc/stale_variants.rs`). Effort variants of the agents are gone: an orchestrator passes the `effort` parameter on the `Agent` call instead.
- Sets `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` for that session when `memory.source` is `playbook`. It never edits Claude Code's settings or memory.
- After `claude` exits, prunes to the newest `CCD_KEEP` transcripts per project (default 5, floor 2), with their sidecars and runtime state.

A session started without the launcher, such as a plain `claude`, gets only the hooks and the status line: no playbook prompt, effort default, model overrides or memory switch. The `claude` shell function that older releases offered is gone with the shell line.

### The `pb` short name

`pb` is a symlink to the `playbook` binary. `install.sh` links `$PLAYBOOK_BIN_DIR/pb`, and the Homebrew formula uses `bin.install_symlink`. For other installs, `playbook setup --short-name` makes the same relative link in the install folder, only when no other `pb` is on your PATH, and stores the answer as `launcher.shortName`. `setup --yes` makes the link too, unless a no is stored.

### Subcommands

| Command | Behavior |
|---|---|
| `playbook cc` | Resumes the latest session for `$PWD` whose `customTitle` matches the directory name, or starts fresh. Forks a new transcript on config drift. |
| `playbook cc fresh` | New session, no history. |
| `playbook cc list` | Recent sessions for `$PWD` with timestamps and titles. |
| `playbook cc clean` | Clones the latest transcript with `/model`, `/effort`, `/config`, `/output-style` and `/style` overrides stripped, then resumes the clone. The original is untouched. |
| `playbook cc raw [id]` | Resumes verbatim: no fork, no cleanup. |
| `playbook cc launch worktree <branch>` | Creates or enters a git worktree, then starts a session there. See [Worktree engine](03-worktree.md). `playbook cc launch new` is an alias. |
| `playbook cc prune` | Prunes old transcripts now. |

### Config-drift detection

On a default resume, the launcher hashes `settings.json` and compares it to the hash stored at session start (`~/.config/playbook/cc-state/<project-slug>`). When they differ, it forks a new transcript so the fresh copy loads the current config. The `session-init` hook does the same check on `source=resume` and warns when a resumed session runs on old config.

Edits to `settings.json` or hooks take effect on a fresh session, so use `playbook cc fresh` or `playbook cc clean` after them. The hash lives in `crates/cli/src/common/config_hash.rs`.

## The hook lifecycle

Every hook is a module under `crates/cli/src/hooks/`, run as `playbook hook <name>` (see ADR 0007). `dispatch` in `crates/cli/src/hooks/mod.rs` matches every `HookName` exhaustively, so a new hook cannot be forgotten. Per-session state lives in `~/.config/playbook/runtime/<session_id>/`: counters, an `edits.jsonl` log, a `seen-reads` list, a `read-state.jsonl` log of full Reads, timestamps and the config hash baseline.

### How hooks get registered

- **5 guards** (`rm-workspace-guard`, `bg-await-guard`, `precommit-check`, `commit-message-sanitizer`, `policy-guard`) are in `settings.shared.json`, which `playbook setup` seeds or three-way merges into `settings.json`. All five are `PreToolUse` on `Bash`. `policy-guard` is also on `Edit|Write`, and is wired but allows every call until `hooks.policyGuard.enabled` is true.
  - `precommit-check` carries an `if` condition (`Bash(git commit:*)`). `rm-workspace-guard` runs for every Bash command and reads the words of each command itself.
  - The `no-slop-guard` hook is retired: `playbook hook no-slop-guard` stays a hidden no-op for one release, and the `no-slop-guard-entry` migration removes the old settings entry.
- **20 functional hooks** (`session-init`, `preread-edit-check`, `preread-size-check`, `search-counter`, `memory-anchors`, `post-edit-track`, `rebuild-memory-graph`, `auto-model-detect`, `auto-guard`, `auto-cost`, `precompact-warn`, `session-clean-exit`, `memory-capture`, `context-hygiene`, `doc-read-hint`, `usage-warm`, `handoff-on-clear`, `session-activity`, `worktree-create`, `worktree-remove`) have no static JSON. `playbook setup` runs a `wire` step (`crates/cli/src/init/wire.rs`) that upserts them from two tables, `PORTED_HOOK_SPECS` and `GUARD_SPECS`. The `PostToolUse` backstop of `commit-message-sanitizer` lives in `GUARD_SPECS` only, so `wire` adds it on every `setup`.

`setup` runs the settings step before the hooks step, so `wire` always has a `.hooks` object. `wire` rewrites only an entry whose command is exactly `playbook hook <name>`, so a repeat `setup` self-heals. A user's own `.py` or `.sh` command is never touched, so an older install's `~/.claude/hooks/<guard>.sh` entries stay next to the new bare commands until you delete them by hand. All the old shell and Python scripts are gone.

### Pausing when the plugin is disabled

`playbook setup` writes the hook entries and `statusLine` into your user `settings.json`. Disabling the plugin in Claude Code sets `enabledPlugins["playbook@amplifiedengineer"]` to `false` and leaves those entries, so playbook checks the key itself. `common::plugin_state::paused()` reads `~/.claude/settings.json` once per call, from the one place every hook (`run_hook` in `crates/cli/src/main.rs`) and `playbook statusline` go through. The old key `playbook@pragmatic-engineer` counts too, for installs that have not moved. When at least one key is `false` and none is `true`, a hook exits 0 with no output and no side effects, and the status line prints nothing.

- Only an explicit `false` pauses, and only when no key is `true`. A missing key, a `true` key, a missing file and an unreadable or unparsable file count as enabled, so Homebrew-only users and broken settings keep their guards.
- The safety guards pause too, which is intended: a disabled plugin means no playbook behaviour. The two worktree hooks always run. They stand in for Claude Code's own worktree handling, and a skipped `WorktreeCreate` hook would print no path.
- `PLAYBOOK_IGNORE_PLUGIN_DISABLED=1` runs hooks and the status line anyway. The check skips the JSON parse unless the settings text mentions the plugin key, and measured about 40 microseconds on a 7 KB `settings.json`.

### SessionStart

| Hook | Purpose |
|---|---|
| `session-init` | Creates the per-session runtime dir and zeros its counters. Clears the statusline PR/CI cache for the current branch. Checks the config hash and warns on drift when a resumed session is running on stale config. Injects `additionalContext` (list below). Read only: it never writes to the memory store. |
| `usage-warm` | Starts a detached, low priority, time bounded `playbook usage warm` when the last finished run is older than `usage.autoIngestMinutes`, so the usage store is filled before you open `playbook usage`. Returns at once with no output. Off with `usage.autoIngest` false. |

`session-init` injects the tool map (what `playbook tools map` prints, on every start including headless, ending with a pointer to `playbook commands`), an auto-learn nudge, a handoff (after `/clear` or `/compact`, the one the session saved for itself, see `context-hygiene`) and one small ranked memory block (pinned, most used, most linked facts, capped near 2,500 characters). The whole string stays under 9,000 characters because Claude Code replaces a hook string over 10,000 with a 2,000 character preview.

**The handoff loop.**

1. `context-hygiene` (UserPromptSubmit) asks the model to run `playbook handoff save --session <id>`. The file is named `<dir>_<hash>_<epoch>_<pid>_<id>.md` under `~/.config/playbook/runtime/handoff/`, keyed by directory like any handoff. `<hash>` is eight hex characters of a hash of the full path, so `my_app` and `my-app` never share a handoff. Files an older version saved as `<dir>-<epoch>-<pid>.md` are still read, and an older tagged file is not.
2. When the session ends with `/clear`, `session-clean-exit` (SessionEnd, reason `clear`) drops a marker in `handoff/cleared/`.
3. With `handoff.onClear` true (off by default), `handoff-on-clear` (SessionEnd, matcher `clear`) writes the tagged handoff when the session saved none in the last 10 minutes. It resumes the ended session with `claude -p --resume <id> --fork-session` on Haiku with no tools and no settings, for at most 50 seconds, skips a transcript over 8 MiB, and saves the reply only when it follows the handoff template. Any failure leaves the clear untouched.
4. The next `session-init` with source `clear` takes the tagged handoff whose marker is freshest. It injects at most 3,000 characters of it with the path of the full file (tombstoned by gc), and removes the marker, so it loads once.

A tagged handoff with no marker belongs to a session still running and is left alone. Tagged handoffs expire after a day and markers after an hour. Handoffs saved without `--session` behave as before. A `/compact` needs no marker, because the session id stays the same: the next `session-init` with source `compact` takes the unread handoff tagged with the current session id, with the same 3,000 character cap, and tombstones it. The compact summary and the handoff can repeat each other in part. Older unread handoffs with the same tag are tombstoned unread. A handoff tagged for another session is left alone. A compact uses up the handoff, so a `/clear` after it loads nothing (see #1392).

### PreToolUse

| Matcher | Hook | Purpose |
|---|---|---|
| `Bash` | `rm-workspace-guard` | Denies `rm`, `rmdir`, `unlink` or `shred` aimed at `/` or the home folder. Details below. |
| `Bash` | `bg-await-guard` | Warns on a backgrounded install, build or typecheck, and on a foreground long command with no timeout. Warns only; never blocks. Details below. |
| `Bash`, only `git commit` | `precommit-check` | A mechanical sanity pass over the staged diff before a commit: debug leftovers, secret-shaped filenames, an oversized commit. Warns only; never blocks. |
| `Bash` | `commit-message-sanitizer` | Removes AI attribution from commit, tag, merge and PR messages, and lints prose. Details below. |
| `Bash`, `Edit`, `Write` | `policy-guard` | Optional, off by default (`hooks.policyGuard.enabled`, [guide](../guides/08-policy-guard.md)). Details below. |
| `Read` | `preread-edit-check` | When the target file was edited by this session in the last 30 minutes, injects a reminder that the post-edit state is already in context. Info only; never blocks. |
| `Read` | `preread-size-check` | Denies a wasteful full-file read. Details below. |
| `AskUserQuestion` | `auto-guard` | Only when the resolved mode is `auto`: denies the question tool so the model takes the recommended option. See [Auto mode](../guides/04-auto-mode.md). |
| any tool | `auto-cost` | Only in auto mode: per-agent cost cap and a session ceiling. Details below. See [Auto mode](../guides/04-auto-mode.md). |
| `Read`, `Grep`, `Glob`, `Edit`, `Write`, `NotebookEdit` | `search-counter` | Tracks exploration breadth. Nudges Claude toward the Explore subagent once, at 8 unique file reads or searches. |
| `Edit`, `Write` | `memory-anchors` | Surfaces facts anchored to the target path before the edit lands. Never blocks. Details below. |

**`bg-await-guard`.** Warns when a Bash call backgrounds an install, build, or typecheck whose output a later step usually needs. Also nudges a foreground `gh run watch`, `gh pr checks --watch`, or `cargo test`/`build`/`nextest`/`clippy` that sets no timeout, to pass a longer timeout or use `run_in_background`, since the default 2 minute timeout kills it.

**`auto-cost`.** Per agent, warns at `auto.warnPct` of that agent's cap. At the cap it denies new work except reads, the closing steps (commit, push, pull request skills and commands, read-only `gh`, `playbook mode status`) and its own park note. The caller comes from `agent_id` in the hook input, and `auto.sessionCeilingUsd` stops new `Agent` spawns for the tree.

**`rm-workspace-guard`.** The target `/` or the home folder (`~`, `$HOME`, the literal path, with `/` or `/*` after it) is denied. That holds after a wrapper such as `sudo`, `env` or `command`, in a `;`, `&&`, `||` or `|` chain, in a group or subshell, and in a `bash -c` script. The line is split on separators and read word by word. `cd`, variables and `find -delete` are not followed, and anything unreadable passes. Every other path is allowed. Best-effort protection against an accidental `rm`, not a security boundary. See [ADR 0045](../adr/0045-rm-guard-roots-and-parser.md).

**`commit-message-sanitizer`** (PreToolUse):

- Removes AI attribution (a `Claude-Session:` line, a claude.ai link, a "Generated with" footer, an AI credit trailer or `--author`) from the message of a commit, tag, merge or PR. It never blocks: it returns the same command with the message cleaned and no permission decision.
- A message kept in a file is never rewritten on disk. The cleaned text is read from `-F -` or a heredoc. A recognised `git commit` also gets `-s` unless the message already has a sign-off, `commit.signOff` is false, or the repo's own hook adds it. Signing (`-S`) is left to git config. The same rule backs `playbook sanitize commit-msg <file>`.
- It also lints the prose of those messages, of PR text, and of `gh pr comment`, `gh pr review` and `gh issue` text against the writing-style phrase and structure table, and adds the findings to the model's context. `hooks.proseLint.mode` set to `block` denies the call on a hard finding. `playbook prose check` runs the same table on files. It also warns on passive voice, a soft finding that never fails `--strict`. When a file `<file>.facts` sits beside the checked file, it checks that every number, identifier, path and reference in the text appears in the facts, and it checks the text against the length limit of the `kind:` named on the first line of the facts file. Best effort, not a security boundary.

**`policy-guard`.** When off it allows every call. When on, it denies:

- skipping git hooks or signing (`--no-verify`, `git commit -n`, `--no-gpg-sign`, `-c commit.gpgsign=false`);
- a hand-run `gh pr create` (use `playbook deliver`, or `/playbook:pr-create`, which writes the title and body and opens the PR);
- a write to Claude Code's own memory (`~/.claude/projects/*/memory/`, `~/.claude/memory/`) from Bash, Edit or Write;
- every write to a Claude Code settings file (`settings.json` or `settings.local.json`, user or project): an Edit or Write, or a Bash redirect, `tee`, `sed -i`, `perl -pi`, `cp`, `mv`, `install`, `rsync`, `ln`, `dd`, `rm`, `truncate` or interpreter one-liner;
- `playbook uninstall --yes` in Bash.

It reads the command text, so heredoc scripts, script files and paths built at run time get through. Settings change through `playbook setup`, `uninstall`, `config set` and `doctor`. The user runs those with `!`. Reads are allowed. It fails safe: input it cannot read is allowed. `POLICY_GUARD=0` turns it off and `POLICY_GUARD=1` turns it on for one session.

**`preread-size-check`:**

- Denies a full-file read of a large file (over 500 lines or 60,000 bytes) with no `offset`/`limit`, pushing toward grep-first, then a targeted read. The note also tells the model to pipe big command output through `head` or `tail` and to use `gh ... --json` with `jq`.
- Images (png, jpg, jpeg, gif, webp) and PDFs are never denied, since `offset` and `limit` cannot override the guard for them. A small set of config and docs files usually needed whole is allowlisted.
- Also denies a full re-read of a file this session already read in full, when the file is unchanged (same mtime and size), has at least 150 lines, and no compaction happened since (`session-init` zeroes `read-state.jsonl` on every SessionStart, compact included). Never denies a Read with `offset`, `limit` or `pages`, a notebook, a subagent Read, or a file whose earlier read was partial. The note names Grep and offset/limit. `offset:1` with a `limit` forces the re-read.

### PostToolUse

| Matcher | Hook | Purpose |
|---|---|---|
| `Edit`, `Write`, `NotebookEdit` | `post-edit-track` | Records the edited file's absolute path and a timestamp to `edits.jsonl` in the session runtime dir. Feeds `preread-edit-check` and the statusline. |
| `Edit`, `Write`, `NotebookEdit` | `rebuild-memory-graph` | Rebuilds the graph in `~/.config/playbook/data/memory.db` after any fact-file save. No-op unless the edited file is inside `~/.config/playbook/memory`. |
| `Bash` | `commit-message-sanitizer` | The `PostToolUse` backstop for the same AI attribution removal described under `PreToolUse`. It never blocks. |

### UserPromptSubmit

| Hook | Purpose |
|---|---|
| `auto-model-detect` | Nudges the main session toward delegating design and architecture-shaped prompts (ADR, schema, tradeoff, alternatives, etc.) to an Opus subagent, rather than reasoning inline on the default model. Skips slash commands and prompts under 20 characters. |
| `auto-guard` | In auto mode with the default permission mode, advises a trusted permission mode once per session. |
| `context-hygiene` | Advisory nudge to save a handoff when the context grows large. Details below. |
| `doc-read-hint` | Advisory only. Suggests `playbook doc read <file>` on a Read of a whole PDF or Office-type file. Details below. |
| `memory-anchors` | Injects facts matched to the prompt. See the memory-anchors block below. |

**`context-hygiene`:**

- Advisory only: it never clears or compacts. It reads the input token size of the newest main-session assistant message from the tail of the transcript.
- Past `hygiene.thresholdTokens` (default 250,000; `hygiene.enabled` turns it off) it injects one message under 400 characters per crossing. The message says to save a handoff now with `playbook handoff save --session <id>`, then tell the user in one line that the handoff is saved and to run `/clear`, then `/playbook:handoff-show` if it did not load on its own. It also says the automatic load needs the SessionStart hook.
- The last level fired is kept in `hygiene-level` in the session dir. The same level never repeats, and a session that shrinks and grows again is nudged again. Any transcript problem means silence. Skipped in headless runs.

**`doc-read-hint`.** On a Read of a whole PDF or an Office, EPUB, RTF or OpenDocument file it adds one line that suggests `playbook doc read <file>`, only when `docs.convert` is `auto` and a converter is found (the configured `docs.anydocPath`, the bundled `pbdoc`, or `anydoc` on PATH). It never rewrites the Read input and never denies. It stays quiet for a PDF Read with `pages`, `offset` or `limit`, and for images, text and CSV. Paused with the rest of playbook when the plugin is disabled.

**`memory-anchors`:**

- On `Edit` or `Write` (PreToolUse), when the target path is anchored in the graph-first memory store (`~/.config/playbook/data/memory.db`), surfaces the facts that describe it, plus their `depends_on` and `contradicts` neighbours, as `additionalContext` before the edit lands. The message is capped at 600 characters (extra facts are summarized as a count) and is emitted once per file per session.
- Prompt side (UserPromptSubmit): matches prompt text and this session's touched files against the same anchor index `PreToolUse` builds. Injects each matched fact's name, description and body (body cut at 1,500 characters), deduped per session. Common words are ignored. At most 3 facts per prompt and 9 per session.
- When the prompt has searchable words and no fact matches, or every match is weak, it adds one hint line of at most 400 characters. The line names up to 6 searched terms, the repo's fact folder to grep, and the step to offer to save a fact.
- The hint counter `prompt-recall-hints.tsv` in the session directory caps hints at 5 per session. A counter that cannot be read or written means no hint. No hint for a prompt with only common words, a strong match, an empty anchor index, or matches all shown earlier. A session at its fact cap stays silent only when the prompt matches some fact. A prompt that matches nothing still gets the hint.

### PreCompact

| Hook | Purpose |
|---|---|
| `precompact-warn` | Fires when Claude Code is about to compact, auto or manual. Emits a user-visible warning and logs the event to `~/.config/playbook/runtime/compactions.log` (time, session, trigger, project, model, context tokens; see `docs/reference/hooks.md`), since `PreCompact` has no `additionalContext` channel to speak to Claude directly. |

### Stop / SessionEnd

| Event | Hook | Purpose |
|---|---|---|
| `Stop` | `session-clean-exit` | Refreshes `last-clean-ts` after every assistant turn, so a stale-session check only fires when a session is genuinely abandoned. |
| `Stop` | `memory-capture` | When the statusline has dropped a `capture-due` marker in the session dir, pauses the turn with a block decision asking the model to persist durable facts before continuing. |
| `SessionEnd` | `session-clean-exit` | Writes a clean-exit marker so the next session's `session-init` hook can tell a graceful exit from an orphaned, crashed one. |
| `SessionEnd` | `usage-warm` | The same background read, so a finished session is already stored. |
| `SessionEnd` (matcher `clear`) | `handoff-on-clear` | With `handoff.onClear` true, writes the handoff for the session that `/clear` ends. See the handoff loop. Timeout 60 seconds. |

### WorktreeCreate / WorktreeRemove

| Event | Hook | Purpose |
|---|---|---|
| `WorktreeCreate` | `worktree-create` | Replaces Claude Code's default worktree creation so worktrees land in `<main-parent>/.worktrees/<repo>/<name>`, the `playbook cc launch worktree` launcher convention, instead of `.claude/worktrees/`. Prints only the absolute path on stdout. Falls back to `.claude/worktrees/<name>` if the primary location fails. See [03-worktree.md](03-worktree.md). |
| `WorktreeRemove` | `worktree-remove` | Removes the worktree only when it is clean and every commit is on a remote-tracking ref or the main branch. Otherwise keeps it and says why on stderr. Never forces. |

### Optional: a git `commit-msg` hook

`playbook setup` does not install a git hook, because a repo may already manage its hooks (a `core.hooksPath`, husky, lefthook) and a second hook would clobber or be clobbered by them. To get the same cleanup for commits made outside an agent, add one line to the repo's own `commit-msg` hook:

```sh
playbook sanitize commit-msg "$1"
```

It rewrites the message file in place and always exits 0, even when the file cannot be read or written, so it never blocks a commit. With `--check` it reports instead: exit 1 for something it would remove or a missing `Signed-off-by` line, exit 2 when the file cannot be read, so CI can tell them apart.

## Status line

`statusLine.command` runs `playbook statusline`, the Rust renderer. `playbook setup` rewrites the old `statusline.sh` command and leaves a custom command alone. The script is gone from the repo, and `playbook uninstall` still removes a copy an older install placed.

## See also

- [Authoring commands, skills and hooks](../authoring/01-commands-skills-hooks.md): how to write your own hook.
- [Model routing and memory](02-model-routing-and-memory.md): model tiers, effort and the memory graph.
- [Docs index](../README.md)
