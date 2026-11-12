# Auto mode

Auto mode lets a command run without asking you questions. It takes the recommended answer at each decision, lists it under Assumptions in the final output, and keeps going. The default is `ask`, where every command asks as usual.

## Turn it on

Playbook picks the first of these it finds:

1. The command flag: `--auto` or `--ask`.
2. The `PLAYBOOK_MODE` environment variable.
3. The `mode` config key.

```bash
playbook mode auto      # on for this repo, until you turn it off
playbook mode ask       # off
playbook mode status    # the mode and where it came from
```

Only you should run these. If auto comes from `PLAYBOOK_MODE`, unset the variable to leave it.

## What changes

- **Plan design.** Plain `--auto` never approves a design. `/playbook:plan --auto` answers only the Work Unit and Segment questions and stops at the design approval. Pass `--auto-design` to approve the design too. Every choice is logged.
- **Land boundary.** `/playbook:implement` never picks the `land` boundary on its own. Pass `--boundary=land`.

## What auto will not do

- `/playbook:setup`, `/playbook:adr` and `/playbook:pr-comments` refuse to run in auto and say why.
- Reviews still ask what to post. `/playbook:review`, `/playbook:review --quick` and `/playbook:review --deep` skip the question only with `--self` or `--no-post`, so pipelines must pass one of them. For a PR in a stack, `/playbook:review` picks this PR only without asking the scope and logs `Assumed: this PR only (auto mode).`; pass `--whole-stack` to review the stack.
- `/playbook:learn` stages its candidates and writes nothing to live memory.
- Auto never force-pushes.
- Auto does not run the self-review on a new pull request.
- `playbook upgrade` refuses unless you pass `--yes`.

## The hooks

Two hooks watch every session whose resolved mode is `auto`.

**Question hook (`auto-guard`).** It denies the question tool, so the model takes the recommended option. In the default permission mode it also advises, once per session, launching with a trusted permission mode. The standing auto-mode rule comes from `session-init` at SessionStart.

**Spend cap hook (`auto-cost`).** The budget applies to each agent, so one chatty subagent no longer stops the others.

- The main session has `auto.budgetUsd` (default 200, in US dollars), counted on its own transcript. Each subagent has `auto.agentBudgetUsd` (default 50), counted on its own transcript. The caps are a guard against a runaway agent, set above what normal sessions spend: see [ADR 0028](../adr/0028-per-agent-auto-budget.md) for the measured basis. At 70 percent of its own cap an agent hears one warning about its own budget.
- At its cap, only that agent is parked. It cannot start new work (edits, builds, merges, releases, new agents), and the others carry on.
- A parked agent can still deliver finished work: the `playbook:pr-create` and `playbook:commit` skills (and `playbook:writing-style` and `playbook:engineering-standards`, which they load), `git add`, `git commit`, `git push` of a named branch, or a bare `git push` when the checked-out branch is not protected and git would push it to an unprotected branch (never forced, never a tag, a refspec must name a local branch, never to `main`, `master`, `trunk` or `develop`), `git status`, `git diff`, `git log`, `git worktree list`, read-only `gh` (`pr view`, `pr list`, `pr checks`, `run view`, `api` with GET), `playbook deliver`, `playbook pr create`, `playbook pr prepare`, `playbook pr rules`, `playbook commit run --auto` (without `--auto` it may force push, so it is denied), `playbook commit prepare`, `playbook config get`, `playbook mode status`, reading files, and writing its own park note. A command chained to anything else is denied, and so is one that uses `$(...)`, `$VAR` or a backtick, an unquoted heredoc that holds one, or a redirect to a file (a `<` or `>` inside quotes is text, so a message like "a -> b" is fine; only the pull request body write to `pr-body.md` is allowed), so give `git commit` a plain message or use `playbook commit run`.
- `auto.sessionCeilingUsd` caps the whole tree. At the ceiling every agent is parked the same way, new agents included, and the closing steps still work. The default, 0, means ten times `auto.budgetUsd`, so $2000 with the default caps. `playbook config set` refuses a set ceiling below either budget, and a budget above a set ceiling. A ceiling stored before this check existed is left as it is, and `playbook config get auto.sessionCeilingUsd` shows it.
- The denial says which cap was hit and gives the exact command to raise it, with double the current cap as the value, for example `playbook config set auto.budgetUsd 400` (`auto.agentBudgetUsd` for a subagent, `auto.sessionCeilingUsd` at the ceiling). Any number of dollars works. It also names `playbook mode ask`. When auto comes from the `PLAYBOOK_MODE` environment variable, `playbook mode ask` cannot override it, so the denial says to unset the variable and restart instead. Raising a cap takes effect on the next tool call.
- The hook knows who is calling from the `agent_id` that Claude Code puts in the hook input of a subagent's tool calls. A call with no `agent_id`, as from a Claude Code version that does not send one, counts as the main session. Then a subagent's own cap cannot be enforced, and the ceiling is the backstop.
- A subagent's cost is its own transcript. When that file holds a model missing from the price table and no status line cost exists, the cost is unreadable.
- If the cost of the calling agent cannot be read, the hook treats it as over the cap. The closing steps and the park note still work, so finished work is delivered.
- The caps are per session, so `/clear` or a resume starts a new count: `session-init` clears the spend state, the notes and the park notes on every start except a compaction.
- While the main session is parked, the memory capture Stop hook stands down, since it could not write the memory store anyway.
- Change them with `playbook config set auto.budgetUsd <usd>`, `auto.agentBudgetUsd` and `auto.sessionCeilingUsd`.
- Before the defaults were raised, both budgets were 5. The `auto-budget-defaults` migration moves a stored budget of exactly 5 to the new default at the next session start or the first run of most playbook commands after the upgrade, or with `playbook migrate`. When you set `auto.sessionCeilingUsd` below the new default, the budget moves up to the ceiling instead. Any other stored value stays as you set it.

While auto is on, the model cannot switch the mode, change the `mode`, `auto.*` or `fix.*` settings, or edit the config files. This covers `auto.agentBudgetUsd` and `auto.sessionCeilingUsd`. Only you can, by typing the command yourself. At a cap it also cannot raise the cap. The one exception is an unreadable cost: then it may run `playbook mode ask` to leave auto.

## Routing approval is a separate setting

Auto mode does not change the model approval prompts. Those come from `routing.escalate`, which is `auto` by default, so they do not appear unless you set `ask` (`playbook config set routing.escalate ask`, add `--global` for every repo). See [Models and effort](14-models-and-effort.md#approval-prompts).

## Flags do not reach hooks

The hooks read only `PLAYBOOK_MODE` and the config, never your command flags. If you run a command with `--auto` while the resolved mode is `ask`, you get auto behavior in the command text only, with no question hook and no spend cap. `playbook mode status --flag auto` prints a warning when the flag and the hooks disagree. To get the hooks, run `playbook mode auto` or set `PLAYBOOK_MODE=auto`. The reverse also holds: `--ask` while the mode is `auto` still has the question tool denied.

## Limits

The cap and the hooks guard against runaway spend and honest mistakes. They are not a security boundary against a model that tries to defeat them. Auto does not answer permission prompts, so run it in a permission mode you trust, or the session can stall.

`PLAYBOOK_HEADLESS` is a separate setting and does not turn auto on. See [Headless mode](../internals/05-headless-mode.md).

## See also

- [Config keys](../reference/config-keys.md): `mode`, `auto.budgetUsd`, `auto.agentBudgetUsd`, `auto.sessionCeilingUsd`.
- [Plan and implement](01-plan-and-implement.md): how `--auto` flows through the commands.
