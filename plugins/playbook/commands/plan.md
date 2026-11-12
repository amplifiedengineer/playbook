---
description: Use when an idea needs to become a plan, or the user says brainstorm, plan, scope, explore this idea, or break this down. Interview-driven; produces a verified plan for /playbook:implement.
allowed-tools: Bash, Read, Grep, Glob, Write, Edit, Agent, Skill, WebFetch
argument-hint: "[idea | PROJ-123 | ./prompt.md] [--ticket <id>] [--depth 0-2] [--adr] [--auto] [--auto-design] [--ask] [--help]"
model: opus
effort: medium
---

# Plan: Discovery Through an Implementation Plan

Turn a raw idea, a settled direction, or a ticket into a verified, self-contained implementation plan in one sitting. The session opens divergent: it challenges the premise, explores the codebase and memory, and weighs 2-3 approaches until you approve a design. It then continues, in the same conversation with no `/clear` in between, into a convergent interview for Work Units and Segments, a 3-phase quality gate, and one saved plan file. There's only one artifact and one command: the old two-step handoff between a design doc and a separate planning session is gone.

Invoked as `/playbook:plan`. The remaining arguments are an optional idea seed, ticket id, or file path.

The terminal state is a verified implementation plan at `$PLANS_DIR/<topic-slug>.md` (Step 0 resolves `$PLANS_DIR`), ready to run with `/playbook:implement`. Do NOT write real code, scaffold anything, or start the implementation here. The one narrow exception is Step 5.5's optional validation spike: throwaway code to check a single uncertain premise, never part of the saved plan's content and never the start of the real build.

## Help

If the arguments contain `--help`, print this and stop:

```
/playbook:plan - One session from a raw idea to a verified implementation plan

USAGE:
  /playbook:plan [idea]              Start an interactive session
  /playbook:plan "offline mode"      Start with an idea seed
  /playbook:plan PROJ-123             Pull a ticket and start from it
  /playbook:plan ./notes.md           Load the idea seed from a file

OPTIONS:
  --ticket <id>  Force ticket mode for <id> (skip seed/file detection).
  --depth <0-2>  How far to crawl ticket links: 0 ticket only, 1 direct
                 links (default), 2 one more hop. Always bounded.
  --adr        Flag this session's decision as ADR-worthy: skip the Step 5
               reversibility test and go straight to recommending
               /playbook:adr at Step 12. The session still continues
               through to a saved implementation plan; --adr adds a
               recommendation, it does not end the session early.
  --auto       Autonomous, but narrower than it sounds: it only self-answers
               the convergent phase (Step 7 onward), taking the recommended
               answer for every Work Unit and Segment decision and recording
               it as an assumption, then runs the quality gate and saves
               without pausing. The divergent phase (approach selection, the
               /playbook:adr route check, design approval) still needs you.
               Plain --auto stops at Step 6 with a message unless you approve
               the design. See Autonomous Mode below.
  --auto-design  Implies --auto and also self-answers Steps 3 to 6: discovery,
               the approach, the /playbook:adr route check and the design
               approval. Every choice is logged in the Assumptions list.
  --ask        Interactive: prompts at each decision, even when the
               environment or the repo config sets auto mode.
  --help       Show this help

Asks one question at a time with a recommended answer. Given a ticket id,
pulls the ticket (description, comments, attachments, linked items) via a
connected MCP or a configured provider command, then explores the codebase and
memory in parallel before asking you. Checks for an in-progress checkpoint
from an earlier, interrupted run on the same topic and offers to resume it.
Confirms a problem statement before proposing 2-3 approaches, gets your
approval on the chosen design, then continues, in the same session, into the
Work Unit and Segment interview, the 3-phase quality gate, and one saved plan
file at ~/.config/playbook/repos/<owner>/<repo>/plans/<topic-slug>.md.
```

## Core Rules (MUST)

**Autonomous mode (`--auto`) narrows to the convergent phase only.** Step 0 reads the run mode before anything else. When `--auto` is set, the divergent phase (Steps 1 through 6) still puts every question to you that it normally would: approach selection, the `/playbook:adr` route check, and the Step 6 design approval are human gates that plain `--auto` never skips. Only `--auto-design` self-answers them, and it logs every choice. From Step 7 onward, `--auto` resolves every decision yourself, taking the answer you would have recommended, recording it in an **Assumptions** list, and skipping the Step 8 and Step 11 confirmation pauses. See **Autonomous Mode (`--auto`)** below for the full boundary and why it changed from `/playbook:scope`'s old behavior. The rules below otherwise describe the default interactive mode.

1. **Ask ONE question at a time.** Not two, not a batch. One question, wait for the answer, then the next. Batch at most 2 into one numbered round only when neither could plausibly depend on exploration or terminology the other's answer might surface, true independence, not just topical proximity (Step 3 covers this in detail). The very first message is always a single question, never a round: there's no established frontier yet to batch from.
2. **Explore before asking.** If the codebase, the memory stores, or an in-progress checkpoint settles a question, resolve it yourself and report what you found. Only ask about intent, constraints, and preferences the code can't answer.
3. **Challenge the premise (divergent phase only).** Don't accept the framing at face value. Ask whether this is the right problem, whether a simpler direction meets the goal, and what "done" actually looks like. Once Step 6 approves a design, stop relitigating it: the convergent phase plans the approved direction, it doesn't reopen it.
4. **Present a design and get unconditional approval before continuing (Step 6).** Hard gate, every time, even for a small idea, even under plain `--auto`. Only `--auto-design` approves it for you, and it logs that choice. The design can be a few sentences, but you MUST present it and get a yes before Step 7 starts.
5. **Walk the decision tree in the convergent phase.** Each answer may open new branches. Track which are resolved and which are still open. Don't jump to unrelated topics while a branch has unresolved dependencies.
6. **Do NOT write real code.** The output is a plan file, not implementation. The one narrow exception is Step 5.5's optional validation spike: throwaway code to check a single uncertain premise, never part of the saved plan's content, never the start of the real build.
7. **Do NOT produce Work Units, Segments, or file-level plan detail before Step 6 approves the design, and not until every convergent branch is resolved.** The divergent phase decides direction; the convergent phase (Step 7 onward) turns an approved direction into a plan `/playbook:implement` can run.

## Argument Resolution

Resolve the argument in this order:

1. **Ticket:** if `--ticket <id>` is set, or the argument matches a ticket key (`[A-Z][A-Z0-9]+-\d+`, e.g. `PROJ-123`) or a known tracker URL (Jira, Linear, GitHub issue), treat it as a ticket and go to Step 1.5 to pull it. `--ticket` forces ticket mode even for an ambiguous value.
2. **File path:** if it starts with `./`, `../`, `/`, or `~`, or ends with `.md`, `.txt`, `.yaml`, `.yml`, check whether it exists with the Read tool. If it exists, read it and use it as the idea seed; if not, treat it as a plain-text seed.
3. **Plain text:** otherwise the argument is the idea seed.
4. **No argument:** ask what we're exploring before anything else.

