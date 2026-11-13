# The Memory System

Each new Claude Code session starts cold: no memory of the project, your conventions, or past decisions. The memory system carries durable facts forward so you don't re-teach the same things session after session. Without memory, every session rediscovers your style, your team's decisions and a codebase's quirks. Corrections repeat and decisions get relitigated. Memory breaks that loop: facts are written once and loaded on demand, scoped to one repo, one owner or everywhere. It is a typed graph stored in plain markdown files in one local store on your machine, split into global, org, and project scopes.

## Playbook memory and Claude Code memory

Claude Code has its own memory: `CLAUDE.md` files you write and an auto memory it writes itself under `~/.claude/projects/<project>/memory/`. Playbook memory is separate, and the two never mix:

- Playbook never changes Claude Code's memory. It does not write to auto memory files, `MEMORY.md` under `~/.claude`, `CLAUDE.md` files, or Claude Code's memory settings (`autoMemoryEnabled`, `autoMemoryDirectory`).
- Every hook, command and skill that stores or loads playbook memory uses only `~/.config/playbook/memory/`.
- You choose whether Claude Code's auto memory runs next to playbook memory. The config key `memory.source` is `both` by default. Set it to `playbook` and sessions started with `playbook cc` or `playbook ccd` run with auto memory off. The launcher sets `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` for that one session, so no Claude Code file changes. Change it any time with `playbook config set --global memory.source <both|playbook>`.

Playbook may read Claude Code's auto memory once, to bring useful notes across. `playbook memory claude import [--dry-run]` copies the notes for the current project into playbook memory as facts (project memory under `<owner>/<repo>/`). It opens Claude Code's files read-only and never changes, moves or deletes anything there. It keeps no marker in Claude Code's directories. What it already imported is tracked by content hash in a small state file under playbook's own state folder. A second run copies nothing, a fact you deleted does not come back, and an existing playbook fact is never overwritten.

`crates/cli/tests/memory_isolation.rs` enforces the first two rules, and that the code reading Claude Code's memory has no write calls.

## Three Scopes

All three scopes use the same file format and the same index structure. The only differences are scope and when the index is loaded.

| Scope | Location | Holds |
|---|---|---|
| Global | `~/.config/playbook/memory/` | Cross-project facts: preferences, corrections, pointers to external resources. Apply in every repo. Index is read on demand, not at session start. |
| Org | `~/.config/playbook/memory/<owner>/` | Shared across every repo under one owner (e.g. every `acme/*` repo), not universal. Namespaced by the first segment of the repo's git remote. |
| Project | `~/.config/playbook/memory/<owner>/<repo>/` | Facts true only inside one repo. Namespaced by `<owner>/<repo>` from `git remote get-url origin`. |

All scopes are indexed in the shared `memory.db`. The whole store lives outside any repo checkout, so these files stay on your machine and never get committed.

The split exists because the three categories are genuinely different. A coding style preference applies everywhere. A team convention applies across every repo your org owns but says nothing about a different org's codebase. The auth layer's token flow is meaningless outside one service. Namespacing org and project facts by owner (and repo) also keeps them from polluting the global root, so the global index stays small enough to load efficiently.

## File Format and Index

Each fact is one kebab-case file with YAML frontmatter:

```yaml
---
name: Auth Flow
description: How tokens are issued and validated in this service
type: project
links:
  depends_on: session-model
  relates_to: rate-limiting
aliases: [auth, token flow]
anchors:
  - src/auth/
---
```

| Field | Meaning |
|---|---|
| `type` | One of `user`, `feedback`, `project`, or `reference`. |
| `description` | A one-line trigger hint: "use when..." |
| `anchors:` (optional) | Repo-relative code locations the fact describes: a directory, a file, or a `file#symbol`. It tells the graph where a fact lives. |
| `aliases:` (optional) | Other names for the fact, such as `aliases: [auth, token flow]`. An alias found in your prompt as a whole word or phrase surfaces the fact as a strong match, so a nickname still reaches it. Playbook reads `aliases:` when it builds the anchor index, so a new alias applies from the next session. |

For `feedback` and `project` facts, the body has a fixed structure: the rule first, then a `**Why:**` section and a `**How to apply:**` section.

