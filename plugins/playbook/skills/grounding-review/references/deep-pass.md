# Deep Review: Multi-Agent PR Review

Review a pull request with a swarm of specialist reviewer subagents run in parallel, each a focused `reviewer` subagent under the `playbook:grounding-review` discipline. The orchestrating session consolidates, dedups, and fact-checks their findings, then posts them as a **pending** GitHub review (same posting flow as `quick-pass.md`). This is heavier and slower than `/playbook:review --quick`; use it for substantial, risky, or cross-cutting PRs.

> **Security caveat:** running `/playbook:review --deep` on an explicit PR installs and runs the PR's code in your local environment: npm preinstall/postinstall hooks, build scripts, test suites. This exposes you to supply-chain attacks and code execution with your credentials in reach. Only run it on PRs you trust to execute locally.

## Contents

Steps 0 to 8 run in order, and the teardown always runs.

## Help

If the arguments contain `--help`, print this and stop:

```
/playbook:review --deep - Multi-agent PR review with a specialist reviewer swarm

USAGE:
  /playbook:review --deep [PR_NUMBER] [options]

OPTIONS:
  --help            Show this help
  --all             Run every reviewer regardless of diff content
  --preset <name>   Named reviewer set: security | architecture | data | docs
  --no-post         Same as --self
  --self            Local self-review, never posts to GitHub (default when no
                    PR number is given, or when the PR is yours)
  --this-pr         When the PR is in a stack, review only this PR
  --whole-stack     When the PR is in a stack, review the whole stack
  --auto            Auto run mode; does not skip the post question, pass --self
  --ask             Force the interactive mode

EXAMPLES:
  /playbook:review --deep               Review the current branch's PR and ask what to post
  /playbook:review --deep 123           Review and post to PR #123 (unless #123 is your own PR)
  /playbook:review --deep 123 --all     PR #123 with every reviewer, posts (unless it's yours)
  /playbook:review --deep 123 --self    Review PR #123, report only, never posts
```

With no PR number the current branch's PR is reviewed and the post question is asked as usual. Only `--self` and `--no-post` skip it, auto mode included. On your own PR GitHub blocks approve and request-changes, so the submit verb is limited to comment or skip.

## Step 0: Routing

**Routing.** Where you choose what to spawn, route the `review` kind. Intent: route-model. If the session's tool map (printed at session start) lists it, use that command and follow its `action` under the `routing.escalate` gate: on `ask` stop and ask the user, on `downgraded` (`deny`) use the lower tier it names, and in auto mode (`auto`) proceed and log the assumption. If the command fails, carry on. Otherwise do this: spawn the agents this file names, with the model their agent files pin.

- **`ask` mode:** behave exactly as this file describes.
- **`auto` mode:** follow the Posting in auto mode paragraph below: it changes nothing about posting.

### Posting in auto mode

Auto mode does not change posting. Only `--self` or `--no-post` skips the post question, so a run from chat always asks. Pipelines and unattended callers must pass `--self`.

## Reviewer Swarm

**Core reviewers** (run unless a preset narrows the set):

| Reviewer | Focus |
| --- | --- |
| logic | algorithm correctness, edge cases, off-by-one, normalisation, false positives/negatives |
| test | coverage gaps, missing scenarios, untested error paths, weak assertions, flakiness |
| security | auth, PII handling, crypto, injection, IDOR, leaked secrets |
| data | query/DAO correctness, N+1, missing indexes, transaction boundaries |
| types | `any`, unsafe casts (`as`) instead of runtime parsing at a boundary, non-null assertions (`!`), weak typing (language-appropriate) |
| perf | N+1, unbounded data, connection leaks, work inside loops |

**Conditional reviewers** (added by default when the diff shows the trigger):

| Reviewer | Trigger |
| --- | --- |
| architecture | new modules, dependency or layer changes, boundary violations |
| big-o | algorithms over collections, sorting, searching, graph traversal |
| complexity | deep nesting, large functions, tight coupling |
| integration | feature flags, events, external APIs, config, deployment changes |
| migration | database migrations, schema changes |
| docs | README changes, breaking changes, new public APIs |
| dedup | many similar files, or a large diff (>300 changed lines) |
| adr | an Architecture Decision Record is added or affected |

