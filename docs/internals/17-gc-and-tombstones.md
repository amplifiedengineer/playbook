# Cleanup, tombstones and `playbook gc`

Playbook keeps state on disk: session folders, logs, databases, handoffs, worktrees and the build output inside them. `playbook gc` and a daily sweep keep that state from growing without bound. The code is in `crates/cli/src/gc/`.

## What it does

`playbook gc` prints, for each store, its size, item count, age and the action a cleanup would take. It changes nothing and takes no lock.

`playbook gc --yes` runs one cleanup: it repairs moves that a crash cut short, applies each store's rule, deletes tombstones past their grace period and compacts the tombstone index. `playbook gc --restore <id>` puts a tombstoned item back where it came from. `playbook gc --build-output` lists build output in hand made worktrees of the current repo and asks before deleting (see "Build output").

The first session start of each day runs a slice of the same cleanup, unless `gc.enabled` is `false`. It checks the 20 second budget between stores, not inside one, so a single large store can run past it. It picks up at the next store on the next start. It stamps the day only when it has been through every store.

These numbers are constants in `crates/cli/src/gc/mod.rs`, not config keys. The twelve `gc.*` keys that once set them are retired: a stored value is ignored and the `fixed-config-keys` migration reports it. `gc.enabled` stays.

## The lifecycle of a tombstone

Most files are not deleted when a rule says they are due. They move to `gc/tombstones/<date>/<id>/` and a line in `gc/index.jsonl` records where they came from. After the item's grace period (7 days, or 14 days for handoffs) a later run deletes the copy for good. Until then `playbook gc --restore <id>` brings it back.

The index is an append-only file, one line per state change, and the last line for an id wins. A move writes `pending`, moves the data, then writes `done`. A crash between the steps leaves a line that the next run finishes or drops, so the index and the disk agree again. A run deletes at most 500 tombstones, and a record dated in the future is kept.

Everything that removes data takes the run lock (`gc/.lock`) and goes through one guard that refuses any path outside a registered store root.

## Stores

| Store | What it holds | Rule | Action |
| --- | --- | --- | --- |
| session folders | `runtime/<session>` | idle for 14 days (newest nested change) | tombstone |
| migration leftovers | `.copied` markers and `MOVED.txt` notes | older than 90 days, and only once the old files they guard are gone | tombstone |
| gate databases | gate databases of removed worktrees of the current repo | idle for 30 days | tombstone, with the `-wal` and `-shm` files |
| logs | `collector.log`, `collector-service.log` | over 5 MiB or older than 30 days | copy to `<log>.1`, then truncate; a rotation replaces an earlier `<log>.1` |
| build output | `target`, `node_modules` and the other table folders in worktrees | see below | delete |

Loaded handoffs are tombstoned when a session start loads them, and worktrees follow the sweep rules in [the worktree engine](03-worktree.md). The sweep removes a worktree only through the gc guard.

`collector-service.log` is only reported until the service manager is known to open it in append mode. Memory facts, plans, `usage.db` and global caches are reported or left alone and never removed by gc.

## Build output

Build output is regenerable, so it is deleted directly, never tombstoned. A folder is deleted only when all of these hold:

- its name equals a table entry byte for byte (`Target` is not `target`);
- it is a real folder, not a symlink, with no `.git` of its own;
- a marker file of the same ecosystem sits in its parent or an ancestor inside the worktree;
- git ignores it and tracks nothing in it;
- the worktree was created by playbook, or `--build-output` was confirmed;
- nothing outside the folder changed for 7 days, nothing inside it changed within 15 minutes, and no process has it open.

When free space is under 10 GiB or under 5 percent of the volume, the idle limit drops to 1 day, oldest first, until space recovers. The primary checkout is never scanned. Virtual environments (`.venv`, `venv` and any folder with a `pyvenv.cfg`) are out of v1: they are not rebuilt by a plain build, so gc leaves them alone.

### The ecosystem table

| Ecosystem | Markers | Folders |
| --- | --- | --- |
| Rust | `Cargo.toml` | `target` |
| Node | `package.json` | `node_modules`, `.next`, `.turbo` |
| Python | `pyproject.toml`, `setup.py`, `requirements.txt` | `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache`, `.tox` |
| Java and Kotlin | `pom.xml`, `build.gradle`, `build.gradle.kts` | `target`, `.gradle` |
| .NET | `*.csproj`, `*.sln` | `obj` |
| Swift and Zig | `Package.swift`, `build.zig` | `.build`, `.zig-cache` |
| Go | `go.mod` | none |

`dist/`, `build/` and `bin/` are left out because they often hold release artifacts or local tools. To add an ecosystem, add a row to `TABLE` in `gc/build_output.rs` and a fixture test that shows a match and a safety case.

## Migration and first run

The `handoff-used-to-tombstone` migration moves every file in an old `handoff/used/` folder into the tombstone folder. Each file gets a fresh grace period and keeps its original modified time. A rerun finishes a partial move, and it reports `gc busy` instead of waiting when a cleanup is running.

The first session start after an upgrade can tombstone session folders, leftovers and gate databases that have been idle for years. They stay restorable for the grace period. `playbook gc` shows what would move before anything does.

## Review worktrees

Review worktrees under `<git dir>/review-worktrees/` follow one rule in both the daily sweep and `playbook worktree review setup`: an unlocked one goes after 60 seconds, a locked one needs a dead owner pid and 24 hours, and the tree must be clean. Age comes from the lock's `ts=` when it is readable, otherwise from the worktree's creation time. Setup sweeps only review worktrees, and skips the sweep when a cleanup holds the lock. `REVIEW_WT_TTL_SECONDS` no longer does anything.
