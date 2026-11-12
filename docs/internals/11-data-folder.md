# Data folder

Every database a playbook tool creates lives in `~/.config/playbook/data`, shared by `playbook`, `pbtop` and `playbook-code`. It is owner only (mode 0700) and is made when the first database opens. `playbook path data` prints it.

## What is in it

| File | Holds | Safe to delete |
|---|---|---|
| `config.db` | All settings: the global, org, repo and per-app tiers, plus the internal `state` table | No: it is your configuration |
| `memory.db` | The memory graph: one row per fact and per link, built from the fact files | Yes: `playbook memory rebuild` makes it again |
| `usage.db` | Token and cost events from the usage view | No: events outlive Claude Code's own history |
| `gate/<owner>/<repo>/<worktree-id>.db` | Gate records of one worktree (`gate record`, `gate check`) | Only when the worktree is gone |
| `watch.db` | Watch mode: polling cursors, events already seen, and the work queue | No: deleting it forgets what was seen and what is queued |
| `watch/audit.jsonl` | Watch mode: one line per decision, rotated at 10 MiB, five files kept | Yes: it is a record, nothing reads it back |
| `watch.lock` | The one-instance lock of watch mode, held by the OS while a watch process runs | Yes |
| `code/<owner>/<repo>/store-v<N>.sqlite3` | The code index of a repo: one store for all its worktrees, with a `<worktree-id>.lock` run lock beside it | Yes: `playbook code index` rebuilds it |

Per-worktree gate files keep worktrees of one repo apart, as the old `repos/<owner>/<repo>/<worktree-id>/` folders did. The code store holds one manifest per worktree and one copy of each distinct file content, so worktrees share the parsing work and the disk, and a query still sees only its own worktree's paths. Memory facts stay Markdown files under `~/.config/playbook/memory` and are the source of truth for `memory.db`. Plans, implement notes and worktree folders stay under `repos/`.

## How databases got here

Earlier versions kept these files elsewhere:

| Now | Before |
|---|---|
| `data/config.db` | `~/.config/playbook/playbook.db` |
| `data/memory.db` | `~/.config/playbook/memory/memory.graph.json` |
| `data/gate/<owner>/<repo>/<worktree-id>.db` | `~/.config/playbook/repos/<owner>/<repo>/<worktree-id>/state.db` |
| `data/code/<owner>/<repo>/store-v<N>.sqlite3` | `data/code/<owner>/<repo>/<worktree-id>.db`, and before that `~/.config/playbook/repos/<owner>/<repo>/<worktree-id>/code/index.sqlite3` |

The first command that needs a database moves the old file. The move never deletes data found nowhere else:

- Copy: SQLite's `VACUUM INTO` reads a consistent snapshot including the write-ahead log. The copy is checked table by table and renamed into place only if row counts match. Playbook holds the old file's write lock meanwhile, so no writer can add a row between copy and rename.
- Rename: the old file, with its `-wal` and `-shm` files, becomes `<name>.migrated`. An earlier `<name>.migrated` that reads as a sound database came from such a move and so its rows are in the new file, and it is replaced. One that does not read was parked as corrupt and stays, and the new copy takes a numbered name (`.migrated.1`). The `.migrated` copies are kept. Delete them by hand once the new store works. The code index is a cache, so it moves by a plain rename.
- Lock: a folder `<name>.db.migrate.lock` next to the new file stops two processes moving the same file. One older than five minutes is treated as left by a killed process.
- Notice: one line to stderr, only on a terminal. Hooks never print it.

If the move cannot finish (another process holds the lock, or the old file is busy), the command keeps using the old file for that call, and the next call tries again. A setting or gate verdict is never hidden behind an empty new file.

If both files exist, because an older playbook recreated the old path, the rows are merged by primary key and the old file is renamed again:

| Data | Rule |
|---|---|
| `config.db` | The row with the later `updated_at` wins. Values imported from a leftover `config.json` count as oldest |
| Gate phase rows | The later `recorded_at` wins. The rounds of one plan and phase come whole from one file, never mixed |
| Rows only in the old file | Added |

The graph is the one exception to copying. `memory.graph.json` is derived from the fact files, so `memory.db` is rebuilt from them and the file is renamed to `memory.graph.json.migrated`. If an older playbook writes the file again, it is folded in the same way the next time a tool reads the graph. `playbook memory export --json` prints the graph in the old file's shape.

A file that is not a database is left where it is, or renamed to `.migrated` for the settings database, and the notice names it.

> [!WARNING]
> Older playbook versions (0.25.0 and earlier) keep working but read and write the old paths. Upgrade every installed copy.

## Schema version

`usage.db` and `config.db` each carry a `schema_version` row in a `meta` table, `usage.db` at version 5 and `config.db` at version 1 today. On open, a database stamped newer than the build is refused before anything is written, with an error that names both versions and says to run `playbook upgrade`. An equal or older database opens and is stamped. A database from before the row existed counts as version 0, so it always opens. A `schema_version` that is not a whole number is also refused, with nothing changed. A change to either layout bumps its version, so an older build refuses the changed database instead of misreading it.

Version 2 of `usage.db` added a `machine` column to `usage_events`, `tool_invocation_events`, `telemetry_requests`, `telemetry_tools`, `telemetry_errors` and `telemetry_metrics`. The first open on this build adds the column with an empty default, so old rows stay valid, and an older build then refuses the database. Primary keys do not change. The id of this machine is kept in `usage.db` (see version 3) and in `meta` as `machine_id`, and stamped on every new row. A one-time backfill then stamps the id on rows with an empty machine, 5,000 rows per write so hooks and the statusline never wait on one long write. It can stop at any point and resumes on the next open, and once it completes it writes nothing more.