Strip `--ticket <id>`, `--depth <n>`, `--adr`, `--auto-design`, `--auto`, and `--ask` (like `--help`) before resolving the seed. Don't read `.gitignore`d files even if the seed or ticket mentions them.

## How It Works

### Step 0: Read the run mode, load skills and check for an in-progress checkpoint

**Read the run mode (MUST, first).** Resolve whether this run is `ask` or `auto`. Intent: run-mode. If the session's tool map (printed at session start) lists it, use that command with this command's name (`plan`) and `$ARGUMENTS`, and follow `../skills/delegating-subagents/references/run-context.md` (relative to this file) for its output: the warning, the effort ceiling, a conflicting-flags exit, and any other failure. Otherwise do this: the mode is `auto` when the arguments carry `--auto` or `--auto-design`, and `ask` when they carry `--ask`. When both `--ask` and an auto flag are given, stop with one line saying they conflict. With neither, run `printenv PLAYBOOK_MODE` and use its value when it is `ask` or `auto`; in every other case the mode is `ask`. There is no effort ceiling on this path.

**Routing.** Where you choose what to spawn, pick the model for a `design` task. Intent: route-model. If the session's tool map lists it, use that command for the `design` kind and follow its `action` under the `routing.escalate` gate: on `ask` stop and ask the user, on `downgraded` (`deny`) use the lower tier it names, and in auto mode (`auto`) proceed and log the assumption. If the command fails, carry on. Otherwise do this: spawn each agent on the model its own agent file sets, and carry on.

**Resolve the plans folder.** Plans live outside the checkout, in one folder per repo shared by every worktree. Intent: plans-dir. If the session's tool map lists it, use that command and take the path it prints. Otherwise do this:

```bash
slug=$(git remote get-url origin | sed -E 's#\.git/?$##; s#^[A-Za-z]+://##; s#^[^@/]*@##; s#^[^/:]+[/:]##')
[ -n "$slug" ] || { echo "no origin remote"; exit 1; }
mkdir -p "$HOME/.config/playbook/repos/$slug/plans" && echo "$HOME/.config/playbook/repos/$slug/plans"
```

The `sed` drops the `.git` suffix, the scheme, any `user@` and the host, leaving `<owner>/<repo>`. With no `origin` remote, stop and say the plans folder needs one. This file calls the printed absolute path `$PLANS_DIR` in prose and `<plans-dir>` in commands. Shell variables do not survive between `Bash` calls, so write the path out in full in each later command.

- **`ask` mode:** behave exactly as this file describes. Every question below stays.
- **`auto` mode:** take the recommended answer at each decision instead of asking, within the limits in Autonomous Mode below, and record every answer you chose yourself in an **Assumptions** list. Print that list in the final output. Auto mode that comes from the environment or the repo config acts like plain `--auto`. Only the `--auto-design` flag, given on the command line, self-answers the design.

