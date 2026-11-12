# Models and effort

Each command and agent names its model and effort in its own frontmatter. Spend on judgment, save on routine work.

| Tier | Model | Falls back to | Used for |
|---|---|---|---|
| Opus | `claude-opus-5-5` | `claude-opus-5` | Design, the reviewer, the repo auditor |
| Sonnet | `claude-sonnet-5-5` | `claude-sonnet-5` | Session default, implementer, critic |
| Haiku | `claude-haiku-5-5` | `claude-haiku-4-5` | Mechanical and checking agents: git, collector, cheap-checker, triage, fact-checker, test-reviewer, analyst, writer |

Plugin files name only the alias, and `playbook cc` passes the fallback chain. Run `playbook doctor models` to see the table, your overrides and the chain in force.

- **Effort ceiling.** `playbook effort <auto|low|medium|high|xhigh|max>` sets `maxEffortLevel`. The lower of this and Claude Code's own `maxEffortLevel` wins, and playbook never changes Claude Code's setting. The ceiling does not cap a command, skill or base agent that ships its own effort, or a level you state for the session, since only Claude Code's setting can. See the [config keys page](../reference/config-keys.md#what-the-effort-ceiling-does-and-does-not-cap). Override one component with `effort.agents.<name>`, `effort.commands.<name>` or `effort.skills.<name>`.
- **Effort per spawn.** The orchestrator passes the `effort` parameter on the `Agent` call, lowered by every ceiling you set. The old `agents.variants` setting is retired: `config set` refuses it and a stored value is ignored.
- **Routing.** `playbook route <kind>` says which model, effort and agent a task goes to, and when you must approve first. See [Approval prompts](#approval-prompts).
- **Measure.** `playbook eval bench` compares roles across models and efforts on real cases, with a cost cap.

## Approval prompts

With `routing.escalate` set to `ask`, `playbook route` returns `action: ask` for three cases: the top model (Opus), the high `implement` tier, and a task that has failed twice. The design and review kinds both run on Opus. The `plan`, `adr` and `implement` commands then stop and ask you before they dispatch.

- `/playbook:plan` and `/playbook:adr` ask before the `fact-checker`, `critic` and `test-reviewer` run, because they route the `design` kind.
- `/playbook:implement` routes the `implement` kind. It asks only for the `high` tier or after two failures.

The `routing.escalate` key decides what happens:

| Value | What happens |
|---|---|
| `ask` | The command stops and asks you first. |
| `auto` (default) | The command proceeds on the top model and logs the assumption. `auto.budgetUsd` still caps the spend. |
| `deny` | The command never escalates. It uses the next cheaper route (Sonnet at high effort, or the medium implement tier). |

To turn the prompts on, run this in the repo, or add `--global` for every repo. The default is `auto`.

```bash
playbook config set routing.escalate ask
playbook config set --global routing.escalate ask
```

Your effort ceiling always wins over the route. Auto mode does not change this key.

The per-component table is the [policy table](../reference/policy-table.md). Reasoning is in [Why the pieces are shaped this way](../internals/14-why-the-pieces-are-shaped-this-way.md) and [Model routing](../internals/02-model-routing-and-memory.md).
