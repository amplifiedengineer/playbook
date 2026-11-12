---
description: Use when a bug is small and well understood ("fix this", "fix #123"). Reproduces with a failing test, makes the smallest fix, opens one PR.
allowed-tools: Bash, Read, Grep, Glob, Edit, Write, Skill, AskUserQuestion
argument-hint: "[#issue | description] [--auto] [--ask]"
model: sonnet
effort: medium
---

# Fix

Fix one small bug end to end: a failing test, the smallest change that makes it pass, one pull request. If the fix turns out not to be small, stop and tell the user to run `/playbook:plan` instead of growing the change.

## Step 0: Read the run mode

Do this first, before Step 1. Read the run mode. Intent: run-mode. If the session's tool map (printed at session start) lists it, use that command with this command's arguments, and follow the `run-context` reference of the `playbook:delegating-subagents` skill for its warning, exit 2 and failures. If the tool map lacks it, follow `without-binary.md#run-mode` in `skills/delegating-subagents/references/without-binary.md` in this plugin.

- **`ask` mode:** behave exactly as this file describes. Every step marked "if mode is ask" stays.
- **`auto` mode:** take the recommended answer at each of those steps and keep going. Record every answer you chose yourself in an Assumptions list and print it in the final output. Never force-push.

## Step 1: Resolve the bug

Strip `--auto` and `--ask` from `$ARGUMENTS`, then read what is left.

- **`#N` or a number:** read the issue with `gh issue view <N> --json number,title,body,comments`.
- **Text:** treat it as the bug report.
- **Empty:** stop with one line: "Pass an issue number or describe the bug."

State the bug in one sentence: what happens, what should happen, and where. If the report is too vague to state that:

- If mode is ask, ask the user for the missing detail, one question at a time.
- If mode is auto, take the most likely reading, add it to the Assumptions list, and go on. If no reading is likely enough to test, escalate with the "no failing test" rule in Step 4.

## Step 2: Reproduce it with a failing test

Write a test that fails because of the bug, and run it. Use the repo's existing test style and put the test next to the tests for the same code. The test must fail for the reason in the report, not for an unrelated reason such as a typo or a missing import.

If a test cannot be written for this bug, go to Step 4.

## Step 3: Find the smallest fix

Load the `playbook:systematic-debugging` and `playbook:engineering-standards` skills and use the first to find the root cause before you change any code. Test each hypothesis. Count them: two tested hypotheses that do not explain the failure is the stop point in Step 4.

Then make the smallest change that fixes the cause. Do not clean up nearby code and do not fix other bugs you notice. Mention them in the final output instead.

Check the size of the change as it grows, using the thresholds from Step 4. Run the scoped test run for the code you touched. The new test must pass and the tests around it must stay green.

- If mode is ask, show the fix and the test result and ask the user to confirm before you commit.
- If mode is auto, commit once the scoped tests pass.

## Step 4: Escalate to /playbook:plan

Read the two thresholds, `fix.maxFiles` and `fix.maxLines`. Intent: config-get. If the session's tool map lists it, use that command for each key. If the tool map lacks it, follow `without-binary.md#config-get` in `skills/delegating-subagents/references/without-binary.md` in this plugin.

Escalate when any one of these is true:

1. More than `fix.maxFiles` files change.
2. More than `fix.maxLines` lines change, excluding the new test.
3. The root cause is still unclear after two tested hypotheses.
4. The fix needs a new dependency, a schema change, a config format change or a public interface change.
5. No failing test is possible for this bug.

Check rules 1 and 2 against `git diff --stat` before you commit. Check rule 3 during Step 3, rule 4 as soon as you see it, and rule 5 in Step 2.

To escalate, stop making changes. Tell the user to run `/playbook:plan` and say which rule fired, with the numbers or the evidence behind it. Pass along the one-sentence bug, what you tested, and what you learned. Leave the working tree as it is, or revert your half-made fix if it would mislead the plan. Do not open a pull request.

- If mode is ask, ask the user whether to start `/playbook:plan` now or stop.
- If mode is auto, invoke `/playbook:plan` with the Skill tool and the same hand-off, and add the rule that fired to the Assumptions list. It stops at the design approval unless you pass `--auto-design`.

## Step 5: Commit

If you are on the default branch, create a branch named `fix/<short-slug>` first. Then invoke the `/playbook:commit` skill with the Skill tool, with the worktree root and branch in its prompt ("Worktree root: <path>, branch: <name>") when you work in a git worktree. `/playbook:commit` drafts the message from the diff; the branch name `fix/<short-slug>` carries the intent.

In auto mode never force-push. If the push is rejected, stop and report it in the final output.

- If mode is ask, `/playbook:commit` can use `--force-with-lease` after its own rebase. Say so in the final report.

## Step 6: Open the pull request

Invoke the `/playbook:pr-create` skill with the Skill tool. When you work in a git worktree, put its absolute root (`git rev-parse --show-toplevel`) and branch (`git branch --show-current`) in the prompt of this and of every `/playbook:commit` call, as "Worktree root: <path>, branch: <name>". Do not run `gh pr create` yourself. Open one pull request for the fix. Link the issue when there is one.

When it returns the PR URL, invoke the `playbook:finish-pull-request` skill with the Skill tool for that PR. It runs the self-review the `pr.review.*` settings select, fixes findings, promotes the draft, and merges only when `pr.merge.enabled` allows it. The pr-create command does not run the review, so this step is needed. In auto mode no self-review runs for a fix PR and it is not merged; say so in the report.

## Step 7: Report

Print a short summary:

- The bug in one sentence and the root cause.
- The test you added and the scoped test result.
- The pull request link.
- In auto mode, the Assumptions list.
