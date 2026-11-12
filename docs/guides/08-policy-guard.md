# The policy guard

The policy guard is the `policy-guard` hook. It denies a short list of tool calls that the model should not make on its own. It is **optional and off by default**. Nothing is denied until you turn it on.

It is a guard against an agent drifting, not a security boundary. It reads the text of a command or a file path and does not run, expand or resolve what it reads, so an obfuscated spelling passes.

## Off by default

| Question | Answer |
| :- | :- |
| Is it on after a fresh install? | No. `playbook setup` asks whether to turn it on, and the default answer is no. `--yes` leaves it off. |
| Is the hook still in `settings.json`? | Yes. The entries `playbook hook policy-guard` stay wired on `Bash` and on `Edit\|Write`, so you can switch the guard on and off without touching `settings.json`. While it is off the hook denies nothing. It still shows the memory fact format when a fact file is written to playbook's store, which is a hint and not a denial. `POLICY_GUARD=0` drops that hint too. |
| What happens on upgrade? | The guard is on only if you set `hooks.policyGuard.enabled` to true yourself (or answered yes in setup). An install that never stored the key had the guard on by default before, and now has it off. Nothing is migrated and nothing is deleted. The Auto migration `policy-guard-off-by-default` prints one notice on the first run after the upgrade (the line the upgrade prints, or `playbook migrate`), with the command that turns the guard on. `POLICY_GUARD` in the environment still decides for a session, so a shell that sets it follows the variable, whatever the notice says. It never turns it on, and it prints nothing where the key is already stored or the guard was never wired. |
| Why is it off? | It blocked ordinary work: any Edit or Write on `settings.json` or `settings.local.json` under a `.claude` folder, including a project's own `.claude/settings.json`, and any write to Claude Code's own memory folders, even with `memory.source` set to `both`, which leaves Claude Code's auto memory on. You decide whether the trade is worth it. |

## Turn it on or off

| Goal | Command |
| :- | :- |
| Turn it on | `playbook config set --global hooks.policyGuard.enabled true` |
| Turn it on during setup | `playbook setup --policy-guard`, or `playbook setup --only policy-guard`, or answer yes to the question in the wizard |
| Turn it off | `playbook config set --global hooks.policyGuard.enabled false` |
| Turn it on for one shell | `POLICY_GUARD=1` in that shell. In the `env` block of `settings.json` it applies to every session |
| Turn it off for one shell | `POLICY_GUARD=0` in that shell. In the `env` block of `settings.json` it applies to every session |
| See the state | `playbook doctor check` shows a row that says whether the guard is on |

`POLICY_GUARD` wins over the stored key in both directions. Any other value of `POLICY_GUARD` is ignored. The key is a per-user setting: `config set` writes it to the global tier and refuses `--org` and repo scope. When the guard is on, the model cannot change this key with `config set` (it is one of the protected keys below). Run the change yourself in the prompt with `!`.

The hook only runs when it is wired. If `playbook doctor check` reports the safety guards as not wired, run `playbook setup --only hooks` first.

## How it decides

Claude Code runs the hook before a tool call (`PreToolUse`) and sends the call as JSON on stdin. Setup wires one command, `playbook hook policy-guard`, on two matchers:

| Matcher | What the hook reads | Rules that apply |
| :- | :- | :- |
| `Bash` | `tool_input.command` | Everything in the Bash table below |
| `Edit\|Write` | `tool_input.file_path` | The settings file, config store and memory rules in the file table below |

The flow, in order:

1. `POLICY_GUARD=0` returns at once and allows the call.
2. With the guard off (no `POLICY_GUARD=1` and `hooks.policyGuard.enabled` not true), the call is allowed. The only thing left is the memory fact hint.
3. With the guard on, a non-empty command is checked as Bash. Otherwise a path that is a Claude Code settings file is denied, and any other path goes to the config store and memory checks.
4. A deny prints a `permissionDecision` of `deny` with a reason, and Claude Code stops the call and shows the reason to the model. Anything the hook cannot read, split or understand is allowed.

