# Landing a Segment (`--boundary=land`)

Read by `/playbook:implement` Step 10, only when the run uses `--boundary=land`. The step headings and decision rows below keep the names `commands/implement.md` refers to.

Landing now runs through `playbook pr merge <n>`: it waits for checks, arms auto-merge pinned to the head and confirms the result. Use it in place of the manual gate, merge and confirm steps below when it is in the tool map, and keep the `--delete-branch` and `--admin` rules.
## Contents

- Step 10: Land the Segment (promote, gate on CI, merge, confirm, fix tiers, park)
- Decision rows for `land`

## Step 10: Land the Segment

Runs once per Segment, immediately after Step 9 opened that Segment's PR, and MUST complete with the PR reading `MERGED` before Step 5 creates Segment N+1's branch. Skip this step entirely under **savepoint** and **pause**.

**The governing rule: attempt, then re-read state. Never conclude from an exit code.** `gh pr merge --auto` returns 0 when it merely *arms* auto-merge, and returns non-zero while still enqueueing successfully (this repo prints `! The merge strategy for main is set by the merge queue` to stderr and enqueues anyway). Both directions of the exit code lie. The exit code and stderr route the next probe; only a fresh `gh pr view` adjudicates. Equally, do NOT pre-compute whether a human review is required from `gh api repos/{owner}/{repo}` (`allow_auto_merge`, `permissions.admin`) or from branch-protection/ruleset introspection: those are two different APIs a repo may use either of, and neither tells you whether *this* PR is already satisfied. `reviewDecision` on the PR does, because GitHub computes it across whichever system is active, including approvals already present.

**Never pass `--delete-branch`.** It is redundant where the repo sets `delete_branch_on_merge`, and it deletes the LOCAL branch even when the merge itself fails (seen during a 503), which is the one state this loop must not land in.

**1. Promote to ready.** `/playbook:pr-create` opens a draft unless `pr.draft` is `false`, and a draft cannot enqueue (`Pull request is a draft`). Promote it directly (a no-op when the PR already opened ready):

```bash
gh pr ready "$BRANCH"
```

**Skip `playbook:finish-pull-request` here (it runs `/playbook:review --self --deep`), and pass no `--ready` to `/playbook:pr-create`.** That step exists to guarantee a human is never the first reviewer of unreviewed code; Steps 8 and 9 above already satisfied that invariant for this Segment, more strongly, with a 5-lens swarm whose reviewers ran in fresh contexts that never saw the implementation. Reviewers are dispatched subagents, so they are fresh by construction. Record in the ledger which review satisfied the gate: `review: implement-step9 (5 lenses, <n> findings fixed)`.

**2. Gate on required checks.** Poll with a deadline; never `--watch` unbounded. Wait 120 seconds, then read `gh pr checks "$PR" --required --json name,bucket` every 30 seconds for up to 300 seconds, each read its own Bash call. Ignore the exit code of `gh pr checks` (it is non-zero while checks are pending or failing) and read the JSON; a "no required checks reported" error counts as an empty list. `CI_VERDICT` is `NONE` for an empty list, `FAIL` when any bucket is `fail`, `CANCELLED` when none is `pending` and any is `cancel`, `PASS` when none is `pending`, and `TIMEOUT` when time runs out.

On `CI_VERDICT=TIMEOUT` repeat it up to 5 times, then park the Segment. Waiting on required checks only (the default, no `--all`) is deliberate: the merge gate is what the repo enforces, not every check that exists. A failing NON-required check does not block the merge, but MUST be reported as a follow-up rather than silently dropped. `CI_VERDICT=NONE` (the repo requires no checks) is a legitimate pass; record it as `NONE`, never as `PASS`, so the ledger does not claim a gate that never ran. `CI_VERDICT=TIMEOUT` parks the Segment; do not merge a PR whose checks never finished.

**3. CI-fix loop (two budgets, counted separately).** **3 fix attempts** per Segment, matching the existing `--auto: WU fails after 3 retries -> stop` rule, plus **2 reruns** per Segment. A rerun is not a fix and MUST NOT consume the fix budget.

