# Reviewing a pull request stack

Read this after Step 1 of the review command has resolved `PR_NUMBER`. A stack is a chain of PRs where each one targets the branch of the PR below it.

## 1. Detect

The two limits are fixed: 6 PRs and 3000 changed lines.

Then find the stack. Intent: pr-stack. If the session's tool map lists it, use that command with the arguments as one quoted string: `"<PR_NUMBER> --json"`. If it fails, or the JSON has `in_stack: false`, the PR is not in a stack: stop reading this file, say nothing about stacks, and review exactly as the command says. Print each entry of `warnings` once.

Otherwise do this, by branch names:

1. `gh repo view --json defaultBranchRef -q .defaultBranchRef.name` gives the default branch. `gh pr view <PR_NUMBER> --json number,title,state,baseRefName,headRefName,headRefOid,additions,deletions,mergedAt` gives the start.
2. Walk down: while the lowest PR's base is not the default branch, run `gh pr list --state all --head <base> --json number,title,state,baseRefName,headRefName,headRefOid,additions,deletions,mergedAt --limit 20` and put the match below. Walk up: run the same with `--base <head of the topmost PR>` and put the match above. Stop a walk when nothing matches, a PR repeats, or the chain holds 25 PRs. When several PRs match, follow the open one with the lowest number (then merged, then closed) and print `<branch> matches <n> PRs; following #<n>` once.
3. Fewer than 2 PRs in the chain: the PR is not in a stack. Stop reading this file and say nothing about stacks.
4. Build what the rest of this file reads: `prs`, bottom first, each with `position` (1 at the bottom), `number`, `state` (`closed` with a `mergedAt` counts as `merged`), `base`, `head`, `head_sha` and `role`; `open_count`; `merged_count`; `open_lines` (additions plus deletions of the open PRs); `ask`, true; and `over_budget`, true when `open_count` is over 6 or `open_lines` over 3000, with one line per breach in `over_budget_reasons`. Keep it as JSON in a `stack.json` file under `/tmp/pr-stack-<PR_NUMBER>/` for section 5.

Read `prs` (bottom first), `open_count`, `merged_count`, `open_lines`, `ask` and `over_budget`. Each PR has a `role`:

- `review`: open. Read in full, and findings may go to it.
- `context`: merged. Read for context only. Never review it, never comment on it.
- `skip`: closed without merging. Not read, not commented on.

If the PR you were given is `context` or `skip`, say so in one line and stop: there is nothing to review on it.

## 2. Choose the scope

If `ask` is false, the PR is not in a stack: there is no question and nothing changes. A PR in a stack always gets the question, even when only one PR in the stack is still open. Merged PRs are never reviewed, but are always read as background context.

`/playbook:review` asks the scope itself and passes `--this-pr` or `--whole-stack`. The two summary lines and the two options below therefore run only when no scope flag is passed.

