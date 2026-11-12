# ADR-0019: Review a Whole Pull Request Stack With Shared Context

- **Status:** Accepted
- **Date created:** 2026-10-09
- **Date modified:** 2026-10-09

## Context

Issue #596. A stacked pull request series is a chain of PRs where each one targets the branch of the PR below it. Reviewing one PR of a stack alone loses the context of the others: a helper added in PR 1 looks unused in PR 1 and is used in PR 3. Reviewing each PR in a separate run also repeats the same reading and the same tokens.

GitHub now has native stacked PRs. The facts below were checked against real stacked PRs on `github/gh-stack` with `gh api`.

## What GitHub exposes

- **GraphQL (read-only):** `PullRequest.stack` (type `PullRequestStack`) has `number`, `size`, `baseRefName` and `entries`. Each entry (`PullRequestStackEntry`) has a 1-based `position` and a `pullRequest`. Position 1 is the PR closest to the trunk. `PullRequest.stackEntry` gives the position of one PR. Merged PRs stay in `entries`, so a stack with a merged PR at the bottom is still one stack.
- **REST:** the pull request resource has a `stack` object (`id`, `number`, `position`, `size`, `base`). `GET /repos/{o}/{r}/stacks` and `GET /repos/{o}/{r}/stacks/{n}` list and read stacks, with the PRs, their state and `merged_at`.
- **`gh` CLI:** `gh pr view --json stack` fails with "Unknown JSON field". The `gh stack` extension manages stacks but is not needed to read them.
- **Not exposed:** stacks made by Graphite, ghstack, git-town or by hand are plain PRs. Only the branch names link them.

## Decision

### Detection

`playbook pr stack [<pr>]` finds the stack in this order:

1. One GraphQL call for `stack.entries` of the PR. If the PR is in the returned entries and there are two or more, the source is `api`.
2. If the call fails (an older GitHub Enterprise Server without the field), or the stack is null, follow the branch chain: walk down with `gh pr list --state all --head <base branch>` until the base is the default branch or no PR matches, and walk up with `--base <head branch>`. The source is `branch-chain`. A failed API call adds a warning.
3. Fewer than two PRs: source `none`, and the PR behaves exactly as today.

The REST stacks endpoint is not used. The GraphQL call returns the same data in one request with the fields the review needs.

The chain walk is capped at 25 PRs and guards against loops. A branch with several child PRs follows the open one with the lowest number and adds a warning.

### Output

`--json` prints one object: `in_stack`, `source`, `repo`, `current`, `ask`, `open_count`, `merged_count`, `open_lines`, `over_budget`, `over_budget_reasons`, `warnings` and `prs`, bottom first. Each PR has `position`, `number`, `title`, `state` (`open`, `merged`, `closed`), `role`, `draft`, `base`, `head`, `head_sha`, line counts, `author` and `url`.

`role` is the rule the review follows:

- `review`: an open PR. Read in full and eligible for findings.
- `context`: a merged PR. Read for title, description and a diff summary. Never reviewed, never commented on.
- `skip`: a PR closed without merging. Not read, and a warning names it.

`ask` is true whenever the PR belongs to a stack, including a stack with one open PR and merged PRs below it (the user's rule: always raise the question). A PR outside any stack never sees a question.

### Review flow (later stages)

- When `ask` is true, the review command always asks: this PR only, the whole stack (quick review), or the whole stack with deep review. Quick review is the default. Deep review runs only when asked. In auto mode the command takes the recommended answer (quick review of the whole stack unless a flag says otherwise) and logs the assumption.
- The whole-stack review builds one context object: the open PRs' diffs and the merged PRs' summaries. Reviewers share it. Findings are mapped back to the PR whose diff holds the line (`playbook pr stack map`, using the diff hunks) and posted to that PR's pending review. A line that no PR holds goes in the review summary.
- `--this-pr` and `--whole-stack` skip the question.
- `playbook pr stack <pr> --context` writes the shared context and one diff file per open PR. A finding carries `file`, `line`, `body` and the `pr` the reviewer was reading. With `pr`, that PR must hold the line. Without it, the topmost open PR whose diff covers the line wins.
- Summaries of merged PRs that are too thin are written by the agent `playbook route mechanical` names, so a cheap model does that work.

### Token budget

The command prints the PR count and the changed-line total before it starts. Two config keys set the point where it asks again: `review.stackMaxPrs` (default 6, counts open PRs) and `review.stackMaxLines` (default 3000, open PRs only). `--json` reports `over_budget` and the reasons. Merged PRs do not count, because they are read in summary form.

### Failure modes

- **Force-pushed stack:** each PR records `head_sha`. Before posting, the command compares it with the live head. A changed SHA stops the post for that PR and names it.
- **Retargeted mid-review:** `base` is recorded the same way and rechecked before posting. A change re-detects the stack and asks the user to confirm.
- **Closed, unmerged PR:** role `skip`, with a warning. It is never read or commented on.
- **Base branch deleted after a merge:** GitHub retargets the PR above to the trunk. The API source still lists the merged PR, so the context is kept. The branch-chain source cannot see it and falls back to a single PR plus a warning when the chain breaks.

### Parallelism

Per-PR `gh` calls (diffs, summaries) run through `common::par::map`, capped at 4. No new dependency is added.

## Alternatives considered

- **Require the `gh stack` extension.** Rejected. It adds an install step and does not cover Graphite or ghstack stacks.
- **REST stacks endpoint.** Rejected for the extra requests with no extra data.
- **Review each PR alone and merge the findings.** Rejected. It repeats the reading and still misses cross-PR context.
- **Skip the question for a lone open PR with merged PRs below.** Rejected: the user decided that any detected stack always gets the question.

## Consequences

- One new module, `src/pr/stack.rs`, and one subcommand family, `playbook pr stack`. Commands change only by a small addition each.
- Detection has two sources. Both are tested with a fake `gh` and fixtures for no stack, a 2-stack, a 5-stack, a merged middle PR, a closed top PR, a branch chain only, and the API field present.
- The branch-chain source is best effort. A stack whose bottom PR was merged and whose branch was deleted looks like a smaller stack.
- Delivery was in four PRs: detection, `--context` and `map`, the command integration with the config keys, then docs.
- Not proven against a live stack: the whole-stack review flow in the two commands is instructions for the model, covered by the CLI tests for detection, context and mapping but not by an end to end run.

## Note 2026-11-13: new names

The keys `review.stackMaxPrs` and `review.stackMaxLines` are no longer config keys. Their values are fixed at 6 open PRs and 3000 changed lines. A stored value is ignored.
