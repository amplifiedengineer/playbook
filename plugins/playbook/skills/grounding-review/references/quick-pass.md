# Quick Review

Review a pull request with grounding-review discipline. Output a structured report, then orchestrate posting findings as inline comments on a **pending** GitHub review so the user picks the submit verb.

## Contents

Steps 0 to 6 run in order, and the teardown always runs.

## Argument parsing

Parse `$ARGUMENTS` (strip `--self`, `--no-post`, `--auto` and `--ask` before reading the rest, and `--help` (print the argument-hint line and stop)):

- **Integer or `#N`** (e.g. `4265`, `#4265`) → explicit PR number; resolve `HEAD_SHA` via `gh pr view <PR_NUMBER> --json headRefOid -q .headRefOid`.
- **Branch name** (anything that isn't an integer and isn't empty, and passes `git check-ref-format --branch <arg>`) → resolve to its open PR number via:
  ```bash
  PR_NUMBER=$(gh pr list --head <branch> --json number -q '.[0].number' 2>/dev/null)
  ```
  Error (abort) if no PR found: `error: no open PR for branch <name>; create one first or pass a PR number`.
- **Empty (no PR number or branch left after stripping `--self`)** → resolve the current branch's PR via `gh pr view --json number,headRefOid,author,headRefName`, and post to it exactly as for an explicit number: the current branch's PR is the one the user means.

`--self` and `--no-post` (the same flag) force `SELF_MODE=true` regardless of whether a PR number was also given: review and report, skip Step 4's posting orchestration entirely. Those flags are the only causes: no PR number and self-authorship never skip the posting question. When the PR is yours, GitHub rejects `APPROVE` and `REQUEST_CHANGES`, so Q2 offers `comment` and `skip` only.

## Step 0: Routing

**Routing.** Where you choose what to spawn, route the `review` kind. Intent: route-model. If the session's tool map (printed at session start) lists it, use that command and follow its `action` under the `routing.escalate` gate: on `ask` stop and ask the user, on `downgraded` (`deny`) use the lower tier it names, and in auto mode (`auto`) proceed and log the assumption. If the command fails, carry on. Otherwise do this: spawn the agents this file names, with the model their agent files pin.

- **`ask` mode:** behave exactly as this file describes.
- **`auto` mode:** follow the Posting in auto mode paragraph below: it changes nothing about posting.

### Posting in auto mode

Auto mode does not change posting. Only `--self` or `--no-post` skips the post question, so a run from chat always asks. Pipelines and unattended callers must pass `--self`.

## Self-review awareness

`SELF_REVIEW` (the resolved PR is authored by you, detected by comparing `gh pr view --json author -q .author.login` against `gh api /user -q .login`) is computed for every run and does not set `SELF_MODE`. It only narrows Q2 to `comment` and `skip`, because GitHub blocks approve and request-changes from a PR's own author.

## Worktree vs in-place mode

After resolving `PR_NUMBER` and `HEAD_SHA`, decide how to read the PR's files:

**In-place** (no worktree): use the current working tree when BOTH conditions hold:
1. `git rev-parse HEAD` equals `HEAD_SHA`
2. `git status --porcelain --untracked-files=no` is empty (no staged or unstaged tracked-file changes)

With no argument, the in-place predicate runs the same check. If the current branch's HEAD matches `HEAD_SHA` and the tree is clean, review in place.

**Worktree mode** (all other cases): set up an isolated, detached worktree at `HEAD_SHA`. When Step 1's tool ran, it already did this; if it reports `error: worktree setup failed`, abort. Otherwise set it up yourself. Intent: review-worktree-setup. If the session's tool map (printed at session start) lists it, use that command with `PR_NUMBER` and `HEAD_SHA`, and capture stdout only: it prints the worktree path on stdout and sends git's progress to stderr on purpose, so folding them together corrupts the path. Otherwise do this:

```bash
DIR="$(git rev-parse --path-format=absolute --git-common-dir)/review-worktrees/$PR_NUMBER-${HEAD_SHA:0:7}"
[ ! -e "$DIR" ] || { echo "another review of this PR may be running: $DIR"; exit 1; }
git fetch "$(gh repo view --json url -q .url)" "refs/pull/$PR_NUMBER/head"
git cat-file -e "$HEAD_SHA^{commit}"
mkdir -p "$(dirname "$DIR")" && git worktree add --detach --lock --reason "review pr=$PR_NUMBER ts=$(date +%s)" "$DIR" "$HEAD_SHA" && WT="$DIR"
```

