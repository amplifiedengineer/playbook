---
description: Use to review a PR or your own branch. Picks a quick or a deep review (or takes --quick or --deep), says why in one line, then runs it.
allowed-tools: Bash, Read, Grep, Glob, Write, Agent, SendMessage, TaskStop, Skill, AskUserQuestion
argument-hint: "[PR number] [--quick] [--deep] [--self] [--no-post] [--all] [--preset <name>] [--this-pr] [--whole-stack] [--auto] [--ask] [--help]"
model: sonnet
effort: medium
---

# Review

One command for every review. For a PR in a stack it settles the scope first (this PR only, or the whole stack). It then decides a quick or a deep review, says why in one line, and runs it from `skills/grounding-review/references/quick-pass.md` or `deep-pass.md`. In ask mode you can switch the depth before it runs.

## Help

If the arguments contain `--help`, print the argument-hint line and these lines, then stop:

```
/playbook:review [PR number] [options]

  --quick      Quick review, no depth question
  --deep       Deep review, no depth question
  --this-pr    Review this PR only, no scope question
  --whole-stack  Review the whole stack, no scope question
  --all, --preset <name>   Deep review options (they imply --deep)
  Every other option goes to the pass that runs: --self, --no-post,
  --auto, --ask.
```

## Step 0: Read the run mode

Do this first, before Step 1. Read the run mode and the effort ceiling for `review` with `$ARGUMENTS`: read `skills/delegating-subagents/references/run-context.md` in this plugin and follow it. It covers the tool, the plain steps without it, the warning, the ceiling, the flag conflict and any failure.

**Routing.** Where you choose what to spawn, route the `review` kind. Intent: route-model. If the session's tool map (printed at session start) lists it, use that command and follow its `action` under the `routing.escalate` gate: on `ask` stop and ask the user, on `downgraded` (`deny`) use the lower tier it names, and in auto mode (`auto`) proceed and log the assumption. If the command fails, carry on. If the tool map lacks it, follow `without-binary.md#route-model` in `skills/delegating-subagents/references/without-binary.md` in this plugin.

- **`ask` mode:** behave exactly as this file describes.
- **`auto` mode:** Step 3 never waits. The pass keeps its own auto rules.

## Step 1: Read the arguments

Parse `$ARGUMENTS`:

- `--quick` and `--deep` together: stop with `--quick and --deep conflict; pass one.`
- `--this-pr` and `--whole-stack` together: stop with `--this-pr and --whole-stack conflict; pass one.`
- `--all` or `--preset <name>` exist only in the deep review. They mean `--deep`. With `--quick`, stop with `--all and --preset belong to the deep review; drop --quick.`
- The target is the first integer or `#N`, else the first branch name, else the current branch. The word after `--preset` is its value, never a target. A branch name must pass `git check-ref-format --branch "<name>"`; pass it to `gh` as one quoted argument. It resolves to its open PR number with `gh pr list --head "<branch>" --json number -q '.[0].number'`; stop with `error: no open PR for branch <name>; create one first or pass a PR number` when there is none. With no target, resolve the current branch with `gh pr view --json number -q .number`; when it has no PR, review the local branch.
- Every other word goes to the pass that runs, unchanged, with `--quick` and `--deep` removed. The pass reads `--self`, `--no-post`, `--all`, `--preset`, `--auto` and `--ask` itself, and `--this-pr` and `--whole-stack` too, which Step 1b also reads.

A depth from `--quick`, `--deep`, `--all` or `--preset` is the user's choice: run Step 1b, then skip to Step 4. The scope is still asked with a depth flag.

## Step 1b: Stack scope

Only for a PR target. A local branch with no PR skips this step.

Intent: pr-stack. If the session's tool map lists it, use that command with the arguments as one quoted string: `"<PR> --json"`. If the tool map lacks it, follow `without-binary.md#pr-stack` in `skills/delegating-subagents/references/without-binary.md` in this plugin. If both fail, or the JSON has `in_stack: false`, say nothing about stacks. The scope is `not a stack`. Go on as Step 1 says: to Step 4 with a depth flag, else to Step 2.

If the given PR's `role` is `context` or `skip`, say so in one line and stop: there is nothing to review on it.

Otherwise print the same two summary lines as section 2 of `stack-review.md`. Line one: `Stack of <n> PRs (<open_count> open, <merged_count> merged), <open_lines> changed lines in the open PRs.` Line two names this PR's neighbours and its own diff base. Then pick the scope:

- `--this-pr` or `--whole-stack` in the arguments: that scope, no question.
- Auto mode: this PR only. Log `Assumed: this PR only (auto mode).`
- Otherwise ask: `1. This PR only (default). 2. Whole stack.` Accept `1`, `2`, or an empty answer for this PR only.

Merged PRs never count toward the recommendation and are never reviewed or commented on. Remember the scope for Steps 2 and 4.

## Step 2: Decide the depth

A depth from a flag in Step 1 stands: say `Depth: <depth> (from a flag)` and go to Step 4. `--all` and `--preset` mean `deep`.

Otherwise read the repo setting `pr.review.type`. Intent: config-get. If the session's tool map lists it, use that command. If the tool map lacks it, follow `without-binary.md#config-get` in `skills/delegating-subagents/references/without-binary.md` in this plugin. `quick` or `deep` is used as is, with the reason `repo setting`. `auto`, the default, goes to the triage below. A read error or any other value is `deep`. A direct review ignores the setting that gates only the finish flow's self-review.

The triage. Intent: pr-review-triage. If the session's tool map lists it, use that command with `--pr <number>`, or `--base <default branch>` for a local branch with no PR, and read its first line, `review=quick` or `review=deep`. The reason is the first `signal:` line's reason, else the first `note:` line, else `the triage chose <depth>`. A non-zero exit, any other output, or no intent in the tool map means `deep`: a failed triage never lightens a review. For the whole-stack scope, add `--scope stack`, so the triage measures every open PR in the stack together.

Print one line, `Depth: <depth> (<reason>)`, and go on.

## Step 3: Confirm in ask mode

Ask once, only in ask mode, in an interactive session, without `--self` and without a depth flag. Add to the `Depth:` line: `Press Enter to run it, or type the other depth.` and wait. An empty answer runs the stated depth. Auto mode, `--self`, a flag or a headless run never waits.

### Auto path

Auto mode never waits and takes the decided depth without asking. The pass runs with `--auto` and keeps its own auto rules. Auto mode does not skip a pass's post question: only `--self` or `--no-post` does, so an unattended caller passes one of them.

## Step 4: Run the review

Run the pass for the depth, with the arguments from Step 1: the PR number when there is one, plus every other word. Add `--this-pr` or `--whole-stack` for the scope from Step 1b, unless the user already passed one, so the pass does not ask the scope again. Read the pass file with `${CLAUDE_PLUGIN_ROOT}/skills/grounding-review/references/` before the name:

- Quick: `quick-pass.md`, `skills/grounding-review/references/quick-pass.md`.
- Deep: `deep-pass.md`, `skills/grounding-review/references/deep-pass.md`.

The run mode and the effort ceiling are the ones Step 0 read, and the pass reads neither again. From here the pass owns everything: the worktree, the reviewers, the sweep and the posting questions. Its report is the result. Add one line at the end naming the depth and scope that ran: `Depth: <depth> (<repo setting | triage | chosen by you | from a flag>), scope: <this PR | whole stack | not a stack>.`
