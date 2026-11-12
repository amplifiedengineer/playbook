# ADR-0061: Sync Usage Between One User's Computers Only When the User Turns It On

- **Status:** Proposed
- **Amended 2026-11-13:** users now run the sync commands as `playbook sync ...`, and `pbsync` stays as the helper and a short alias. Commands below are as they were when this record was written.
- **Date created:** 2026-11-07
- **Date modified:** 2026-11-07
- **Relates to:** [ADR 0025](0025-self-improving-playbook.md) (it rejected remote learning across users; this ADR covers one user's own machines and does not change that), [ADR 0020](0020-usage-terminal-ui.md) (the usage UI that reads these rows)

## Context

One person often uses playbook on more than one computer, for example a home machine and a work machine. The machines run different projects and can use different Claude accounts. Limits belong to the account. Sessions and consumption belong to the machine, and the next section sets the exact scopes. Today every number stays on the machine that made it. No view of one account across both machines exists.

The owner first chose libSQL as the shared store and asked for a swap of the engine under `playbook-store`. An evidence spike measured that swap and the owner rejected it. The numbers are under Considered alternatives. Five crates call `rusqlite` directly, in 49 files, and nothing sits between the code and the engine.

The user guides say nothing leaves the machine (`docs/guides/05-what-setup-installs.md`, `docs/guides/10-telemetry.md`). Sync changes that claim. The claim needs a precise new wording and a record of the rules behind it.

## Decision

Keep `rusqlite` in `playbook-store`. Sync is a separate tool that talks to Turso Cloud over HTTP. The owner has decided the points in the Decisions section below.

1. **Transport.** Sync uses the Turso SQL-over-HTTP API, the `/v2/pipeline` JSON endpoint, through `ureq`. `crates/cli` already pins `ureq`, so no new crate is added. The token comes from a config key or an environment variable. It is never printed or logged.
2. **A separate binary.** The tool is `pbsync`, a new bin crate `crates/pbsync`, like pbtop and pbdoc. It ships as its own binary. `playbook` shows the sync state in doctor.
3. **Three layers, kept apart.** The transport covers the HTTP client, auth and retries. The row push and pull engine covers batching, watermarks, idempotent upsert, the remote key and the schema-version refusal. The per-table definitions list the columns allowed, the key and the privacy transform. A new kind of data is then one small table definition.
4. **Commands.** `pbsync enable`, `disable`, `status`, `push`, `pull` and `purge`. `purge` removes this machine's rows.
5. **Opt-in, off by default.** Nothing leaves your machine unless you run `pbsync enable`. A fresh install, an upgrade and a doctor run do not contact a remote.
6. **What leaves the machine.** Only usage rows (`usage_events`) and a snapshot of shareable config keys. The repo goes as a salted hash of the repo name (128 bits, a truncated SHA-256 written as hex) with a per-user salt kept in the remote `shared_meta`, like the account salt. The account goes as a salted hash, worked out per event (see the next section). Sync sends no `branch`, `cwd`, file path or raw email, and it does not send `session_id` or `agent`.
7. **Allowlists.** A per-account list (`sync.accounts`, empty by default) decides which accounts sync. The work account syncs too, once the owner adds it to the list. An explicit column list picks the row columns, and an explicit key list (`SHARED_KEYS`) picks the config keys. A new column or key stays local until someone adds it to the list. Paths and machine-bound keys stay off the list.
8. **Transcripts stay machine-local.** Sync does not read or upload a transcript. Memory facts, the code index, the gate databases and `watch.db` are out of scope for the same reason.
9. **Remote keys.** The remote primary key is `(machine, local key)`, where the machine is the id described in the next section. Rows from two machines cannot collide. A push repeated with the same rows writes nothing new.
10. **A pulled config is a proposal.** A pull writes a proposal and does not apply it. The user sees the diff before anything changes.
11. **Schema safety.** An older binary refuses a newer schema with a message that names both versions. This stops two machines on different versions from corrupting a shared store.

## Scope of each panel and the account rule

1. **Two scopes.** Limits and usage windows are per account, and budgets follow them. They sum across every machine of that account. Sessions and consumption rows are per machine, with their agents, and the machine selector filters exactly these. One machine can hold several accounts, so account and machine are separate filters.
2. **The account is set per event.** Each assistant turn and each usage row gets its own account, not one account per session. One session can span two accounts, for example a resume under another login. The label comes from the credential recorded at the time of the event. The session hooks record spans of `(session, since time, account)`. Each event takes the latest span at or before its own timestamp. A session total sums its rows across accounts, and a session lists all of its accounts.
3. **Correcting an account.** `playbook usage account assign` moves rows by time range. A follow-up adds a limit to one session. The move is reversible, and it applies inside a session.
4. **Schema.** A new `session_account_spans` table replaces the `session_accounts` table, which holds one row per session. The migration copies each old row with `since` set to 0. The `usage.db` `schema_version` goes from 3 to 4. The migration runs on first open, and an older build refuses the newer database (decision 11).
5. **Sync.** The pushed account hash is per event. A session that spans two accounts pushes each row under its own account hash.
6. **Machine id.** The id lives in a one-row `machine` table in `usage.db`. `machine.json` is legacy and stays in place. The code reads the hostname live and does not store or sync it.

## Privacy and the opt-in carve-out

The promise to users becomes: nothing leaves your machine unless you turn on sync. Before this ADR the promise had no condition. After it, the condition is one explicit command, and the guides name it once sync exists. The user guides stay as they are until then, because they describe what the code does today.

ADR 0025 rejected "remote or shared learning across users". Its reason was that one user's habits are not another's and that learning must stay local. This ADR does not share anything across users. It moves one user's own usage between that user's own computers, to a store that user controls. The two decisions do not conflict. ADR 0025 carries a short pointer to this record.

### Threat model of the salted hashes

The account hash and the repo-name hash hide the email and the repo name from someone who opens the remote and reads it casually. They do not hide either from a determined guess.

- The salts sit in the same remote as the data (`shared_meta`), because every new machine must fetch them to compute the same hashes. Anyone who can read the rows can read the salts.
- With a salt in hand, a person can hash a list of known emails or known repo names and compare. Both are short and guessable. This works against a known candidate, and the repo-name hash has the same limit as the account hash.
- Each hash is therefore a pseudonym. Treat the remote as data that identifies the account to anyone who can read it, and protect it with the provider's access controls, not with the hash.

### Revoking access

1. Rotate or delete the access token at the provider. This is what cuts off a machine, because the provider decides what a token can reach.
2. Run `pbsync disable` on each machine. It stops all remote traffic from that machine.

Data already in the remote stays there until the owner deletes it at the provider. `pbsync purge` removes this machine's rows.

## Considered alternatives

**A. Port every call site to `libsql` directly.** Rejected for the reasons under C. It also changes every call site once, then again if the engine changes.

**B. A `pbsync` helper that links `libsql`.** Rejected. The helper would keep its own copy of the table code, and it still pulls in the `libsql` crates and an async runtime.

**C. Swap `playbook-store` to a `libsql` engine behind a feature.** Rejected after an evidence spike (branch `spike/gh-1585-libsql-evidence`, `libsql` 0.10.0 core, 25 days old). The size and startup were fine: the binary grew by 105 to 147 KB (0.7 to 0.9 percent), startup grew by about 1 ms per process, and all five targets built, including musl and Windows MSVC. The rest failed.

- The `libsql` API is async only and the workspace has no async runtime.
- About 360 `rusqlite` call sites in five crates would change.
- The API has gaps: no `optional()`, no `pragma_update`, no `NO_MUTEX` or URI open flags, no `ErrorCode` enum, no `get_ref`, and a transaction that takes a shared reference.
- It bundles SQLite 3.47, against 3.53.2 in `rusqlite`.
- It links only when the repo's `SQLITE_OMIT_LOAD_EXTENSION` flag is removed, which loosens security.
- It adds about 50 crates, against the owner's minimal-dependency rule.
- Both engines link into one binary, but `rusqlite` then silently runs on the older SQLite, and `libsql` panics if `rusqlite` initialises first.

**D. HTTP with `ureq` from a `pbsync` binary.** Chosen. See the Decision section.

**E. Files only, with JSONL export and import.** Rejected. It has no shared view without a manual step. The row format keeps this option open.

**Send the raw email, or the repo and branch, for a richer view.** Rejected. Those fields identify people and work, and the account view does not need them.

**Sync by default with an opt-out.** Rejected. It breaks the local-only promise for every existing user on upgrade.

## Consequences

- The storage crate and the schema check help even if the owner cancels sync, and so does the machine scope in pbtop.
- `playbook` keeps one engine, one SQLite version and no new dependency. Its binary size and startup do not change.
- `pbsync` is one more binary in the release, built like pbtop and pbdoc.
- A new kind of data costs one small table definition. The transport and the push and pull engine do not change.
- The remote holds pseudonymous account data, with the limits described above.
- Sync runs off the hook and statusline paths. Local reads and hooks do not wait on the network.
- Doctor shows the sync state, and the guides need a sync section once sync exists.

## Decisions

The owner answered the earlier open questions.

1. **Hosting: Turso Cloud, over HTTP.** The URL stays a setting. The token comes from a config key or an environment variable. Playbook never prints or logs it.
2. **Engine: keep `rusqlite`.** The `libsql` swap is rejected. `playbook-store` stays on `rusqlite`. Sync is a separate `pbsync` binary that talks to Turso over HTTP with `ureq`.
3. **Privacy: sync a salted repo-name hash.** The hash is 128 bits of hex from a truncated SHA-256, with a per-user salt in the remote `shared_meta`. Sync still sends no branch or working directory, no raw email, and no `session_id` or `agent`.
4. **Work account: it syncs too.** The explicit per-account allowlist `sync.accounts` stays in config and starts empty. The owner adds accounts. Enabling sync stays a visible opt-in with a consent prompt, because company policy still applies.

The Status stays Proposed until the owner accepts this record.