`WT` is set only after the worktree is added, so a failed setup leaves it empty and teardown skips it. If the fetch, the `cat-file` check (the PR was force-pushed: re-run) or the `worktree add` fails, show the error and stop. No fallback, no degraded mode.

When in worktree mode, read and grep all files under `$WT` instead of the local working tree. `WT` is not empty exactly when a worktree was created: use that to decide the teardown step.

## Voice rules (mandatory)

Findings are plain: a label, `file:line`, the exact evidence, a short failure scenario, and one fix in plain words. They are not comment bodies yet. Step 4 drafts the comment for each finding after the sweep, and only when something will be posted. The non-negotiable points for every finding:

- **Conventional Comments on every finding, PLAIN TEXT (no bold).** Read `playbook:grounding-review` reference `references/conventional-comments.md` now and follow it: `<label> [decorations]: <subject>`, for example `issue (blocking): ...` or `nitpick: ...`. NEVER wrap the label in `**...**`. Never write a bare `blocking:`. The label and decorations order the findings and are what posts.
- **One sentence by default, two at most: the problem, then what breaks.** State the defect and its failure, then stop. A second sentence only when the mechanism is genuinely non-obvious. Avoid jargon; plainest words available. Don't teach the author what they already know or recap the diff.
- **Pick one pragmatic fix.** No "X, or Y" options. If both work, prefer the smallest diff and recommend that one.
- **Paraphrase, don't quote, in the sentence itself.** The exact code belongs in the evidence line. Block-quoting the README or source code is almost always longer than restating it in your own words.
- **Don't restate the diff or the anchor.** The author wrote the code; the comment is already on the line. Skip "this function adds X" and skip "at file:line" when the comment IS at that line.
- **Cause or consequence, not both.** State the cause; trust the reader to infer the consequence.
- **Drop intermediate-state padding.** "X is blank" beats "ships a blank X to the CSV".
- **No hedging.** Ban: "may actually be", "I'd lean toward", "that said", "worth noting", "it's worth mentioning", "one could argue".
- **No meta-justification.** "since X is a foot-gun" is reviewer-reasoning, not actionable info.
- **Plain register.** Full short sentences. The same simple English as everywhere else.
- **No em dashes or en dashes.** Use commas, colons, or periods. Hard rule, also enforced in the system prompt.

## Execution rules

1. Run every bash block for real with the `Bash` tool. Don't simulate.
2. The `reviewer` subagent reads every file it cites at the PR's head SHA (grounding-review evidence rule). It can still be wrong, so the orchestrator sweeps every finding in Step 3b before anything is shown or posted, reading the cited lines plus what the trace needs.
3. Combine independent `Bash` calls into a single tool call.
4. Anchor every inline comment to a real `file:line` in the diff. If the line isn't in the diff (e.g. a referenced helper), make it a report-level finding instead.
5. Never auto-submit. Always create the review in `PENDING` state and ask the user how to submit.
6. Never post findings in the review body. The body is for a short human-voiced framing sentence or blank. All findings go inline.

## Step 1: Resolve PR and gather context

Resolve the PR, work out `SELF_MODE`, decide in-place or worktree and set the worktree up, and create the folder of `REVIEW_JSON`. Intent: review-prepare. If the session's tool map (printed at session start) lists it, use that command with the `quick` kind and `"$ARGUMENTS"` as one quoted argument, adding `--auto` when Step 0 read auto mode (it does not skip the post question). It prints these lines: `PR`, `REPO`, `PR_NUMBER`, `HEAD_SHA`, `AUTHOR`, `SELF_REVIEW`, `SELF_MODE`, `MODE`, `WT` (empty when in place) and `REVIEW_JSON`. On a problem it prints `error: ...` and exits 1: stop and show it. Otherwise do this:

1. Resolve `PR_NUMBER` as Argument parsing above says.
2. Run `gh repo view --json nameWithOwner -q .nameWithOwner` for `REPO`, `gh pr view <PR_NUMBER> --json headRefOid,author -q '.headRefOid + " " + .author.login'` for `HEAD_SHA` and `AUTHOR`, and `gh api /user -q .login` for your login. `SELF_REVIEW` is true when `AUTHOR` is your login. `SELF_MODE` is true only when `--self` or `--no-post` was passed.
3. Apply the in-place test from Worktree vs in-place mode. When it fails, set up the worktree as that section says and keep its path in `WT`; otherwise `WT` is empty.
4. Set `REVIEW_JSON=/tmp/$REPO/quick-review-$PR_NUMBER.json` and run `mkdir -p "$(dirname "$REVIEW_JSON")"`.

