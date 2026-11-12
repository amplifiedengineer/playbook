---
name: finish-pull-request
description: Self-reviews a freshly opened PR, fixes findings, promotes the draft, merges when allowed. Use right after pr-create returns a PR URL.
user-invocable: false
---

# Finish Pull Request

Run this in the main session or in an agent that holds the `Agent` tool, right after `/playbook:pr-create` returns a PR URL. The review, fix, promote and merge work lives here because it spawns review swarms.

Inputs: the PR number, its branch, whether `--ready` was passed, and an optional worktree path. With a worktree, run git as `git -C <worktree>`, pass the PR number to every tool, and never leave the worktree. If an input is missing, read it with `gh pr view`.

If `playbook pr status` or `playbook pr land` is not in the session tool map, read `references/legacy-land.md` and follow it instead of this file.

## 1. Read the context

Run `playbook pr status <n> --json` (add `--flag auto` or `--flag ask` if the session started with one, and `--dir <worktree>` for a worktree). It always exits 0. Read `ok` first: when false, report `error` and stop. Otherwise it gives:

- `mode`: `ask` or `auto`.
- `settings`: `pr.review.fix` and `pr.merge.enabled`, each with `value` and `error`. An error on either means `false`.
- `review`: `skip`, `quick` or `deep`, already decided from `pr.review.enabled`, `pr.review.type` and the triage. Never lighten it.
- `pr`: `number`, `url`, `isDraft`, `head`, `base`.

## 2. Review

- `quick` or `deep`: run `/playbook:review <n> --self --<review>`, with the `review` value as the flag.
- `skip`: say "auto-review is disabled for this repo, review skipped" in the final report.

In auto mode, skip the self-review below when `/playbook:implement` opened this PR, because its review covered the code. Any other caller gets no review here, and the final report says that no self-review ran. This skill never merges a PR whose code no review covered. Never issue `/clear`.

## 3. Fix

Check the PR text: the body follows the short template, and the title and commits carry the ticket key in the scope. Fix a title or body miss with `gh pr edit`. A commit miss is reworded only in ask mode with `git push --force-with-lease`; in auto mode report it.

Fix the review findings. With `pr.review.fix` true, fix every finding that survived the sweep and commit by invoking `/playbook:commit` (worktree root and branch in its prompt). A push updates the draft.

## 4. Promote and land

If the PR is a draft and (`--ready` was passed or `pr.merge.enabled` is true), run `gh pr ready <n>`. If `pr.merge.enabled` is not true, stop here and leave the PR as it is.

If `pr.merge.enabled` is true, merge only when a review covered the code (no review means no merge: say so and stop). Then run `playbook pr land <n>` as its own Bash call with `timeout` 600000. It waits for checks, arms auto-merge pinned to the head, and confirms the result. Read the `LAND=<verdict>` line (exit 0 merged, 3 a gate, 6 failed or closed, 7 still waiting, 8 not queued):

- `MERGED`: merged.
- `STILL_WAITING`: run it again with `--settle 0`, up to five more times, then report it as still waiting.
- `WAITING_ON_BASE`: the base PR has not merged. Report it and stop.
- `FAIL`, `CLOSED`, `DRAFT`, `HEAD_MOVED`, `REVIEW_GATE`, `CHANGES_REQUESTED`, `RESTATE`, `NOT_QUEUED`: report the verdict and what it printed, and stop.

Never call a PR merged unless the verdict is `MERGED`. Never use `--admin` or `--delete-branch`.
