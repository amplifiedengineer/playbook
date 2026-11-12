# Legacy finish flow

Read only when `playbook pr status` or `playbook pr merge` is missing from the session tool map (an older binary). This is the step by step flow that the finish skill ran before those two commands existed.


Run this in the main session or in a delegated agent that holds the `Agent` tool, right after `/playbook:pr-create` returns a PR URL. The self-review, fix, promote and merge work lives here because it spawns review swarms.

Inputs from the pr-create report: the PR number or URL, its branch, and whether `--ready` was passed. If any is missing, read it with `gh pr view`.

An optional worktree path may also be given, for a caller whose working directory is not the PR's checkout. When it is, run every git command as `git -C <worktree>`, run `gh` and any tool from the tool map with the PR number so they do not depend on the current directory, pass the path on to the review commands, and put it with the branch in the prompt of `/playbook:commit` as "Worktree root: <path>, branch: <name>", and never change directory or touch files outside the worktree.

## Run mode

Read the run mode. Intent: mode-status. If the session's tool map (printed at session start) lists it, use that command, with `--json`, adding `--flag auto` or `--flag ask` if the session was started with one; its `mode` key is `ask` or `auto`. Otherwise do this: the mode is `auto` when the caller passed `--auto` or said it runs in auto mode, else `ask`, the default. In auto mode, skip the self-review below when `/playbook:implement` opened this PR, because its review already covered the code. Any other caller gets no review here, and the final report says that no self-review ran. If `--ready` was passed, it still promotes the draft. The `pr.review.fix` and `pr.merge.enabled` settings still apply in auto mode, except that this skill never merges a PR whose code no review covered.

## Reading a setting

Intent: config-get. If the session's tool map lists it, use that command; it prints `<key>: <value> (source: <tier>)`. A config read error is a non-zero exit, or a line without the expected value. Never treat an error as `false` for a review setting: use the review fallback in Step 5.1. For the two opt-in settings (`pr.review.fix`, `pr.merge.enabled`) an error means `false`, because a broken config must never turn on an extra action. Name what the command printed to stderr in the final report either way. Otherwise do this: use the defaults, `pr.review.enabled` `true`, `pr.review.type` `auto`, `pr.review.fix` `false` and `pr.merge.enabled` `false`, unless the user told you a different value in this session.

## Step 5: Self-review before ready

Review comments follow Conventional Comments: the review commands read `playbook:grounding-review` `references/conventional-comments.md`, and any finding you fix from the review is read by its label and decorations.
1. Decide whether a review runs, and which one, before touching the PR further.
   - Read `pr.review.enabled`. If it is `false`, skip the review and say so in the final report ("auto-review is disabled for this repo, review skipped"). If it is `true`, or the key is unset (it defaults to `true`), read `pr.review.type`, which is `auto` (the default), `deep` or `quick`.
   - `auto`: pick the depth. Intent: pr-review-triage. If the session's tool map lists it, use that command with the PR number and read its first line, `review=quick` or `review=deep`. It prints `quick` only when three small-model reads agree. If it exits non-zero or prints anything else, use `deep`: a triage failure never skips or lightens the review. Otherwise do this: use `deep`.
   - `deep`: run `/playbook:review <n> --self --deep` against the PR just created (a draft is a real PR, so this works).
   - `quick`: run `/playbook:review <n> --self --quick`.
   - Config read error on either call: do not block and do not skip. Run `/playbook:review <n> --self --deep`.
   - Never issue `/clear` here: a model cannot, and the reviewers are fresh subagents that never saw the code. A person may clear first; nothing depends on it. If this session has no `Agent` tool, the review command runs its lenses inline and says so in its report; treat that report as the review.
2. After the review, read `pr.review.fix` and `pr.merge.enabled`. Both default to `false`, apply in ask and auto mode alike, and need no question.
3. Check the PR text. The body follows the short template (first line the ticket key when one is known, `## Summary`, optional Notes and Related work, under 1200 characters, `Closes #N` or `Refs #N` for a handled issue). The title and every commit carry the key in the scope, for example `feat(PROJ-123): add retry to the sync job`; a stacked PR must have it everywhere. Fix a miss with `gh pr edit <n> --title` or `--body-file`, or, in ask mode only, reword the commit with an interactive rebase and `git push --force-with-lease`. In auto mode never force: report the commit-scope miss instead of fixing it.
4. Fix the findings. A push updates the draft, no new PR needed.
   - `pr.review.fix` is `false`: fix any findings the review surfaced, as you judge best.
   - `pr.review.fix` is `true`: fix every finding that survived the review's sweep. Commit by invoking `/playbook:commit` (worktree root and branch in its prompt, as above), then re-run the scoped checks for the changed area and fix what they report. If no review ran, there are no findings to fix: say so.