Version 3 of `usage.db` added the one-row `machine` table (`id` fixed at 1, `machine_id`, and a `name` that is empty unless set with `playbook usage machine --name`). The table is the source of truth for the machine id. On the first open of a database whose table is empty, a readable `machine.json` beside it supplies the id, and its name only when it differs from the hostname stored there; with no usable file, a new random id is made. After that the table wins, even if the file differs. `machine.json` is legacy: it is never written, changed or deleted, so an older build keeps working, and a damaged one is ignored. The hostname is not stored; it is read when needed. A build that understands only version 2 refuses a version 3 database.

Version 4 of `usage.db` added the `session_account_spans` table (`session_id`, `since`, `account`, keyed by the first two). Each `SessionStart` line in `session-accounts.log` now ends with the unix time it was written, and ingest labels every event with the account of the latest span that began at or before the event, else the earliest span of the session, else the signed in account. A session that moves between accounts is therefore split at the move. Lines without a time still parse and count as a span from time 0. The first open on this build copies each row of the old `session_accounts` table into a span from time 0, once, and leaves the old table in place. A build that understands only version 3 refuses a version 4 database. Builds before 0.42 wrote a log line as the account, a tab and the time into one key, so an old binary run on a version 4 store can leave keys like `email<TAB>digits`. Each ingest of a newer build checks the two session tables for one and, when it finds one, rewrites the plain key in the events, the tool events, both session tables and the move history in one transaction. A second ingest changes nothing.

Version 5 of `usage.db` added the local tables that usage sync reads and writes, all created on first open and inert until sync is enabled: `sync_state` (the local salt copy, `last_success`, `last_error`, `enabled_at`), `sync_watermarks` (the last pushed rowid per table), `sync_cursors` (the keyset position per remote machine and table), `usage_events_remote` (events pulled from other machines, with the account as a hash and the repo as `repo_hash`), and `account_aliases` (hash to account key). The `usage_events_all` view merges local and remote events into one whole row per `event_id`: the row with the larger `output_tokens` wins and the local row wins a tie. A remote row shows `(not synced)` for the repo and branch, and its account is the alias key when one exists, else `synced:` and the first 8 hex digits of the hash. A build that understands only version 4 refuses a version 5 database.

## Look at it

`playbook doctor check` has a row for the folder: path, file count, kilobytes, any `*.migrated` leftovers or `.tmp-` copies, and per-worktree databases that match no worktree of this clone. Those belong to removed worktrees or another clone of the repo. Check before deleting.

A second row, `store schema`, shows the version stored in `usage.db` and `config.db` next to the one this build writes. It reads the files without changing them. A newer database shows as a failure with `run: playbook upgrade`.

A third row, `sync`, appears only when `sync.json` in the same folder turns usage sync on. It reads `sync.json` and `usage.db` without changing them and shows the age of the last success and the rows still pending, for example `sync: on, last success 2m ago, 0 rows pending`. It warns when the last success is older than `interval_seconds` (`run: playbook sync push`), when the last push failed (`run: playbook sync status`), when the `pbsync` helper is not found, when the local sync tables are missing, or when `sync.json` cannot be read. It fails when the environment variable named by `token_env` is unset; only the variable name is shown, never its value or the URL.

`playbook worktree sweep` and `playbook worktree remove` delete the gate database of the worktree they remove, and drop its rows and run lock from the code store, unless another registered worktree still maps to the same id. `playbook code index` also removes content that no worktree lists and worktree rows whose folder has been gone for over a week. It deletes store files of older schema versions once nothing has touched them for thirty days. The first `playbook setup` after an upgrade deletes the old per-worktree code files.

## Uninstall

`playbook uninstall` leaves the data folder alone, like memory and every other piece of state.

## Repo folders

Files that are not databases live under `~/.config/playbook/repos/<owner>/<repo>/`, where `<owner>/<repo>` comes from the `origin` remote. The folder is the same from every checkout and every worktree of the repo, so anyone can work the path out with `git remote get-url origin`.

| Folder | Holds | Printed by |
|---|---|---|
| `plans/` | Plans, their quality reports and checkpoints | `playbook path plans` |
| `implement/` | Per Work Unit briefs and progress notes | `playbook path implement` |
| `worktrees/` | Worktrees `/playbook:implement` creates | `playbook path worktrees` |
| `handoff/` | Handoffs saved with `playbook handoff save` | `playbook path handoff` |

Earlier versions kept `plans/`, `implement/` and `worktrees/` one level deeper, in `<worktree-id>/`, and handoffs for every directory in `runtime/handoff/`. The first call that needs a folder copies old files in. Nothing is deleted or overwritten:

- `plans/` and `implement/` are copied from every old `<worktree-id>/` folder. A name that already exists in the new folder is left alone, and the clash is counted in the note.
- `worktrees/` is not copied. It holds live git checkouts, which a copy would break. Old ones stay where they are and keep working.
- Handoffs for the current directory, and the directories under it, are copied from `runtime/handoff/` the first time a session starts there. A marker in `handoff/.copied/` stops a handoff that a session already read from coming back.
- A `MOVED.txt` beside the old files says where the copy went and marks the copy as done. One line goes to stderr when files were copied.

Outside a repo with an `origin` remote, handoffs stay in `runtime/handoff/` and the other kinds report that no folder can be resolved.
