---
description: Use when an approved plan or ADR blueprint exists and the user says let's implement or build this. Execute-only, no new design. Delivers each Segment as one small PR.
allowed-tools: Bash, Read, Grep, Glob, Write, Edit, Agent, SendMessage, TaskStop, Skill, AskUserQuestion
argument-hint: "[plan | adr-blueprint | #issue | KEY-123 | ./spec.md | text] [--auto] [--ask] [--no-tdd] [--no-tests] [--force] [--pr-strategy=<stacked|independent|single>] [--boundary=<savepoint|pause|land>] [--all-lenses] [--help]"
model: sonnet
effort: medium
---

# Implement: Execute a Verified Plan

Execute an approved implementation plan or ADR blueprint. **This command is execute-only: it does NOT design or plan new scope.** Produce the plan with `/playbook:plan` (or `/playbook:adr` for an architectural decision) first, then implement it here. The one exception is the Step 8 refinement pass, which re-plans and applies behaviour-preserving cleanups to the code it just wrote (never new features).

**Incremental delivery.** `/playbook:implement` delivers the plan as PR-sized **Segments**, not one big change: it executes Segment by Segment, commits each Work Unit as a savepoint, and opens one small pull request per Segment (independent off the default branch when Segments are disjoint, stacked only when they truly depend on each other). Before executing, it prompts for how to deliver (PR topology and Segment-boundary behaviour) and recommends an option based on the plan's scope; in auto mode it self-selects the recommended options and records them as assumptions. This follows `playbook:engineering-standards`: PRs under 500 lines, one concern each, "ship a sequence of small PRs".

Invoked as `/playbook:implement`. The remaining arguments are the task reference and flags.

## Help

If the arguments contain `--help`, print this and stop:

```
/playbook:implement - Execute an approved plan or ADR blueprint

USAGE:
  /playbook:implement                  List saved plans and pick one to execute
  /playbook:implement <task-reference> [options]

TASK SOURCES:
  Plan file      /playbook:implement <plans folder>/user-avatar-upload.md
  ADR blueprint  /playbook:implement docs/adr/0001-websocket-push-blueprint.md
  GitHub issue   /playbook:implement #42   |   /playbook:implement https://github.com/org/repo/issues/42
  Jira ticket    /playbook:implement PROJ-123   (via Atlassian, see `playbook:atlassian`)
  File spec      /playbook:implement ./tasks/feature-spec.md
  Plain text     /playbook:implement "Add user avatar upload"

OPTIONS:
  --help     Show this help
  --auto     Autonomous: execute Segments in dependency order, each Work Unit
             committed as a savepoint, then open the PR set (no prompts;
             self-selects the recommended delivery strategy)
  --ask      Interactive: prompt at each decision, even when the environment
             or the repo config sets auto mode
  --no-tdd   Write tests alongside implementation instead of red/green/refactor;
             also presets the TDD-approach question and skips it (default: ask)
  --no-tests Write no new tests for this run (config-only, docs-only, or
             mechanical plans). Presets the TDD-approach question and skips
             it. Never self-selected under --auto. Conflicts with --no-tdd.
  --force    In --auto mode, override quality-gate FAILs (logged)
  --pr-strategy=<stacked|independent|single>
             Preset the PR topology and skip that question (default: ask)
  --boundary=<savepoint|pause|land>
             Preset the Segment-boundary behaviour and skip that question
             (default: ask; recommended: savepoint). The three are an
             ascending ladder of autonomy. `land` merges each Segment before
             starting the next and is OPT-IN ONLY: --auto never self-selects
             it, because it is the only boundary that puts code on the
             default branch with no human in the loop.
  --all-lenses
             Skip the Step 9 triage dispatch entirely: run all 5 lenses
             (correctness, behaviour drift, principles, scope, tests) at
             full-lens

DELIVERY: /playbook:implement splits the plan into PR-sized Segments and, before
executing, prompts for three things (unless preset by flag or running --auto):
  - PR topology: independent (default, disjoint Segments) | stacked (dependency chain) | single
  - Boundary:    savepoint (default) | pause | land (opt-in, merges each Segment)
It honors the plan's Segments. Each Segment becomes one pull request, kept as
small as possible and self-contained by area or feature.

PLANNING: /playbook:implement never designs. If the reference isn't a ready plan, it
stops and tells you to run /playbook:plan or /playbook:adr first.

REFINEMENT: after implementing, /playbook:implement runs one SOLID/DRY/KISS/YAGNI
simplify pass (executed autonomously) then an adversarial review (one reviewer for a
small diff, a lens swarm for a large one), before opening the PR set (or finishing,
per the chosen boundary).
```

## Step 0: Read the run mode

Do this first, after the `--help` check above and before anything else. Read the run mode for the `implement` command and this run's arguments. Intent: run-mode. If the session's tool map (printed at session start) lists it, use that command, then read `skills/delegating-subagents/references/run-context.md` in this plugin and follow it for the warning, the ceiling, the both-flags exit, any other failure, and an outdated tool. Otherwise do this: if `$ARGUMENTS` holds both `--auto` and `--ask`, stop with one line saying they conflict; `--auto` or `--ask` alone sets the mode; with neither, use `PLAYBOOK_MODE` when it is `auto` or `ask`; else the mode is `ask`. There is no effort ceiling.

**Routing.** Where you choose what to spawn, also pick the model tier for the `implement` kind. Intent: route-model. If the session's tool map lists it, use that command and follow its `action` under the `routing.escalate` gate: on `ask` stop and ask the user, on `downgraded` (`deny`) use the lower tier it names, and in auto mode (`auto`) proceed and log the assumption. If the command fails, carry on. For the implement tier pass the tier (`low`, `medium` or `high`) as `playbook:delegating-subagents` says. Otherwise do this: use the model each agent file pins, with no routing.

- **`ask` mode:** behave exactly as this file describes for an interactive run. Every prompt below stays.
- **`auto` mode:** take the recommended answer at each decision instead of prompting, and record every answer you chose yourself in an Assumptions list. Print that list in the final output.

Everything below that says `--auto` applies whenever the mode is auto, whether it came from the flag, the environment or the repo config.

### Auto path

In auto mode this command never force-pushes and never uses a forced lease. A push rejected as non-fast-forward parks the Segment and reports, so a person can decide what to do.

## Execution Rules (MUST)

