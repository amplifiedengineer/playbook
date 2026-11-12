# Usage sync

If you use playbook on more than one computer, `playbook sync` can copy your usage numbers between them. Then `pbtop` can show one account across both machines. The work runs in a separate helper, `pbsync`, that ships beside `playbook`. `pbsync` is a short name for the same commands. Sync is off by default.

## What sync does and does not do

- It copies usage rows (tokens and cost, with the model and effort, per event) to a database that you own, and copies the rows of your other machines back.
- It copies a small list of shareable config keys, as a proposal you accept or ignore.
- It never copies transcripts, memory, the code index or any file. It does not sync between different people.
- Installing the helper does not turn it on. A fresh install, an upgrade and `playbook doctor check` never contact a remote.
- Nothing leaves your machine until you run `playbook sync enable` and type `yes`.

## Where the data goes

The remote is a Turso Cloud database that you create and control. `playbook sync` talks to it over HTTPS. Two things tell it how:

- The database URL, which you pass once to `playbook sync enable --url`. `playbook sync` takes plain `http` only for localhost.
- The name of the environment variable that holds the database token. The default is `PBSYNC_TOKEN`, and `--token-env NAME` picks another.

Playbook stores the name of the variable and never the token. It does not print or log the token. If the variable is empty when a command needs it, the command stops with an error.

## Turn it on

```bash
export PBSYNC_TOKEN=...        # the token from your Turso database
playbook sync enable --url https://YOUR-DB.turso.io --account you@example.com
```

Before it changes anything, `enable` lists the columns it will send, the host they go to and the accounts that will sync. Nothing leaves the machine until you answer `yes`. `--yes` skips the question for a script.

`--account KEY` is repeatable. The key is the account name as `playbook usage` shows it on this machine. Run `enable` again with a new list to change the accounts.

## What leaves the machine

Only these columns of the usage rows are sent: `event_id`, `timestamp`, `account`, `repo`, `model`, `effort`, the input, output, cache creation, cache read and one-hour cache creation token counts, and `cost_usd`. Two of them change on the way out:

- `account` and `repo` go as salted 128-bit hashes. `playbook sync` makes the salt once and keeps it in the remote database, so every machine computes the same hash.
- Each row carries the id of the machine that made it. The id is a random value, not your hostname.

These are never sent: the session id, the agent, the branch, the working folder, any file path, the raw email, transcripts, memory facts and the code index. A column that is not on the list stays local, even if a later version adds it to the local table.

## Which accounts sync

The account list is empty by default, so enabling sync with no `--account` syncs nothing. A work account syncs only when you list it. Company policy still applies: check that your employer allows work usage data in a database you control before you list a work account.

Each event gets its own account, from the login in effect at that moment. One session can therefore push rows under two account hashes.

Removing an account from the list only stops new pushes. Rows already sent stay in the remote until you purge them (see below).

## What each view shows

- Limits and budgets belong to the account, and they add up across all your machines.
- Sessions and consumption belong to the machine that ran them.
- In `pbtop`, press `x` to step through your machines and then back to all of them. The machine selector filters the per-machine panels. The account panels always count every machine you synced.

## Run it

```bash
playbook sync push    # send new rows
playbook sync pull    # copy the other machines' rows
playbook sync run     # push, then pull
```

After a failure, the next run backs off for a while. `--force` runs now. A run that finds the remote salt changed stops and tells you to run `playbook sync purge` on this machine, then enable again.

Pulled rows are copies. They sit in separate local tables, and the usage views merge them with your own rows by event id, so nothing counts twice.

## Share config between machines

Only keys on a fixed list can sync: `usage.theme`, `usage.maskAccount`, `usage.weekStart`, `usage.timeZone`, the two `usage.budget` keys, `usage.ui.layout`, `effort.max`, `effort.auto`, `routing.escalate`, the three `auto.*` budget keys, `pr.draft` and `commit.signOff`. Paths, hosts, token names and safety switches never sync, and no `sync.*` key does. See [Settings](12-settings.md).

```bash
playbook sync config push     # send this machine's shared keys
playbook sync config pull     # fetch another machine's keys as a proposal
playbook sync config apply    # show the diff, ask, then apply
```

A pulled config is a proposal. It changes nothing by itself, and your local values win. `playbook sync config apply` shows the diff and applies it only when you say yes. `--yes` skips the question.

## Check the state

```bash
playbook sync status
playbook doctor check
```

`playbook sync status` makes no network call. It shows whether sync is on, the host, the accounts, whether the token variable is set, the last success and last error, the failure count, the rows waiting for a push and the schema versions. `playbook doctor check` adds one `sync` row once sync is on. It shows the age of the last success and the rows pending, and fails with a fix command when the token variable is empty.

## Stop

- `playbook sync disable` turns sync off and keeps every local and remote row. Pulled copies stay visible in `pbtop`.
- `playbook sync purge` deletes this machine's rows from the remote, clears the local copies that sync wrote and turns sync off. Your own usage data stays.
- `playbook sync purge --account KEY` removes only the rows of that account from this machine's remote rows.
- `playbook sync purge --all` deletes every machine's rows and the shared salt. It asks first, and `--yes` skips the question.

A remote failure during a purge leaves the local state as it was, so you can run it again.

## Revoke access

1. Rotate or delete the token at Turso. This is what cuts a machine off, because the provider decides what a token can reach.
2. Run `playbook sync disable` on each machine.

Data already in the remote stays there until you delete it at Turso or purge it.

## Limits you should know

- **The hashes are pseudonyms.** The salt lives in the same database as the rows. Anyone who can read the rows can read the salt, then hash a list of known emails or repo names and compare. Treat the remote as data that identifies your account, and protect it with Turso's access controls, not with the hash.
- **A clock that runs behind can hide rows.** Pull re-reads the last hour of pushes. A machine whose clock is more than one hour behind can push rows that other machines never see. Keep the clocks right.
- **Resetting the salt takes two steps.** Run `playbook sync purge --all` on one machine, then `playbook sync purge` on each of the others, then enable sync again where you want it.
- **Removing an account only stops new pushes.** Use `playbook sync purge --account KEY` to delete what was sent.

See [ADR 0061](../adr/0061-opt-in-usage-sync.md) for the decision and the threat model, [pbtop](11-pbtop.md) for the dashboard and [Telemetry](10-telemetry.md) for the local collector, which is a separate thing.