Then read the PR with the number you resolved:

```bash
gh pr view <PR_NUMBER>
gh pr diff <PR_NUMBER>
```

Capture: `REPO`, `PR_NUMBER`, `HEAD_SHA`, `SELF_REVIEW`, `SELF_MODE`, `REVIEW_JSON`. You'll need them for the API calls in Step 4. `REVIEW_JSON` resolves to `/tmp/<org>/<repo>/quick-review-<number>.json`, and its directory is created here so the Step 4 write succeeds.

## Step 1b: Stack check

Right after Step 1, read `skills/grounding-review/references/stack-review.md` in this plugin and follow it. A PR that is not in a stack gets no question and nothing changes in this file. When the PR is in a stack, that file decides the scope (this PR only, the whole stack, or the whole stack with deep review), builds one shared context, and says how Steps 2 to 4 and the posting step differ for a whole stack. Strip `--this-pr` and `--whole-stack` before reading the other arguments.

## Step 2: Delegate the review pass (isolated reviewer subagent)

If you have no `Agent` tool (a delegated or forked caller), do the review pass yourself in this session, say in the report that no subagent was used, and never report APPROVE.

Reading and analysing the changed files is where main-context rot accumulates, so it runs in an isolated `reviewer` subagent, not the main session. The orchestrator keeps only the returned report, never the file contents.

Before spawning, `SELF_MODE` is already settled (Step 1), so decide here whether anything can be posted. The orchestrator invokes `playbook:grounding-review` now, for the sweep and the report format, and loads no `playbook:writing-style` here.

Spawn ONE `reviewer` subagent at **medium effort** (Opus model, set by the agent file, and the reviewer's own effort). Resolve the reviewer under the user's effort ceiling with a `medium` cap. Intent: effort-resolve. If the session's tool map (printed at session start) lists it, use that command with `agents reviewer --cap medium --json`, spawn `playbook:reviewer` and pass the `effort` it returns as the `effort` parameter of the `Agent` call (`medium` normally, or lower when the user's ceilings are lower). The cap never raises anything and a lower user ceiling still wins. If the command fails, spawn `playbook:reviewer` with no `effort`. Otherwise do this: spawn `playbook:reviewer`, which ships at medium. The orchestrator doesn't set `model` on this call. Because the review is single-pass, its focus is the ENTIRE diff (logic, tests, security, data, types, perf, docs), not one lens.

The subagent prompt MUST include:

- The PR diff and `HEAD_SHA`, plus numbered excerpts of each changed file, the scoped test command and the stated limit to read beyond them only when needed (the "Review brief builder" in `skills/delegating-subagents/references/briefs.md`), and its `Callers` block only when the user asked for code facts.
- How to read files: **worktree mode** → the absolute `$WT` path with "read and grep files under $WT; do not install or build"; **in-place mode** (`WT` empty) → "read the local working tree, which is at HEAD_SHA".
- The full **Voice rules (mandatory)** and **Anti-patterns to refuse** sections from this pass, verbatim, plus the instruction to load `playbook:grounding-review` for the rest of the discipline. The reviewer loads no other skill and writes no comment body.
- The output contract in Step 3: it MUST return exactly that report of plain findings, with no `Post:` block.
- Cite from the excerpts; read a file at `HEAD_SHA` only when they cannot settle a claim. Quote exact evidence; tag anything unconfirmed `[unverified]`.

Spawn it with a stable `name` (e.g. `qr-<PR_NUMBER>`). If stopping it reports `no task found`, the agent was already cleaned up: treat that as a no-op and continue to Step 3. There is no gh-api fallback; if the worktree setup in Step 1 failed, execution has already stopped.

## Step 3: Review report contract

**The `reviewer` agent has only one channel, and it is unreliable. Plan for that** (`playbook:delegating-subagents`). It is structurally read-only (Read, Grep, Glob, Skill), so it cannot write its report to a file: the repo's agent checks forbid `Write` and `Bash` for that tier by design, and granting them would fail CI. Its return value is therefore the only route, and Agent-tool return values have failed outright in measured use.

So:

