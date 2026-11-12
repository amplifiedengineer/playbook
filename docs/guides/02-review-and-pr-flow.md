# Review and PR Flow

Four commands take a ready branch to a merged PR: `/playbook:commit`, `/playbook:pr-create`, `/playbook:review`, and `/playbook:pr-comments`.

## Commit and push

`/playbook:commit` writes a commit message from the staged diff, commits signed with a `Signed-off-by` trailer (see [PR and commit settings](#pr-and-commit-settings)), rebases if you are behind, then pushes. It runs in a forked `git` subagent, so the diff stays out of your main context. There is no confirmation gate.

```bash
/playbook:commit               # commit staged changes and push
/playbook:commit -A            # git add -A first
/playbook:commit -u            # git add -u first
/playbook:commit -a            # amend the previous commit
/playbook:commit --no-signoff  # no Signed-off-by on this commit
```

With `-A` and no message, the command is one call to `playbook commit run -A`. It stages everything, drafts a conventional message from the staged files with no model, commits, rebases if you are behind and pushes. The type comes from the files: only docs gives `docs`, only tests `test`, only workflow files `ci`, only lock files `chore(deps)`, a new source file `feat`, and any other change `fix`. The scope is the shared top level area, and it is left out when the files span areas. The message is a guide. The PR title and body carry the meaning. Run it yourself too:

```bash
playbook commit run -A              # stage everything, draft the message, commit, push
playbook commit run -A -m "fix: x"  # use your own message
playbook commit run -A --no-signoff # no Signed-off-by on this commit
```

It prints `NO_STAGED_CHANGES` and stops when there is nothing to commit. It refuses the default branch and never forces a push.

Flags combine (`-Au`). After a rebase or amend it pushes with `--force-with-lease`. Hooks run normally: if one fails, fix the issue and commit again.

## Create a pull request

`/playbook:pr-create` runs pre-flight checks, drafts a conventional-commit title and the team template body, then opens the PR. It opens a draft unless `pr.draft` is `false`.

## Review

`/playbook:review` is the one command to use. When the PR sits in a stack it first asks whether to review this PR only or the whole stack. It then picks a quick or a deep review: a flag wins, then the `pr.review.type` setting, then a triage of the diff. It prints the depth and the reason in one line. In ask mode you can type the other depth before it runs.

```bash
/playbook:review                # the PR for the current branch
/playbook:review 4265           # PR #4265
/playbook:review 4265 --deep    # no question, deep review
/playbook:review --quick --self # no question, local quick review
playbook review suggest 4265    # the recommendation alone, with its reasons
playbook review suggest 4265 --scope stack   # measure every open PR in the stack together
```

The triage is `playbook pr review-triage`. It says quick only when three small model reads agree the change cannot break behavior, and deep otherwise, so a failed triage never lightens a review. The rule list that `playbook review suggest` uses is in the [review routing reference](../../plugins/playbook/skills/grounding-review/references/review-routing.md), and `review.suggest.*` config keys move its cuts, see [config keys](../reference/config-keys.md). In auto mode the command never waits.

With `--scope stack` the files of every open PR in the stack are measured together, so a stack of small PRs can be recommended deep when the whole is large. Merged PRs never count and are never reviewed or commented on, and closed PRs are skipped. A stack with more than 6 open PRs is answered deep without reading their files.

`/playbook:review --quick` and `/playbook:review --deep` run the two passes directly. The two sections below describe them.

## Quick review

A single-pass review under the `grounding-review` discipline, posted as a pending GitHub review for you to submit.

```bash
/playbook:review --quick              # the PR for the current branch
/playbook:review --quick 4265         # PR #4265
```

It shows a report with a drafted comment under each finding. Then it asks which to post with a real checkbox picker. The first question offers must-fix and questions (the default), all findings, pick individually, or none. Pick individually shows checkboxes grouped by label tier, and an unchecked finding is not posted. Where the picker is not available (auto mode, headless `-p`, Codex), it prints a numbered list instead and you reply with numbers (`1,3,5-7`), `all except 4`, `all`, `none`, or a tier name such as `only blockers`. Auto mode takes the default: the blocking ones, issues and questions. After that it asks which verb to use (`approve`, `comment`, `request-changes` or `skip`). It never submits on its own. A PR you authored gets a local report only.

## Deep review

A swarm of specialist reviewers runs in parallel (logic, test, security, data, types and perf by default). The command consolidates, deduplicates and fact-checks the findings, then posts the way quick review does.

```bash
/playbook:review --deep                     # the current branch's PR, reviewers picked from the diff
/playbook:review --deep 123 --all           # every reviewer
/playbook:review --deep --preset security   # security + data + types + logic
/playbook:review --self --deep              # local report, never posts
```

Presets are `security`, `architecture`, `data` and `docs`. Conditional reviewers (architecture, migration, docs, complexity) switch on from what the diff contains. Full reviewers run on Opus. A lens that triage marks narrow gets a cheap Haiku checker. See [Model routing](../internals/02-model-routing-and-memory.md).

Use quick review for everyday PRs. Use deep review for large, risky or cross-layer changes.

## Review a stack

A stack is a series of PRs where each one targets the branch of the PR below it. Playbook finds GitHub native stacks, and stacks made with Graphite, ghstack, git-town or by hand, by following the branch chain. Design: [ADR-0019](../adr/0019-pr-stack-review.md).

A PR outside a stack is reviewed with no extra question. When the PR sits in a stack, even with a single open PR, `/playbook:review` prints the size and asks the scope first, then decides a depth and passes the scope on, so the pass it runs does not ask again. When you pass `--quick` or `--deep`, it still prints the size and asks the scope:

1. This PR only. This is the default.
2. Whole stack, quick review.
3. Whole stack, deep review.

Pass `--this-pr` or `--whole-stack` to skip the question, to any of the three commands. In auto mode the command takes option 1 and logs the assumption. With option 1 the PR gets its own size budget and a short digest of what the PRs above change, so the review skips code that is rewritten higher up. Either way it first says which PRs are below and above and which branch the PR's own diff is taken against.

```bash
/playbook:review --quick 123 --whole-stack   # quick review of the stack PR #123 belongs to
/playbook:review --deep 123 --this-pr        # only PR #123, even if it is stacked
playbook pr stack 123 --json               # what the commands read: the PRs, their state and size
playbook pr stack 123 --digest             # what the open PRs above #123 change, per file
```

What a whole-stack review does:

- **One shared context.** Open PRs are read in full. Merged PRs are read as a short summary (title, description, file list) so the reviewer understands the code below.
- **Merged PRs are never reviewed or commented on.** A PR closed without merging is skipped.
- **Findings go to the PR that holds the line.** `playbook pr stack map` matches each finding's `file:line` against the diff hunks of the open PRs and creates one pending review per PR. A finding no open PR changes is shown in the report and not posted.
- **Fixed higher up is dropped.** A finding on code that an open PR above changes or removes is not posted. The report lists it as `fixed above in #<n>`. `playbook pr stack map` does the check and returns it as `fixed_above`.
- **Safe against change.** If a PR was pushed to, retargeted or closed after the review started, its findings are held back and listed as stale.
- **Cost guard.** The command asks again before it starts when the open PRs exceed 6 or change more than 3000 lines in total. `playbook pr stack` takes `--max-prs` and `--max-lines` to change them for one run.

## How findings are graded

Both commands follow the `grounding-review` discipline.

**Labels** follow [Conventional Comments](https://conventionalcomments.org/) in plain text: `<label> [decorations]: <subject>`, for example `issue (blocking): ...` or `nitpick: ...`. The nine labels and the decorations are in the grounding-review skill, `references/conventional-comments.md`. Anything marked `(blocking)`, plus `issue`, `chore`, `todo` and `typo`, must be fixed before merge. Each finding is one or two sentences.

Every review ends with a **Verification Summary**: each file, whether it was read, which lines were checked, and its findings. Confidence is HIGH (all verified), MEDIUM (1 or 2 unverified) or LOW (more).

## Address review comments

`/playbook:pr-comments` walks unresolved threads one at a time. It reads the code, proposes a fix or reply, waits for your approval, then applies it.

```bash
/playbook:pr-comments           # current branch's PR
/playbook:pr-comments 123       # PR #123
/playbook:pr-comments --bots    # include bot comments (CodeRabbit, Copilot, etc.)
/playbook:pr-comments --dry-run # preview everything, no edits or posts
/playbook:pr-comments -y        # skip the final commit confirmation
```

It first asks which comments to work on with the same checkbox picker, and falls back to a numbered list with the same reply forms where the picker is not available. A comment you leave out is skipped and named in the final summary. With one comment it does not ask. For each comment you then choose `[F]ix`, `[R]eply`, `[B]oth`, `[S]kip`, `[Q]uit` or `[E]dit-then-fix`. Replies queue until after the commit. At the end it runs `commit -A`, then posts the replies. It never resolves threads. Bot authors are skipped unless you pass `--bots`.

## Auto mode

In `ask` mode, the default, nothing changes. Pass `--auto` or `--ask` to override the mode for one run. See [Auto mode](../guides/04-auto-mode.md).

- `/playbook:commit` commits and pushes without asking. It never force-pushes. A push that would need a force is left on the local branch and reported, so you decide how to publish it.
- `/playbook:review`, `/playbook:review --quick` and `/playbook:review --deep` take the decided depth without asking. A review still asks what to post unless you pass `--self` or `--no-post`, because a posted review speaks as you.
- `/playbook:pr-comments` refuses to run. It replies on GitHub in your name, so each reply needs your approval.

## PR and commit settings

Six keys shape the PR flow and the commit trailer. Read one with `playbook config get <key>`. Set it with `playbook config set [--global|--org] <key> <value>`: no flag writes the repo tier. Repo wins over org, and org over global. All keys: [Config keys](../reference/config-keys.md).

| Key | Default | What it does |
|---|---|---|
| `pr.review.enabled` | `true` | `playbook:finish-pull-request` reviews the PR before it goes ready. |
| `pr.review.type` | `auto` | Which review it runs: `deep`, `quick`, or `auto`. `auto` runs `playbook pr review-triage`, which asks a small model three times and picks `quick` only when all three agree; any disagreement or failure runs `deep`. |
| `pr.review.fix` | `false` | After the self-review, fixes every finding that survived the review's verification, pushes the fixes to the PR branch, and re-runs the scoped checks. |
| `pr.merge.enabled` | `false` | After the review and fixes, marks the PR ready, waits until every check on the PR head is green, runs `gh pr merge <n> --auto`, then re-reads the PR and reports whether it merged, is queued, was rejected, or was closed. |
| `commit.signOff` | `true` | Commits made through `/playbook:commit` carry a `Signed-off-by` trailer. |
| `pr.draft` | `true` | `/playbook:pr-create` opens the PR as a draft. Set it to `false` to open PRs ready for review. |

```bash
playbook config set --global pr.review.fix true
playbook config set --global pr.merge.enabled true
playbook config set --global pr.draft false
```

### How auto-merge behaves

The merge step is cautious on purpose.

- It waits two minutes after marking the PR ready, so checks have registered. A PR with no checks at all merges once that look finds none.
- It polls every check on the PR every 30 seconds, for up to 45 minutes. `pass` and `skipping` are fine. `fail`, `cancel` or a check still pending at the deadline stops it without merging.
- It runs `gh pr merge <n> --auto --match-head-commit <sha>`, so a commit pushed during the wait is never merged unseen. With no method flag, a merge queue picks how the PR lands. If `gh` asks for a method, it retries with the one the repo allows, preferring squash. It never passes `--admin` or `--delete-branch`.
- It then reads the PR and reports one of: merged, queued, rejected, or closed.

A stacked PR whose base is another open PR is not merged before its base. In auto mode, a PR that no review covered is marked ready but never merged, even with `pr.merge.enabled` on. The settings work the same in `ask` and `auto` mode and ask no question, because setting the key is your opt-in.

`commit.signOff` stands down when the message already has the trailer, when you pass `--no-signoff`, or when the repo's `prepare-commit-msg` or `commit-msg` hook writes the trailer itself. A hook that only checks for it does not count. It does not control cryptographic signing: commits are signed whenever your git config has a `user.signingkey` (`--gpg-sign`), using the key and format from that config.

## A typical cycle

```bash
/playbook:commit -A                   # commit and push
/playbook:pr-create                   # open the PR
/playbook:review --self               # self-review; it picks quick or deep
/playbook:pr-comments         # after a reviewer leaves comments
```

## Deliver from any worktree

`playbook deliver` commits, rebases, pushes and opens or updates the PR in one deterministic step, from any worktree, so a subagent can deliver its own work.

```bash
playbook deliver --dir ../.worktrees/repo/feature -A \
  --message "fix(api): retry on timeout" --title "fix(api): retry on timeout" --ready
```

- It refuses the default branch, a protected branch, a detached HEAD, and any title, body or commit message with an em or en dash outside code, AI attribution, a title that is not a conventional commit, or a title over 72 characters.
- With nothing staged and unpushed or rebased commits, it only pushes. A force push uses a lease and happens only after its own rebase.
- `--auto-merge` verifies the merge queued. `--wait` polls until the PR merges, closes or a required check fails, and names the failing checks.
- `--dry-run` prints the plan. `--json` prints the report. Exit codes: 0 delivered, 3 refused, 4 rebase conflict, 5 git or gh failure, 6 closed or checks failed, 7 wait timed out, 8 auto-merge not queued, 9 nothing to do.

## See also

- [Plan and Implement](01-plan-and-implement.md): producing the branch that this flow starts from.
- [Internals: Model Routing and Memory](../internals/02-model-routing-and-memory.md): why the deep review's subagents run on a different model than the orchestrating session.
- [Docs index](../README.md)