**Load skills.** Load `playbook:writing-style` (voice, banned words, no dashes; every question and every written section follows it) and `playbook:grounding-research` (cite `file:line`, tag `[unverified]` when you can't confirm; governs the context digest and any self-answering).

**Derive `<topic-slug>` (MUST, before anything else).** Kebab-case the plain-text idea seed as typed, or, in ticket mode, the ticket id itself (e.g. `PROJ-123` becomes `proj-123`), the same convention `commands/adr.md:21`'s `{kebab-title}` filename uses. Ticket mode derives the slug from the id, not the ticket's title, because Step 1.5 (which pulls the title) runs after this check; using the id keeps the slug available before the ticket is fetched, and it stays the same value for the rest of the session, including Step 12's save. This is the same slug the checkpoint file and the final plan file both use.

**Check for a checkpoint.** This command's own bespoke checkpoint/resume mechanism, not the generic `/playbook:handoff-save` (that one is keyed by directory and holds a free-text summary, not a structured plan in progress keyed by topic). Look for a matching checkpoint. Intent: checkpoint. If the session's tool map lists it, use that command with `<topic-slug>`: it prints the checkpoint path and `found` or `not-found`, and exits 1 on a bad slug or when the repo has no `origin` remote. Otherwise do this: the checkpoint is `$PLANS_DIR/<topic-slug>.checkpoint.md`. The slug may hold only letters, digits, `-`, `_` and `.`, and may not start with a dot. Check whether the file exists:

```bash
test -f "<plans-dir>/<topic-slug>.checkpoint.md" && echo found || echo not-found
```

- **Found:** read it. Show its `Goal:` line and its last-modified date (not a bare yes/no), then ask once (in auto mode, take the recommended answer, yes, and record it as an assumption): **"Found an in-progress plan for `<topic>` last touched `<date>`, goal: `<goal-line>`. Resume it? I'd recommend yes because picking up mid-session avoids redoing settled decisions."**
  - **Yes:** load its Decisions Made, Out of Scope, Open Risks, chosen Approach, `Design approved` marker, and the Work Units/Segments table as far as they got. Resume from the first step whose output the checkpoint doesn't yet have: only a Goal and no Decisions Made resumes at Step 1; an Approach with no `Design approved` marker resumes at Step 5 (the route check still needs an answer); a `Design approved` marker with no Work Units resumes at Step 7. Never resume at Step 7 on an Approach alone: Step 6's approval is a hard gate (Core Rules), and a checkpoint that hasn't recorded it hasn't cleared that gate yet, no matter how settled the approach looks.
  - **No:** start fresh. The stale checkpoint is not deleted here: it gets overwritten in place as new decisions are appended through Step 3 onward (see the write shape below), and only Step 12 clears it, once a completed plan actually replaces it (through the plan-cleanup intent; without it the file stays and the report lists it for the user to delete). Silently deleting a stale checkpoint the moment someone declines to resume would destroy a session's progress on a whim, on the chance they meant to resume a different topic under the same seed.
- **Not found:** proceed to Step 1 with nothing to resume.

**Checkpoint content and write shape.** The checkpoint is a single Markdown file: a `Goal:` line, `Decisions Made`, `Out of Scope`, `Open Risks`, `Approach`, a `Design approved` marker (set only once Step 6's gate clears), and the `Work Units / Segments` table as far as they've been settled, the same structured shape the final plan's condensed sections use. Rewrite it after each resolved decision, not only in Step 3 and Step 7: Step 4 (approach chosen), Step 5 (route-check answer), and Step 6 (the `Design approved` marker) each trigger a rewrite too, under the same mkdir-based advisory lock pattern this repo uses elsewhere for concurrent small-file writes, like `GLOSSARY.md`'s append (`crates/common/src/atomic.rs`'s `with_dir_lock`), adapted here to a full rewrite rather than a one-line append, since the checkpoint's content is a structured document, not an append-only log. The content is always the full updated checkpoint: Goal, last-touched date, Decisions Made, Out of Scope, Open Risks, Approach, the `Design approved` marker once set, and the Work Units/Segments table as far as they got.

If the session's tool map lists the checkpoint intent, use that command's write option with the content on stdin; it takes the lock, replaces the file whole and prints the path. Otherwise do this: write the content with `Write` to `<plans-dir>/<topic-slug>.checkpoint.md.tmp`, then swap it in under the lock:

```bash
f="<plans-dir>/<topic-slug>.checkpoint.md"
mkdir "$f.lock" 2>/dev/null; got=$?
mv "$f.tmp" "$f"; ok=$?
[ "$got" -eq 0 ] && rmdir "$f.lock"
[ "$ok" -eq 0 ] && echo "$f"
```

The lock is advisory: when `mkdir` fails because another writer holds it, the write still happens. When the save itself fails, print `couldn't save checkpoint: <reason>, continuing without resume safety this turn` and carry on.

A checkpoint write failure warns inline with that message and the interview keeps going. Never abort a session over a persistence hiccup: a human is present to see the warning, and losing resume safety for one turn is a far smaller cost than losing the whole session.

### Step 1: Frame the idea and check scope

Restate the idea in one or two sentences so we agree on what we're exploring.

**Scope check (MUST).** If the idea is several independent subsystems (e.g. "a platform with chat, billing, and analytics"), stop and flag it. Help decompose into independent pieces, name how they relate and what order to build them, then plan the first piece through the normal flow. Don't design a tangle.

### Step 1.5: Pull the ticket (ticket mode only)

Run this only when Argument Resolution found a ticket. Skip it entirely for a plain idea or file seed.

**Connect (layered).** Find a way to reach the tracker, in order:

1. **MCP:** search for a connected ticket tool (`ToolSearch` for Jira, Linear, Atlassian, or issue tools). If one is connected, use it. For a Jira or other Atlassian ticket, load `playbook:atlassian` first and follow its selection order (MCP, acli, twg) instead of the steps below.
2. **Provider command:** else look for a configured fetch command. Read `.claude/plan.config` (or the repo's existing tracker config) for a per-tracker command with an `{id}` placeholder, for example `jira issue view {id} --raw` or `linear issue {id} --json`, and run it with Bash. A public tracker URL with no auth can be read with `WebFetch`. A page that needs auth or JavaScript rendering that `WebFetch` can't handle can be opened with the `agent-browser` MCP, if it's connected: `open` the url, then `snapshot` for the accessibility tree and `screenshot` for visual content.
3. **Neither:** stop and tell the user how to connect one (an MCP server or a provider command), then offer to continue with the ticket id as a plain-text seed. Do NOT fabricate ticket contents.

**Crawl (bounded, never infinite).** Gather, then stop:

- The ticket itself: title, description, status, and comments.
- Attachments: read images visually and PDFs or docs as pages. For an attachment the tracker exposes only as a web link, open it with the `agent-browser` MCP (if connected) and `snapshot` or `screenshot` it. Note and skip binaries and anything the tracker doesn't expose.
- One hop of direct links: linked issues, sub-tasks, parent epic, and linked PRs. `--depth` controls this (0 = ticket only, 1 = direct links (default), 2 = one more hop). Clamp `--depth` to the range 0 to 2, so the crawl is never unbounded.
- Bounds: cap total related items at about 15, dedup visited tickets by id, and stop early when a hop adds nothing new.

**Discover in parallel.** Fan out discovery agents (`subagent_type: general-purpose` with a stand-alone brief) over the gathered sources (issue the Agent calls in one message so they run at once, per Step 2): the ticket body plus comments, batches of linked items, and the attachments. Each returns a short cited summary (the source id or url, and the facts that bear on the work). These feed the Step 2 digest alongside the codebase exploration. Assign each discovery agent a stable `name` at spawn.

The ticket's title and description become the idea seed for Step 1's framing. Record the ticket id and link so Step 9 can cite them in the plan.

### Step 2: Explore context in parallel

Fan out `Explore` agents (`subagent_type: Explore`, each with a stand-alone brief per `playbook:delegating-subagents`, never an omitted type, which forks the whole session) to map what the dialogue needs. **Dispatch them in parallel: issue all the Agent calls in a single message so they run at once.** Read-only exploration has no shared state, so parallel is always the default here. Each `Explore` brief carries the code search rule: run `playbook code status`, then `playbook code index` when it says stale or missing, then `playbook code context "<area>"` before Grep or Glob, which are the fallback only when the binary or the index is unavailable. Add the `Callers` block from `playbook:delegating-subagents` to a brief only when the user asked for code facts.

Scale the fan-out to the idea: one agent for a tiny change, up to about four for a broad feature. Give each a distinct area, for example:

- Existing patterns and prior art for this kind of change.
- Integration points and the consumers a change would touch.
- Constraints: config, conventions, and anything in the code that limits the options.

Alongside the `Explore` agents, dispatch one independent `critic` agent (`subagent_type: playbook:critic`, focus `premise`), prompted to challenge the premise rather than explore code. Its return feeds the Step 2 digest and the Step 4 approach exploration, so premise-challenge isn't only in the orchestrator's head. Close it on return with the others (Step 2 teardown).

Built-in `Explore` agents have been reliable at returning results; the `critic` is structurally read-only and has only the return channel, so it may deliver nothing (`playbook:delegating-subagents`). An area whose agent returned nothing was NOT explored: it does not mean there is nothing there. Say which areas are unexplored rather than treating the digest as complete, and if the premise-challenge came back empty, challenge the premise yourself before moving to Step 3.

**Check memory and prior plans.** Alongside the `Explore` agents, read this repo's memory context. `<owner>/<repo>` is the same slug Step 0 derived from `git remote get-url origin`. Intent: memory-context. If the session's tool map lists it, use that command for `<owner>/<repo>`; it reads the memory graph itself and prints a `Memory context: none` line saying why when it has nothing. Otherwise do this: read the `name:` and `description:` frontmatter lines of the fact files in `~/.config/playbook/memory/<owner>/<repo>/` (project facts) and of the `*.md` files directly in `~/.config/playbook/memory/` (global facts), and the `*.md` files directly in `~/.config/playbook/memory/<owner>/` (org facts). Either folder may be missing; with no facts in either, treat it as `Memory context: none`. Either way, load the fact files that bear on the idea on demand rather than treating the compact list alone as the full content.

Also scan `$PLANS_DIR/*.md` (excluding `*.checkpoint.md` and `*-quality.md`) for a prior saved plan whose title or topic overlaps this idea, a cheap keyword match, not semantic search. When neither memory nor prior plans have anything relevant, skip this silently. When either surfaces a plausible match, a decision already made or an idea already rejected, say so in the digest: what was decided, when, and why. Either way, note in the digest whether memory context found facts. Ask directly whether anything has changed before diverging into new approaches, rather than re-litigating a settled call from scratch.

Consolidate into a short cited digest (a few bullets, each with `file:line`). This grounds the questions that follow so you ask about intent, not about facts the code already holds. In ticket mode, fold the Step 1.5 ticket findings into the same digest, citing the source id or url for those. Assign each `Explore` agent a stable `name` at spawn.

**Verify the load-bearing premises before diverging.** From the digest, list the load-bearing citations: the premises the design will rest on (for example "the code already does X", "there is no existing helper for Y"). Re-read each cited `file:line`. Drop or tag `[unverified]` any that don't hold, and tag each surviving context bullet HIGH / MEDIUM / LOW (the `playbook:grounding-review` skill defines the levels). Spot-check the load-bearing claims only; don't audit every citation, or the divergent phase drags. Dropped or LOW-confidence premises become open items carried into Step 7 (see below), not a separate handoff document.

### Step 3: Interactive discovery

Ask questions one at a time by default, each with a recommended answer and reasoning, each following from the last. Under `--auto-design`, do not stop for answers: take the recommended answer to each question, record it in the Assumptions list with its reason, and checkpoint it like any other decision. Cover:

- **Purpose:** why this, why now? What breaks or stays broken without it?
- **Success criteria:** what does "done" look like, observably?
- **Constraints:** technical, product, or time limits that rule options in or out.
- **Non-goals:** what this explicitly won't do.

**Rounds (narrow exception to one-at-a-time).** Think of the open questions as a frontier: the ones you could ask right now without guessing at an answer you haven't heard yet. The frontier here is almost always one question deep, since these four categories inform each other adaptively (Purpose shapes what Success criteria means, and so on), not as an upfront-resolvable tree. Batch two into one numbered round only on the rare case where they're genuinely independent. Never batch more than 2: a small idea's whole question budget (see Adapting to Complexity) is 2-4, and a bigger round reintroduces the wall-of-text problem the recommended-answer mechanic exists to avoid. Format a round like this:

```
1. **<question title>**: <question body>
   Recommended: <your recommended answer>

2. **<question title>**: <question body>
   Recommended: <your recommended answer>
```

A single question uses the same numbered, recommended-answer shape without the second entry.

Between questions or rounds, explore further if an answer opens a new area, and report what you found before the next question or round. After a round, re-run this check and the domain glossary check (below) against every answer just received, not only the most recent one, before drafting the next round. If either check would change a question the user hasn't answered yet (asked in the same round as an answer that reshapes it), withdraw it: don't let the user answer it against context that's already stale, re-derive it in the next round instead.

**Checkpoint after each resolved decision (MUST).** The moment a question in this step resolves, append it to the checkpoint's Decisions Made list and rewrite the checkpoint file using Step 0's locked write shape. This is what makes a mid-interview interruption resumable: without it, a crash between Step 3 and Step 12 would lose everything settled so far.

**Domain glossary (when a term is genuinely ambiguous or new).** If the conversation turns on a term that's overloaded, vague, or new to this codebase, don't just use it and move on: propose a precise definition and check it with the user. This isn't for every noun in a small idea, only for a term the design actually hinges on. Write it to `GLOSSARY.md` at the target repo's root (create the file only on its first real entry; it's tracked in git, not ignored, since its value is shared vocabulary across future sessions, not scratch). Each entry states what the term IS in one or two sentences, not what it does, plus a short list of synonyms to avoid so the disambiguation is recorded, not just implied. Write it the moment it resolves, don't batch it for later. If `GLOSSARY.md` already has a conflicting entry for the term, surface the conflict to the user instead of overwriting it silently.

Append with the same mkdir-based advisory lock pattern the checkpoint's rewrite uses, matching the Rust hooks' `with_dir_lock` (`crates/common/src/atomic.rs`). Two concurrent sessions can each resolve a term at the same moment; a plain check-then-append can silently drop one of the two lines. Intent: add-glossary-term. If the session's tool map lists it, use that command with the entry line; it appends one line to `GLOSSARY.md` at the repo root, creating the file on the first entry, and prints the file path. Otherwise do this:

```bash
g="$(git rev-parse --show-toplevel)/GLOSSARY.md"
mkdir "$g.lock" 2>/dev/null; got=$?
cat >> "$g" <<'EOF'
<term entry>
EOF
[ "$got" -eq 0 ] && rmdir "$g.lock"
echo "$g"
```

Scale the depth: 2-4 questions for a small idea, more for a broad one. Don't over-interview a simple thing (see Adapting to Complexity).

### Step 3.5: Draft and confirm the problem statement

Synthesize the running document's problem section from the Step 3 answers: Purpose and Success criteria become Problem and Goals, Non-goals stays Non-goals, and a new Requirements section states the user-facing capabilities this needs, in behavior terms, not implementation. Present it and ask: **"Does this capture the problem and what it needs to do? Anything to add or change?"** Revise until confirmed. Under `--auto-design`, confirm it yourself and log that in the Assumptions list. This is the requirements gate: Step 4 designs approaches against a confirmed problem statement, not an implicit one. Keep this section free of scope details, technical approach, or components: those come later, once a direction is chosen. This stays folded into the one running document rather than a separate file: there's no PRD to hand off, because nothing hands off until Step 12.

### Step 4: Propose approaches

Present 2-3 distinct approaches with their trade-offs. Lead with your recommendation and say why. Keep each approach to what matters: what it does, its main cost, and what it rules out. Let the user pick or push back. Under `--auto-design`, pick your recommended approach and log the choice and the reason in the Assumptions list.

**Checkpoint the chosen approach (MUST).** The moment the user picks one, write it to the checkpoint's `Approach` field and rewrite the file using Step 0's locked write shape.

### Step 5: Route check

Look at the chosen direction against a three-part test, all required: the decision is hard to reverse once made, it would be non-obvious to a future reader why it was made this way, and it's the product of a genuine trade-off, not a forced or obvious choice. When all three hold (a data model, a public contract, a cross-cutting dependency are common shapes), flag it and offer `/playbook:adr` for the deep record: **"This carries an architectural call worth a formal record. Route to /playbook:adr for that decision? I'd recommend yes because it's hard to reverse."** `--adr` forces this recommendation without running the three-part test.

**This fires unconditionally under plain `--auto`.** Even in autonomous mode, the divergent phase, including this route check, still stops for a human: `--auto` only reaches its self-answering behavior from Step 7 onward (see Autonomous Mode below). Under `--auto-design`, take the recommended answer and log it in the Assumptions list. Record the answer (recommend `/playbook:adr` or not) either way; Step 12 surfaces it. Answering this question does not end the session: whether or not the user wants an ADR, the flow continues into Step 5.5 and onward to a saved implementation plan. An ADR and an implementation plan are not alternatives, they're two different artifacts this decision may need.

**Checkpoint the route-check answer (MUST).** Write the recommendation and the user's answer to the checkpoint's `Decisions Made` list and rewrite the file using Step 0's locked write shape.

### Step 5.5: Offer a validation spike

If the chosen approach rests on a premise Step 2 tagged LOW confidence, offer to check it before writing anything down: **"This approach assumes [premise], which I couldn't verify. Want a quick throwaway spike to check it first?"** Skip this step entirely when nothing is LOW confidence. Under `--auto-design`, answer no and carry the premise forward as an open item.

On yes:

- Build one small, self-contained, runnable artifact that exercises just the uncertain logic. No real persistence, no polish, no setup beyond running it.
- Walk it through the specific edge cases the premise is actually in doubt about, not just the happy path.
- Report what you learned. Update the Step 2 confidence tag: resolved, or still open and now an explicit open item carried into Step 9's plan.
- Commit the spike to a dedicated throwaway branch (never main, never the working branch), then leave the working tree clean. If it stays worth keeping as a reference, note the branch name as an open item; otherwise it's just there if anyone needs to check the reasoning later.

The spike is disposable and scoped to one premise. It never becomes part of the saved plan's content and never starts the real implementation; that's still `/playbook:implement`'s job.

### Step 6: Present the design

Present the design in sections scaled to complexity: a few sentences where it's straightforward, more where it's nuanced. The problem and requirements are already confirmed (Step 3.5); cover the chosen approach, the key components and their boundaries, and the main risks. Ask after each section whether it looks right. Revise until the user approves. Do NOT continue into Step 7 before approval, even under `--auto`.

**In plain `--auto`, the run never approves the design for you and stops here.** If the user has not approved the design in this session, end the run with this message: "The design needs your approval. Approve it in this session, or rerun with `--auto-design` to let the run approve it and log each choice." Do not start Step 7.

**With `--auto-design`,** approve the design yourself. Log the approval in the Assumptions list, with the main risks you accepted, and continue.

**Checkpoint the approval (MUST).** The moment the design is approved, set the checkpoint's `Design approved` marker and rewrite the file using Step 0's locked write shape. This is the field Step 0's resume logic checks before it will resume at Step 7: an `Approach` alone never implies approval.

Keep applying the domain glossary discipline from Step 3 here too: a term that turns out ambiguous while presenting the design gets the same treatment, resolved and written to `GLOSSARY.md` immediately, with the same locked append.

**This is the divergent/convergent boundary.** No `/clear` happens here: the session continues directly into Step 7 with the same context, the same digest, the same approved design. Collapsing this boundary is the entire reason this command exists instead of two separate ones: the old handoff between a design doc and a fresh planning session is gone, along with the phrase-matching ambiguity it created at the routing-hook level between "still exploring" and "ready to plan."

### Step 7: Convergent interview for Work Units and Segments

**This is where `--auto` starts applying.** Everything from here through Step 11 can be self-answered under `--auto`; everything before this point cannot, unless `--auto-design` is set (see Autonomous Mode below).

Skip Goal clarification and Scope boundaries here: Step 3.5 already confirmed the problem, goals, and non-goals, and re-asking them would relitigate a decision the divergent phase already settled. Start directly at implementation-detail questions, aimed first at whatever's still open: Step 2's dropped or LOW-confidence premises, and Step 5.5's spike findings if any are still unresolved. Those are exactly the premises the divergent phase couldn't fully verify.

Ask questions one at a time. Each question should follow from the previous answer (don't jump topics), include your recommended answer with reasoning, and resolve a specific branch.

Types of questions, in roughly this order (adapt):

- **Existing patterns:** "I found [pattern] in the codebase. Follow it or break from it? **I'd recommend following it** because [reason]."
- **Architecture decisions:** "Two approaches: A does [X] but [downside]. B does [Y] but [cost]. **I'd recommend A** because [reason]."
- **Dependencies:** "This depends on [system]. How should it interact? **I'd recommend [approach]** because I see [evidence in code]."
- **Edge cases:** "What happens when [failure scenario]? **I'd recommend [handling]** because [reason]."
- **Migration/rollout:** "How do we get from current state to the new state? **I'd recommend [strategy]** because [reason]."

Between questions, explore the codebase if the answer reveals new areas. Report what you found before asking the next question.

**Checkpoint after each resolved decision (MUST), same as Step 3:** append it to the checkpoint's Decisions Made list (or its Work Units/Segments table, once those start taking shape) and rewrite the checkpoint file using Step 0's locked write shape.

**Knowledge capture (memory).** When exploration reveals a durable convention or gotcha about the codebase (true regardless of this plan), and a project store is present at `~/.config/playbook/memory/<owner>/<repo>/`, persist it as a project memory fact right then: a kebab-case file in `~/.config/playbook/memory/<owner>/<repo>/` with `name`/`description`/`type: project`/`links:`/`anchors:` (to the files). When no project store is present, skip this step silently.

**Work Unit sizing (MUST).** Each WU is one coherent commit: small enough to review on its own, following the `playbook:engineering-standards` size rule and incremental-delivery guidance. Prefer more, smaller WUs over a few large ones; `/playbook:implement` commits each separately. The `Files` column lists production and test files. The `Requires` column is the dependency edge `/playbook:implement` topologically orders and cycle-checks. The `Segment` column names the PR-sized group each WU belongs to (see below).

**Segment sizing (MUST).** A Segment groups Work Units into one PR-sized, reviewable increment. Each Segment becomes one pull request, so:

1. **One concern per Segment.** Data layer, service layer, wire-up, and docs are separate Segments, not one, per `playbook:engineering-standards` "one concern per PR".
2. **Size (from `playbook:engineering-standards`).** Keep each Segment as small as possible and self-contained by area or feature. Prefer one larger Segment that stands on its own over several small ones that add little value, but never plan one so big no one can review it. Tests do not count toward size.
3. **Ordering respects WU dependencies.** A Segment's WUs may only `Require` WUs in the same or an earlier Segment; no forward cross-Segment dependency. Default to a **linear** Segment chain (`S1 -> S2 -> S3`), which `/playbook:implement` maps to stacked PRs.
4. **Coverage.** Every WU belongs to exactly one Segment; no WU is left out and none appears in two.
5. **Suggestions, not law.** These are `/playbook:implement`'s starting point. Mark two Segments as parallel-safe only when their file sets are disjoint and neither `Requires` the other.

**Parallel-safety (MUST mark explicitly).** Assign a shared `Parallel group` label only when every member of the group:

1. has no dependency on another member (none appears in another's `Requires`),
2. touches a disjoint set of files (no file in two members), and
3. shares no mutable runtime state and no ordering-sensitive step (e.g. sequential DB migrations).

If any condition fails, leave the WUs ungrouped (they run sequentially). When unsure, leave sequential: a wrong parallel flag makes `/playbook:implement` run concurrent agents over the same files and corrupt the working tree. `/playbook:implement` re-verifies the flags before dispatching, but the plan should not assert parallelism it can't justify.

### Step 8: Confirm understanding

When every convergent branch is resolved, summarize:

```
## Design Summary

**Goal:** [one sentence]

### Decisions Made
1. [Decision]: [what was chosen] (because [reason])
2. ...

### Out of Scope
- [what was explicitly excluded]

### Open Risks
- [anything flagged but accepted]
```

This folds together decisions from both phases: the direction chosen at Steps 4 and 6, and the implementation detail settled in Step 7. Ask: **"Does this capture everything? Anything to change?"** Do NOT proceed until confirmed. **In `--auto`, skip this gate:** fold the summary and the Assumptions list into the plan and proceed.

### Step 9: Generate the implementation plan

Produce one self-contained document that `/playbook:implement` can consume directly. A condensed problem/goals/non-goals/approaches/decision section (from the divergent phase) comes first, followed directly by the Work Units/Segments plan (from the convergent phase). There's no separate PRD or design-doc file: approval already happened in-session at Steps 3.5 and 6, so the saved file only needs to record what was decided, not re-seek approval for it.

```
## Implementation Plan

**Status:** Proposed | **Date:** <YYYY-MM-DD>

### Background
[Why this, why now. One or two sentences from Step 1/Step 3's Purpose.]

### Problem
[From Step 3.5's confirmed problem statement.]

### Goals
[From Step 3.5.]

### Non-goals
[From Step 3.5.]

### Approaches considered
[2-3, trade-offs, which was chosen (Step 4). Rejection notes for the others.]

### Decision
[The chosen approach and why (Step 6). Ticket id and link, if ticket mode.]

### Architecture
[High-level design. Reference specific files and functions found during research.]

### Files to Create/Modify
| File | Action | Purpose |
|------|--------|---------|
| path/to/file.ts | modify | [what changes and why] |
| path/to/new.ts  | create | [what it does] |

### Segments (suggested PRs)
Ordered, PR-sized groups of Work Units. One concern each; each Segment becomes one pull request.
`/playbook:implement` honors these.
| Seg | Title | Work Units | Requires | Concern | Est. lines |
|-----|-------|-----------|----------|---------|-----------|
| S1 | [title] | WU-0, WU-1, WU-2 | none | [schema + types] | ~180 |
| S2 | [title] | WU-3 | S1 | [wire-up] | ~90 |

### Deliverables (Work Units)
Smallest independently-committable units, in dependency order. One WU = one small commit. Each WU
belongs to exactly one Segment.
| WU | Title | Files | Requires | Segment | Parallel group | Done When |
|----|-------|-------|----------|---------|----------------|-----------|
| WU-0 | [title] | path/to/types.ts | none | S1 | none | [observable acceptance] |
| WU-1 | [title] | path/to/a.ts, path/to/a.test.ts | WU-0 | S1 | P1 | ... |
| WU-2 | [title] | path/to/b.ts, path/to/b.test.ts | WU-0 | S1 | P1 | ... |
| WU-3 | [title] | path/to/index.ts | WU-1, WU-2 | S2 | none | ... |

### Parallel Groups
- **P1** (after WU-0): WU-1 and WU-2. Disjoint files, no shared state, safe to run concurrently by separate agents.
- **Sequential:** WU-0 first; WU-3 last (needs WU-1 and WU-2).

### Per-Work-Unit Detail
For each WU, in dependency order:

#### WU-N: [title]
- **Requires:** [WU-x, WU-y | nothing]
- **Files:** [exact paths, production + test]
- **Changes:** [concrete what-to-do]
- **Test scenarios:** [Gherkin Given/When/Then or TDD cycles this WU satisfies]
- **Done When:**
  - [ ] [observable acceptance criterion]

### Testing Strategy
- [What to test, which test files, what assertions]

### Routing note
[Recorded from Step 5: recommend /playbook:adr for the chosen decision, or none.]

## Confidence + open items

- Confidence: HIGH | MEDIUM | LOW, <one line on what makes it that>
- Open items (verify downstream): each one MUST be stated precisely enough that whoever picks it up next knows exactly what to check or decide. If it can't be phrased that precisely yet, say so plainly instead of listing a vague placeholder that only looks actionable.
  - <blind spot or LOW-confidence premise>, <who verifies: /playbook:plan interview, /playbook:implement watch>
```

The Testing Strategy MUST follow the `playbook:engineering-standards` skill (test types, isolation, TDD red/green/refactor, no coverage decrease).

### Step 10: Quality Gate (MUST)

After producing the plan, run the three-phase gate. Do NOT skip it. Do NOT ask the user to verify it. Do NOT proceed to Step 11 until it passes. The criteria are inline below. Run Phase 1 first (Phases 2 and 3 read its report), then dispatch Phase 2 and Phase 3 in parallel: issue both Agent calls in a single message so they run at once. The 1-before-(2,3) order is the only real dependency here; Phase 2 and Phase 3 are independent, so they never run one at a time. Consolidate all three at the Quality Gate Result.

**All three phase agents are structurally read-only, so the return value is their only channel and it is unreliable** (`playbook:delegating-subagents`; invoke it before dispatching). `fact-checker`, `critic` and `test-reviewer` hold Read, Grep, Glob and Skill only, and cannot write a report file: the repo's agent checks forbid `Write` and `Bash` for that tier by design. Phase 1 runs as a `fact-checker` dispatch like the other two. Its checks are mechanical and finish in about two minutes with re-runnable output, so when it returns nothing, redo them inline rather than waiting on it. A phase that returned nothing is **INCONCLUSIVE, never PASS**, and INCONCLUSIVE blocks exactly like FAIL: redo it inline, or record the gap and name the phase that did not run. A gate that checked nothing must never read as a pass.

**Gate source has two tiers.** Keep the current draft in the shared file `$PLANS_DIR/<topic-slug>.gate-source.md`, written with `Write` every time the draft changes. Right before each phase's dispatch (its first and any retry), freeze a copy for that phase at `$PLANS_DIR/<topic-slug>.gate-source.<phase>.md` (`<phase>` is `fact-check`, `adversarial` or `test-review`). Intent: gate-snapshot. If the session's tool map lists it, use that command with `<topic-slug>` and the phases, naming Phase 2 and Phase 3 in one call since they dispatch together. Otherwise do this: `cp` the shared file to each phase's snapshot path. A verdict is then recorded against what the phase actually reviewed even if the draft is revised before a sibling phase returns. Only Step 11's final gate check reads the shared file.

**Gate ledger.** Every phase return is recorded, and one final check decides whether Step 11 may run. Intent: gate-record. Intent: gate-check. If the session's tool map lists them, use those commands. Record with `<topic-slug> plan <phase> -` and the agent's full raw return text on stdin (a quoted heredoc, no scratch file); it reads that phase's own snapshot by default. Check with `<topic-slug> plan fact-check adversarial test-review`; it reads the shared file by default, and its exit code is the verdict. Otherwise do this: keep a `## Gate checklist` section at the end of the checkpoint file, one line per phase with its latest verdict (PASS, WARN, FAIL or INCONCLUSIVE), the round, and the snapshot it reviewed, and rewrite the checkpoint after every return. A later verdict for a phase replaces its earlier line. The check passes only when all three lines read PASS or WARN and `cmp -s` finds each phase's snapshot identical to the current shared file; a missing line is MISSING and a differing snapshot is STALE. Treat anything else as a non-zero exit. Without the tool this enforcement is advisory, since nothing outside this session holds you to it, so apply it strictly and copy the checklist into the quality report at Step 12.

Before running the gate, read `../skills/delegating-subagents/references/quality-gate.md` (relative to this file); it holds the recording, Verification Summary, Test Review criteria, INCONCLUSIVE and gate check rules used below.

#### Phase 1: Fact-Check

**Before Phase 1's first dispatch, and before each retry of Phase 1**, write the current draft to the shared file and freeze Phase 1's `fact-check` snapshot. Refresh the shared file every time the draft changes, including after every revision in the FAIL-then-revise-and-re-run cycles below: a stale snapshot would let an outdated draft pass off as the one actually reviewed.

Spawn a `fact-checker` agent (`subagent_type: playbook:fact-checker`) with the full plan and these criteria:

- Every referenced file path exists.
- Function/type signatures and imports in the plan match the real code.
- The plan is consistent with existing patterns and conventions.
- Downstream consumers of changed code are identified.
- The test infrastructure the plan assumes actually exists.
- The Work Unit dependency graph is acyclic, and each Parallel group's WUs have disjoint files with no dependency on each other (the parallel-safe flags are accurate).
- **Segments are well-formed:** every WU maps to exactly one Segment (full coverage, no WU in two); Segment order respects WU `Requires` (no forward cross-Segment dependency); each Segment is self-contained by area or feature (WARN if a planned Segment is too big to review or too small to stand alone); any parallel-marked Segments have disjoint files and no mutual `Requires`.

Returns a structured PASS / FAIL / WARN report. Phase 1 folds a Verification Summary into it (table shape in the quality-gate reference).

Spawn it with a stable `name`. **Record the gate** for `fact-check` (Gate ledger above). Do this every time Phase 1 returns, including every retry (the recording rule is in the quality-gate reference). **After it returns**, if a project store is present at `~/.config/playbook/memory/<owner>/<repo>/`, persist any durable gotcha it found as a memory fact; otherwise skip. **If any FAILs:** revise the plan (refreshing both the shared file and this phase's own snapshot per the two-tier scheme above) and re-run Phase 1 under the gate loop (`playbook:delegating-subagents`, `references/gate-loop.md`: hard cap `gate.maxRounds` rounds, stop at PASS, STUCK on no progress). Don't proceed until it passes.

#### Phase 2: Adversarial Review

**Before Phase 2's own dispatch, and before each retry of Phase 2**, refresh the shared file if the draft changed and freeze Phase 2's `adversarial` snapshot.

Spawn a `critic` agent (`subagent_type: playbook:critic`, focus `plan`) with the full plan and the Phase 1 report. It challenges the design:

- Is there a simpler alternative that meets the goal?
- Scope creep or over-engineering?
- Missing error paths or failure handling?
- Blast radius: what could this break?
- Contradictions with the fact-check report.

Returns a structured report. Spawn it with a stable `name`. **Record the gate** for `adversarial` (Gate ledger above). Do this every time Phase 2 returns, including every retry. **After it returns**, record any rejected simpler alternative (with reasoning) in the plan's Risks section. **If any FAILs:** revise and re-run Phase 2 under the same gate loop, refreshing this phase's snapshot and the shared file before the re-run.

#### Phase 3: Test Review

**Before Phase 3's own dispatch, and before each retry of Phase 3**, refresh the shared file if the draft changed and freeze Phase 3's `test-review` snapshot.

Spawn a `test-reviewer` agent (`subagent_type: playbook:test-reviewer`) with the plan's Testing Strategy and the Phase 1 report (it runs in parallel with Phase 2, so it doesn't wait on the adversarial findings). It evaluates the proposed tests against the Test Review criteria in the quality-gate reference.

Returns a structured report. Spawn it with a stable `name`. **Record the gate** for `test-review` (Gate ledger above). Do this every time Phase 3 returns, including every retry. **After it returns**, if a project store is present at `~/.config/playbook/memory/<owner>/<repo>/`, persist any durable test-quality pattern as a memory fact; otherwise skip. **If any FAILs:** revise the test plan and re-run Phase 3 under the same gate loop, refreshing this phase's snapshot and the shared file before the re-run.

#### Quality Gate Result

Present all three reports:

```
## Quality Gate Result

**Fact-Check:**        PASS (N/N checks passed)
**Adversarial Review:** PASS (N/N challenges passed)
**Test Review:**       PASS (N/N checks passed)
[or: BLOCKED: N FAILs, M WARNs]
```

WARNs are shown for awareness but do not block.

- **INCONCLUSIVE** blocks the save exactly like FAIL (definition in the quality-gate reference).

**Gate check (MUST, before Step 11):** run the gate check from the Gate ledger above for `fact-check`, `adversarial` and `test-review`, against the shared file in its current, fully refreshed state (never a phase's snapshot). Everything above (the phase reports, the Quality Gate Result presentation, the INCONCLUSIVE rule) is for the human reading it; the check's result is what actually decides whether Step 11 may run. Exit 0: proceed to Step 11. Non-zero exit: do NOT proceed to Step 11, and do NOT write any "gate passed" language; handle it as the quality-gate reference says (verbatim output, FAIL and INCONCLUSIVE retry, STALE re-run).

### Step 11: User Approval

**In `--auto`, skip this step:** there is no approval pause. After the gate passes, go straight to Step 12; a gate FAIL stops the run (Step 10) instead of prompting for an override. The rest of this step is the default interactive flow.

After the gate passes, present the full plan and the gate reports. Ask:

**"Quality gate passed. Does this plan look right? Anything to change before I save it?"**

If there are WARNs, highlight them: **"Note: N warnings flagged (see report). None are blocking."**

Do NOT save until the user explicitly approves. If they request changes, revise the affected parts, re-run the quality gate (Step 10), and present again.

**Override:** if the gate has FAILs and the user says to proceed anyway, record the override in the quality report file: `Quality gate override: proceeding despite FAIL on <check> because <user's reason>`.

### Step 12: Save & Next Steps

Only after the user approves. The plans directory lives outside this repo checkout (`$HOME/.config/playbook/repos/<owner>/<repo>/plans/`, one folder per repo shared by every worktree, resolved in Step 0), not gitignored: there is nothing repo-local left to ignore.

1. Save the plan to `$PLANS_DIR/<topic-slug>.md` and the gate reports to `$PLANS_DIR/<topic-slug>-quality.md`.
2. Clear the scratch files this run left in the plans folder: the checkpoint `<topic-slug>.checkpoint.md`, its lock directory `<topic-slug>.checkpoint.md.lock`, the shared gate source `<topic-slug>.gate-source.md` and every per-phase snapshot `<topic-slug>.gate-source.<phase>.md`. They would otherwise linger in the plans folder. Touch only those names: `<topic-slug>.md` and `<topic-slug>-quality.md` stay. Intent: plan-cleanup. If the session's tool map lists it, use that command with `<topic-slug>`; it removes exactly those names, is safe to repeat, and exits 0 when there is nothing to remove. If it exits non-zero, or the tool map does not list it, do this: leave the files where they are and list their paths in the report, so the user can delete them. Do not delete them with `rm`.
3. If a project store is present at `~/.config/playbook/memory/<owner>/<repo>/`, persist the plan's accepted key decisions (from both the divergent and convergent phases) as project memory facts (`type: project`, `anchors:` to the files they touch). The graph rebuilds automatically on fact save via the PostToolUse hook. Otherwise skip.
4. Tell the user:
   - "Saved to `<plans-dir>/<topic-slug>.md`" (`<plans-dir>` is the plans folder Step 0 resolved).
   - "The plan is self-contained. Run `/clear`, then implement it with a clean context: `/playbook:implement <plans-dir>/<topic-slug>.md`." This session's back-and-forth is exactly what a fresh execution phase shouldn't carry forward; there's no way to clear it from inside this session, so say so instead of leaving it implicit. **In auto mode,** skip the `/clear` instruction and hand the saved plan path straight to `/playbook:implement`: print `/playbook:implement <plans-dir>/<topic-slug>.md` as the next command to run.
   - If Step 5 recommended it or `--adr` was set: "Want to record the architectural call too? Run `/clear`, then `/playbook:adr` for that decision." Note it's a separate command producing a separate artifact, not a replacement for the saved plan. In auto mode, leave out the `/clear` here too.
   - **In `--auto`:** also list the **Assumptions** made (especially any `OPEN` ones) so the user can audit the autonomous choices before running `/playbook:implement`.

### Teardown (MUST run, even on failure or abort)

Close any subagent from this run that is still alive before finishing.

## Autonomous Mode (`--auto`)

`--auto` is scoped narrower than the old `/playbook:scope --auto`: it automates only the convergent phase, Step 7 onward. The divergent phase, Steps 1 through 6, stops for a human under plain `--auto`: approach selection (Step 4), the `/playbook:adr` route check (Step 5), and the design approval (Step 6) are unconditional gates. This is an intentional, stated breaking change from today's `/playbook:scope --auto`, which, on a raw topic-only invocation with no prior design doc, auto-answered approach selection too. Now that the divergent phase and the convergent phase are one command instead of two, letting plain `--auto` skip the divergent phase's gates would mean nobody ever looks at the chosen direction before the plan gets written, which is a materially bigger risk than skipping the old `/playbook:scope --auto`'s implementation-detail assumptions.

Enable when Step 0 resolves the mode to auto (the `--auto` flag, `PLAYBOOK_MODE`, or the repo config); the flags are stripped (like `--help`) before resolving the topic seed. From Step 7 onward:

- **No questions.** For every decision Step 7 would ask, take the answer you would have recommended ("I'd recommend X because Y") and proceed. Still do the Step 2 research first: explore the codebase and, if a memory store is present, read it too, since a preference or convention there may override your default choice. When no memory store exists, skip that step silently.
- **Record assumptions.** Every self-made decision from Step 7 onward goes into an **Assumptions** list with its rationale, so the user can audit what was chosen for them. When you're genuinely split on a decision, record it as an `OPEN` assumption (with the leading option and why) rather than silently picking.
- **Skip the confirmation gates.** Do not pause at Step 8 ("Does this capture everything?") or Step 11 ("Does this plan look right?"). Fold the Design Summary and the Assumptions list into the saved plan instead.
- **`--auto-design` also self-answers Steps 3 to 6.** It is opt-in on the command line only; the environment and the repo config never turn it on. It takes the recommended answer to every discovery question, the problem statement check, the approach, the `/playbook:adr` route check, the spike offer (no) and the design approval, and logs each one in the Assumptions list with its reason. Plain `--auto` never approves the design: it stops at Step 6 with a message.
- **Quality gate still runs (Step 10).** It needs no user input. If a phase ends STUCK or FAIL at the gate loop's cap, STOP: do not save; report the stop report from `references/gate-loop.md` and the assumptions made. No user is present to override a FAIL in `--auto`.
- **Save and report (Step 12).** On a passing gate, save the plan and quality report, then tell the user the paths and assumptions (flag `OPEN` ones). Tell the user to run `/playbook:implement` when ready. If a project store is present at `~/.config/playbook/memory/<owner>/<repo>/`, persist the accepted decisions there; otherwise skip.

## Adapting to Complexity

- **Simple change (1-2 files):** 2-4 questions in Step 3, 3-5 in Step 7. Don't over-interview a trivial change.
- **Medium feature (3-10 files):** more in Step 3 as the idea needs; 8-15 in Step 7, focused on integration points and edge cases.
- **Large feature (new module, architecture):** the fullest Step 3 discovery; 15-25 in Step 7, covering dependencies, rollout, migration.
- **Bug fix:** replace design questions with diagnosis: reproduction steps, error messages, root-cause hypotheses. Still provide recommended answers.