**Transient, not a code failure** (rerun, don't fix) when either holds:
- `CI_VERDICT=CANCELLED`: `concurrency: cancel-in-progress` cancelled the run behind a newer push. `CANCELLED` is not `FAILURE`.
- The failed *step* is `Set up job`, which runs before any repo code, so the failure cannot be yours (typically a rate-limited action download taking the whole matrix red at once).

Read both from `playbook pr checks "$PR" --json` (intent `pr-checks`): each `failed[]` entry carries `run_id`, `step` and `transient`, true for a cancelled job or a failed `Set up job` step. Without the intent, read the failed job's steps with `gh api "repos/{owner}/{repo}/actions/runs/<id>/jobs"`.

Either case: `gh run rerun <id> --failed`, then return to step 2. Max 2 reruns; a third consecutive transient parks the Segment as `CI_UNSTABLE` rather than burning turns.

**Genuine failure:** apply `playbook:systematic-debugging` to find the root cause before touching code, dispatch an `implementer` scoped to the failing check's diagnosis on the Segment branch, then invoke `/playbook:commit` (with the worktree root and branch in its prompt ("Worktree root: <path>, branch: <name>") from a worktree), then return to step 2. **A formatter failure MUST be fixed by running the project's own formatter** (`cargo fmt`, `prettier --write`, `ruff format`, per the Step 3 stack detection), never by hand-editing lines to satisfy it: a hand-edit burns a fix attempt and usually trips the next wrapping rule. Capture `<pre-fix-sha>` before the fix; step 4 needs it.

**4. Re-review the fix, tiered by what the fix actually is.** Re-running the full 5-lens swarm for a one-line format fix is waste; skipping review for a logic change is not. Classify the fix diff mechanically, do not judge:

- **Tier 0, no re-review.** The fix diff is empty (rerun only), or the fix is confined to CI workflow config that touches no shipped code, or the fix is provably tool-generated. Prove the last case, don't assert it: from the pre-fix tree, re-run the formatter/codegen and confirm it reproduces the fix exactly (`git diff --quiet` against the fix's tree). If the tool reproduces it, there is no human-authored change to review.
- **Tier 1, one scoped lens.** Touches production or test code, under 50 changed lines, and no file outside the Segment's `Files`. Dispatch ONE `playbook:reviewer` with the `effort` parameter set to `low` (small diff per "Pick the effort" in `playbook:delegating-subagents`; `low` is under every ceiling, so no resolve step is needed) with focus `correctness` (plus `tests` when the fix touched test files), scoped to `git diff <pre-fix-sha>..HEAD`, the fix diff only, never the whole Segment diff.
- **Tier 2, full Step 9 swarm scoped to this Segment.** Any one of: a file outside the Segment's `Files`; over 50 changed lines; a changed public signature, or a deleted or weakened assertion; the failing check was a security check (secret scanning, dependency/vulnerability scanning) rather than build/lint/test; **or this is the 2nd or 3rd fix attempt**. Repeated failure means the first root-cause diagnosis was wrong, which is exactly when a narrow re-look is worthless.

Record the tier and its finding count in the ledger. Fix Tier 1/2 findings per Step 9's rules, then return to step 2.

**5. Read the PR's real state before attempting anything.**

```bash
gh pr view "$PR" --json state,mergedAt,isDraft,mergeable,mergeStateStatus,reviewDecision,autoMergeRequest,baseRefName \
  -q '{state,mergedAt,isDraft,mergeable,mergeStateStatus,reviewDecision,autoMerge:(.autoMergeRequest!=null),base:.baseRefName}'
```

Route in this order, first match wins:

1. `state == "MERGED"` -> go to step 8.
2. `state == "CLOSED"` and `mergedAt == null` -> the PR was closed out from under this run. `mergeable`/`mergeStateStatus` will sit at `UNKNOWN` forever and look like GitHub computation lag; it is not. Under `land` no sibling PR is ever open, so this should be impossible: report it as an invariant violation and STOP. Do not auto-reopen.
3. `isDraft == true` -> step 1 did not take; re-run `gh pr ready` once, then re-read. Still a draft: park with the `gh` error.
4. `mergeable == "CONFLICTING"` or `mergeStateStatus == "DIRTY"` -> **conflict, a different problem entirely.** This is not a permissions question and `--admin` cannot bypass it. `git fetch origin <default-branch>` and `git rebase origin/<default-branch>`, resolve, `git push` (chained with `&&`, never as a separate statement), return to step 2. If a conflict hunk falls in a file outside the Segment's `Files`, STOP and report: resolving it would silently rewrite work this Segment does not own. Max 2 conflict-resolution attempts, then park as `CONFLICT`.
5. `mergeable == "UNKNOWN"` -> GitHub is still computing. Re-read after 10s, up to 6 times. Route 2 already ruled out the closed-PR case that makes `UNKNOWN` permanent.
6. `mergeStateStatus == "BEHIND"` -> the base moved while CI ran. Rebase onto `origin/<default-branch>` and push; do not merge. Return to step 2. At most 3 rebases per Segment, then park as `BEHIND_LOOP`.
7. `reviewDecision == "CHANGES_REQUESTED"` -> **park unconditionally** (step 7). A human looked and objected; that is categorically different from nobody having looked yet, and it MUST NOT be bypassed even where `--admin` would succeed.
8. Otherwise -> step 6.