If `ask` is true and the arguments hold neither flag, say it plainly first, in two lines. Line one: `Stack of <n> PRs (<open_count> open, <merged_count> merged), <open_lines> changed lines in the open PRs.` Line two names this PR's neighbours from `prs` by `position`: `This is #<PR_NUMBER>. Below: #<a> (<head>), #<b>. Above: #<c>, #<d>. Own diff base: <base>.` Write `none` for an empty side. `<base>` is this PR's own `base` (the parent PR's branch, not the default branch). Then pick the scope:

- `--this-pr` in the arguments: this PR only, no question.
- `--whole-stack` in the arguments: the whole stack, no question.
- Auto mode: take this PR only, unless `--whole-stack` was passed. Log it with `Assumed: this PR only (auto mode).`
- Otherwise ask, with these two options:
  1. This PR only. This is the default.
  2. Whole stack.

When the scope is a whole stack and `over_budget` is true, ask once more before starting, naming the reasons from `over_budget_reasons`: continue anyway, this PR only, or stop. The limits are 6 open PRs and 3000 changed lines.

## 2b. This PR only

Review only this PR's own diff against its real base, the parent PR's branch from `prs`, never the default branch. `gh pr diff <PR_NUMBER>` already gives exactly that, so use it and do not run `git diff` against the default branch or read the lower PRs' changes as part of this PR. Tell the reviewer the base branch and that the lower PRs are out of scope. A finding that needs a lower or upper PR to make sense is raised as a `question:` naming that PR, not as a defect in this one. Each PR has its own budget: the review size limit applies to this PR's own lines, never to the stack's total. Its entry in `prs` has `over_line_budget`; when it is true, say so and follow the command's size rules for one PR.

Then read what the open PRs above change. Intent: pr-stack. If the session's tool map lists it, use that command with the arguments as one quoted string: `"<PR_NUMBER> --digest"`. It prints one line per file and upper PR, with the line numbers it changes in this PR's code, cut at 60 lines. Otherwise do this: for each open PR above, read `gh pr diff <n>` and note the old-side line ranges of its hunks per file. Nothing is above when it prints `No open PR above this one.`

Give the digest to the reviewers with the diff. It is the only view of the upper PRs: do not read their full files. Then skip section 3 and go on with Steps 2 to 4 of the command, applying the fixed-above rule of section 4.

## 3. Build the shared context

Whole-stack scope only.

Intent: pr-stack. If the session's tool map (printed at session start) lists it, use that command with the arguments as one quoted string: `"<PR_NUMBER> --context"`. It prints `context_file`, `stack_file` and one `diff_file_<n>` per open PR. Otherwise do this, in `/tmp/pr-stack-<PR_NUMBER>/`: write each open PR's diff with `gh pr diff <n> > pr-<n>.diff` (that path is its `diff_file`), and write `context.md` (the `context_file`) listing every PR bottom first with its number, title, role (REVIEW, CONTEXT for merged, SKIPPED for closed), branch and base, its description cut to a few paragraphs, and for a merged PR its files from `gh pr view <n> --json body,files`. The `stack.json` from section 1 is the `stack_file`.

Read `context_file` once. Pass its path and every `diff_file` path to the reviewer or reviewers, so the whole stack is read with one context. Merged PRs appear in `context_file` as a short summary. If a summary is too thin to follow the code, summarize that PR with the agent routing gives for the `mechanical` kind (see `playbook:delegating-subagents`), not with the review model.

For a whole-stack scope, read the files at the head of the topmost open PR, since that is where all the stack's code exists together. If it is not `PR_NUMBER`, tear down any worktree from Step 1 and run Step 1 again with the topmost open PR's number. Keep every PR's own number for posting.

Tell the reviewers:

- Review only the PRs marked REVIEW. Use the merged ones to understand the code.
- Cite every finding as `file:line` as it appears in that PR's own diff file, and name the PR (`pr`).
- A finding that depends on another PR in the stack says which one.

## 4. Sweep and report

The Verification Sweep checks the anchor: the finding must sit on the PR that owns the line. Show each finding with its PR number.

Fixed-above rule, for every scope with an open PR above the one the finding is on. Before a finding is kept, check whether an open PR above changes or removes the same lines: its old-side hunk range holds the finding's `file:line` (the digest lists them). If it does, drop the finding and list it in the report as `fixed above in #<n>`. Post nothing for it. Do not drop a finding because the PR above only adds lines near it. A finding that stays valid in the final state of the stack is kept on the PR that owns the code. Line numbers after the first PR above can drift by the lines the PRs between them add or remove, so when a range is close but not exact, keep the finding and add `also touched in #<n>` to its body.

## 5. Post

Not in `SELF_MODE` (report only, set only by `--self` or `--no-post`). Otherwise, after the tier question, write the kept findings as JSON (`file`, `line`, `body`, `pr`) and map them:

Intent: pr-stack. If the session's tool map (printed at session start) lists it, use its findings mapping with the one quoted string `"map --stack <stack_file> --findings <findings.json> --out <mapped.json>"`. `mapped.json` has `by_pr`, `unmapped` and `stale`. Otherwise do this: re-read each open PR with `gh pr view <n> --json state,headRefOid,baseRefName`. A PR that is no longer open, has a new head SHA, or has a new base is stale. Then place each finding by the new side of the hunks in each open PR's `diff_file`. A finding that names a PR goes to that PR when its diff holds the `file:line`; otherwise it is unmapped. A finding that names none goes to the topmost open PR whose diff holds the line. A finding with no file or line, on a line no open PR changes, or naming a merged, closed or unknown PR is unmapped. A finding placed on a stale PR is held. The rest form `by_pr`, each comment as `{"path", "line", "side": "RIGHT", "body"}`.

`mapped.json` also has `fixed_above` (`finding`, `pr`, `fixed_by`): the findings dropped by the rule in section 4. Show them as `fixed above in #<fixed_by>` and post none.

Show `unmapped` and `stale` to the user with their reasons and post none of those: a line no open PR changes, a merged or closed PR, a PR pushed to or retargeted since the review started. Offer to re-run detection when anything is `stale`.

For each PR in `by_pr`, create one pending review on that PR: `commit_id` is its `head_sha` and `comments` is its list, with no review `body`. Confirm each PR is still open first. Ask the submit verb once and apply it to every review created in this run.
