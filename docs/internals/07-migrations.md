# Internals: Migrations

The migration registry carries an existing install across breaking changes. It lives in `crates/cli/src/init/migrate.rs`. The reasoning is in [ADR 0016](../adr/0016-migration-framework.md), [ADR 0034](../adr/0034-self-healing-upgrades.md) and [ADR 0046](../adr/0046-migrations-by-release-version.md). This page says how it behaves and what runs when you update.

## When it runs

Each migration has a scope:

| Scope | Writes |
|---|---|
| `Owned` | Only playbook's own files. |
| `Consent` | A file you own. |
| `Report` | Nothing. It reports on files Claude Code owns. |

Only `Owned` migrations run on their own:

- `playbook migrate` runs them, then prints what it did and what is left for you. `playbook doctor --fix` is the same step. `--status` and `--dry-run` show what stands and what would run, plus the same list, and write nothing.
- The first command after an upgrade runs them once, before the command, and prints one notice on stderr. Hot paths skip this so they stay fast: hooks, the status line, `version` and the hidden `json` helpers. So do `usage`, which the session hooks start in the background, the commands that run their own heal (`migrate`, `doctor --fix`, `setup` or its old name `init`), `update`, `uninstall`, and `doctor` and `state`, which show the home as it is.
- The `session-init` hook runs them once per session when the binary is newer than the record, and adds the list of what needs you to the session message.
- `playbook upgrade` runs the new binary's `migrate` after the swap. The install script runs it after an install when it has a terminal or `--yes`, and not with `--no-init`.

`playbook setup` runs every migration, `Consent` ones included, because it asks first. Nothing else writes Claude Code settings or your shell files, with one exception: `playbook doctor --fix` adds the missing `attribution` keys (attribution OFF, the default) after a yes at a terminal ([ADR 0042](../adr/0042-attribution-off-in-setup.md)).

The version record in the `state` table says which playbook version last ran the `Owned` migrations cleanly. The first-run step and the session hook run once per version by comparing it with the running build by version alone. A newer record means a downgrade, and nothing runs. `migrate` and `doctor --fix` run every time.

## What needs you

The session start, `playbook migrate`, `migrate --status`, `migrate --dry-run` and `doctor --fix` show the same list. The doctor's upgrade row shows it without the plugin items ([ADR 0034, 2026-10-19 amendment](../adr/0034-self-healing-upgrades.md#amendment-2026-10-19-one-list-of-what-needs-you)). One bullet per open `Consent` or `Report` item and per plugin problem gives what it is, why it matters and the command that fixes it. A last line gives the command that handles the most. For example:

```text
playbook 0.37.0: 2 items need you:
  - statusLine.command in settings.json still runs the old script.
    Why: the status line still runs a script that newer playbook no longer ships.
    Fix: playbook setup --statusline
  - The playbook plugin 0.36.0 is older than the 0.37 line of this binary (0.37.0).
    Why: skills and commands may call binary features that work differently in this version.
    Fix: claude plugin update playbook@amplifiedengineer
To go through every setup change at once, run: playbook setup
```

The session shows a new item in full once. After that it shows it at most once a day, in one line: `playbook: 2 items still need you (statusline-rust-command, plugin-version-range). To see the list, run: playbook migrate --status`.

- The plugin fix hint names the id that is installed. It reads `playbook@pragmatic-engineer` on an install that has not moved to the new marketplace name.
- A list that `playbook migrate` or `doctor --fix` printed in a terminal counts as shown. A setup item then waits a day, and a report item does not come back in the session.
- Captured or `--json` output does not count, and a headless session marks nothing.
- An item disappears as soon as it is fixed.
- The plugin items read `installed_plugins.json` and the folder the session runs from. A plugin update clears them. A session on an old plugin folder says to start Claude Code again.

A `playbook migrate` run, and `playbook upgrade`, also run `claude plugin update` for the playbook plugin before the list. A plugin they updated is not on the list, and they say to restart Claude Code. When the plugin needs that update and no setup change is open, the list's last line names `playbook migrate`. `--dry-run` and `--status` do not update the plugin. `--no-plugin` or `update.plugin` set to `off` turns it off. A missing `claude` command or a failed update prints the command and does not change the exit code.

