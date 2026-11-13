---
name: playbook-usage
description: Lists /playbook:* commands and when to use each, and points to the binary command list. Use when asked what the plugin does or which command fits, or when a task might have a playbook command the tool map lacks.
---

# Playbook Usage

`amplifiedengineer/playbook` is a Claude Code plugin: slash commands under `/playbook:*` plus supporting skills and hooks. The main flow is a planning pipeline: `/playbook:plan` (or `/playbook:adr` for a hard-to-reverse decision), then `/playbook:implement`, which takes a raw idea through a verified plan to delivered code. `/playbook:plan` runs the moment the intent matches. `/playbook:adr` and `/playbook:implement` only offer in one line and wait for a yes. Each command's own description lists its trigger phrases.

## Planning pipeline

| Command | Use when |
|---|---|
| `/playbook:plan` | An idea needs to become a plan, raw or settled. One interview-driven session that challenges the premise, weighs 2-3 approaches, then produces a verified plan of Work Units grouped into PR-sized Segments. |
| `/playbook:adr` | Choosing between named alternatives, a decision that is expensive to undo, or recording why a choice was made. Saves a fact-checked ADR with an optional blueprint to `docs/adr/`. |
| `/playbook:implement` | An approved plan or blueprint exists. Delegates Work Units to subagents, commits each, delivers PR-sized Segments, ends with a refinement pass and an adversarial review. `--boundary=land` merges each Segment before starting the next. Never designs new scope. |

## Review

| Command | Use when |
|---|---|
| `/playbook:review` | Reviewing a PR or your own branch. Start here. For a PR in a stack it asks the scope first (this PR only or the whole stack), then picks a quick or a deep review for that scope, says why in one line, then runs it. `--quick` is a single pass that asks which findings to post as a pending GitHub review, or only reports with `--self` or `--no-post`. `--deep` is a parallel swarm of specialist reviewers, fact-checked into one pending review, for a substantial, risky or cross-cutting PR. `--this-pr` and `--whole-stack` skip the scope question. |
| `/playbook:pr-comments` | Working through open review feedback on a PR, one comment at a time. |

## Delivery

| Command | Use when |
|---|---|
| `/playbook:commit` | Committing staged changes with a generated, signed message and pushing, instead of hand-running `git commit` and `git push`. |
| `/playbook:pr-create` | Opening a PR with pre-flight checks, a conventional-commit title and the team template, instead of hand-running `gh pr create`. Then run the `playbook:finish-pull-request` skill. |
| `/playbook:fix` | A small, well understood bug. A failing test, the smallest fix, one PR. Escalates to `/playbook:plan` when the fix is not small. |

## Utilities

| Command | Use when |
|---|---|
| `/playbook:audit` | Assessing an unfamiliar or long-lived repo without changing it. Read-only, every claim cited to `file:line`. |
| `/playbook:learn` | Building durable project knowledge in the memory store from git history, PRs, Jira and Confluence. |
| `/playbook:doctor` | Checking the seven playbook layers after install or update, or when hooks seem not to run. |
| `/playbook:handoff` | Saves a handoff before `/clear` or when you stop. `/playbook:handoff load` loads it after `/clear`, when the saved handoff did not load. `/playbook:session-handoff` and `/playbook:session-start` are aliases of the two. |
| `/playbook:setup` | First install, or repairing the plugin's own settings. |

## Delivery pattern

`/playbook:audit` runs in an isolated forked context (`context: fork`). `/playbook:implement` delegates each Work Unit to `implementer` subagents, some in isolated git worktrees. A subagent's return value alone is not a reliable signal of what it did. See `playbook:delegating-subagents` for the file-based handoff discipline.

The memory store, its scopes and its edge types are described in `docs/guides/03-decisions-and-memory.md`.

## Binary commands

Run `playbook commands` to list every playbook command and subcommand (an older binary without it: run `playbook --help`); run `playbook commands <group>` for one group with its flags; never run a command marked user-only (tagged `(user-only)` or a name ending in `!`); instead, ask the user to run it with `!`. Use this before running a playbook command you are not sure about.
