---
description: Use after install or update, or when hooks or the system prompt seem not to run. Prints a status table of the seven playbook layers.
allowed-tools: Bash, Read
argument-hint: ""
model: haiku
effort: low
---

# Doctor

This command checks the optional playbook binary and what it wires. The plugin's skills and commands work without the binary, falling back to plain git, gh and file steps; the binary adds the safety guards, the hooks and the tool map printed at session start.

Run the checks with the `Bash` tool:

```bash
playbook doctor check
```

The binary runs all seven layer checks and the informational ones, and prints
one row per line as `LEVEL  message -- fix hint`, then a closing verdict.
Print its output as the status table. Add `--json` for the same rows as JSON
(`layer`, `level`, `message`, `hint`).

If the command fails with `command not found`, the binary is not on PATH: print
`INFO  plugin only, binary not installed: skills and commands work; guards, hooks and the tool map are off -- install the binary (see the Layer 6 hint), then run /playbook:setup` and the install
hint from Layer 6 below, and stop. If it fails with
`unrecognized subcommand 'check'`, the installed binary is older than this
plugin: print "playbook binary is older than the plugin, upgrade it with the install hint below."
and stop.

## What each layer checks

1. **Plugin enabled.** `claude plugin list` names playbook and does not show it
   disabled. Hint on a miss: `claude plugin marketplace add amplifiedengineer/marketplace && claude plugin install playbook@amplifiedengineer`. When the plugin is installed, the enable hint names the id that is listed.
2. **Safety guards wired.** `settings.json` runs the binary's hook command for the
   five PreToolUse guards (`rm-workspace-guard`, `bg-await-guard`,
   `precommit-check`, `commit-message-sanitizer`,
   `policy-guard`) and for the
   `commit-message-sanitizer` backstop on PostToolUse. A legacy `.sh` command or
   a near-miss name does not count. Fix: `playbook setup` (older versions: `playbook init`).
3. **Launcher.** `playbook cc` needs no shell line. WARN when `~/.zshrc` or
   `~/.bashrc` still has a retired launcher line, such as an old
   `source .../cc.zsh` line or `eval "$(playbook shell-init)"`. Fix: `playbook setup`
   (older versions: `playbook init`). A file that is not writable gets a WARN with a
   permissions hint. PASS otherwise, with a note when no `pb` on PATH runs this binary.
4. **System prompt (opt-in).** `~/.config/playbook/prompts/SYSTEM_PROMPT.md`
   exists. Absent is INFO.
5. **Status line command is current.** The binary's status line command passes. The retired
   `bash ~/.config/playbook/statusline.sh` fails (run `playbook setup`; older versions: `playbook init`). A custom
   command whose file is missing fails. A custom command that exists, or none at
   all, is INFO.
6. **Binary resolves.** `settings.json` runs every ported hook as a bare
   binary hook command, so the binary must be on PATH or all of them silently
   do nothing. Layer 6 prints these rows:
   - **Version row.** The binary and the plugin share one version line. The
     row passes while the plugin has the same MAJOR.MINOR as the binary on
     PATH (a different patch is normal) and the binary meets the plugin's
     `minBinaryVersion` from `plugin.json` metadata, the X.Y.0 of its line. Otherwise it warns with
     one fix command. A binary below `minBinaryVersion` or a plugin on a newer
     minor line gets the binary upgrade (`brew upgrade playbook` when the
     binary path resolves to Homebrew, else `playbook upgrade`). A plugin on an
     older minor line gets `claude plugin update` with the id that is listed (`playbook@amplifiedengineer`, or `playbook@pragmatic-engineer` on an install that kept the old name). The
     plugin is found the same way as `playbook path plugin-root`, with
     `PLAYBOOK_PLUGIN_DIR` first.
   - **Upgrade record row.** Reads the version record in `config.db` read-only
     and compares it with the `playbook` first on PATH. It warns when the last
     heal failed or the record cannot be read, with the hint
     `run: playbook migrate`. When a setup change or a report is open, it warns
     with the list `playbook migrate` prints, one bullet per item with its
     `Fix:` command, and the hint `run: playbook setup` (a setup change is open)
     or `run: playbook migrate --status`. No record yet, or an older
     one, is INFO with the same hint. The next session start or `playbook
     migrate` brings it up to date. A record newer
     than the binary (a downgrade) is INFO because nothing runs after a downgrade.
   - It also requires the quality gate flags this plugin expects, and warns when an older `playbook` earlier on
     PATH hides a newer one.

   Install hint: `curl -fsSL https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install.sh | bash`
   or `brew install amplifiedengineer/tap/playbook`, with its directory on PATH.

   After an upgrade, `playbook migrate` (binary 0.27.0 or later) brings
   playbook's own files up to date, and `playbook doctor --fix` runs the same
   step. `--status` and `--dry-run` show what is pending and write nothing.
   `playbook upgrade` runs it and prints the breaking changes of the new
   release. The install script does the same with a terminal or `--yes`,
   unless `--no-init` is set. After `brew upgrade`, or to see the breaking
   changes of releases you skipped, read the release pages on GitHub.
7. **No hook command points at a missing file.** A hook that names a path
   which does not exist fails open: it never runs and nothing says so. Bare
   binary hook commands are skipped, as is any path with an
   unresolved `$VARIABLE`. `playbook setup --hooks` (older versions: `playbook init --hooks`) removes a dead entry that runs
   a missing script under `~/.claude/hooks` (it makes a backup first). Live
   scripts and commands with shell operators stay, so delete any other stray
   entry from `~/.claude/settings.json` by hand.

Layer numbers stay as they are. Do not renumber them: users and docs refer to
them.

## Informational rows

These never fail the check and carry no fix hint:

- The effective `pr.review.*`, `pr.merge.enabled`, `commit.signOff` and
  `pr.draft` values, each as `key: value (source: tier)`.
- Stale worktrees the binary's sweep dry run reports (read the
  convention from the path: `.claude/worktrees/agent-*` is the Agent tool,
  `.worktrees/<repo>/` is the session launcher's worktree, `review-worktrees/` is
  the PR review, `repos/<owner>/<repo>/worktrees/<plan-slug>/<wu-id>` is a `/playbook:implement`
  Work Unit), or "no stale worktrees found".
- A WARN row for the Claude Code worktree guard bug, shown only when the
  directory is a linked worktree and Claude Code is the known affected version.
  Pass its hint on: call `ExitWorktree` with action `keep`.
- Files you edited after playbook placed them, as `migration <id>: <message>`,
  where `<id>` is a slug such as `system-prompt-edited`.

## Output format

Print the binary's rows unchanged, then its closing line. If a required layer
failed, the closing line names the right tool: `/playbook:setup` for layers 1 to
5, an install of the binary for layer 6 (setup cannot fix it), and a hand edit of
`~/.claude/settings.json` for layer 7.