- **Run your own grounding pass on the diff in parallel**, starting as soon as the subagent is dispatched rather than after it goes quiet. That way a silent reviewer costs latency, not coverage.
- **A silent reviewer is NOT a clean review.** If nothing comes back, say the review did not run. Do not report zero findings, and do not post a pending review implying the diff was reviewed. Those are different outcomes and only one is safe to act on.
- After the idle notification fires, one `SendMessage` asking for partial results is worth a single try; it sometimes works. Do not spend more than one round on it.

The report is rendered in the `playbook:grounding-review` Review Report Format. `/playbook:review --quick` is single-pass, so it OMITS the `### Reviewers` line; every other line matches the canonical shape. Each finding is plain, with no comment body: label, `file:line`, evidence, the failure, and one fix. A finding whose evidence is not on a changed diff line ends with `Report-only: not on a changed line, no inline comment.`

Do not relay the report yet. It goes through Step 3b first.

## Step 3b: Verification sweep (MUST, before relaying or posting)

Run this before the report is shown to the user and before anything is posted. You do it yourself, not the reviewer: the reviewer is read-only and can be wrong. Check every finding against the code at `HEAD_SHA` (under `$WT` in worktree mode):

1. **True.** Re-read the cited lines. Trace the failure scenario where that is cheap; never run PR code. A finding tagged `[unverified]` is either confirmed, dropped, or kept with the `[unverified]` tag stated plainly.
2. **Label.** The label and decorations match the real impact.
3. **Anchor.** The file and line are right. In a stacked or multi-PR review, the finding sits on the PR or branch that owns the code.

Drop findings that do not hold, relabel the mislabelled, and move the misplaced. To keep main context small, read the cited lines plus what the trace needs, never whole files. Then put a `Sweep:` line under the Overview with the counts: kept, dropped, relabelled, moved, and recompute the verdict, confidence and finding order from the swept list. See the Verification Sweep section of `playbook:grounding-review`.

Only now continue. In `SELF_MODE`, or when nothing is postable (zero findings, or only `Report-only` ones), relay the swept report as it is and go to Step 5: nothing will be posted, so no comment is drafted. Otherwise go to Step 4, which relays the swept report together with the drafted comments.

## Step 4: Orchestrate posting

If `SELF_MODE` is true (explicit `--self` or `--no-post`), or nothing is postable (zero findings, or only `Report-only` ones), stop here: the report IS the deliverable, no `playbook:writing-style` load, no question, no GitHub posting. The post question is skipped only by the flag.

### Draft the comments (not in `SELF_MODE`, and only when something is postable)

Now that the sweep is done and something can be posted, hand the drafting to the cheap writer. For every swept finding that is not `Report-only`, dispatch `playbook:writer` (route kind `write`, see "Writing text" in `playbook:delegating-subagents`), all in one message, with the kind `review-comment`, the finding's label, decorations, failure, fix and `file:line` as the only facts, and the absolute path of the `github-review-comments` reference of the `writing-style` skill. Run `playbook prose check` on each returned text with its facts file, send a failure back once, then write that comment yourself: load `playbook:writing-style` first. The writer skips a ```suggestion``` block: add it yourself when the fix is mechanical. A body starts with the bare plain-text label, never bold, and carries no `file:line` prefix: GitHub anchors it. Comment bodies follow `playbook:writing-style`: the same simple, plain, direct English as chat.

Relay the swept report with each draft shown under its finding as a `Draft:` block. Each draft is the final text: label, voice, no dashes, GitHub rules, all applied. Nothing is rewritten between this preview and the post, so what the user reads is exactly what posts. If the user asks for a change to a draft, redraft it with `playbook:writing-style` and show the new preview before posting.

### Ask what to post