**Presets:** `security` = security+data+types+logic · `architecture` = architecture+complexity+big-o+dedup · `data` = data+migration+perf · `docs` = docs+adr. For every reviewer regardless of diff content, use `--all` directly.

## Execution rules (MUST)

1. Run every bash block for real with the `Bash` tool. Don't simulate.
2. No caching: every invocation is a fresh run, even if you reviewed this PR earlier in the conversation. The code may have changed.
3. No skipping (except steps guarded by a flag the user didn't set).
4. No assumptions: run the command and read the result.
5. Follow the command's gates, not your own.
6. Show real data: tables and reports come from actual output, never placeholders.
7. **No selective filtering at presentation.** After consolidation (Step 4) and the verification sweep (Step 4b) you present EVERY surviving finding; the user decides what to post in Step 6. (Consolidation's dedup/drop rules and the sweep's drops are the only removals, and they happen in Steps 4 and 4b, not by hiding findings in Step 5.)
8. **Never ask whether to run.** Calling `/playbook:review --deep` IS the instruction to run; start immediately.

## Voice rules

Findings are plain: Conventional Comments label and decorations (read `playbook:grounding-review` reference `references/conventional-comments.md`), `file:line`, evidence, a short failure scenario, and one fix in plain words, with no comment body (same discipline as `quick-pass.md`). Step 5 drafts the comments after the sweep, and only when something can be posted. Non-negotiables for every finding: Conventional Comments label in **plain text** and bare (`blocking:`, `issue:`, `suggestion:`, `nitpick:`, `question:`, never bold; `blocking:` replaces `issue:` specifically for a merge-blocking finding); plain, jargon-free language; findings kept short (one sentence when possible, two at most, the problem then what breaks, a second only when the mechanism is non-obvious); one pragmatic fix, not a menu; no hedging; no meta-justification; no em or en dashes.

## Step 1: Resolve PR and gather context

Resolve the PR (a number or `#N`, else a branch name, else the current branch's PR), work out `SELF_MODE`, decide in-place or worktree and set the worktree up, and create the folder of `REVIEW_JSON`. Intent: review-prepare. If the session's tool map (printed at session start) lists it, use that command with the `deep` kind and `"$ARGUMENTS"` as one quoted argument, adding `--auto` when Step 0 read auto mode (it does not skip the post question). It prints these lines: `PR`, `REPO`, `PR_NUMBER`, `HEAD_SHA`, `AUTHOR`, `SELF_REVIEW`, `SELF_MODE`, `MODE`, `WT` (empty when in place) and `REVIEW_JSON`. On a problem it prints `error: ...` and exits 1: stop and show it. Otherwise do this:

1. Resolve `PR_NUMBER`: the first word that is a number or `#N`; else a branch name that passes `git check-ref-format --branch <arg>`, resolved with `gh pr list --head <branch> --json number -q '.[0].number'` (none found: stop with `error: no open PR for branch <name>; create one first or pass a PR number`); else the current branch's PR from `gh pr view --json number -q .number`.
2. Run `gh repo view --json nameWithOwner -q .nameWithOwner` for `REPO`, `gh pr view <PR_NUMBER> --json headRefOid,author -q '.headRefOid + " " + .author.login'` for `HEAD_SHA` and `AUTHOR`, and `gh api /user -q .login` for your login. `SELF_REVIEW` is true when `AUTHOR` is your login.
3. In place when `git rev-parse HEAD` equals `HEAD_SHA` and `git status --porcelain --untracked-files=no` is empty; `WT` is then empty. Otherwise set up the review worktree (Intent: review-worktree-setup; if the tool map lists it, use that command with `PR_NUMBER` and `HEAD_SHA` and capture stdout only), or by hand:

   ```bash
   DIR="$(git rev-parse --path-format=absolute --git-common-dir)/review-worktrees/$PR_NUMBER-${HEAD_SHA:0:7}"
[ ! -e "$DIR" ] || { echo "another review of this PR may be running: $DIR"; exit 1; }
   git fetch "$(gh repo view --json url -q .url)" "refs/pull/$PR_NUMBER/head"
   git cat-file -e "$HEAD_SHA^{commit}"
   mkdir -p "$(dirname "$DIR")" && git worktree add --detach --lock --reason "review pr=$PR_NUMBER ts=$(date +%s)" "$DIR" "$HEAD_SHA" && WT="$DIR"
   ```

   `WT` is set only after the worktree is added, so a failed setup leaves it empty and teardown skips it. If a command fails, show the error and stop.
4. Set `REVIEW_JSON=/tmp/$REPO/deep-review-$PR_NUMBER.json` and run `mkdir -p "$(dirname "$REVIEW_JSON")"`.

Then read the PR with the number you resolved:

```bash
gh pr view <PR_NUMBER>
gh pr diff <PR_NUMBER>
```

Capture `REPO`, `PR_NUMBER`, `HEAD_SHA`, `SELF_REVIEW`, `SELF_MODE`, `REVIEW_JSON`. `SELF_MODE` is true, and posting is skipped entirely, only when `--self` or `--no-post` is passed. `SELF_REVIEW` (the PR is yours) does not set it: it only narrows the submit verb to `comment` or `skip`.

In worktree mode, `WT` holds the absolute path to the isolated checkout. In in-place mode it is empty. Subagents use `$WT` for all reads; if empty, they read from the local working tree.

## Step 1b: Stack check

Right after Step 1, read `skills/grounding-review/references/stack-review.md` in this plugin and follow it. A PR that is not in a stack gets no question and nothing changes in this file. When the PR is in a stack, that file decides the scope (this PR only, the whole stack, or the whole stack with deep review), builds one shared context, and says how Steps 2 to 4 and the posting step differ for a whole stack. Strip `--this-pr` and `--whole-stack` before reading the other arguments.

## Step 2: Select reviewers

- `--all` → every core + conditional reviewer.
- `--preset <name>` → that preset's set.
- otherwise (default) → all core reviewers, plus each conditional reviewer the diff triggers. Intent: review-lenses-pick. If the session's tool map lists it, run it with `--base <PR base>` (`lens: trigger` lines, or `NONE`); else grep the diff for the table's triggers. Say which you picked and why.

## Step 2b: Run checks in the worktree (best-effort, worktree mode only)

Skip this step entirely when `WT` is empty (in-place mode).

Run the project's own check suite once in the worktree, picking the toolchain by the file at the top of `$WT`. Intent: review-checks. If the session's tool map (printed at session start) lists it, use that command with `"$WT"`; it prints the combined output. Otherwise do this: run, inside `$WT`, the first row whose file exists, and keep stdout and stderr of every command:

| File | Setup | Checks |
|---|---|---|
| `package.json` | `npm install --prefer-offline` (keep the last 5 lines) | `npm run typecheck`, `npm run lint`, `npm test` |
| `pyproject.toml` or `setup.py` | `pip install -e . -q` (keep the last 3 lines) | `python -m mypy .`, `python -m pytest` |
| `go.mod` | none | `go vet ./...`, `go test ./...` |
| `Cargo.toml` | none | `cargo check`, `cargo test` |

With none of these files, the output is `[no recognised toolchain; checks skipped]`.

A failing install or check is printed as output, never an error: never block the review. Keep the output as `CHECK_OUTPUT` so Step 3 can embed it verbatim in each subagent prompt.

## Step 2c: Load memory (best-effort)

Load the saved facts for `<owner>/<repo>` (from `git remote get-url origin`). Intent: memory-context. If the session's tool map (printed at session start) lists it, use that command with `--repo <owner>/<repo>`, and load the fact files it names on demand. The command reads the memory graph itself and prints a `Memory context: none` line saying why when it has nothing. Otherwise do this: skip the step, because only that tool reads the memory graph. Skipped, or with a `Memory context: none` line, the step ends silently; Step 3's reviewers get no memory section and that's expected, not an error. Note in the report whether facts were found.

## Step 2d: Haiku triage

Skip this step entirely when `--all` was passed (check `$ARGUMENTS` the same way Step 2 does): every core + conditional reviewer Step 2 selected already runs `full-lens`, matching `--all`'s existing "run every reviewer regardless of diff content" guarantee, so there is nothing left for triage to narrow.

Otherwise, dispatch `review-triage` (`subagent_type: playbook:review-triage`) exactly once per review run, regardless of how many lenses Step 2 selected. The prompt includes: the PR diff (the same diff Step 1 already captured via `gh pr diff "$PR_NUMBER"` and that Step 3's reviewer prompts also embed), `HEAD_SHA`, the absolute `$WT` path (or a note that the tree is in-place when `WT` is empty, same convention Step 3 uses), and the full set of lens names Step 2 selected (core reviewers plus any triggered conditional reviewers). Capture the returned tier map, a JSON object of `{lens: {tier, reason}}`, into context for Step 3 to read.

Complete the map so every lens Step 2 selected has a tier. Intent: review-triage-merge. If the session's tool map (printed at session start) lists it, use that command with `--lenses <comma separated lens names>` and the triage return text as `--tiers <file>` (or `--tiers -` on stdin; omit `--tiers` when the dispatch failed or returned nothing). It prints the full tier map as JSON, one entry per lens, then a `Triage: lens=tier, ...` line. Otherwise do this: complete it yourself by the rules in "Completing the map" in `skills/grounding-review/references/triage-tiers.md`, one entry per lens in Step 2's order, then write the `Triage: lens=tier, ...` line. Either way, failed, empty or unknown triage results become `full-lens` and never narrow coverage. Use the completed map in Step 3.

Report the `Triage:` line (e.g. "Triage: security=full-lens, docs=cheap-check, perf=skip"), the same way Step 2 reports which reviewers it selected and why.

## Step 2e: Load skills and reference files

Only now, with the lenses and tiers settled, load what the run needs. The orchestrator invokes `playbook:grounding-review` (for the sweep and the report format) and loads no `playbook:writing-style` here. Each dispatched `reviewer` loads `playbook:grounding-review` plus the one reference file mapped to its lens (Step 3). A `cheap-checker` reads only that resolved file. A skipped lens loads nothing.

## Step 3: Spawn the reviewer swarm (parallel reviewer subagents)

If you have no `Agent` tool (a delegated or forked caller), run the selected lenses yourself in this session, say in the report that the swarm was not used, and never report APPROVE.

**Concurrency cap (MUST).** Dispatch at most 8 reviewers at once. When the selected set (Step 2) is 8 or fewer, dispatch it in one wave exactly as below. When it's larger (only possible under `--all`, up to 14 lenses), split into waves of at most 8: issue the first wave's `Agent` calls in one message, wait for them to return, then issue the remaining lenses as a second wave. This bounds concurrent spawns; it never drops a lens to stay under the cap; every selected reviewer still runs, just possibly across two waves instead of one.

For each lens in a wave, read its tier from the map Step 2d completed before dispatching (it is already complete, so no lens lacks a tier). Dispatch by tier:

- **`full-lens`:** dispatch a `reviewer` subagent (`subagent_type: playbook:reviewer`, with the `effort` parameter chosen by "Pick the effort" in `playbook:delegating-subagents`) as everything below through "Instruct each to" describes. It loads `playbook:grounding-review` and, when the lens table in the triage-tiers reference maps its lens to a reference file rather than `SKILL.md`, only that reference file. It never loads `playbook:writing-style`.
- **`cheap-check`:** dispatch a `cheap-checker` subagent (`subagent_type: playbook:cheap-checker`) instead of `reviewer`. Its prompt names the lens's narrow concern, the PR diff and `HEAD_SHA`, the absolute `$WT` path (or the in-place note when `WT` is empty), same conventions as the `reviewer` dispatch below, and ONE absolute reference file path for its lens. Read `skills/grounding-review/references/triage-tiers.md` in this plugin and follow it for the concern text, the lens to file table, the absolute path rule and the `SKILL.md` fallback.
- **`skip`:** dispatch nothing for that lens and track it as skipped in the `Triage:` line Step 2d reports. A skipped lens is never conflated with "returned nothing" below (rule in the triage-tiers reference).

Spawn each wave **in parallel** (one message, multiple `Agent` calls). The `reviewer` agent is structurally read-only (Read/Grep/Glob only, no Edit/Write/Bash) and pins its own model tier, so the orchestrator no longer sets `model` per call; `cheap-checker` is the same shape (Read/Grep/Glob/Skill, its own pinned `haiku` model). Each reviewer prompt MUST include: its focus area (from the table), the PR diff and `HEAD_SHA`, numbered excerpts of each changed file, the scoped test command and the stated limit to read beyond them only when needed (build it once per run, per "Review brief builder" in `skills/delegating-subagents/references/briefs.md`, which adds the `Callers` block only when the user asked for code facts), the instruction to load `playbook:grounding-review` (plus its mapped reference file), the absolute `$WT` path (or a note that the tree is in-place if `WT` is empty) with the instruction "Read and grep files under <WT>; do not install or build anything.", the `CHECK_OUTPUT` captured in Step 2b verbatim under a heading "Check suite output (from orchestrator)", and, when Step 2c loaded anything, a memory slice: facts anchored to a file touched in the diff, or otherwise related to the lens's focus area (for example, security-tagged facts for the security lens), listed by one-line hook or short body under a heading "Relevant memory (from orchestrator)". No matching facts means no section, not an empty placeholder.

In worktree mode, each reviewer prompt includes the absolute `$WT` path with the instruction "read and grep files under $WT; do not install or build." Subagents are read-only. The orchestrator has already run the checks once in Step 2b; subagents use the captured output as context, not as a trigger to re-run.

For full-lens dispatches, instruct the reviewer to:

- Read every file it cites at `HEAD_SHA` (diff context alone is insufficient); quote exact code with `file:line`; tag anything unconfirmed `[unverified]`.
- Stay within its focus; don't report issues another reviewer owns.
- Return plain findings as a JSON array, one object per finding, with no comment body and no `Post:` block:

```json
{"file": "...", "line": N, "side": "RIGHT", "label": "issue", "decorations": ["blocking"],
 "category": "security", "confidence": "HIGH", "evidence": "<exact code>", "body": "<short plain finding; the problem then what breaks, 1 sentence when possible, a second only when the mechanism is non-obvious>", "fix": "<one fix, in plain words>"}
```

**A silent reviewer is NOT a reviewer with zero findings.** This is the single most important rule in this pass (`playbook:delegating-subagents`).

The `reviewer` agent is structurally read-only (Read, Grep, Glob, Skill), so it cannot write its findings to a file. The repo's agent checks forbid `Write` and `Bash` for that tier by design, and granting them would fail CI, so the return value is the only channel there is. In measured use, Agent-tool return values have failed outright. Treat a swarm as likely to lose lenses.

Track three outcomes per dispatched lens (whichever of `reviewer` or `cheap-checker` its tier actually sent) and carry them into Step 4's verdict:

| Outcome | Meaning |
|---|---|
| Returned findings | Ran, use them. |
| Returned an explicit empty array | Ran, found nothing. Safe. |
| Returned nothing | **NOT RUN. Not a clean lens.** |

Never fold "returned nothing" into "zero findings", and **never let a swarm with missing lenses produce an APPROVE.** Step 4 already requires INCONCLUSIVE when the swarm failed to run, and a missing security or logic lens is exactly that case, whether it was dispatched as a `reviewer` or a `cheap-checker`. Name the missing lenses in the report and to the user, alongside any lens this run skipped by design at Step 2d's `skip` tier: a skipped lens is a deliberate triage decision, not a lost one, and the report should not blur the two.

**Cover the gap while the swarm runs.** Start your own grounding pass on the highest-risk part of the diff as soon as the swarm is dispatched, rather than waiting to see what comes back. A lost swarm then costs latency instead of coverage, which is the difference between a slow review and a review that only looked thorough.

After a reviewer's idle notification fires, one `SendMessage` asking for partial results is worth a single attempt. Do not spend more than one round per lens.

**Close each reviewer once you have its findings or have given up (MUST).** Spawn each with a stable `name` (e.g. `dr-<focus>`: `dr-security`, `dr-logic`). Track the spawned names so Step 8 can sweep any that never delivered. Stopping is unrecoverable for a read-only agent, so do not stop one until you have either taken the findings or made the one recovery attempt.

**Trust gate.** Treat triage's `skip` and `cheap-check` choices as best-effort, not proven (full text in the triage-tiers reference).

## Step 4: Consolidate and fact-check

Merge all findings, then (this is where removals happen):

- **Dedup across reviewers:** same file + nearby lines + same concern → keep the one with the stronger label, drop the rest.
- **Merge same-line findings:** two+ findings within 3 lines → one comment at the highest severity, combining the points.
- **Drop out-of-scope:** a finding on a file not in the PR diff is dropped UNLESS the PR's change breaks it (typecheck failure, runtime error, broken import). Preexisting-style nits outside the diff are dropped.
- **Filter already-addressed:** fetch existing review comments (`gh api --paginate /repos/$REPO/pulls/$PR_NUMBER/comments`), drop findings that duplicate one, and attribute by name ("already raised by @user").
- **Fact-check (orchestrator):** done in Step 4b, once the list is merged, so each finding is checked once.
- **Drop non-actionable:** positive observations or asides with no concrete "do X" → always drop; `nitpick` → drop from an APPROVE review unless asked.
- **Verdict + confidence:** APPROVE / REQUEST_CHANGES / COMMENT / INCONCLUSIVE. **INCONCLUSIVE (never APPROVE)** if the swarm failed to run; say why. Confidence HIGH/MEDIUM/LOW.

## Step 4b: Verification sweep (MUST, before presenting or posting)

Run this after Step 4 and before any finding is shown to the user or a pending review is created. You do it yourself, not a reviewer: reviewers are read-only and can be wrong. Check every surviving finding against the code at `HEAD_SHA` (under `$WT` in worktree mode):

1. **True.** Re-read the cited lines. Trace or run the failure scenario where that is cheap. A finding tagged `[unverified]` is either confirmed, dropped, or kept with the `[unverified]` tag stated plainly.
2. **Label.** The label and decorations match the real impact. Read `playbook:grounding-review` reference `references/conventional-comments.md` for the nine labels.
3. **Anchor.** The file and line are right. In a stacked or multi-PR review, the finding sits on the PR or branch that owns the code.

Drop findings that do not hold, relabel the mislabelled, and move the misplaced. Read the cited lines plus what the trace needs, never whole files. Then put a `Sweep:` line under the Overview with the counts: kept, dropped, relabelled, moved, and recompute the verdict, confidence and finding order from the swept list (Step 4 set them before the sweep). See the Verification Sweep section of `playbook:grounding-review`.

## Step 5: Present the consolidated report

Show this plain report only in `SELF_MODE` or when nothing is postable. Otherwise it is shown once, with the drafts below. Present ALL findings that survived the Step 4b sweep (rule 7), with its `Sweep:` line under the Overview. Render the `playbook:grounding-review` Review Report Format exactly, INCLUDING the `### Reviewers` line. List every lens Step 2 selected, including any Step 2d resolved to `skip`, so a reader can see what was deliberately not looked at, not just what fired. Show each lens's Step 2d tier alongside its finding count: a `full-lens` or `cheap-check` lens renders `<lens>: <tier> (<count>)` (tier written as `full` or `cheap-check`); a `skip` lens renders `<lens>: skip` with NO count, since it never ran and a count of 0 would misleadingly read the same as "ran and found nothing". For example: "security: full (2) · docs: cheap-check (0) · perf: skip". Each finding is plain: label, `file:line`, evidence, the failure, and one fix. A finding whose evidence is not on a changed diff line ends with `Report-only: not on a changed line, no inline comment.`

### Draft the comments (non-self runs only)

Skip this in `SELF_MODE`, or when nothing is postable: the plain report above is the deliverable, no comment is drafted, and Step 6 stops. Otherwise, with the sweep done, hand the drafting to the cheap writer. For every swept finding that is not `Report-only`, dispatch `playbook:writer` (route kind `write`, see "Writing text" in `playbook:delegating-subagents`), all in one message, with the kind `review-comment`, the finding's label, decorations, failure, fix and `file:line` as the only facts, and the absolute path of the `github-review-comments` reference of the `writing-style` skill. Run `playbook prose check` on each returned text with its facts file, send a failure back once, then write that comment yourself: load `playbook:writing-style` first. The writer skips a ```suggestion``` block: add it yourself when the fix is mechanical. Each body starts with the bare plain-text label, never bold, and carries no `file:line` prefix.

Present the report with each draft shown under its finding as a `Draft:` block. Each draft is the final text, in the `playbook:writing-style` register (simple, plain, direct): label, voice, no dashes, GitHub rules all applied. Nothing is rewritten between this preview and the post, so what the user reads is exactly what posts. If the user asks for a change to a draft, redraft it with `playbook:writing-style` and show the new preview before posting.

## Step 6: Orchestrate posting

If `SELF_MODE` (`--self` or `--no-post` passed), or nothing postable, stop here: the report IS the deliverable, no GitHub posting. The post question is skipped only by the flag.

Otherwise, **print every finding first (MUST).** Before asking anything, print all surviving findings in the chat, none left out and none summarized away: a code (F1, F2, and so on), the label, severity, `file:line` and a one-line description each. With zero findings, say so. The questions come only after this list, and the options refer to findings by code. A question with no list above it is a bug (#593).

Then ask **one question at a time**:

- **Q1:** "Post which findings as a pending review?" Read `skills/grounding-review/references/pick-list.md` in this plugin and follow it: ask with `AskUserQuestion` checkboxes, or its typed list when the tool is not available. Number the findings in the order of the list above. `--self` and `--no-post` still skip the question. If the choice is `none`, stop.
- Build the payload at `$REVIEW_JSON` (the `quick-pass.md` findings format, no review `body`). Build each inline comment's `body` from that finding's previewed `Draft:` block verbatim, anchored to the finding's `file:line`. Post the previewed bodies as they are, never a rewording. Skip any finding marked `Report-only`. Then create the pending review (intent pr-review-post: `"$PR_NUMBER" --findings "$REVIEW_JSON" --json`), or without the tool run:

```bash
gh api -X POST /repos/$REPO/pulls/$PR_NUMBER/reviews --input "$REVIEW_JSON" --jq '{id, state, html_url}'
```

- **Pre-post verification (MUST):** Step 4b settled each finding. Before this call, only confirm the PR is still OPEN and not CONFLICTING (`gh pr view "$PR_NUMBER" --json state,mergeable`). Don't post on a merged/closed/conflicting PR.
- **Q2:** "Submit verb? approve / comment / request-changes / skip." When `SELF_REVIEW` is true, offer only `comment` and `skip`, because GitHub rejects approve and request-changes on your own PR. On `skip`, leave it PENDING. Otherwise:
- **Q3:** "Add a comment for the review?" (optional free text, blank to skip). Leave `BODY` empty on a blank answer, except: on `approve` with a blank answer, default `BODY` to `LGTM`.

Submit (intent pr-review-submit: `"$PR_NUMBER" "$REVIEW_ID" --event <approve|comment|request-changes> --json`, `--body "$BODY"` when set), or without the tool run:

```bash
gh api -X POST /repos/$REPO/pulls/$PR_NUMBER/reviews/$REVIEW_ID/events -f event=<APPROVE|COMMENT|REQUEST_CHANGES> ${BODY:+-f body="$BODY"}
```

Never invent URLs; use the returned `html_url`.

## Step 7: Capture and wrap up

- If a project store is present at `~/.config/playbook/memory/<owner>/<repo>/` (derive `<owner>/<repo>` from `git remote get-url origin`), persist each POSTED blocking/non-blocking finding as a project memory fact (`type: project`, tag it a review gotcha, `anchors:` to the file), deduping against existing memory first. Skip suggestions/nitpicks and anything not posted; if there's no project store, skip silently.
- If not `SELF_MODE` and the PR has unaddressed review threads, offer to run `/playbook:pr-comments $PR_NUMBER`.
- Final message: one line per outcome (pending review id + count, or submitted verb + timestamp).

## Step 8: Teardown (MUST run, even on failure, abort, or skip)

**Close any reviewer subagent from Step 3 that is still alive first**, whether the review completed, failed, was skipped, or aborted mid-swarm.

Then, if `WT` is not empty, always remove the review worktree. Intent: review-worktree-teardown. If the session's tool map (printed at session start) lists it, use that command with `"$WT"`. Otherwise do this:

```bash
git worktree remove --force --force "$WT"
git worktree prune
```

Run this whether the review completed, failed, was skipped, or was aborted mid-swarm. It's a no-op if the worktree is already gone.

## Anti-patterns to refuse

1. Presenting a padded review: dedup, merge, and drop per Step 4, but never hide a surviving finding from the user in Step 5.
2. APPROVE when the swarm didn't actually run. Zero findings from a broken swarm is INCONCLUSIVE, not LGTM.
3. Auto-submitting without the two-question orchestration.
4. Posting findings in the review body instead of inline.
5. Fabricated evidence, line numbers, or URLs. Verify against the file at HEAD; use API-returned URLs.