`memory.db` is the sole index, built automatically from every fact file's frontmatter across all three scopes. Commands query it through `playbook memory context`, and `playbook memory export --json` prints the whole graph as JSON. The `session-init` SessionStart injection renders the same slice in process. See [memory.db](#memorydb) below for the node shape, and [Internals: Model Routing and Memory](../internals/02-model-routing-and-memory.md) for the format mechanics.

An install that predates this design may have a leftover `MEMORY.md` file on disk from before the graph became the index. Nothing reads it any more. It is safe to ignore or delete.

## Typed Edges

Facts link to each other via `links:` in frontmatter. Values are bare basenames (no path, no extension). Four edge types:

| Edge | Direction | What it means |
|---|---|---|
| `supersedes` | new → old | This fact replaces the target. Act on the chain head; treat superseded facts as historical. |
| `depends_on` | authoring → prerequisite | Load the prerequisite before acting on this fact. |
| `relates_to` | symmetric | Pull the neighbor for related context. |
| `contradicts` | symmetric | Both facts are live but conflict. Surface the conflict; don't silently pick one. |

Edges are typed because each type carries a different action. `supersedes` says ignore the old one, `depends_on` says load this first, `relates_to` says pull in the neighbor, `contradicts` says surface the conflict. An untyped link would be ambiguous: load the neighbor, replace it, or warn about it? The type resolves that.

Each edge is stored once on the authoring fact. Reverse links are inferred at load by scanning frontmatter, not stored explicitly. Traversal depth is 1 for all types except `supersedes`, which the system follows fully to the chain head.

Resolution checks the source's own scope first, then falls back to global. This applies independently to org and project sources: a project fact does not resolve through its org tier, only through its own project scope or global. A project or org fact can still link to a global target and resolve. A fact shadows a same-basename fact in a less specific scope reachable this way. A basename missing from both scopes tried is dangling, and it surfaces rather than fails silently.

## How Facts Are Created

Facts get written two ways.

**Ad-hoc during work.** When `/playbook:plan`, `/playbook:adr`, or `/playbook:implement` meets a durable convention, a decision, a rejected alternative, or an error fix, it writes a fact immediately. `/playbook:review --deep` writes findings from a review pass. These are narrow, targeted writes tied to the work at hand.

**Bulk analysis via `/playbook:learn`.** This command reads the repo broadly (git history, code structure, PRs, and JIRA/Confluence when reachable), clusters what it finds into topics, and writes one fact file per topic. Before writing anything, it shows a candidate table and asks once. It won't write without your confirmation.

Both paths produce the same file format and land in the right scope of the store.

## How Facts Are Loaded

Facts reach context three ways, described in full in [ADR 0004](../adr/0004-graph-first-memory.md) and [ADR 0008](../adr/0008-bounded-memory-injection-with-prompt-recall-and-handoff-continuity.md).

**At session start**, the `session-init` hook injects one small ranked block from `memory.db`: the global and repo-scoped facts that are pinned, most used (hit counts in `memory.signals.json`), or most linked (edges and anchors), as names and one-line descriptions.

It is capped near 2,500 characters, so it does not grow with the store, and it only reads the store. Facts with no pin, use or link are left to prompt and edit recall. With no graph, it injects nothing.

**Editing or writing a file** surfaces the facts anchored to it. The `memory-anchors` hook matches the edited path against the graph's anchor index and injects the matching facts' names, descriptions, and `depends_on`/`contradicts` neighbours.

**Asking about a file or topic**, a prompt rather than an edit, surfaces matching facts: name, description and the start of the body (cut at 1,500 characters). `memory-anchors` also runs on every prompt, matching prompt words (common words ignored) and this session's already-touched files against the same index, best match first.

It injects at most 3 facts per prompt and 9 per session, and a fact at most once per session. When the prompt has searchable words and no fact matches, or every match is weak, the hook adds one hint line (at most 400 characters, 5 per session) with the searched terms, the repo's fact folder to grep and a reminder to offer to save a fact. A strong match adds no hint.

A match is strong in any of these cases. Every other match is weak, such as one plain word on a fact name.

- A name hit comes from a hyphenated word or from 2 or more distinct words.
- An alias of the fact appears in the prompt.
- The word before `crate` or `crates` in the prompt names the crate folder the fact is anchored to ("auth crate" reaches a fact anchored to `crates/auth/`).
- The fact is anchored to a file touched this session.

Prompt recall first asks a full text index over fact names, descriptions and bodies in `memory.db` for the best 12 candidates. The hook then scores them with the rules above. If the index is missing, stale or busy, it scans the session index instead.

