# Model routing and memory

The session model defaults to Sonnet. Four things shape which model and effort a task gets: the tier table, the policy in each file's frontmatter, `playbook route`, and a hook that nudges design prompts toward Opus. Memory is a typed graph in plain markdown files.

## Model routing

The policy lives in each file's frontmatter (`agents/*.md` and `commands/*.md`) and in `playbook route`. The system prompt points there and does not restate models, so the two cannot disagree.

| Tier | Takes |
|---|---|
| **Sonnet** (session default) | Most coding: `/playbook:implement`, `/playbook:fix`, the `implementer` and the `critic`. |
| **Haiku** | Mechanical, formatting and search work: `collector`, `cheap-checker`, `review-triage`, `fact-checker`, `test-reviewer`, `analyst`, `writer`, and the `doctor`, `session-handoff`, `session-start` and `setup` commands. |
| **Opus** | Design (`/playbook:plan`, `/playbook:adr`), every `reviewer` (`/playbook:review --quick`, `/playbook:review --deep` and `/playbook:implement` Step 9, all at the reviewer's own medium effort unless a variant is picked), the `auditor` behind `/playbook:audit`, `/playbook:pr-comments` and `/playbook:learn`. |

Design work uses `/playbook:plan` and `/playbook:adr`, not the built-in plan mode.

### The routing table

`playbook route <kind>` (`crates/cli/src/routing.rs`) maps a kind of task to a model, effort and agent for every command that dispatches agents, and says whether you must approve first.

| Kind | Model and effort | Agent |
|---|---|---|
| `mechanical` | haiku, low | `collector` |
| `classify` | haiku, medium | `review-triage` |
| `check` | haiku, medium | `cheap-checker` |
| `review` | opus, medium | `reviewer` |
| `write` | haiku, medium | `writer` (turns decided facts into a short text; replies to PR comments stay with the orchestrator) |
| `implement` (`--tier low\|medium\|high`) | sonnet, low, medium or high | `implementer` |
| `design` | opus, xhigh | `critic` (stress-tests the design at its own Sonnet medium; the design itself runs in the command on Opus) |

Approval is needed for the top model, the high implement tier, and a task that has failed twice (`--failures 2`). `routing.escalate` decides what happens then: `ask` tells the orchestrator to ask you, `auto` (default) proceeds (auto mode and `auto.budget.mainUsd` still cap the spend), `deny` returns the next cheaper route. Your effort ceiling always wins. `--json` prints the decision for scripts.

### auto-model-detect

The `auto-model-detect` hook (`playbook hook auto-model-detect`, `crates/cli/src/hooks/auto_model_detect.rs`) runs on every `UserPromptSubmit` event, wired in `settings.json` under the `UserPromptSubmit` hook list. Claude Code cannot flip the session model mid-stream, so the hook detects design intent and injects a context message nudging Claude toward an Opus subagent.

It skips slash commands and prompts under 20 characters. For natural-prose prompts it matches a hand-expanded set of phrases (case-insensitive, word-boundary matched, ported from the original regex):

- Design nouns: `design`, `architecture`, `ADR`, `schema`, `tradeoffs`, `migration`, `data model`, `interface design`, and related terms.
- Decision verbs: `evaluate`, `compare`, `brainstorm`, `propose`, `critique`, `review the approach`.
- Design-shaped questions: `should we`, `how would/should we/you/I`, `what's the best`, `which approach/design/pattern`, `pros and cons`.

On a match, the hook reminds Claude the session runs on Sonnet and recommends one of two paths:

- The `Plan` agent (via `Agent` tool with `model: "opus"`) for implementation planning with codebase grounding.
- `/playbook:plan` for design work before any code.

For narrow prompts (a quick choice between two named options) staying inline on Sonnet is fine. No match: the hook exits silently.

### Effort policy

Reasoning: [Why the pieces are shaped this way](../internals/14-why-the-pieces-are-shaped-this-way.md).

Effort is a second dial next to the model tier. Agents, commands and skills accept an `effort` key (`low`, `medium`, `high`, `xhigh`, `max`). The `Agent` tool takes an `effort` parameter per call when a skill asks for it, and the `delegating-subagents` skill asks for it on every spawn. The agent file sets the default, and the orchestrator passes a lower or higher level for one spawn (see [Authoring agents](../authoring/02-authoring-agents.md)). To compare settings on real inputs, run `playbook eval bench`.

**Main session default.** The `playbook cc` and `playbook ccd` launchers add `--effort medium` to the `claude` command line when the session states no effort. Stating one turns it off: pass `--effort high` (or any level), set `effortLevel` in `~/.claude/settings.json`, or export `CLAUDE_CODE_EFFORT_LEVEL`. An effort ceiling still lowers the default. Playbook only shapes its own launcher command line and never writes Claude Code's settings or memory, so a session started without the launcher keeps the model default. See [ADR-0023](../adr/0023-commands-at-medium.md).

The rule: lower effort where the work is mechanical or already decided, and never where a missed finding is costly. A wrong model costs a rerun. A reviewer that stops looking costs a bug in production, which is why risky diffs still get the `-xhigh` reviewer.

| File | Model | Effort | Why |
| --- | --- | --- | --- |
| `agents/collector` | haiku | `low` (was medium) | Gathers and compacts raw history. The analyst thinks later. |
| `agents/cheap-checker` | haiku | `low` (was medium) | One narrow concern from a named reference file. A full lens covers the rest. |
| `agents/review-triage` | haiku | `low` (was medium) | A three-way classifier. Any bad or missing answer falls back to `full-lens`, so a wrong call fails safe. |
| `commands/review` | sonnet (orchestrator); its reviewer runs on opus | `medium` for the command and its reviewer | See below. |
| `agents/reviewer` | opus | `medium` (was high) | Lowered by [ADR-0021](../adr/0021-reviewer-at-opus-medium.md). See below. |
| `agents/critic` | sonnet | `medium` (was high) | See below. |
| `agents/fact-checker` | haiku | `medium` (was high) | See below. |
| `agents/test-reviewer` | haiku | `low` (kept) | See below. |
| `agents/writer` | haiku | `medium` | Writes the final text of one kind from facts the orchestrator decided. Pilot on 18 cases: no invented fact, no hard prose finding. It does not decide anything. |
| `agents/analyst` | haiku (was sonnet) | `medium` (was high) | See below. |
| `agents/implementer` | sonnet | `medium` (was high) | See the implementer bench below. |
| `agents/auditor` | opus | `high` (kept) | A whole repository audit whose findings people act on. No headless bench exists for it. The maintainer kept it at `high` on 2026-10-11 ([ADR-0023](../adr/0023-commands-at-medium.md)). |
| `implement`, `plan`, `adr`, `learn`, `fix`, `pr-comments` | opus, except implement and fix (sonnet) | `medium` (was high) | Lowered by the maintainer on 2026-10-11 ([ADR-0023](../adr/0023-commands-at-medium.md)). No bench exists for the design commands. The evidence is the reviewer, implementer and critic results. Watch rework and missed findings. |
| `commands/audit` | fork into auditor (opus) | none | It forks into `playbook:auditor` (`high`), which sets the effort. |
| `commands/doctor`, `handoff-show`, `setup` | haiku (was sonnet) | `low` (kept) | Run compiled helpers and print the result, or load a saved handoff. Mechanical, so the cheapest tier fits (about 95% less per call, 20x cheaper per token). Checked live (see PR). |
| the `effort` parameter of the `Agent` call | the agent file's own effort | any level up to every ceiling | Passed by the orchestrator, from `playbook effort resolve` or `playbook effort choose`. The orchestrator picks by ceiling, then diff size and risk; see `playbook:delegating-subagents`. |
| `skills/*` | none | none | A skill is knowledge loaded into whoever uses it. An `effort` key would override the caller's choice, so none sets one. |

Evidence for the rows marked "See below":

- **`commands/review`.** It runs one pass over a diff the user chose not to deep review, or the deep swarm, on Sonnet either way; `/playbook:review --deep` is the thorough path. The reviewer's base is now `medium` too. It is spawned through `playbook effort resolve agents reviewer --cap medium`, which returns the effort to pass on the `Agent` call: `medium`, or lower when the user's ceiling is lower. See [ADR-0021](../adr/0021-reviewer-at-opus-medium.md).
- **`agents/reviewer`.** Benched on 10 real diffs (7 with a seeded real finding, 3 clean), 20 runs per cell (200 calls):
  - Opus high 200/200. Opus medium 199/200 (the one miss was `rd-lower`, a seeded finding, 139/140 recall). Sonnet medium and high 200/200. Opus low 49/50 in a 5 run round.
  - Cost per call: Opus high $0.0155 to $0.0168, Opus medium $0.0125 to $0.0136 (19% less), Sonnet high $0.0066 to $0.0072, Sonnet medium $0.0050 to $0.0054.
  - The 7 seeded findings are a small set of distinct bugs. The bench does not prove recall on harder ones.
  - Risk is bounded by the variants: an orchestrator sends a security lens, a diff over 60 KB or an auth, secrets or crypto diff to the reviewer at `high` effort, and a small diff to `low`, both set with the `effort` parameter of the `Agent` call.
  - Sonnet matched Opus on this set at about 60% less. Moving the reviewer to Sonnet would also change the tier rule above and stays unapplied until a larger set of seeded diffs exists.
- **`agents/critic`.** Round 2 bench (2026-10-10, 90 calls per cell on 6 plans, 4 with a seeded flaw and 2 clean): Sonnet medium 89/90 (98.9%) at $0.0088, Sonnet low 89/90 at $0.0078, Sonnet high 71/90 (78.9%) at $0.0145. Every miss at `high` was a false blocking finding on a clean plan. The only missed flaw in 180 medium and low calls was one `cr-destructive` run. Medium is 39% cheaper and no worse on seeded flaws. Haiku scored 67 to 73% on the same cases (false blocks on clean plans), so the critic stays on Sonnet.
- **`agents/fact-checker`.** The `high` in the file was already cut to `medium` by the Haiku effort cap, so the file now says what runs. Measured with `playbook eval bench` on real playbook excerpts over three rounds (2026-10-09 and 2026-10-10, 6 cases): Haiku high 137/138 (99.3%) at $0.00027 a call, medium 134/138 (97.1%) at $0.00021, low 86/90 (95.6%, four misses on one case) at $0.00015, Sonnet high 78/78. Medium is within 3 points of high, so it is the setting.
- **`agents/test-reviewer`.** Pilot, same benchmark (18 calls per cell, then 90 per Haiku cell in round 2 on real tests with weakened assertions and clean controls): Haiku low 86/90 (95.6%), medium 86/90, high 90/90, Sonnet high 48/48, Sonnet low 42/48. Every Haiku low miss was a false alarm on a clean control, not a missed weak test, so `low` stays; it is the lowest setting there is.
- **`agents/analyst`.** Round 2 bench (2026-10-10, 3 cluster cases checked for well formed facts and for anchors drawn only from the allowed set): Haiku low 45/45 at $0.00035, medium 45/45 at $0.00049, high 44/45 at $0.00072, Sonnet medium and high 15/15 at $0.005 and $0.007. About 14 times cheaper than Sonnet high at the same pass rate. The bench checks structure and grounding, not how useful a fact is. The `learn` Phase 3 confirmation by the user is still the quality gate, and the analyst never writes to the store. Medium rather than low keeps one step of margin on a bench of only three cases.

To undo the Haiku moves, set `model: sonnet` and `effort: high` in the agent file and re-run `playbook eval bench --role <role> --model haiku,sonnet --effort medium,high` to compare. Tier overrides (`models.haiku`) stay inside one tier, so no config key moves a single agent to Sonnet.

**Implementer bench.** The `implementer` stays on Sonnet and moves from `high` to `medium`. Round 1 (2026-10-10, 8 cases, 3 runs) and round 2 (20 repo tasks in `crates/cli/tests/fixtures/bench/implementer-repo-tasks.json` and `-2.json`) apply each reply in a temp dir and run visible and hidden tests. Any edit to a test file or to a file outside the allowed list fails, with no judge model. The tasks: a stub to fill, a refactor, multi file changes, a bug whose root is in a helper, existing patterns to follow, a tempting hardcode, a test that must not be edited, scope and input mutation traps.

| Run | Haiku | Sonnet |
|---|---|---|
| Round 2, 5 runs per case (100 calls per cell) | low 89%, medium 88%, high 91% at $0.0003 to $0.0004 a call | low 99%, medium 96%, high 95% at $0.0035 to $0.0037 |
| One fixture bug fixed (a hidden input the spec never asked for), 10 runs per case (200 calls per cell) | | medium 200/200 at $0.0027, low 199/200 at $0.0027, high 198/200 at $0.0029 |

- Haiku never reaches 95% on 20 tasks (it drops the file sections or misses a multi file thread), so it stays out.
- On Sonnet, effort barely changes the cost of a single reply. The saving here is about 7%, and `medium` is a no-loss setting with only a small saving.
- The bench is a single turn with no tools. It does not measure reading a real repo, running the verify command or committing, which is why `low` is not taken even though it scored the same.

**Dispatch granularity.** `playbook eval bench --cases crates/cli/tests/fixtures/bench/implementer-wu-tdd.json --strategy per-step,per-wu --model sonnet --effort medium --runs 3` runs 12 of the repo tasks in a scratch git repo with the real `implementer` agent, the plugin loaded and every tool on. It runs once with one dispatch per TDD step (RED, GREEN, REFACTOR, three calls) and once with one dispatch for the whole Work Unit. A run passes only when the final code passes the original visible test and the hidden tests, nothing outside the allowed files changed, the RED commit holds only the test file and that test fails at that commit, and RED comes before GREEN in the log.

Two batches of 36 runs per strategy (the first lost two runs to a scratch directory name collision, fixed since), 71 scored runs each:

| Strategy | Pass rate | Cost per Work Unit | Time |
|---|---|---|---|
| per-step | 71/71 (100%) | $0.151 | 27.0 s |
| per-wu | 68/71 (95.8%) | $0.064 | 13.8 s |

Per-wu cost 58% less and took 49% less time, but its pass rate is 4.2 points lower, outside the 3 point rule, so `/playbook:implement` keeps one dispatch per step. All three misses were the same task (`wu-imx2-error-pattern`, a hidden edge case on whitespace and number formats that the RED step did not cover when it wrote its own tests). Per-step passed it 6 of 6. Each task has one scenario, so a real Work Unit with N scenarios saves more than the 3 to 1 measured here. Orchestrator turns between steps are not counted. A prompt that makes the single dispatch derive edge cases from the Changes text before writing RED is the obvious next experiment.

### Policy table: every component

Every agent, command and skill with its model, effort and reason is in [`docs/reference/policy-table.md`](../reference/policy-table.md). A test there keeps it equal to the frontmatter.

### Effort cap per model

The Haiku 5.5 evaluation (#576) found `xhigh` costs 2x `medium` for the same pass rate. `max` costs 13x (about 7,100 thinking tokens and 32 s per call) and scores worst (91% against 97% at `medium`). `crates/cli/src/effort/model_cap.rs` therefore caps every component whose frontmatter model is Haiku:

- `max` is never used on Haiku, whatever a role or a spawn asks for.
- Haiku roles stop at `medium` by default.
- A role may go up to `xhigh` only when it has an entry in `OPT_INS` with a recorded reason. No role has one today.

The cap is a ceiling like the others. The lowest ceiling wins, your Claude Code `maxEffortLevel` still beats it when lower, and it never raises anything. It applies in `playbook effort resolve`, in the variants `playbook cc` and `playbook ccd` render, and it shows in `playbook effort list` and `playbook doctor models`.

An agent runs at its own effort unless the orchestrator passes another `effort` on the `Agent` call, chosen by ceiling, diff size and risk. Do not request xhigh or max on your own for anything else. If a deployment rejects it, omit the parameter.

### Per-component ceilings

A ceiling for one component is the key `effort.<agents|commands|skills>.<name>` (see [ADR-0017](../adr/0017-per-component-effort-ceilings.md)). `playbook effort list` prints every component with its shipped, configured and effective effort. `playbook effort resolve agents <name> --json` returns, for an agent, the file to dispatch under the ceilings and every file allowed at or below them. The `Agent` tool has no per-call effort, so choosing the file is how an agent's ceiling is enforced.

### Tuning from usage

**Stage 1.** `playbook effort suggest` is a read only report (stage 1 of [ADR-0022](../adr/0022-effort-auto-tune-from-evidence.md)). For each agent it shows the dispatches in the window and how many used a variant above or below the shipped effort, repeats inside a session, the cost per effort bucket, your ceilings, and whether lowering is permitted or something needs a look.

Permission to lower comes only from the quality floor table in `crates/cli/src/effort/floor.rs`, one row per agent with the benchmark run behind it. A floor is pinned to the shipped effort when the bench is thin, so a row moves below shipped only when a larger bench says so. The policy table above stays the human readable record. Usage evidence can hold a change back or ask for a look, and it never grants a lowering. `crates/cli/tests/effort_suggest.rs` fails when an agent has no floor row or a floor is above the effort in the agent file, so change both with the agent file.

**Stage 2** of the same ADR: `playbook effort suggest --patch`, `playbook effort apply` and `playbook effort reset` (`crates/cli/src/effort/patch.rs`).

- The patch is built only from agents the report marks `lower`.
- `validate` repeats the checks at apply time: a floor row exists, the agent is not costly to miss, the value is not below the floor, and it is below what the agent runs at now (so it cannot pass any ceiling).
- Apply needs a `y` at a terminal or `--yes`. It writes a backup of the global `effort.*` keys, records each key in `effort/ledger.json`, appends to `effort/changes.log`, and sets the keys through the config store.
- `reset` restores ledgered keys that still hold the applied value and leaves edited ones.
- All files are under `~/.config/playbook/effort/`, written with `write_atomic` and `atomic_append`. Claude Code settings and memory are never written.

**Stage 3.** The key `effort.auto` (`off`, `suggest`, `on`; `crates/cli/src/effort/auto.rs`). `playbook effort choose <agent>` takes:

- the diff shape (from `git diff --numstat <ref>` or explicit flags);
- a risk class (path patterns and memory facts anchored to touched paths whose name or description names a risk);
- the stage 1 evidence.

The candidates are the levels at or under every ceiling, so the result is an `effort` that `resolve` would allow. The orchestrator passes it as the `effort` parameter of the `Agent` call on the base agent.

- Raising (large diff, risky path, security lens) aims for `high`.
- Lowering needs a small, clean diff, a row in the floor table, an agent that is not costly to miss, a level at or above the floor, and evidence with enough history and no escalation or repeat signal.
- Any error or panic gives the default.
- Every call in `suggest` or `on` mode appends a row to `effort/auto.log`.
- `resolve` keeps its contract, so `choose` is a separate lookup.

No new ceiling key was added: the patch works inside the existing ceilings (`effort.max` and `effort.<kind>.<name>`, ADR-0017).

### Model tiers and the 5.5 rule

Every model playbook picks is the 5.5 generation. Plugin files name a tier by alias (`haiku`, `sonnet`, `opus`) and never pin a model id. Claude Code resolves each alias to the newest model the provider offers (on the Anthropic API, the 5.5 model).

| Tier | Alias | Preferred (5.5) | Falls back to |
|---|---|---|---|
| fast | `haiku` | `claude-haiku-5-5` | `claude-haiku-4-5` |
| balanced | `sonnet` | `claude-sonnet-5-5` | `claude-sonnet-5` |
| deep | `opus` | `claude-opus-5-5` | `claude-opus-5` |

The table lives in `crates/cli/src/models/mod.rs`. When the preferred model is overloaded or unavailable (including a Bedrock or Vertex account that cannot invoke it), Claude Code switches to a fallback for that turn. `playbook cc` and `playbook ccd` pass the chain `claude-opus-5,claude-sonnet-5,claude-haiku-4-5` with `--fallback-model`. They add nothing when you pass `--fallback-model` yourself or set `fallbackModel` in `~/.claude/settings.json`. A session started without the launcher gets no playbook fallback.

Measured on Claude Code 2.1.293 (2026-10-09): all six ids answer, and the aliases resolve to the 5.5 ids. With an unknown primary id and `--fallback-model`, Claude Code runs the fallback. Without it the call fails with `api_error`. A chain of more than three models gives no flag error, so the three-model cap is only a documented limit.

**Effort and the fallback.** The previous Sonnet and Opus reject `xhigh` and `max` with HTTP 400 `Invalid effort level`. They accept `low`, `medium` and `high`. Haiku 4.5 accepts all four. A 400 also triggers the fallback, so a session at `xhigh` would walk the chain and end at Haiku anyway. When the effort in force is `xhigh` or `max` (from `--effort`, or `effortLevel` in your user settings, lowered to the effective ceiling), `playbook cc` therefore passes only `claude-haiku-4-5`. When no effort is stated, the model default applies and the full chain is passed. Playbook never changes your effort to suit a fallback.

**Overrides.** `playbook config set --global models.sonnet claude-sonnet-5` (also `models.haiku` and `models.opus`) points an alias at another model of the same tier. The value is empty (no override) or `claude-<tier>-<major>[-<minor>]` for its own tier, and anything else is refused. `playbook cc` and `playbook ccd` export `ANTHROPIC_DEFAULT_<ALIAS>_MODEL` for each override, so subagents and skills that name the alias follow it. A variable you exported yourself wins. Shipped plugin files still name only aliases, so the pin check is unchanged.

`playbook doctor models` (add `--json`) prints the table, any override, the chain `playbook cc` passes, whether your own fallback replaces it, and whether the effort in force trimmed the chain.

A test fails when an agent, command, skill, prompt, output style, workflow or `settings.shared.json` names a model id outside this table, or when an agent uses anything but a tier alias.

## Memory Protocol

Memory is a typed graph. Both scopes share the same file format.

### Scopes

| Scope | Path | Coverage | Committed to git? |
|---|---|---|---|
| Global | `~/.config/playbook/memory/` | Cross-project | No, outside any repo |
| Org | `~/.config/playbook/memory/<owner>/` | Every repo under one owner | No, outside any repo |
| Project | `~/.config/playbook/memory/<owner>/<repo>/` | One repo only | No, outside any repo |

The `<owner>/<repo>` project index is injected at session start. The global index is read on demand.

### Claude Code's memory

Playbook memory and Claude Code's auto memory stay separate, and playbook never writes Claude Code's memory or settings. `memory.source` is `both` (default) or `playbook`. With `playbook`, `playbook cc` and `playbook ccd` set `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` for that session only. `playbook memory import-claude` copies Claude Code's notes for a project into playbook memory, read-only on the Claude side.

### File format

One fact per file. Filenames are kebab-case (e.g. `commits-must-be-signed.md`). Every file opens with YAML frontmatter:

```yaml
---
name: fact-name
description: one-line trigger hint for when to use this fact
type: user | feedback | project | reference
links:
  supersedes: old-fact-name
  depends_on: prerequisite-name
  relates_to: neighbor-name
  contradicts: conflicting-name
anchors:           # optional; mainly used in the project store
  - src/auth/login.py#authenticate
  - src/auth/
---
```

Edge values are bare basenames with no path and no extension. For `feedback` and `project` type facts, the body follows a fixed structure: the rule first, then a **Why:** section and a **How to apply:** section.

### Index

`memory.db` is the index. Each fact becomes one node, shown here in the JSON form `playbook memory export --json` prints:

```json
{
  "id": "acme/api/auth-flow",
  "file": "acme/api/auth-flow.md",
  "scope": "project",
  "type": "project",
  "name": "Auth Flow",
  "description": "How tokens are issued and validated in this service"
}
```

Nodes carry names and descriptions for navigation. Edge declarations live only in each fact's frontmatter, not duplicated on the node.

### Edge types

| Edge | Direction | Meaning |
|---|---|---|
| `supersedes` | new → old | The authoring fact replaces the target. Act on the chain head; treat superseded facts as historical. |
| `depends_on` | authoring → prerequisite | Load the prerequisite before acting on this fact. |
| `relates_to` | symmetric | Pull the neighbor for related context. |
| `contradicts` | symmetric | Both facts are live but conflict. Surface the conflict; don't silently choose one. |

Each edge is stored once on the authoring node. Reverse links are inferred by scanning frontmatter at load time, not stored explicitly.

Traversal depth is 1 for all edge types except `supersedes`, which is followed fully (chain head wins). A project fact that contradicts a global fact wins for that repo. Dangling basenames (the target isn't in the store) are surfaced, not dropped.

### memory.db

`~/.config/playbook/data/memory.db` is the single navigable store of the full memory graph. Nodes are facts and referenced code locations. Edges are `links:` between facts plus `anchors:` from facts to code. It covers all scopes: global, org, and project. The `rebuild-memory-graph` PostToolUse hook rebuilds it automatically after any fact file is saved. The database is derived from the fact files and safe to delete. See [Decisions and Memory](../guides/03-decisions-and-memory.md) for how to query and use it day-to-day.

## See also

- [Decisions and Memory](../guides/03-decisions-and-memory.md): using memory day-to-day and the `/playbook:learn` command.
- [Launcher and hooks](01-launcher-and-hooks.md): the `playbook cc` launcher that sets the session model.
- [Docs index](../README.md)

## Forks versus fresh subagents

An omitted `subagent_type` forks the parent conversation and bills its full history again. Playbook never relies on that: dispatching skills and commands set a `subagent_type` and pass a brief. `context: fork` in command frontmatter starts a fresh agent without parent history and is unaffected.

### Inline briefs for reviewers and implementers

`/playbook:review` and `/playbook:implement` build the brief once per run: diff, numbered excerpts, test command, and the limit to read beyond them only when needed. On the seeded-defect reviewer fixture (7 seeded bugs, 3 clean, 3 repeats, Opus medium) recall stayed 21 of 21 and clean cases 9 of 9. Turns fell from 3.4 to 2.2 per run and cost from $0.0447 to $0.0395.

### Return cap

Subagent returns are capped at about 300 words by the agent prompts and the `Return` line of the brief template. The named output file is the deliverable and is not capped, so the delegating-subagents file handoff rule is unchanged.