Two more touch points update the plugin ([ADR 0034, second 2026-10-19 amendment](../adr/0034-self-healing-upgrades.md#amendment-2026-10-19-second-the-touch-points-update-the-plugin)). `update.plugin` is `auto` by default, so they run unless you set it to `off`:

- The first command at a terminal after an upgrade runs `claude plugin update` in the foreground, before the command, for the same commands as the first-run step. A run whose stderr is not a terminal, a `--json` call, CI and a headless run do nothing. With `ask`, it asks once for each binary version.
- The `session-init` hook starts the update in a detached child and never waits. The session shows one line saying it started, and the next session shows the result once. With `ask`, the hook adds nothing.

Both need the installed plugin to be older than the binary. One process wins a claim in the `versions/plugin-update` row of the `state` table, so the update runs once for each binary version. A failed run, or one that leaves the plugin older, is due again after 24 hours. A run that never finished is due again after 10 minutes. `PLAYBOOK_SKIP_PLUGIN_UPDATE=1` skips both.

## The three kinds

Each migration has an id, a slug such as `statusline-rust-command`, and a `since`: the release that ships it. A build runs and checks only the migrations whose `since` is at or below its own version. `playbook migrate --status` lists a later one as waiting for the next release. A waiting migration says `next`. The release pull request writes the version in its place, so a dev build of `main` never runs it early. A migration is one of three kinds:

| Kind | Runs | Recorded | What it does |
|---|---|---|---|
| Auto | Once, until it succeeds | Yes, as `applied <id>` (`migrations/applied/<id>`) | Moves or rewrites state, then never runs again. A failure is not recorded and stops the later Auto migrations, so order holds. |
| Idempotent | On every `setup`, `migrate` and heal | No | Repairs state and does nothing when there is nothing to repair. It takes no lock, and a failure in one does not block the others. |
| Manual | On every `setup` and in `playbook doctor migrations` | No | Never changes anything. It prints a warning that tells you what to do. |

`auto-budget-defaults` is the first Auto migration. The current entries include:

| Id | Since | Kind | What it does |
|---|---|---|---|
| `memory-store-move` | 0.18.0 | Idempotent | Moves the old memory store to `~/.config/playbook`. |
| `gate-repo-local-move` | 0.18.0 | Idempotent | Moves repo-local gate state to the per-repo store, only when setup runs inside a repo. |
| `system-prompt-edited` | 0.18.0 | Manual | Warns when you edited the installed system prompt. |
| `skills-edited` | 0.18.0 | Manual | Warns when you edited a cached plugin skill. |
| `statusline-edited` | 0.18.0 | Manual | Warns when you edited the old status line script. |
| `statusline-rust-command` | 0.18.0 | Idempotent | Points `statusLine.command` at `playbook statusline`. |
| `shell-init-rc-line` | 0.18.0 | Idempotent | Retired by [ADR 0044](../adr/0044-drop-the-shell-launcher.md). It reports nothing and changes nothing. |
| `remove-config-hash-script` | 0.20.0 | Idempotent | Removes the `hooks/lib/config-hash.sh` copy older installs placed under `~/.config/playbook`. The hash is computed inside the binary now. |
| `adopt-late-config-json` | 0.20.0 | Idempotent | Adopts a `config.json` written after the SQLite import when it agrees with the store, and warns with its path when it does not. |
| `state-files-to-sqlite` | 0.20.0 | Idempotent | Moves `migrations.state` and the worktree sweep markers into the `state` table. See [State store](08-state-store.md). |
| `dead-hook-scripts` | 0.34.0 | Idempotent, Consent | Removes hook entries in `settings.json` whose script under `~/.claude/hooks/` is gone, and the `playbook hook disk-guard` entry of the removed disk guard, after a backup. `playbook migrate` lists it with `playbook setup --hooks`. |
| `other-playbook-plugins` | 0.34.0 | Manual, Report | Reports the retired `playbook-hooks` plugin, or a `playbook` plugin from another marketplace next to this one, with its `claude plugin uninstall` command. |
| `plugin-version-range` | 0.34.0 | Manual, Report | Reports a plugin outside the minor line of this binary, with the command that fixes it. |
| `orphaned-plugin-caches` | 0.34.0 | Manual | Notes old plugin folders Claude Code marked for removal. Playbook ignores them and never deletes them. |
| `attribution-off` | 0.36.0 | Retired | Shipped in 0.36.0 and 0.37.0 as a Consent reminder for the `attribution` keys. Retired on 2026-10-19. A record that names it (`attribution-off` or `0015-attribution-off`) still loads, and the id is ignored. A retired slug stays in `RETIRED_IDS` and is never given to a new migration. `playbook doctor check` shows the attribution row instead. See [ADR 0042](../adr/0042-attribution-off-in-setup.md). |
| `collector-service` | 0.36.0 | Retired | Shipped from 0.36.0 as a Consent reminder to install the collector service. Retired with the service. A record that names it (`collector-service` or `0016-collector-service`) still loads, and the id is ignored. `collector-service-removed` deletes the service and the stored answers. See [ADR 0043](../adr/0043-collector-service-in-setup.md) and [ADR 0048](../adr/0048-collector-daemon-started-by-commands.md). |
| `auto-budget-defaults` | 0.36.0 | Auto | Moves a stored `auto.budgetUsd` or `auto.agentBudgetUsd` of exactly 5, the old default, to the new default once, held at a set global `auto.sessionCeilingUsd`. See [ADR 0028](../adr/0028-per-agent-auto-budget.md). |
| `retired-launcher-rc-line` | 0.37.0 | Idempotent, Consent | Removes the launcher lines older setups wrote to `~/.zshrc` and `~/.bashrc`, after a copy to `<rc>.bak-<time>`. `playbook migrate` lists it with `playbook setup`, which applies it. A hand-written `eval "$(playbook shell-init)"` at the start of a line goes too, since the command is removed. See [ADR 0044](../adr/0044-drop-the-shell-launcher.md). |
| `system-prompt-default-on` | 0.37.0 | Idempotent, Consent | Open on an earlier install with playbook hooks or the status line, no system prompt file and no stored `systemPrompt.enabled`. `playbook migrate` lists it with `playbook setup --only system-prompt`. A yes or a no in setup stores the key and closes it. See [ADR 0044](../adr/0044-drop-the-shell-launcher.md). |
| `plugin-update-mode` | 0.39.0 | Auto | Rewrites a stored `update.plugin` of `true` to `auto` and `false` to `off`, in every tier, once. A stored `ask` stays. See [ADR 0034](../adr/0034-self-healing-upgrades.md#amendment-2026-10-19-second-the-touch-points-update-the-plugin). |
| `collector-service-label` | 0.38.0 | Retired | Shipped in 0.38.0 to move the collector agent from the 0.37.0 label to the current one. Retired with the service. A record that names it still loads, and the id is ignored. `collector-service-removed` removes the agent under either label. See [ADR 0043](../adr/0043-collector-service-in-setup.md#amendment-2026-10-18-the-launchd-label). |
| `disk-guard-hook` | 0.38.0 | Retired | Shipped in 0.38.0 as a Consent reminder to wire the disk guard hook. Retired when the disk guard was removed. The id stays in `RETIRED_IDS`. `dead-hook-scripts` now also removes a `playbook hook disk-guard` entry left in `settings.json`. The hook name still runs and does nothing until then. |
| `collector-service-removed` | 0.39.0 | Idempotent | Stops and deletes every collector service file playbook wrote: the launchd agent under the current label and under the 0.37.0 label, and the systemd user unit. Then it deletes the stored answers of the retired setup part and starts the collector with no wait, so open sessions keep a receiver. A file at any of those three paths that playbook did not write (it lacks playbook's comment and label, or is not text) stays, and the warning names the commands that remove it. In a home that is not the home of your account it changes nothing and says so. |
| `policy-guard-off-by-default` | 0.39.0 | Auto | Runs once on an install that has `playbook hook policy-guard` wired and never stored `hooks.policyGuard.enabled`, which had the guard on before. It changes nothing, never turns the guard on, and prints one line: the guard is now off by default, and `playbook config set --global hooks.policyGuard.enabled true` turns it on. A fresh install, and an install that stored the key either way, get nothing. See [The policy guard](../guides/08-policy-guard.md). |
| `no-slop-guard-entry` | 0.39.0 | Idempotent, Consent | Removes the bare `playbook hook no-slop-guard` and `playbook hook no-dash-guard` entries from `settings.json` after a backup and prints one line. Only `playbook setup` (the hooks part) removes them, so `doctor --fix` and the session hook leave `settings.json` alone. Other entries and a user's own command stay. The hook is retired; its rules live in the system prompt, the `writing-style` skill and the output style, and the subcommand stays a hidden no-op for one release. |
| `rm-guard-simplified` | 0.39.0 | Auto | Runs once on an install with `rm-workspace-guard` wired. Changes nothing and prints one line: the rm guard now denies only `rm` of `/` or the home folder, and `hooks.rmGuard.allowedRoots` is no longer used. A fresh install gets nothing. See [ADR 0045](../adr/0045-rm-guard-roots-and-parser.md). |
| `code-index-per-worktree-files` | 0.40.0 | Auto | Deletes the old per worktree code index files, which the shared store replaces. It keeps a current fallback store, a file that is not SQLite and a file from a newer build. |
| `handoff-used-to-tombstone` | 0.40.0 | Idempotent | Moves handoffs from the old `used/` folder into the gc tombstone folder, where they get the same grace period and restore as any other tombstone. |

## How playbook knows you edited a file

When playbook places a file (the system prompt, a status line script), it records a hash of what it wrote in the `state` table (see [State store](08-state-store.md)), under a `migrations/shipped/<key>` key. If the file's hash later differs, you edited it. A file with no record is never reported as edited. The hash is FNV-1a 64. It detects edits and does not protect against tampering.

Playbook does not overwrite a file you edited. It leaves it, prints a warning, and names the command that takes the shipped copy: delete the file, then run `playbook setup --system-prompt` for the system prompt. The old status line script has no shipped copy, so the warning only says to delete it once nothing runs it. Skills are checked per plugin version folder, and a dev checkout (a folder with `.git`) is skipped.

## State and locking

- The record is the `state` table: one `migrations/applied/<id>` row per finished Auto migration, one `migrations/shipped/<key>` row per placed file, and the `versions/` keys:
  - `versions/binary`: the version that last ran clean
  - `versions/schema`: always 20, for older binaries
  - `versions/plugin`
  - `versions/pending`: the ids still open
  - `versions/failed-at`

  `state-files-to-sqlite` imports the old `migrations.state` file.
- Releases up to 0.37.0 named migrations by number, such as `0017-auto-budget-defaults`. A row or a pending id under one of those nineteen old ids reads as its slug. Every run that runs migrations adds a slug row next to each old applied row and keeps the old row. Applying one of them writes both rows, so an older playbook still finds the id it knows. The hook, first-run step, `migrate` and `doctor --fix` of an older playbook read the record as newer and run nothing. Its `setup` still runs its own registry and finds the numbered row of `0017-auto-budget-defaults`, so that one does not run again.
- Pending Auto migrations run under the directory lock `~/.config/playbook/migrations.lock`. The record is read again under the lock, so two sessions apply a migration once. A lock older than 300 seconds counts as stale.
- If the lock cannot be taken, Auto migrations are skipped and `setup` says so. If the record cannot be read (anything except a missing file), the run is skipped and never treated as empty.

## Add a migration

1. Append an entry to `registry()` in `crates/cli/src/init/migrate.rs` with a new slug and `since: "next",` on its own line, in exactly that form. `scripts/release-prepare.sh` replaces it with the release version and fails the release when one is left. Never move an entry, and never rename or reuse its id.
2. Pick the kind: Auto for a one-time move, Idempotent for a self-healing repair, Manual for something only you can resolve.
3. Pick the scope from the table in [When it runs](#when-it-runs). `Consent` needs a check and a `setup` command.
4. Add a test next to the existing ones in that file. A dev build skips a `next` migration, so the test calls it directly or passes a version to `shipped`.
5. A change that needs the user is a breaking change: mark its commit with `!` or a `BREAKING CHANGE:` footer that says what to do ([ADR 0037](../adr/0037-release-notes-from-conventional-commits.md)).

See also: [Launcher and hooks](01-launcher-and-hooks.md), [Docs index](../README.md).