The anchor index builds once per session and is not rebuilt within it. A fact written mid-session does not surface on edits until the next session starts. Prompt recall reads the live full text index, so it can surface such a fact, matched by name, description and body only.

**Across a session boundary**, `/playbook:handoff` saves a handoff document with `playbook handoff save`. When the `SessionStart` hook is installed and Claude Code runs it, `session-init` loads the handoff at the next start, including after `/clear` and `/compact` (`playbook doctor check` checks the install), and tombstones it for 14 days, so `playbook handoff load` can still print it. `/clear` no longer discards what the session had figured out. If the automatic load does not appear, run `/playbook:handoff load` to read it on demand. `playbook handoff status` shows whether the hook fired at all.

The `memory-capture` hook fires once per context-usage threshold crossing during a session (a `Stop` hook, not a `SessionEnd` one). It prompts you to write down durable facts and, since ADR 0008, to run a handoff too, in case the session runs long enough to approach compaction.

Commands differ in how they use memory:

| Commands | Memory use |
|---|---|
| `/playbook:plan`, `/playbook:adr`, `/playbook:implement` | Read all three scopes before planning or executing. Project facts override org, which overrides global, for that repo. Conflicts between scopes surface rather than resolve silently. |
| `/playbook:commit`, `/playbook:review --quick`, `/playbook:pr-comments` | Don't touch memory. |

## memory.db

A single `memory.db` lives at `~/.config/playbook/data/memory.db` and covers every fact, global, org, and project.

Nodes are facts and referenced code locations. Edges are `links:` between facts, plus `anchors:` pointing facts to code. Each node carries a `scope` (`global`, `org`, or `project`). Org facts also carry the `project` (`owner`), and project facts carry the `project` (`owner/repo`).

It is the primary retrieval path, not just a navigation aid. Every mechanism in "How Facts Are Loaded" reads this database: the SessionStart slice, the edit-time anchor lookup, and prompt-time recall. See [ADR 0004: Graph-first memory retrieval and triggered capture](../adr/0004-graph-first-memory.md) for the decision, and [ADR 0008](../adr/0008-bounded-memory-injection-with-prompt-recall-and-handoff-continuity.md) for what was added on top of it.

| Task | How |
|---|---|
| Rebuild automatically | The `rebuild-memory-graph` hook fires whenever a file under `~/.config/playbook/memory/` is saved, so writing or editing any fact keeps the graph current without a manual step. |
| Rebuild by hand | `playbook memory rebuild` |
| Inspect | `playbook memory stats`: facts per scope, nodes, edges per relation, anchors, dangling edges and anchors, the path, size and last rebuild time. You never need `sqlite3`. |

The fact files stay the source of truth, so the database is safe to delete: the next rebuild makes it again. Older versions kept the graph in `~/.config/playbook/memory/memory.graph.json`. The first run of this version builds `memory.db` from the facts and renames that file to `memory.graph.json.migrated`. Nothing is deleted.

## The Auto-Learn Loop

When a session ends after making at least five edits in a repo, the `session-clean-exit` hook drops a flag in `~/.config/playbook/runtime/to-learn/`. The next time you open a session in that repo, `session-init` reads the flag and surfaces a nudge: consider running `/playbook:learn` to refresh project memory, or `/playbook:learn --stage` to queue candidate facts for review.

`--stage` collects candidates into `~/.config/playbook/memory/<owner>/<repo>/staging/` without touching the live store. `--from-staged` reviews them and promotes confirmed facts through the normal write flow.

The loop is nudge-and-approve by design. Memory is durable. A bad fact, a duplicate, or an outdated convention in the store gets loaded and acted on in future sessions. Automatic writes let errors compound unattended. The confirmation gate keeps the human in the loop, every time. Memory writes are permanent, so the gate catches duplicates and stale facts.

## Design Rationale

- **Durable over re-learned.** Write once, load on demand.
- **Typed edges.** Each link type tells the system what to do.
- **Three scopes.** Preferences, org conventions and project facts live in different parts of the store.
- **Human approve.** Memory writes are permanent, so a gate catches duplicates and stale facts.
- **Local only.** The store is never committed.

## See also

- [Decisions and Memory](../guides/03-decisions-and-memory.md)
- [Internals: Model Routing and Memory](../internals/02-model-routing-and-memory.md)
- [The system prompt](12-system-prompt.md)
- [Docs index](../README.md)