For Bash, a cheap text check runs first. A command that does not mention `git`, `gh`, `.claude`, `~`, `playbook`, `config`, `uninstall`, `cargo` or `WORKTREE_BASE_DIR` is allowed without any parsing. The hook never starts a process and never writes anything.

## What it blocks

Each row is checked only while the guard is on. Source: `crates/cli/src/hooks/policy_guard.rs` and its `policy_guard/` folder.

### Bash commands

| Rule | Denied | Allowed |
| :- | :- | :- |
| Skipping git hooks | `--no-verify` (or a long abbreviation of at least 8 characters) on `git commit`, `push`, `merge`, `rebase`, `cherry-pick` or `am`, and `-n` on `git commit`, also inside a cluster such as `-an` | The same commands without the flag, and `-n` on `git push`, which means dry run |
| Turning signing off | `--no-gpg-sign` on `git commit`, `tag`, `merge`, `rebase`, `cherry-pick`, `revert` or `am`, and `git -c commit.gpgsign=false` or `-c tag.gpgsign=false` (also `no`, `off`, `0` or empty) | Signed commits and tags |
| A hand-run `gh pr create` | `gh pr create`, also with `-R` or `--repo` before it | `playbook deliver`, `/playbook:pr-create` (which runs `playbook pr create`), `gh pr view`, `gh pr edit` and every other `gh` command |
| Memory outside playbook's store | A write to `~/.claude/memory/` or `~/.claude/projects/<project>/memory/`: a redirect into it, or `tee`, `cp`, `mv`, `rm`, `touch`, `mkdir`, `ln`, `install`, `rsync`, `dd`, `sed -i` or `perl -i` naming it | Reading those folders, and playbook's own store at `~/.config/playbook/memory/` (see the next row for how it may be written) |
| Config database, config files and the memory store | A write to `config.db`, `playbook.db`, `memory.db`, `config.json` or the playbook `data` folder (a redirect, a writer program, an interpreter, `sed -i`, `cp`, `install`, `rsync`, `dd`, `mv`, `ln` or `sqlite3` with anything but a read), and a redirect, `tee`, `sed -i`, `mv`, `ln`, `cp`, `install`, `rsync` or `dd` into `~/.config/playbook/memory/` | Reading them (`cat`, `ls`, `jq`, `sqlite3` with a `SELECT`), copying them out, `rm`, which the rm guard judges, and writing a fact with the Write tool |
| Protected config keys | `playbook config set` on `hooks.policyGuard.*`, `hooks.proseLint.*`, `pr.rules.*` or `pr.ticketPatterns`, and every `config import`, `config unset` and `config edit`. Matching ignores case. | `config get`, `config list`, `config export`, `config check`, and `config set` on any other key |
| Worktree base assignment | An inline `WORKTREE_BASE_DIR=` assignment | Setting it in your own shell or in `settings.json` |
| Claude Code settings files | A redirect, `tee`, `sed -i`, `perl -pi`, `cp`, `mv`, `install`, `rsync`, `ln`, `dd`, `rm`, `truncate` or an interpreter one-liner (`python`, `python3`, `node`, `deno`, `bun`, `ruby`, `perl`, `php`, `osascript` with a write call) aimed at a `settings.json` or `settings.local.json` in a `.claude` folder, user or project, also through a symlink or after `cd` into the folder | Reading the file, and `playbook setup`, `uninstall`, `config set` and `doctor`, which change settings for you |
| Uninstall | `playbook uninstall --yes`, also through `pb`, `cargo run`, a runner such as `env` or `sudo`, or `sh -c` | `playbook uninstall` without `--yes`, and `--dry-run` |

The denial for an uninstall, and for a settings write, tells the model to ask you to run the command with `!` in the prompt.

### Edit and Write