**Print every finding first (MUST).** Before asking anything, print all findings in the chat, none left out and none summarized away: a code (F1, F2, and so on), the label, severity, `file:line` and a one-line description each. With zero findings, say so. The question below comes only after this list, and its options refer to findings by code. A question with no list above it is a bug (#593).

**Ask the user, one question at a time** (memory rule):

**Q1**: "Post which findings as a pending review?" Read `skills/grounding-review/references/pick-list.md` in this plugin and follow it: ask with `AskUserQuestion` checkboxes, or its typed list when the tool is not available. Number the findings in the order of the list above, so a number and its code (F1, F2) match. `--self` and `--no-post` still skip the question. If the choice is `none`, stop here.

Build each inline comment from that finding's previewed `Draft:` block verbatim as the comment `body`, anchored to the finding's `file:line`. Post the previewed bodies as they are, never a rewording. Skip any finding marked `Report-only`.

Build the findings file at `$REVIEW_JSON` (`/tmp/<org>/<repo>/quick-review-<number>.json`; the directory was created in Step 1):

```json
{
  "commit_id": "<HEAD_SHA>",
  "comments": [
    {"path": "...", "line": N, "side": "RIGHT", "body": "issue (blocking): ..."},
    {"path": "...", "start_line": N, "start_side": "RIGHT", "line": M, "side": "RIGHT", "body": "..."}
  ]
}
```

**No `body` field on the pending review.** The author chooses their own framing when they submit from the GitHub UI. (If the user explicitly supplies a body, include it.)

Create the pending review. Intent: pr-review-post. If the session's tool map lists it, run that command with `"$PR_NUMBER" --findings "$REVIEW_JSON" --json`; it creates the review, checks it is `PENDING` and returns `id` and `url`. Otherwise run:

```bash
gh api -X POST /repos/$REPO/pulls/$PR_NUMBER/reviews --input "$REVIEW_JSON" --jq '{id, state, html_url}'
```

Confirm `state: PENDING` and capture the review id + html_url. Show the user the link.

**Q2**: "Submit verb? approve / comment / request-changes / skip." When `SELF_REVIEW` is true, offer only `comment` and `skip`, because GitHub rejects approve and request-changes on your own PR.

If `skip`, stop. The pending review stays for manual submit from the UI. Otherwise:

**Q3**: "Add a comment for the review?" (optional free text, blank to skip). Leave `BODY` empty on a blank answer, except: on `approve` with a blank answer, default `BODY` to `LGTM`.

Submit the review. Intent: pr-review-submit. If the session's tool map lists it, run that command with `"$PR_NUMBER" "$REVIEW_ID" --event <approve|comment|request-changes> --json`, adding `--body "$BODY"` when `BODY` is not empty (an approve with no body says `LGTM` by itself). Otherwise run:

```bash
if [ -n "$BODY" ]; then
  gh api -X POST /repos/$REPO/pulls/$PR_NUMBER/reviews/$REVIEW_ID/events -f event=<APPROVE|COMMENT|REQUEST_CHANGES> -f body="$BODY"
else
  gh api -X POST /repos/$REPO/pulls/$PR_NUMBER/reviews/$REVIEW_ID/events -f event=<APPROVE|COMMENT|REQUEST_CHANGES>
fi
```

Confirm the returned `state` flipped from `PENDING` to the corresponding terminal state.

## Step 5: Verify and report

Final user-facing message: one sentence per outcome.

- `SELF_MODE`: "Report-only, nothing posted. N findings above."
- "Pending review id `<id>` created, 7 inline comments queued. Submit from the UI when ready."
- OR: "Submitted as `COMMENT` at <timestamp>. Author will get one notification."

## Step 6: Teardown (MUST run, even on failure, abort, or skip)

If `WT` is not empty, always remove the review worktree. Intent: review-worktree-teardown. If the session's tool map (printed at session start) lists it, use that command with `"$WT"`. Otherwise do this:

```bash
git worktree remove --force --force "$WT"
git worktree prune
```

This step is unconditional: run it whether the review completed, failed, was skipped, or was aborted by the user. It is a no-op if the worktree was already cleaned up.

## Anti-patterns to refuse

If you catch yourself doing any of these while drafting findings, stop and rewrite:

1. **Diff restatement.** "This change moves X into Y so that...". Delete the entire setup sentence and lead with the finding.
2. **Hedging stack.** "may actually be" + "I'd lean toward" + "that said" in a single comment is a tell.
3. **Meta-justification.** "since a non-timestamp string in a timestamp column is its own foot-gun". The recommendation is enough; trust the reader.
4. **Bullet-list explanation inside an inline comment.** A comment is the label plus 1 to 2 sentences (`LEN-REVIEW`), with no bullets. If bullets feel necessary, the finding is too big: split or simplify.
5. **Posting findings in the review body** instead of inline.
6. **Auto-submitting** without the two-question orchestration.

## Tradeoffs intentionally accepted

- **Self-review submit is COMMENT-only.** Documented limitation of the GitHub API, not a bug to work around.
- **Once submitted, the review wrapper can't be deleted.** Body can be mutated via `PUT /reviews/{id}` but must remain non-empty for COMMENT/REQUEST_CHANGES state. Prefer leaving in PENDING until the body and findings are settled.
- **Conventional Comments labels and decorations are mandatory even on nits.** They cost a few characters; they buy bot/human triage.