5. If the PR is already ready (the create report said `draft` was false), skip `gh pr ready`. Otherwise, if `--ready` was passed or `pr.merge.enabled` is `true` (a PR cannot merge while it is a draft), run `gh pr ready <branch>` now, after Step 5.1 decided whether a review runs and after fixing any findings, not before.
6. If `pr.merge.enabled` is `true`, merge only when the code was reviewed and every check is green. Take these in order and stop at the first that fails:
   - **Reviewed:** in auto mode, when no review ran (any caller other than `/playbook:implement`), do not merge, even with `pr.merge.enabled` on. The PR is already ready from step 5. Say in the final report that auto-merge was skipped because no review covered the code, and stop.
   - **Stacked PR:** if the PR's base is another open PR's branch rather than the default branch, do not merge it before that base merges. Check with `gh pr view <n> --json baseRefName` and `gh pr list --state open --head <baseRefName> --json number`. Say it is waiting on its base and stop, leaving the PR ready.
   - **Wait for green:** record the head with `gh pr view <n> --json headRefOid -q .headRefOid`. Then wait for the checks: wait 120 seconds, then read `gh pr checks <n> --json name,bucket` every 30 seconds (ignore its exit code, which is non-zero while checks are pending or failing, and treat a "no checks reported" error as an empty list), each read its own Bash call under 10 minutes, for up to about 45 minutes. The verdict is `NONE` when the list is empty, `FAIL` when any bucket is `fail`, `CANCELLED` when none is `pending` and any is `cancel`, `PASS` when none is `pending`, and otherwise keep waiting; `TIMEOUT` when time runs out. Read the verdict:
     - `PASS`: go on, once the head still equals the recorded one. A changed head means do not merge: report it and stop.
     - `NONE`: the PR has no checks. Go on and say so in the final report.
     - `FAIL` or `CANCELLED`: do not merge. Report the printed counts, name the failing checks with `gh pr checks <n>`, and stop.
     - `TIMEOUT` after the last run: report that checks were unfinished and stop without merging.
   - **Merge:** arm auto-merge pinned to the recorded head. Intent: pr-merge. If the session's tool map lists it, use that command with the PR number, `--no-wait` and `--match-head <recorded head>`; it prints `merge_rc=<code>` and what `gh` said. Otherwise do this: run `gh pr merge <n> --auto --match-head-commit <recorded headRefOid>` and read its exit code and output. Either way it arms auto-merge with no method flag, so a merge queue picks how it lands, and `gh` refuses if the head moved. If the text says a merge method is required, read `gh repo view --json squashMergeAllowed,mergeCommitAllowed,rebaseMergeAllowed` and run `gh pr merge <n> --auto --match-head-commit <recorded headRefOid>` with the one allowed method flag (`--squash`, `--merge` or `--rebase`), preferring squash. On any other refusal, report the message and stop. Never `--admin`, never `--delete-branch`, never a method flag to get around any other refusal.
   - **Confirm:** do not trust the exit code. Wait for the PR to land. Every 15 seconds for up to 540 seconds, read `gh pr view <n> --json state,mergeStateStatus,reviewDecision,autoMergeRequest` and form the same line (`armed` when `autoMergeRequest` is not null, else `-`). The verdict is `MERGED` when the state is `MERGED`, `REVIEW_GATE` on `REVIEW_REQUIRED`, `CHANGES_REQUESTED` on that review, `RESTATE` when the merge state is `DIRTY` or `BEHIND`, and `TIMEOUT` when time runs out. Report which case it is, and never call a PR merged unless the verdict is `MERGED`:
     - `MERGED`: merged.
     - `TIMEOUT`, last line `OPEN` with `armed`: queued, or auto-merge armed and waiting. Report it as still waiting.
     - `TIMEOUT`, last line `OPEN` with `-` in the last column: the merge was rejected or auto-merge was cleared. Report it and stop.
     - `TIMEOUT`, last line `CLOSED`: closed without merging.
     - `REVIEW_GATE` or `CHANGES_REQUESTED`: a person has to act. Report it and stop.
     - `RESTATE`: the PR is behind or in conflict. Report it and stop.
7. If neither `--ready` nor `pr.merge.enabled` applies: stop after step 4. Leave the PR in the state it opened in (a draft by default).

Never skip straight to `gh pr ready` on a fresh draft without running Step 5.1 first, even when it concludes with "review skipped" rather than an actual review.