1. **Execute every bash block for real with the `Bash` tool.** Don't simulate or predict output; drive the next step from real output.
2. **No caching.** Every invocation is a fresh run. Don't reuse results from prior conversations or training data.
3. **No skipping.** Execute steps in order. The only exception: steps guarded by a flag the user didn't set.
4. **No assumptions.** Don't guess file contents, command output, or environment state. Run it and read the result.
5. **Follow the command's gates, not your own.** If a step says "ask the user", ask. If it doesn't, don't add a gate.
6. **Show real data.** Tables and reports come from actual command output, never placeholders.
7. **Ground with the right tool for the search's size.** This command's own `allowed-tools` grants `Read`, `Grep`, and `Glob` directly, so a narrow, already-located lookup (a known file, a known symbol, confirming one pattern in one or two calls) MUST use them directly: an ad hoc `Agent` dispatch adds a round trip and returns through the unreliable channel `playbook:delegating-subagents` documents, for no gain when the answer is already cheap to fetch. A broad or open-ended search (the location isn't known, or it fans across many files) is the opposite case: doing that with `Read`/`Grep` yourself pulls raw, unfiltered output into this session's context, where an isolated `Explore` dispatch does the fan-out in its own context and returns only a short digest, keeping this session lean. Delegate that shape instead. A narrow lookup of a name runs `playbook code symbol <name>` or `playbook code refs <name>` directly first, after `playbook code status` (and `playbook code index` when it says stale or missing); Grep and Glob replace it only when the binary or the index is unavailable, and every `Explore` brief carries the same rule. Beyond this exploratory use, `Agent` is reserved for this command's named roles (`implementer`, `reviewer`, `critic`, `fact-checker`, `test-reviewer`, `review-triage`, `cheap-checker`), each with a defined prompt shape and delivery path an improvised dispatch lacks.

## Step 1: Resolve the Task Reference

**Storage folders.** This command uses three folders outside the checkout: the plans folder (Intent: plans-dir.), the implement folder (Intent: implement-dir.) and the worktrees folder (Intent: worktrees-dir.). If the session's tool map (printed at session start) lists them, use those commands. Otherwise do this: the folders are `~/.config/playbook/repos/<owner>/<repo>/plans/`, `.../implement/` and `.../worktrees/`, where `<owner>/<repo>` is `git remote get-url origin` with the scheme, any `user@`, the host and a trailing `.git` stripped (`git@github.com:acme/api.git` and `https://github.com/acme/api.git` both give `acme/api`). Create a folder with `mkdir -p` the first time you write to it. With no `origin` remote the folders cannot be resolved: stop with that error where a step needs one. The rest of this file calls them `<plans folder>`, `<implement folder>` and `<worktrees folder>`.

**No task reference given (empty, or only flags)?** Run the Plan Picker: list the saved plans. Intent: list-plans. If the session's tool map lists it, use that command. Otherwise do this: take every `*.md` file directly in `<plans folder>` except `*-quality.md`, `*.checkpoint.md` and gate-source files (`*.gate-source*.md`), plus every `docs/adr/*-blueprint.md`. For each, the title is its first Markdown heading and the status is `proposed`, `accepted` or `implemented` from the first line that mentions "status" (`?` when none).

The plain steps give one `path<TAB>[status]<TAB>title` row per plan file and ADR blueprint, or `NO_PLANS` when there are none. The tool's `--json` form gives `{path,status,title}` objects, and an empty array means `NO_PLANS`. Either way a failure prints `PLAN_PATH_ERROR: ...` when the plans folder cannot be resolved.
Present the rows as a numbered menu (index, status, title, path), listing unexecuted entries (`Proposed`/`Accepted`) first and `Implemented` last. Ask the user to pick a number, or to preview one first (Read it, show the summary, then re-ask). In auto mode never ask: when exactly one unexecuted entry exists, use it and log the assumption, otherwise stop and print the list. If the output is `NO_PLANS`, stop and tell the user to run `/playbook:plan` or `/playbook:adr` to create one. If the output starts with `PLAN_PATH_ERROR:`, stop and show the user that error verbatim (most likely no git `origin` remote); do NOT suggest running `/playbook:plan`, since the problem isn't an absence of plans. Use the chosen file as the task reference, then continue below. Any flags passed (e.g. `--auto`) still apply to the chosen plan.

Otherwise, resolve `$ARGUMENTS` (minus flags) by format:

- **Plan/blueprint file** (a file under `<plans folder>`, `docs/adr/*-blueprint.md`, or any path ending `.md`): Read it. This is the plan.
- **GitHub issue** (`#N`, or a github.com issue URL): `gh issue view <N> --json title,body,url,labels,state`.
- **Jira ticket** (`ABC-123`): load `playbook:atlassian` first and follow its selection order; if no route works, ask the user to paste the ticket.
- **Other file path:** Read it.
- **Plain text:** the text is the task statement.

If a fetch fails, ask the user to paste the task content. In auto mode stop with the fetch error instead of asking.

## Step 2: Execute-Only Gate (MUST)

Decide whether the resolved reference is a **ready, executable plan**: it names concrete files to change, an ordered set of steps or Work Units, acceptance criteria, and a test plan (Gherkin scenarios or TDD cycles).

- A `/playbook:plan` plan or `/playbook:adr` blueprint → ready. Proceed.
- A spec/issue/ticket detailed enough (explicit files, steps, acceptance criteria, tests) → ready. Proceed.
- **Anything else** (raw text, a thin issue/ticket, a vague request) → **STOP.** Tell the user: "This isn't a ready plan. Tell the user to run `/playbook:plan`, or `/playbook:adr` for an architectural decision, to produce one, then implement it here." Do NOT generate a plan inline; planning is `/playbook:plan` and `/playbook:adr`'s job. (The Step 8 refinement pass is the sole exception, and only to re-plan refactors of code already written, never new scope.)

If the plan or ADR blueprint ends with a "Confidence + open items" trailer, read it and carry the open items as a watch list through execution and the refinement/adversarial review: treat them as the spots most likely to be wrong, and confirm or resolve each before claiming the work done.

## Step 3: Load Standards and Context

- Invoke the `playbook:engineering-standards` skill (testing requirements, mocking, PR readiness, deployment), the `playbook:grounding-research` skill (verify before asserting), `playbook:delegating-subagents` (every dispatch names an output file and the orchestrator reads it; this command delegates every Work Unit, so it governs the whole run), and `playbook:writing-style` (for any prose, e.g. commit messages and the PR body).
- Load memory context for `<owner>/<repo>` (derived from `git remote get-url origin` as above). Intent: memory-context. If the session's tool map lists it, use that command, then load the fact files it names on demand (cross-project preferences, corrections, conventions, gotchas, and prior decisions); it prints a `Memory context: none` line saying why when it has nothing. Otherwise do this: list the fact files in `~/.config/playbook/memory/<owner>/<repo>/` (project facts), directly in `~/.config/playbook/memory/<owner>/` (org facts) and directly in `~/.config/playbook/memory/` (global facts), and read the ones whose names bear on the plan; with no store, skip memory. Note whether facts were found, so a gap or conflict can be traced to its source. Honor the typed edges: a project fact that contradicts a global one wins for this repo, and surface any conflict bearing on the work rather than silently choosing. If nothing is found, skip this step silently and proceed on the codebase and the plan alone.
- **Cost baseline:** find the most recently written `telemetry.jsonl` under `~/.config/playbook/runtime/` (one per session, written by the binary's status line on each render when it is installed), read its last line, and record the `cost_usd` field as this run's starting cost. No file yet (statusline hasn't rendered this session) means no baseline: Step 7 then reports the cost as unavailable rather than a delta.
- Read every file the plan references before changing it (grounding).
- **Detect the stack** to know the verify commands: check `tsconfig.json` / `package.json` (TS/JS), `pyproject.toml` / `setup.py` (Python), `go.mod` (Go), `Cargo.toml` (Rust). Derive the type-check / lint / test commands from what you find.

**Knowledge capture:** when you discover a durable convention or gotcha, write it as a project memory fact only if a project store is present at `~/.config/playbook/memory/<owner>/<repo>/`; otherwise skip silently.

## Step 4: Quality Gate (conditional)

If the plan came from `/playbook:plan` or `/playbook:adr` it already has a companion `*-quality.md` report; trust it and skip to Step 5. Otherwise (a file/issue/ticket spec), run the inlined 3-phase gate before executing:

After each phase returns, record its verdict (Intent: gate-record.), and before leaving the gate, check that every phase passed (Intent: gate-check.). If the session's tool map (printed at session start) lists them, use those commands: record with `<plan-slug> implement <phase> - --source <source-path>` and the agent's full raw return text on stdin (a quoted heredoc, no scratch file), and check with `<plan-slug> implement fact-check adversarial test-review --source <source-path>`. Otherwise do this: keep a checklist of the three phases in the progress ledger (Step 5), writing each phase's latest verdict (PASS, WARN, FAIL or STUCK) and the source file's hash (`git hash-object <source-path>`) after every return. Without the tools this gate is advisory: nothing enforces it, so say so in the final report.

Every record and check takes the source path, `<source-path>`, which depends on which of Step 1's task-reference formats resolved: a **plan/blueprint file** (under the plans dir or `docs/adr/*-blueprint.md`) or an **other file path** already exist on disk, so `<source-path>` is that same file's own path, no extra write needed. A **GitHub issue**, a **Jira ticket**, or **plain text** exist only as fetched/typed content with no stable file of their own, so write that content to a scratch file first (e.g. under `/tmp/<repo>/`) and pass that scratch file's path as `<source-path>`.

1. **Fact-Check** (`fact-checker` agent): every referenced path exists, signatures/imports match, downstream consumers identified, test infra present. After it returns, record the `fact-check` verdict.
2. **Adversarial Review** (`critic` agent, focus `pre-exec`, + the fact-check report): simpler alternatives, scope creep, missing error paths, blast radius. After it returns, record the `adversarial` verdict.
3. **Test Review** (`test-reviewer` agent): regression-pinning, flakiness, independence, mock quality, assertion strength. After it returns, record the `test-review` verdict.

Revise on FAIL under the gate loop (`playbook:delegating-subagents`, `references/gate-loop.md`: hard cap `gate.maxRounds` rounds, stop at PASS, STUCK on no progress), recording again after every retry's return so a later PASS overwrites an earlier FAIL (a record replaces the earlier one for the same plan and phase; only the last recorded value before the check below matters).

Before proceeding past this gate, run the check. Only continue to Step 4.5 if it passes (the tool exits 0, or every checklist row reads PASS or WARN against the current source hash); otherwise report exactly which phase(s) are missing, failed, or stale, copying the tool's own output verbatim when there is one rather than re-narrating it. A FAIL or STUCK blocks execution unless the user explicitly overrides (or `--auto --force`): the override never changes or fakes the check's result, it is an explicit, recorded decision to proceed despite a real, honestly reported failure, not a claim that the gate actually passed. A STALE phase is different: it means that phase's recorded verdict no longer matches the current source hash, not that the phase's own review was wrong, so re-run that same phase against the current source without revising anything, since revising in response to a STALE would itself cascade staleness onto sibling phases. `--force` overrides the block; it must never write a fake PASS into the gate record or the checklist. If a project store is present at `~/.config/playbook/memory/<owner>/<repo>/`, record gotchas and rejected alternatives as memory facts (one file write per fact); otherwise skip silently.

## Step 4.5: Delivery Strategy Gate (MUST, before executing)

`/playbook:implement` delivers the plan as PR-sized **Segments** (one concern, one pull request each), not one monolithic change. Settle the delivery shape now, before any code is written.

**1. Resolve Segments.**

- If the plan has a **Segments** table (a `/playbook:plan` plan does), use it: each Segment is an ordered group of Work Units, and the WU table's `Segment` column assigns every WU.
- If it does NOT (an older plan, an issue, or a spec), **derive** Segments here: pack the Work Units into groups that are self-contained by area or feature and as small as possible (tests do not count toward size), in dependency order, one concern per group where the WU titles make the boundary obvious. A tiny plan may be a single Segment.

Either way, confirm the Segment ordering respects the WU `Requires` graph (no forward cross-Segment dependency) before continuing; reorder if needed.

**2. Choose the delivery strategy.** In interactive mode you MUST ask the user before any code is written (this is the "always ask before implementing" gate). Do this by **calling the `AskUserQuestion` tool** with the three questions below in a single call, each option's recommended choice listed first and labelled, recommended per the plan's scope. Do NOT infer the answers, and do NOT start executing Step 5 until the user has answered. The three questions:

- **PR topology:**
  - **Independent off the default branch** (default recommendation): each Segment branches directly off the default branch and opens its own PR against it, no PR based on another PR. Recommend whenever the Segments have disjoint files and no cross-Segment `Requires`.
  - **Stacked:** each Segment branches off the previous one; PR N targets Segment N-1's branch. Recommend only when Segments form a genuine dependency chain (Segment N's code doesn't exist without Segment N-1). Caveat, confirmed 2026-08-26: GitHub's native PR-stack detection routes a base-chained PR set through its async merge API, which (unlike the classic merge API) has no admin/bypass-override parameter. On a repo whose branch protection requires a review the author can't self-grant, an agent can merge every other topology via bypass but cannot merge a stacked chain at all; it needs a human to click through each PR in order. Recommend stacked only when the dependency is real and the user is ready to merge by hand, or knows an independent equivalent isn't practical for this plan.
  - **Single PR:** the current whole-plan behaviour. Recommend for a one-Segment or tiny plan (escape hatch).
- **Segment-boundary behaviour** (always recommend **Savepoints**):
  - **Savepoint commits, PRs at end** (default recommendation, and the norm under `--auto`): implement every Segment as savepoint commits on their (unpushed) branches, run Steps 7-9 once over the full diff, then open the PR set at the end. Because nothing is pushed until then, Step 8's stack rebase stays local and each PR-open push is a first push, not a force-push.
  - **Pause after each PR:** finish a Segment, run Steps 7-9 scoped to just that Segment, open its PR, then stop for the user before the next Segment. No cross-Segment rebase happens (earlier PRs are already open), so a fix implicating an already-delivered Segment becomes a follow-up, not a rebased commit.
  - **Land each Segment before the next** (opt-in only; never recommended by default, and `--auto` never self-selects it): finish a Segment, run Steps 7-9 scoped to it, open its PR, promote it to ready, wait for its required checks, fix what CI finds, then merge it and confirm `state: MERGED` before Segment N+1's branch is even created (Step 10). Recommend this ONLY when the user states this repo has no human review gate they want honoured, typically a solo repo. **This is the only boundary that puts code on the default branch with no human in the loop**, so it also fires whatever merging to that branch triggers (deploys, releases, tag automation), once per Segment. Where a review gate turns out to be real, the Segment parks and the run stops rather than bypassing it.

**`land` overrides stacked topology (MUST, and record it).** Under `land`, Segment N is merged into the default branch before Segment N+1's branch exists, so Segment N+1 branches off the freshly-fetched `origin/<default-branch>`, exactly as under **independent**. If the user chose **stacked**, honor the dependency but not the branch chain: the declared `Requires` is still satisfied, through merged history rather than through an open sibling's branch. Say so in the run's assumptions. This is also why the caveat at the **Stacked** option above (GitHub's async merge API has no bypass parameter, so an agent cannot merge a stacked chain) never applies under `land`: no two PRs in the chain are ever open at the same time, so no PR's base is ever another open PR's branch.

**`land` is never auto-selected (MUST).** `--auto` with no `--boundary` still self-selects **savepoint**. `land` requires an explicit `--boundary=land` or an explicit answer to this question. `--auto --boundary=land` is legal and means the user opted in deliberately. Before Step 10's first merge attempt, print the resolved `<owner>/<repo>` and default branch once, so a wrong-repo invocation is visible without costing a prompt.

- **TDD approach** (always recommend **red/green/refactor**):
  - **Red/green/refactor** (default recommendation): each test scenario gets its own failing-test, minimal-implementation, and refactor dispatch, per Step 5's TDD flow below. Recommend whenever the plan's Work Units carry Gherkin scenarios (the common case).
  - **Tests alongside implementation:** one dispatch per logical file group writes code and tests together (still encoding the plan's scenarios), skipping the three-dispatch cycle. Recommend for a plan that is mostly deletions/mechanical edits with little new logic (TDD adds dispatch overhead without adding rigor there).
  - **No new tests:** skip writing tests for this run. Step 7 still runs the existing suite, type check, and lint. Suited to config-only, docs-only, or purely mechanical plans. Never the recommended option and never self-selected under `--auto`. `playbook:engineering-standards` says code changes MUST include tests, so this is a per-run human override of that rule, logged in the ledger and the PR body, never a default.

**Flag presets.** `--pr-strategy=<stacked|independent|single>`, `--boundary=<savepoint|pause>`, `--no-tdd`, and `--no-tests` each preset a choice and skip its question. If both `--no-tdd` and `--no-tests` are passed, stop with one line: "--no-tdd and --no-tests conflict; pass one." Absent (and not `--auto`) means ask; this preserves "ask every time" as the default. **Single** topology opens its one PR at the end regardless of boundary (it has a single PR, so "pause after each" is moot).

**3. `--auto`:** do NOT ask. Self-select the recommended options (independent topology when the Segments are disjoint, stacked otherwise; savepoints; red/green/refactor unless `--no-tdd` presets tests-alongside; never "no new tests", which only `--no-tests` or an explicit answer selects) and record them in the run's assumptions, surfaced in the final report / PR follow-ups. Flag presets still win over the auto default. `--force` still overrides quality-gate FAILs.

Record the resolved Segments and the chosen strategy in the progress ledger (Step 5) so a resumed run continues with the same shape.

## Step 5: Execute (delegated subagents, reviewed)

**Delegation (MUST):** this command runs on Sonnet. The orchestrating session reads the plan and delegates each implementation chunk to a fresh `playbook:implementer` subagent via the Agent tool (never a forked one, which would inherit this whole session), then reviews the result. Delegation keeps each chunk in a fresh, isolated context (no bleed between cycles); the orchestrator spends its turn reviewing, not editing. Independent Work Units run in parallel by default, each isolated in its own git worktree, with the model tier set per role (see the scheduler below). The deep design reasoning already happened in `/playbook:plan` or `/playbook:adr`, so execution doesn't need Opus.

Every Agent prompt MUST include: a pointer to its Work Unit's brief file (Step 5's File-based handoff), which carries the plan content that dispatch needs, and the specific cycle/step. The `implementer` agent already carries the design principles, the grounding rules and the test structure, so do not restate them in the prompt. Point at the brief; don't paste the whole multi-WU plan into every prompt.

**Design principles (MUST).** The rules every change follows (SOLID, DRY, KISS, YAGNI, composition over inheritance, explicit logged steps, idempotent operations, assertive errors without PII) live in `playbook:engineering-standards` (loaded in Step 3) and in the `implementer` agent that writes the code. When SOLID's abstraction pulls against KISS/YAGNI, favour the simplest thing that meets the plan. They are also the lens for the refinement pass (Step 8) and the principles review lens (Step 9).

**Execution unit (MUST): the plan's Work Units, grouped by Segment.** Execute **one Segment at a time** in dependency order (Step 4.5). Within a Segment, execute its Work Units with the wave scheduler below; each WU becomes one small savepoint commit. The scheduler, worktree isolation, TDD flow, and verify-by-diff are unchanged; they just run scoped to the current Segment's WUs. **A wave never mixes WUs from two Segments:** the outer Segment loop is strictly sequential relative to the inner wave loop, so the ready set is always drawn from the current Segment only.

**Per-Segment setup (MUST): branch per topology.** Before a Segment's first commit, put HEAD on the right branch (always branch-first; never commit to the default branch). Intent: segment-branch. If the session's tool map (printed at session start) lists it, use that command with the type, `<plan-slug>`, `N`, the Segment title, the topology, the default branch, the previous Segment's branch when stacked, and the land flag under `land`; it prints `branch=<name> base=<sha> base_ref=<ref>` and exits 1 when the branch already exists or, under single, when HEAD is on the default branch. Otherwise do this:

1. `<seg-slug>` is the Segment's Title lowercased, every run of characters that are not ASCII letters or digits turned into one `-`, cut to 40 characters, with leading and trailing dashes trimmed (the same way `<plan-slug>` derives from the topic in Step 7 of `/playbook:plan`). The branch is `<type>/<plan-slug>-s<N>-<seg-slug>`, for example `feat/my-plan-s2-parser-core`.
2. Under **single** (without `land`) nothing is created: the whole plan shares one branch. If `git rev-parse --abbrev-ref HEAD` is the default branch, stop and switch to the plan's branch first. `<segment-base>` is `git rev-parse HEAD`.
3. Otherwise, if `git rev-parse --verify --quiet refs/heads/<branch>` succeeds, the branch already exists: stop, unless this is a resume (below).
4. Pick the base ref. Under `land` (all three topologies): `git fetch origin <default-branch>` and use `origin/<default-branch>`, never the previous Segment's branch (already merged, possibly deleted) and never the stale local default branch. **Stacked**, Segment 2 or later: the previous Segment's branch, or, when it is not local, `git fetch origin <prev-branch>` and `origin/<prev-branch>`. Everything else (**independent**, and stacked Segment 1): the default branch.
5. `<segment-base>` is `git rev-parse <base ref>`. Run `git switch -c <branch> <segment-base>`.

The base for Segment N's PR is Segment N-1's branch under **stacked** and the default branch under **independent**.

On a **ledger-driven resume** (Step 5 ledger, e.g. after `/clear`, a crash, or a fresh checkout), a Segment branch that already exists is resumed with `git switch`; fetch a stacked previous branch that is not local. Record `Segment id -> branch -> <segment-base> -> WU commit range` in the ledger as you go.


**Parallel-by-default scheduler (MUST).** Don't wait for the plan to pre-label groups. Build the WU dependency graph from the `Requires` column and run the acyclicity check (the procedure in Step 6) now, before executing. Then execute in waves until every WU is done:

1. **Ready set.** The WUs whose `Requires` are all complete.
2. **Form a wave.** Take the largest subset of the ready set that's safe to run together. Parallel is the default. Two ready WUs run sequentially ONLY when one of these forces it:
   - a real dependency edge between them (transitive `Requires`),
   - their `Files` lists intersect,
   - shared mutable state: both touch a denylisted shared surface (migration dirs; lockfiles `package-lock.json`, `yarn.lock`, `pnpm-lock.yaml`, `poetry.lock`, `Cargo.lock`, `go.sum`; generated, barrel, or index files; global registries; codegen outputs), or there's genuine doubt about shared state.

   A plan `Parallel group` annotation, when present, confirms safety but isn't required. A WU that clashes with the forming wave drops to a later wave.
3. **Write the wave's briefs.** Before dispatching, write one brief per WU in the wave, `<implement folder>/<plan-slug>/<wu-id>.brief.md` (the WU id lowercased, e.g. `wu-3.brief.md`). Intent: work-unit-briefs. If the session's tool map (printed at session start) lists it, use that command once for the whole wave with the plan file, the WU ids, that output folder, the worktree base `<worktrees folder>/<plan-slug>` (or the current tree for a wave of one) and the scoped verify with `{tests}` for the WU's test files; it exits 1 and writes nothing when an id has no section in the plan. Otherwise write each brief yourself:

   - A header: `# Brief: <WU-id> <title>`, then `Plan: <plan file>`, `Worktree: <worktrees folder>/<plan-slug>/<wu-id>` (or the main tree's path for a wave of one), and `Scoped verify: <command>` with the WU's test files (paths under `Files` that contain `test` or `spec`) in place of `{tests}`.
   - The WU's own `### WU-N` or `#### WU-N` section of the plan, copied verbatim up to the next heading of the same or a higher level.
   - When a memory store was loaded in Step 3, a `## Relevant memory` list of the facts that name the WU's files or match its title; leave the section out when there are none.

   If an id has no section in the plan, write nothing for the wave: correct the id, or stop and tell the user the plan has no section for that Work Unit.
4. **Dispatch the wave concurrently.** Issue the Agent calls in a single message so they run at once, one worktree per WU (see Worktree isolation). A wave of one runs in the main tree with no worktree. Give each Agent a stable `name`.
5. **Integrate, then recompute.** After the wave returns, integrate (below), append to the ledger, then recompute the ready set for the next wave.

Scope each WU's verify command to its own test files (the full suite runs in Step 7) so an in-progress sibling can't trip another's tests.

**Worktree isolation (parallel waves).** For each WU in a multi-WU wave:

- Add one worktree per WU off the current branch. Intent: segment-unit. If the session's tool map (printed at session start) lists it, run `segment unit start <wu-id> --base <branch tip> --path "<worktrees folder>/<plan-slug>/<wu-id>"`; otherwise `git worktree add --detach "<that path>" HEAD`. Record the base SHA in the ledger (Progress ledger, below): it's the anchor `git log` resumes against.
- Dispatch the implementer to work in that worktree path (absolute paths in its brief). A Work Unit with several test scenarios means several dispatches into the same worktree (see "With TDD" below); each commits its own step there. It does NOT push.
- **Integrate:** once the WU's last dispatch passes review, with the commit message from Commit per Work Unit below, run (Intent: segment-unit) `segment unit integrate <wu-id> --base <wu-base-sha> --path <worktree> --branch <Segment branch> -m "<message>" --files <the WU's Files>` from the Segment branch's checkout, in dependency order. It squashes, makes one signed, signed off commit of those files only, cherry-picks it onto the Segment branch (never the default branch) and removes the worktree. Disjoint files make this conflict-free; on a conflict the safety test was violated: it exits 1, keeps the worktree with a `.playbook-conflict-stop` marker, and you STOP and report. Without the command: `git reset --soft <wu-base-sha>`, one `-S -s` commit of the files, `git cherry-pick` it, and on a conflict write the marker (reason on line 1, ISO-8601 UTC timestamp on line 2) into the kept worktree.
- Single-WU waves skip the worktree and run in the main tree, on the Segment branch directly. If the session entered a worktree with `EnterWorktree`, the harness refuses the implementer's shell commands in the main tree: call `ExitWorktree` with `keep` first (see "Worktree guard" in `playbook:delegating-subagents`). The WU's base SHA is that branch's tip when the WU starts. The commit and resume mechanics below are the same either way: only the tree differs.

**No lock-step across WUs in a wave.** WUs in a wave proceed independently. If WU-A's RED returns before WU-B's RED, dispatch WU-A's GREEN immediately; don't wait for sibling WUs to reach the same scenario or step.

**File-based handoff.** Keep the orchestrator's context clean over long runs:

- Each WU's brief (its `Files`, `Changes`, `Test scenarios`, `Done When`, the worktree path, the scoped verify command and its relevant memory) is written to `<implement folder>/<plan-slug>/<wu-id>.brief.md` in the scheduler's step 3, above, which points the implementer subagent at that file instead of pasting the whole plan into every prompt. A "Relevant memory" section appears only for a WU that has anchored or title-matching facts.
- A WU dispatches once per step (RED, GREEN, REFACTOR per scenario, or once per file group under `--no-tdd` or `--no-tests`), so its report path is scoped per dispatch: `<wu-id>.<step-slug>.report.md`, e.g. `wu-3.s2-red.report.md`. A shared `<wu-id>.report.md` would let GREEN's report clobber RED's before it's ever read. Each dispatch writes its full report there and returns ONLY: a status (`DONE`, `DONE_WITH_CONCERNS`, `BLOCKED`, or `NEEDS_CONTEXT`), its commit SHA, and a one-line test result.

**Read the report file (MUST, per `playbook:delegating-subagents`).** The dispatch's report file is the deliverable; the returned status is a courtesy. The moment a dispatch finishes, goes idle, or is given up on, **read its report file before doing anything else with that WU**, including before deciding it produced nothing. Agent-tool spawns frequently complete their work and return no result at all, so a silent agent is not an empty one. If the file is missing, say so explicitly rather than inferring what it would have said.

This is not optional when the commit looks fine. The report is the only place a WU records what it deliberately did NOT do: divergences it preserved on purpose, edges it left untested, files it declined to touch as out of scope. On 2026-08-16 a WU report stated plainly that `Command::Init` was still wired to nothing, and that a shell harness would break once a registry file was deleted. Both were blocking, both were rediscovered by hand a day later, and both were sitting in a file the orchestrator never opened while every test passed.

**Verify-by-diff (MUST).** Never take the subagent's word. After a WU returns, confirm the work from git (`git show --stat <sha>`, review the diff against the brief) and the scoped verify. A `DONE` the diff doesn't support is a failure: re-dispatch or stop per Error handling. Git tells you whether the work landed; only the report tells you what the agent observed, so do both.

**Progress ledger.** Record progress in `<implement folder>/<plan-slug>.progress.md`. This folder lives outside the repo checkout (`~/.config/playbook/repos/<owner>/<repo>/implement/`, shared by every checkout and worktree of the repo), so there is nothing to gitignore. At the top, record the resolved Segments and the chosen delivery strategy (topology + boundary + test approach, including "no new tests" when chosen) from Step 4.5. Per Segment, record its id, branch, and its PR URL once opened. Per WU, record one of three statuses: `NOT_STARTED` (or the row absent), `IN_PROGRESS` (its base SHA, recorded the moment its first dispatch starts), or `DONE` (its commit range, after squash and integration). On a fresh run or after compaction, read the ledger first: skip WUs recorded `DONE` and Segments already delivered (a paused run resumes from the first Segment without a PR URL); a WU recorded `IN_PROGRESS` routes to the resume procedure below instead of a fresh dispatch.

**Under the `land` boundary, each Segment additionally records** (nothing else in this schema changes, and a Segment under **savepoint** or **pause** omits these fields entirely rather than writing them empty): `land:` one of `NOT_STARTED | READY | CI_RUNNING | CI_FIXING | CONFLICT | MERGE_ATTEMPTED | MERGED (<merge-sha>) | PARKED | CI_UNSTABLE`; `review:` which review satisfied the ready-promotion gate, e.g. `implement-step9 (5 lenses, 3 findings fixed)`, plus a line per fix re-review, e.g. `fix-review tier-1 (correctness, 0 findings)`; `ci_fix_attempts: <n>/3` and `ci_rerun_attempts: <n>/2` as two separate counters, since a rerun is not a fix; `merge_attempts:` each attempt with its verbatim outcome, e.g. `--auto -> armed`, `--admin -> refused: <message>`; and, when parked, `parked_reason:` (the verbatim blocking field and refusal message) with `parked_at:` (ISO-8601). A resumed run reads `land:` before doing anything else with that Segment: `MERGED` skips it, `PARKED` routes to Step 10's parked-resume procedure, and anything else re-enters Step 10 at the stage that state names, never at Step 5. First use in a repo, create the implement and worktrees folders (Step 1's storage folders, created if missing), and stop if either cannot be resolved.

**Model tiering (MUST, never omit `model`).** Implementer Tasks spawn the `implementer` agent, which pins `model: sonnet` itself, so you don't set the model on those calls. Ledger writes stay a direct orchestrator action: a few-line append per WU, small enough that a separate dispatch would cost more in round-trip latency than it saves in tokens. Verification and the adversarial review (Step 9) run the capable tier. An omitted `model` on a non-typed call silently inherits the priciest default, so always set it there.

**With TDD (default).** A Work Unit with N test scenarios means up to 3N dispatches: for each scenario, in dependency order:

Every implementer brief also carries numbered excerpts of the files the step edits, the scoped verify command and the limit to read beyond them only when needed ("Implementer brief builder" in `skills/delegating-subagents/references/briefs.md`). With `--facts-tokens <N>` above 0 (only when the user asked for code facts), `playbook plan brief` also adds a `## Callers` block from the code index; without it, it adds none.

1. **RED** - `implementer` subagent (`subagent_type: playbook:implementer`): "Write ONLY the failing tests encoding this Gherkin scenario, AAA-structured. Don't touch production code." Then run the verify command: tests MUST fail (if they pass, the test proves nothing - fix it). On success, commit: `git commit -m "wip(<wu-id>): red - <scenario-id>"`, signed and signed off, inside the WU's tree. Never pushed.
2. **GREEN** - `implementer` subagent: "Write ONLY the minimal implementation to pass." Run verify: tests MUST pass (on failure, spawn a follow-up `implementer` with the error output). On success, commit: `wip(<wu-id>): green - <scenario-id>`.
3. **REFACTOR** - `implementer` subagent: "Clean up without changing behaviour; tests stay green." Run verify. On success, commit: `wip(<wu-id>): refactor - <scenario-id>`.
4. **Orchestrator review:** read the modified files; confirm changes match the plan, doc comments explain WHY, no unplanned side effects.

Each `wip` commit is a checkpoint, not the delivered shape: see Commit per Work Unit, below, for the squash that turns them into the WU's one real commit.

**RED anti-patterns to avoid (MUST).** A test that passes for the wrong reason is worse than no test:

- **Testing through implementation details.** Mocking internals, calling a private method, or verifying via a side channel (e.g. querying a table directly) instead of the public interface the scenario describes. Test the behaviour, not how it's built.
- **A tautological test.** The expected value is computed the same way the implementation computes it, including a hand-derived snapshot, so the test can't disagree with a wrong implementation. Derive the expectation independently.
- **Writing every scenario's tests before any implementation.** The per-scenario RED/GREEN/REFACTOR loop above already prevents this structurally; don't defeat it by batching RED across scenarios.

**Without TDD (`--no-tdd`).** For each logical file group: one `implementer` subagent (`subagent_type: playbook:implementer`) implements code + tests together (tests still encode the Gherkin scenarios); run verify; commit `wip(<wu-id>): <group-name>` on success; the orchestrator reviews as above.

**Without tests (`--no-tests`).** For each logical file group: one `implementer` subagent implements the code and is told explicitly not to write or change tests. Verify is the existing scoped checks (type check, lint, the existing tests for the touched code); existing tests must not break. Commit `wip(<wu-id>): <group-name>` on success. A plan Done When item that names a new test is waived under this option; say so in the final report.

**Commit per Work Unit (MUST): small commits.** One coherent commit per WU, in the tree the WU used (worktree for a parallel wave, the Segment branch in the main tree for a single-WU wave). The `wip` commits above are checkpoints while the WU is in flight, not the delivered shape:

1. Before writing the WU's commit message, read the `playbook:writing-style` skill's `references/conventional-commits.md` (invoke the skill and read the file from the skill base directory it shows; if you cannot find it, follow the usual types (`feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`)) (once per run, reuse it while it is still in context). Once the WU's last dispatch (its last REFACTOR, or its last `--no-tdd` or `--no-tests` group) passes review, squash: `git reset --soft <wu-base-sha>` then one signed (`-S`) and signed off (`-s`) commit with the WU's real message, staging exactly its `Files`. When a ticket key is known for the plan, the message scope is the key (`feat(PROJ-123): ...`), because a Segment branch name is a slug that never carries it.
2. **Parallel wave (worktree):** during integration the orchestrator cherry-picks that one commit onto the current Segment branch in dependency order (never the default branch). The Segment's branch stays local until its PR opens (Step 9, `/playbook:pr-create` handles the push); under the pause boundary that push happens per Segment mid-run, under savepoint at the end. Confirm the WU's files landed (`git log --stat`); `segment unit integrate` already removed the worktree (by hand, `git worktree remove <path>` it); the cherry-pick already confirms the work is safely integrated. If it refuses (the worktree is locked, or has uncommitted changes `git log --stat` doesn't explain), the refusal is real and MUST be investigated before forcing with `git worktree remove --force <path>`.
3. **Single-WU wave (main tree):** the squash commit already sits on the Segment branch and stays local; it is the savepoint, and nothing is pushed until Step 9 opens the PR. Confirm the files are committed (they no longer appear in `git status --porcelain`).

If a commit, squash, or cherry-pick fails, retry once, then stop and report.

**Resuming a Work Unit whose dispatch died mid-flight (MUST).** Trigger: a dispatch returns nothing usable (idle, errored, no report file), or a ledger-driven resume lands on a WU recorded `IN_PROGRESS`.

1. Per "Read the report file" above, read the last dispatch's report first, before classifying anything.
2. If that dispatch is still alive but idle, `TaskStop` it before dispatching a replacement. Two write-capable agents in the same tree risks a corrupted tree, not a recovery.
3. Find the WU's tree (its worktree, if still present, else the Segment branch) and its base SHA from the ledger. A missing worktree only means recreate it before continuing; it does not mean there's nothing to resume; a single-WU wave never had one, and its `wip` commits live on the Segment branch itself.
4. Run `segment unit resume <wu-id> --base <wu-base-sha> --dir <tree>` (Intent: segment-unit), or read `git log --oneline <wu-base-sha>..HEAD` in that tree. The last `wip` commit's step and scenario say what's next (none found: start the WU from scratch, first scenario, RED).
5. Re-run the scoped verify against the tree's current state before trusting that last commit (the same Verify-by-diff principle above, applied to the last checkpoint instead of the whole WU). If it doesn't hold, drop that last checkpoint commit with `git reset --keep HEAD~1` (it refuses instead of discarding local edits that conflict), then redo that step from the previous checkpoint.
6. Continue the WU's per-scenario loop from there, dispatching fresh implementers only for what remains.

This restores the work, not the agent: nothing revives a dead or unreachable dispatch (per `playbook:delegating-subagents`, `SendMessage`-based recovery is unreliable for this agent type too). The existing "3 fix retries then stop" rule (Error handling, below) is unchanged and still governs a scenario whose verify genuinely fails after a real, confirmed attempt; this procedure targets the separate case of a dispatch that died or went silent with real, uncommitted-but-checkpointed progress on disk.

This mechanism doesn't apply to Step 8's refinement Work Units: they're synthesized in-session with no plan entry and no brief file, small enough that a failed one is simply redone whole.

## Step 6: Autonomous Mode (`--auto`)

`--auto` executes the Segments in dependency order, committing each Work Unit as a savepoint, then opens the PR set (Step 9). It self-selects the delivery strategy (Step 4.5: independent topology when the Segments are disjoint, stacked otherwise; savepoints), records it as an assumption, and runs without pausing, so:

- **Branch first.** Each Segment is created on its own branch per Step 5's per-Segment setup (never commit to the default branch). Never `--no-verify`, never force-push.
- A FAIL in Step 4 blocks; `--force` overrides it (logged to the quality report).

**Cycle check (MUST, before executing, both modes).** Step 5's scheduler runs this before any wave; in `--auto` it's the same check. Verify the Work Unit dependency graph is acyclic:

1. Build the adjacency list from the plan's Work Units table (`WU-N -> [Requires]`).
2. Count incoming edges per WU; seed a queue with the zero-incoming WUs.
3. Process the queue: mark each WU resolved, decrement the count of every WU it enables, enqueue any that hit zero.
4. If any WU is unresolved, those form a cycle: name them, break the cycle (extract shared logic into a new WU, merge, or reverse an edge), and re-check.

Report the result: `Cycle check: PASS (N WUs resolve in topological order)` or how a detected cycle was fixed.

**Per Segment (in dependency order), then per Work Unit (or parallel batch) within it:** create the Segment's branch per Step 5's per-Segment setup, run its WUs as below, then move to the next Segment.

1. Confirm all WUs in the "Requires" column are done.
2. Dispatch each wave with the Step 5 parallel-by-default scheduler (ready set, safety test, worktree isolation, integration). Don't gate on `Parallel group` annotations; parallelize whatever the safety test allows, sequential only when forced. Within a WU, WU-0 types first, then the RED/GREEN/REFACTOR flow per cycle (or a single subagent for `--no-tdd` or `--no-tests`), delegated to Sonnet.
3. **Post-WU review:** changes match each WU's spec and file plan; doc comments explain WHY; no files outside the file plan touched.
4. Commit and integrate per the Step 5 commit rules: each dispatch checkpoints with a `wip` commit, the orchestrator squashes the WU's checkpoints into one commit, cherry-picks it (worktree) or pushes it (main tree). A dispatch that dies mid-WU resumes per Step 5's resume procedure instead of restarting the WU, same in `--auto` as interactively.
5. Mark each WU's "Done When" checkboxes in the plan file. This is the plan's static acceptance criteria, separate from the `wip`-commit checkpoints above: one records what the WU must achieve, the other records how far a dispatch got.

**Error handling:** when a WU's verify fails or its output is wrong, apply the `playbook:systematic-debugging` skill before retrying: find the root cause first, don't stack blind fix attempts. If it still fails (verify fails, wrong output, or commit fails) after 3 fix retries, **stop**. Don't continue to dependent WUs. Report the failed WU, the root cause found so far, and the remaining WUs. This governs a scenario whose verify genuinely fails after a real attempt; a dispatch that died or went silent uses Step 5's resume procedure first, not this retry count.

## Step 7: Validate

Run the project's checks (from Step 3 detection), e.g. type-check, lint, and tests. In `--auto`, run the full suite (not just affected) and, on failure, apply the `playbook:systematic-debugging` skill to find the root cause, then spawn an `implementer` subagent (`subagent_type: playbook:implementer`) to fix the responsible WU on its Segment branch, then commit the fix as a new commit by invoking `/playbook:commit` (no `-a`, so a later push is never a force; max 3 attempts; if still failing, stop and do NOT open any PRs).

- Fix and re-validate until green.
- **Doc audit:** every new/modified function has a doc comment explaining WHY; add any that are missing.
- **Update status:** change the plan's `Status: Proposed` to `Status: Implemented`.
- **Memory capture (MUST):** if a project store is present at `~/.config/playbook/memory/<owner>/<repo>/`, write every durable fact this run produced: notable errors and their fixes, conventions or gotchas discovered, and decisions made under ambiguity, the same categories the system prompt's Memory section scopes to feedback and project facts. This is required, not conditional on the model remembering to do it: `/playbook:implement` is the one capture path in this design that a command executes and checks, rather than a hook that only prompts. No project store means skip silently. The graph rebuilds automatically on fact save via the PostToolUse hook.
- **Cost delta:** read the run's current sample from the same `telemetry.jsonl` and report `cost_usd` minus the Step 3 baseline as this run's cost; if Step 3 recorded no baseline, report the cost as unavailable rather than guess. This is a session-level delta between two telemetry samples, not per-agent token accounting: no hook payload exposes per-agent tokens, so don't present it as more precise than that.

In `--auto`, after validation passes, continue to the refinement pass (Step 8). The PR opens at the end of Step 9, after the refinement and adversarial review pass, not here.

## Step 8: Refinement Pass (one pass, autonomous)

Once the implementation is green, run ONE refinement pass over the code you just produced. It runs autonomously (no pause, in interactive and `--auto` alike). It executes once per delivery unit: once over the full diff under the **savepoint** boundary, or once per Segment (before that Segment's PR) under **pause**; re-validation never re-enters this step for the same unit. It refines existing code; it does NOT add new scope (YAGNI keeps `/playbook:implement` execute-only). Apply the same commit safety as Step 6: never commit to the default branch, never force-push.

**Multi-Segment routing (MUST).** How this pass runs depends on the Step 4.5 boundary, because you must never force-push (and never rewrite a branch whose PR is already open):

- **Savepoint boundary (default):** no Segment's PR is open yet and the branches are unpushed, so this is safe. Run a single refinement pass over the full implemented diff, commit each fix onto **the Segment branch that owns the touched file** (the earliest Segment that introduced it), then, under stacked topology, rebase the later Segment branches onto their updated bases in order (`git rebase --onto`), purely local. Re-run the Step 7 checks on the affected branches. Step 9 opening the PRs is then the first push of each branch, not a force-push. (Single and independent topologies need no rebase.)
- **Pause boundary:** earlier Segments' PRs are already open, so you cannot rebase them. Run the refinement scoped to the **current** Segment only, before its PR opens (this is why the pause flow runs Steps 7-9 per Segment). A fix that implicates an already-delivered Segment is recorded as a follow-up, not a rebased commit.
- **`land` boundary:** earlier Segments are not merely open, they are merged into the default branch, so nothing about them is rewritable. Run the refinement scoped to the **current** Segment only, before its PR opens, exactly as under **pause**. A fix implicating an already-landed Segment is a follow-up PR, never a rebase. This is stricter than pause, not looser: under pause an earlier Segment's PR could at least still be amended by a human; under `land` it is history.

1. **Simplify & refactor analysis.** Read the changed files through the Design principles (SOLID, DRY, KISS, YAGNI). List concrete, behaviour-preserving changes: collapse needless indirection, delete dead or speculative code, dedupe real repetition, flatten tangled control flow, tighten names. Skip anything that changes behaviour or adds abstraction with no second caller.
2. **Re-plan.** Fold the accepted simplifications into a small set of refinement Work Units (same shape as a `/playbook:plan` plan: `Files`, `Requires`, `Done When`). Scope is limited to code already written. If a simplification implies new feature work, record it as a follow-up; don't build it.
3. **Execute autonomously.** Run the refinement Work Units like `--auto`: TDD where it applies, behaviour-preserving refactors keep tests green, commit each WU by invoking `/playbook:commit` (with the worktree root and branch in its prompt ("Worktree root: <path>, branch: <name>") from a worktree). Then re-run the validation checks from Step 7 (type-check/lint/test only, not the status flip or the continue-to-Step-8 handoff); they MUST stay green.

Run this pass once. Don't loop. Step 9 is the only review of the diff: this pass does not review for bugs, so it neither duplicates nor pre-empts it.

## Step 9: Adversarial Review (MUST)

This reviews the IMPLEMENTED work, not the plan: Step 4's adversarial review ran before execution against the plan; this one runs after, against the diff.

**Pick the path (MUST).** Measure the diff this review covers, from `<segment-base>` (or the default branch for the whole implemented diff):

Intent: review-size. If the session's tool map (printed at session start) lists it, use that command with the base, adding the all-lenses flag when `--all-lenses` was passed; it prints `lines=N files=M path=single|swarm`. Otherwise do this: run `git diff --shortstat <base>...HEAD` and add the insertions and deletions; the path is `single` when that sum is at most 150 and `--all-lenses` was not passed, else `swarm`.

- **`path=single` (at most 150 changed lines, no `--all-lenses`):** dispatch ONE `reviewer` (`subagent_type: playbook:reviewer`, with the `effort` parameter per "Pick the effort" in `playbook:delegating-subagents`) with all five lenses as its focus (the definitions are listed below), the full branch diff, numbered excerpts of the changed files, the test command and the read-beyond-only-when-needed limit ("Review brief builder" in `skills/delegating-subagents/references/briefs.md`), the plan, and the refinement notes, asking for plain findings in the shape the swarm prompt describes plus the lens each finding belongs to. No triage and no `cheap-checker`: a diff this small costs less to review whole than to classify. Under `--no-tests` tell it missing new tests is intentional. Then continue at "Consolidate" below, and apply the same silent-reviewer rule: a reviewer that returned nothing did NOT run.
- **`path=swarm`:** triage and the lens swarm, as the rest of this step describes.

**Haiku triage, before the swarm (swarm path only).** Skip triage entirely when `--all-lenses` was passed: all 5 lenses (correctness, behaviour drift, principles, scope, tests) run `full-lens`, unchanged from today's fixed-5-lens-always-full swarm.

Otherwise, triage needs a tool that merges its return into a complete tier map. Intent: review-triage-merge. If the session's tool map (printed at session start) lists it, use that command as described next. Otherwise do this: complete the tier map yourself using "Completing the map" in `skills/grounding-review/references/triage-tiers.md` in this plugin, report the `Triage:` line, and go on to the `--no-tests` rule below.

With the tool, dispatch `review-triage` (`subagent_type: playbook:review-triage`) exactly once, before the swarm, scoped to Step 9's fixed 5 lenses (`correctness`, `behaviour-drift`, `principles`, `scope`, `tests`), against the implemented diff (the same full branch diff the swarm dispatch below uses), the plan, and the refinement notes. Capture the returned tier map.

Complete the map in code: run that command with the five lens names and the triage return text (from a file or stdin; pass no tiers when the dispatch failed or returned nothing). It prints the full tier map as JSON plus a `Triage:` line. Failed, empty or unknown triage results become `full-lens` (rules in the triage-tiers reference). Use the printed map below.

**Under `--no-tests` (or an explicit "No new tests" answer),** the `tests` lens resolves to `skip` with the recorded reason "no new tests by explicit user choice", overriding triage for that lens only; the other four lenses triage as usual (or run `full-lens` when triage did not run); with the tool, pass the forced tier `tests=skip:no new tests by explicit user choice` to the merge command. Tell every dispatched reviewer that missing new tests is intentional and not a finding. A broken existing test is still a finding.

Report the `Triage:` line before the swarm dispatches.

Dispatch it as a swarm of lens-specialized reviewers in parallel (each reads the diff, none writes, so parallel is always safe): for each of the 5 lenses, read its tier from the tier map above before dispatching. A `full-lens` lens dispatches a `reviewer` agent (`subagent_type: playbook:reviewer`, with the `effort` parameter per "Pick the effort" in `playbook:delegating-subagents`) exactly as this step already did before tiered dispatch existed, issued as one Agent call per lens in a single message, with its lens as the focus, the full branch diff, numbered excerpts of the changed files, the scoped test command and the stated limit to read beyond them only when needed (the "Review brief builder" in `skills/delegating-subagents/references/briefs.md`, built once for the swarm), the plan, and the refinement notes; the prompt shape stays the same apart from asking for plain findings: a Conventional Comments label, `file:line`, evidence, a short failure scenario and one fix in plain words, no comment body, and the reviewer never loads `playbook:writing-style` (nothing here is posted). Each lens tries to break the work, not bless it:

- **Correctness:** bugs, off-by-one, unhandled errors, regressions the tests miss.
- **Behaviour drift:** did any simplification or refactor change observable behaviour?
- **Principles:** remaining SOLID/DRY/KISS/YAGNI violations, leftover speculative code, needless abstraction.
- **Scope:** anything built beyond the plan; anything the plan required but is missing.
- **Tests:** weak assertions, missing boundary or regression coverage, flakiness.

A `cheap-check` lens dispatches a `cheap-checker` agent and a `skip` lens dispatches nothing. For the cheap-check prompt (its prompt here also carries the full branch diff, the plan and the refinement notes), the lens's reference file path, skip tracking and the trust gate, read `skills/grounding-review/references/triage-tiers.md` in this plugin and follow it. In the triage summary above, an example line is "Triage: correctness=full-lens, tests=cheap-check, scope=skip".

Give each reviewer Task a stable `name`.

**The `reviewer` agent is structurally read-only, so a lens can only deliver by returning, and that channel is unreliable** (`playbook:delegating-subagents`). It holds Read, Grep, Glob and Skill; the repo's agent checks forbid `Write` and `Bash` for that tier by design, so there is no file to fall back on. **A lens that returned nothing did NOT run.** Never count it as a clean lens, and never let a swarm with missing lenses read as "no findings": that is how a review swarm silently becomes a no-op while looking thorough. Name the missing lenses in the final report. Start your own pass on the riskiest part of the diff while the swarm runs, so lost lenses cost latency rather than coverage.

Each lens gives plain findings with `file:line` evidence, a Conventional Comments label and decorations (read `playbook:grounding-review` reference `references/conventional-comments.md`) and one fix per finding, never a drafted comment. **Consolidate:** merge the files, dedup overlapping findings, and drop anything already addressed. Then run the verification sweep below before presenting or acting on any finding.

### Verification sweep (MUST, before presenting or acting)

You do this yourself, not a reviewer: reviewers are read-only and can be wrong. Check every consolidated finding against the code at HEAD:

1. **True.** Re-read the cited lines. Trace or run the failure scenario where that is cheap. A finding tagged `[unverified]` is either confirmed, dropped, or kept with the `[unverified]` tag stated plainly.
2. **Label.** The label and decorations match the real impact.
3. **Anchor.** The file and line are right. In a stacked or multi-PR run, the finding sits on the PR or branch that owns the code.

Drop findings that do not hold, relabel the mislabelled, and move the misplaced. Read the cited lines plus what the trace needs, never whole files. Then say in one line what the sweep changed, with the counts: kept, dropped, relabelled, moved, and recompute the verdict, confidence and finding order from the swept list. See the Verification Sweep section of `playbook:grounding-review`.

### Fix, open the PRs, finish

Apply the fixes you hold with HIGH confidence plus every must-fix correctness/security finding, routing each per Step 8's multi-Segment routing (savepoint: fix the owning branch and locally rebase the stack before any push; pause: fix the current Segment only, earlier-Segment fixes become follow-ups), and re-run the Step 7 validation checks. Surface the rest as known follow-ups: don't silently drop them, and don't start a second refinement loop.

**Open the PR set (MUST).** Once the Step 7 checks are green after the adversarial fixes, deliver the Segments as pull requests per the Step 4.5 topology, one PR per Segment, by invoking the `/playbook:pr-create` skill (never raw `gh pr create`; it applies pre-flight checks, the conventional-commit title, the team template, and the `playbook:writing-style` voice). Each PR body follows the short template of `/playbook:pr-create`: the Summary names the Segment's concern, and a short slice of the unresolved follow-ups goes under "Notes for reviewers" only when there are any. Under `--no-tests`, the Notes carry one line: "No new tests, by explicit choice." The PR readiness check prints `VERDICT tests: NONE`, a warning; that is expected here.

- **Ticket rules:** when the plan, the arguments or the branch carry a ticket key (`[A-Z][A-Z0-9]+-[0-9]+`), pass it as `--ticket <KEY>` to every `/playbook:pr-create` call and use it as the scope of every Work Unit commit (`feat(PROJ-123): add retry to the sync job`). In a stacked set this is required: every commit and every PR title carries the key, and the first body line is the key. When the plan names a GitHub issue, pass `--issue <N>`; the last Segment that finishes it says `Closes #N`, the others say `Refs #N`. With no key known, add none.
- **Stacked:** for each Segment **in order**, invoke `/playbook:pr-create --base <prev-Segment-branch>` on its branch (Segment 1 uses the default branch). Opening in order matters: `/playbook:pr-create` pushes the current branch, so each Segment's branch is on origin by the time the next Segment names it as `--base`. This yields the stacked chain (PR #1 targets the default branch, PR #2 targets Segment 1's branch, and so on). Record each PR URL in the ledger.
- **Worktree callers:** for every call in this run, put the worktree root (`git rev-parse --show-toplevel`) and branch (`git branch --show-current`) in the prompt of every `/playbook:pr-create` and `/playbook:commit` call, as "Worktree root: <path>, branch: <name>". The commands check them first and stop with `WRONG_DIRECTORY` on a mismatch.
- **Independent:** invoke `/playbook:pr-create` per Segment branch with the default branch as base.
- **Single:** in `--auto`, invoke `/playbook:pr-create` once for the whole branch; interactively, leave PR creation to the user (the pre-existing behaviour).

These PRs skip `playbook:finish-pull-request`: Steps 8 and 9 already reviewed the code. Promote a PR with `gh pr ready` when the user wants it.

**Boundary behaviour.** With **savepoint** (the default, and `--auto`), open the whole PR set here at the end. With **pause after each PR**, Step 9 has already run per Segment (its scoped review before the PR), so this step opens that one Segment's PR and stops for the user before the next Segment. With **land**, this step opens that one Segment's PR as a draft and then continues straight into Step 10, which promotes, gates on CI, merges, and only then returns to Step 5 for the next Segment.

**Checkpoint before the self-review.** After the PR set is open and before you invoke `/playbook:review --self` on it, and only in ask mode under the **savepoint**, **independent** or **stacked** shapes (not **land**, which continues straight into Step 10, and not **single** without a PR), stop and tell the user: "The PRs are open. To keep this session from compacting on its own midway, run `/compact` now with this focus: `PR #<number> (list every opened PR), plan <plan path>, the self-review is next`." Under **pause**, put this in the same message as that stop. You cannot run `/compact` yourself, so this is a prompt only. When the user replies, run the self-review in the compacted session. In auto mode there is no human, so skip the stop and continue.

**Finish.** Report the applied fixes, the opened PRs (with URLs, bases, and draft state), and the unresolved follow-ups, naming each of the 5 lenses' triage tier alongside its findings (on the single path, one line: "single reviewer, 5 lenses (<count>)"), the same `<lens>: <tier> (<count>)` / `<lens>: skip` shape WU-6 added to `/playbook:review --deep`'s Step 5 `### Reviewers` line: a `full-lens` or `cheap-check` lens shows `<lens>: <tier> (<count>)` (tier written as `full` or `cheap-check` for display, not the raw `full-lens`/`cheap-check` value), a `skip` lens shows `<lens>: skip` with no count, e.g. "correctness: full (1) · tests: cheap-check (0) · scope: skip". In interactive mode with the **single** topology chosen, leave PR creation to the user as before; every other topology opens the PRs as above. Under **land**, this step hands off to Step 10 instead of finishing here; the true finish is Step 10's own report once the Segment reads `MERGED` (or `PARKED`). Starting the next feature: run `/clear` before the next `/playbook:plan`, so this run's plan, dispatch history, and fixes don't carry into it.

## Step 10: Land the Segment (`--boundary=land` only)

Runs once per Segment, immediately after Step 9 opened that Segment's PR, and MUST complete with the PR reading `MERGED` before Step 5 creates Segment N+1's branch. Skip this step entirely under **savepoint** and **pause**. Under `land`, read the procedure first and follow it in full: `skills/finish-pull-request/references/land-segment.md` in this plugin. It holds the attempt-then-re-read rule, the promote, CI gate, merge and confirm loop, the fix tiers, the park conditions and the `land` decision rows. The Decision Rules table below keeps the rows that choose the boundary.

## Decision Rules

| Scenario | Action |
| --- | --- |
| Reference isn't a ready plan | STOP; tell the user to run `/playbook:plan` or `/playbook:adr` first (Step 2) |
| Task is ambiguous | Ask the user before executing; in auto, stop and report |
| Plan needs new dependencies | List them and ask for approval (vet maintenance/license/CVEs); in auto, stop and report the list |
| Touches auth/security code | Flag for extra review; be conservative |
| Requires a DB migration | Execute the migration as its own unit with a rollback note |
| Validation fails | Fix and re-validate before reporting done |
| A WU dispatch goes idle, errors, or returns no report | `TaskStop` it if still alive, then resume from its `wip` commits (Step 5), don't restart the whole WU |
| `--auto`: WU fails after 3 retries | Stop; report the failed WU and the remaining WUs |
| `--auto`: validation fails after 3 fixes (including Step 8/9 re-validations) | Stop; report; do NOT open any PRs |
| `--auto`: on the default branch | Create a feature branch before the first commit |
| `--auto`: commit/push fails | Stop; report (auth, hooks, etc.) |
| Plan has no Segments (old plan/issue/spec) | Derive Segments, small and self-contained by area or feature, in Step 4.5 |
| Interactive, not `--auto`, no strategy flags | Ask the three Step 4.5 questions (topology + boundary + TDD approach), recommend per scope |
| `--auto` or a strategy flag set | Skip that question; self-select recommended (independent when disjoint, else stacked, + savepoint) and record as assumption |
| `--no-tests` set, or "No new tests" answered | Skip writing tests; Step 7 still runs the existing suite, type check, and lint; `tests` lens resolves to `skip`; log it in the ledger and each PR body (Step 5, Step 9) |
| `--no-tdd` and `--no-tests` both passed | Stop with a one-line conflict error; don't guess |
| `--auto` with no test flag | Never self-select "No new tests"; it needs `--no-tests` or an explicit answer |
| Pause boundary chosen | Run Steps 7-9 scoped per Segment, open its PR, stop for the user before the next (Step 9) |
| Refinement/adversarial fix, savepoint boundary | Commit on the owning Segment branch, locally rebase later branches before any push (Step 8) |
| Refinement/adversarial fix implicating an already-open PR (pause) | Record as a follow-up; don't rebase an open PR (Step 8) |
| Refinement (Step 8) implies new feature scope | Record as a follow-up; don't build it (YAGNI) |
| Adversarial review (Step 9) finds a blocking issue | Fix it, re-validate, then finish; report non-blocking findings as follow-ups |
| Refinement or adversarial fix would change behaviour | Don't fold it into the refactor; treat it as a separate fix and re-validate |
| `--auto` with no `--boundary` | Self-select `savepoint`; NEVER self-select `land`, which is opt-in only (Step 4.5) |
| `land` boundary chosen | Run Steps 7-9 scoped per Segment, then Step 10: promote, gate on CI, merge, confirm `MERGED`, only then start Segment N+1 |
| `land` + stacked topology | Reinterpret as independent: Segment N is merged before N+1's branch exists, so branch off the fetched `origin/<default>`. Record the override as an assumption (Step 4.5) |

## Teardown (MUST run, even on failure or abort)

Close any subagent from this run that is still alive before finishing.