**6. Attempt the merge, then re-read.** Intent: pr-merge. If the session's tool map lists it, use that command with `"$PR" --no-wait`; it prints `merge_rc=<code>` and what `gh` said. Otherwise do this:

```bash
gh pr merge "$PR" --auto
```

No strategy flag: where a merge queue is required it sets the strategy, and passing one only adds a stderr warning. If the merge output says auto-merge is not allowed for this repository, fall back to a synchronous `gh pr merge "$PR" --squash` (checks are already green by step 2); that is a repo-settings difference, not a permissions block, and it is discovered by attempting, not by reading `.allow_auto_merge` first.

Then poll to a terminal state, re-reading rather than believing the exit code. Every 15 seconds for up to 540 seconds, read `gh pr view "$PR" --json state,mergeStateStatus,reviewDecision,autoMergeRequest`. `LAND_VERDICT` is `MERGED` when the state is `MERGED`, `REVIEW_GATE` on `REVIEW_REQUIRED`, `CHANGES_REQUESTED` on that review, `RESTATE` when the merge state is `DIRTY` or `BEHIND`, and `TIMEOUT` when time runs out.

`RESTATE` returns to step 5. `TIMEOUT` with everything green falls through to the admin escalation below; a merge queue can hold a PR for its full batching window, so a deadline shorter than that window would escalate needlessly.

**Admin escalation (MUST be gated on green checks).** Only when `LAND_VERDICT` is `REVIEW_GATE` or `TIMEOUT`, `mergeable == "MERGEABLE"`, AND step 2 returned `CI_VERDICT=PASS` or `NONE`:

Use the pr-merge tool with its admin option when the tool map lists it. Otherwise run:

```bash
gh pr merge "$PR" --admin --squash
```

**`--admin` bypasses required status checks, not only reviews.** Never reach it from a failing or unfinished CI state: the 3-attempt fix cap has no admin escape hatch, and "give up on CI, merge it anyway" is never a valid outcome of this loop. Re-read state afterwards, as always. If the output of the admin merge says the PR must be merged using the asynchronous merge REST API, the PR's base is another open PR's branch, which `land` guarantees cannot happen: report the topology invariant as violated and STOP rather than retrying. Any permission refusal goes to step 7 with the message recorded verbatim.

**7. Park (a stop, not a wait).** A human approving and merging cannot happen inside one turn, so parking ends the run, the way **pause** does, but only because genuinely blocked. Record in the ledger:

```
land: PARKED
parked_reason: reviewDecision=REVIEW_REQUIRED; `gh pr merge --admin` refused: <verbatim message>
parked_at: <ISO-8601>
```

Then report and STOP: the PR URL and number; the verbatim blocking field and refusal message; what is already green (required checks passed, Step 9 self-review done, N findings fixed, ledger-confirmed); the remaining Segments by name; and the literal resume command. Do not open the remaining Segments' PRs, do not start their branches, and do not fall back to another boundary without asking.

**Resuming a parked Segment (MUST, and it differs from a normal ledger resume).** A normal resume continues *work*; a parked resume first checks whether the block cleared. Before any dispatch, any branch operation, or any re-review:

```bash
gh pr view "$PR" --json state,mergedAt,reviewDecision,mergeStateStatus \
  -q '[.state,(.mergedAt//"-"),(.reviewDecision//"-"),.mergeStateStatus]|@tsv'
```

- `MERGED` -> go to step 8 and continue to Segment N+1. Do not re-run Steps 7-9 for this Segment.
- `OPEN` with `reviewDecision == "APPROVED"` -> a human approved but did not merge. Re-enter at **step 5 only**. Do not re-review, do not re-push, do not re-run CI unless step 5 routes you there.
- `OPEN` with the block unchanged -> still parked. Report and stop again immediately, having run nothing. A resumed run MUST NOT redo work on a parked PR.
- `CLOSED` with `mergedAt == null` -> ask the user before anything else.