| Rule | Denied | Allowed |
| :- | :- | :- |
| Claude Code settings files | Any Edit or Write on a file named `settings.json` or `settings.local.json` whose folder is `.claude`, in your home or in any project, also through a symlink (a dotfiles setup that links `~/.claude/settings.json` elsewhere is covered) | Every other file, and reading |
| Claude Code's own memory | A write under `~/.claude/memory/` or `~/.claude/projects/<project>/memory/` | `~/.config/playbook/memory/`, where a fact file also shows the fact format |
| Playbook config files | A write to `config.json`, `playbook.db`, `data/config.db`, `data/memory.db` (and their `-wal` and `-shm` files) in the playbook folder, `orgs/<org>/config.json` and `repos/<owner>/<repo>/.config/config.json` | The memory facts under `memory/` |

## What it does not do

- It does not read script files, heredoc scripts that a shell reads, aliases, or paths built at run time. `git checkout` of a settings file passes.
- It does not stop a human. A command you run with `!` in the prompt is yours.
- It does not cover the `auto` mode cost guard, which is a separate hook (`auto-guard` and `auto-cost`) that protects `mode`, `auto.*` and `fix.*`.
- It does not delete anything or write anything. It only denies or adds context.
- The rm guard is separate. See [The rm guard](07-rm-guard.md).

## Example denials

A skipped hook:

```text
Skipping git hooks (--no-verify, commit -n) is not allowed. Fix what the hook reports instead. If a hook is wrong, say so and ask the user. In an unattended run, report the hook's error text and stop.
```

A hand-run `gh pr create`:

```text
Do not hand-run `gh pr create`. Use `playbook deliver` (it commits, pushes and opens or updates the PR from any worktree, `--dir <worktree>`), or the /playbook:pr-create skill, which writes the title and body and opens the PR. ...
```

An Edit of `settings.json`:

```text
Claude Code settings files (settings.json and settings.local.json, user or project) are changed through playbook, not with the Edit or Write tool. Ask the user to run `playbook setup --only hooks` (to add the hooks) or `playbook uninstall` (to remove them) themselves in the prompt with `!`. ...
```

A write to Claude Code's memory:

```text
Claude Code's own memory is off limits here. Save the fact in playbook's store instead, with the Write tool: ~/.config/playbook/memory/ for a global fact, <owner>/<repo>/ under it for a project fact.
```

A protected config key starts with `Matched rule: ...` and names the rule that matched.

## Known pitfalls

- **Project settings.** The settings rule matches `.claude/settings.json` in a project too. A teammate who asks the model to add a permission or a hook to the project settings gets a denial. Make the change yourself or turn the guard off.
- **Claude Code auto memory.** The memory rule denies writes to `~/.claude/projects/<project>/memory/` even when `memory.source` is `both`, which keeps Claude Code's auto memory on. With the guard on, Claude Code's own memory writes through the Edit or Write tools are denied. With it off, both stores work.
- **Settings through the binary only.** With the guard on, a model that needs a settings change has to ask you. `playbook config set` works for playbook keys, but not for the protected ones.
- **Text match, not meaning.** A command that names a protected path and has a redirect can be denied even when it writes somewhere else, and a command that builds the path at run time passes.
- **A broken config store turns it off.** The key is read on every call. If the store cannot be read, the guard counts as off and allows the call. `playbook doctor check` shows the state.
- **It needs the hook.** Removing the `policy-guard` entries from `settings.json` (or `playbook uninstall`) removes the guard whatever the key says. `playbook setup --only hooks` puts them back.
- **The model cannot turn it off while it is on.** The key is protected while the guard is on. While it is off, the model can set it, so ask for the guard to be on only when you mean it.
- **`POLICY_GUARD` is shared.** An exported `POLICY_GUARD=1` in your shell turns the guard on for every session that shell starts, whatever the key says.

## See also

- [Config keys](../reference/config-keys.md): `hooks.policyGuard.enabled`.
- [Hook reference](../reference/hooks.md): every hook and its switch.
- [What setup installs](05-what-setup-installs.md): the `policy-guard` setup part.
- [ADR 0031](../adr/0031-hooks-in-settings-json.md): why settings change only through the binary.