**8. Confirm the content actually landed, then advance (MUST).** A merge command's success message is not evidence, the same way a subagent's `DONE` is not (Verify-by-diff, Step 5):

```bash
git fetch origin "$DEFAULT_BRANCH"
git log --oneline -1 "origin/$DEFAULT_BRANCH"
git diff "origin/$DEFAULT_BRANCH" "$SEGMENT_BRANCH" -- <the Segment's Files>
```

That last diff MUST be empty. A squash-merge collapses the branch's internal shape, so the trees match even though the commits do not. A non-empty diff means either concurrent merges by someone else (re-check scoped to the Segment's own `Files`, which is why the pathspec is there) or content genuinely lost; investigate before continuing, never assume. Record `land: MERGED (<merge-sha>)` in the ledger, then return to Step 5 for Segment N+1, whose branch MUST be created off the `origin/<default-branch>` just fetched.

## Decision rows for `land`

| Situation | Action |
|---|---|
| `land`: Segment's PR just opened | `gh pr ready <branch>` directly; skip `playbook:finish-pull-request` (`/playbook:review --self --deep`) and pass no `--ready`: Steps 8-9 already reviewed this diff (Step 10) |
| `land`: required checks green (or none required) | Attempt `gh pr merge <n> --auto`; re-read `state`/`autoMergeRequest` before believing the exit code, in either direction (Step 10) |
| `land`: `mergeable: CONFLICTING` / `mergeStateStatus: DIRTY` | Conflict, not permissions: rebase on the fetched default branch, push `&&`-chained, re-run CI. Never `--admin`; it cannot bypass a conflict (Step 10) |
| `land`: `mergeStateStatus: BEHIND` | Rebase onto `origin/<default>` and push; don't merge, don't park (Step 10) |
| `land`: a conflict hunk falls outside the Segment's `Files` | STOP and report; resolving it would rewrite work this Segment doesn't own (Step 10) |
| `land`: a required check fails | Root-cause per `playbook:systematic-debugging`, fix, push, re-review per the fix tier; max 3 fix attempts per Segment (Step 10) |
| `land`: check `bucket: cancel`, or the failed step is `Set up job` | Transient, not a code failure: `gh run rerun <id> --failed`, max 2, and it does NOT consume the 3-attempt fix budget (Step 10) |
| `land`: a non-required check fails | Doesn't block the merge; report it as a follow-up, never drop it silently (Step 10) |
| `land`: the CI fix is a formatter failure | Run the project's formatter; never hand-edit lines to satisfy it (Step 10) |
| `land`: the fix diff is reproducible by re-running the formatter/codegen | Tier 0, no re-review; prove it with `git diff --quiet`, don't assert it (Step 10) |
| `land`: fix under 50 lines, inside the Segment's `Files` | Tier 1: one `reviewer` on the fix diff only, focus `correctness` (Step 10) |
| `land`: fix touches logic, a public signature, a security check, or is attempt 2 or 3 | Tier 2: full Step 9 swarm scoped to the Segment before re-attempting the merge (Step 10) |
| `land`: `reviewDecision: REVIEW_REQUIRED` and `--admin` refused | PARK: record `land: PARKED` + verbatim reason, report the PR and what's already green, STOP (Step 10) |
| `land`: `reviewDecision: CHANGES_REQUESTED` | PARK unconditionally; never `--admin` past a human's stated objection, even holding admin rights (Step 10) |
| `land`: required checks not green | Never `--admin`; it bypasses required checks too. Fix or park. The 3-attempt cap has no admin escape hatch (Step 10) |
| `land`: `gh pr merge` fails with "asynchronous merge REST API" | The PR's base is another open PR's branch, which `land` forbids: report the topology invariant as violated and STOP (Step 10) |
| `land`: resume lands on a Segment recorded `PARKED` | Re-check `gh pr view <n> --json state,mergedAt,reviewDecision` FIRST. `MERGED` -> continue; `APPROVED` -> re-enter at the merge attempt only; still blocked -> report and stop, redoing nothing (Step 10) |
| `land`: Segment reads `MERGED` | `git fetch origin <default>`; `git diff origin/<default> <segment-branch> -- <Segment Files>` MUST be empty before Segment N+1's branch is created (Step 10) |
| `land`: any `gh pr merge` invocation | Never pass `--delete-branch`; it deletes the local branch even when the merge fails (Step 10) |
