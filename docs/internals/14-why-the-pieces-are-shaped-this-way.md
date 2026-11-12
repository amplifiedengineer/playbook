# Why the Pieces Are Shaped This Way

This page gives the reasoning behind playbook's building blocks: commands, skills, agents, hooks, models and effort levels. The other docs say what each piece does. This one says why, so you can predict where a new piece belongs and find the setting you want to change.

## The five building blocks

| Block | Lives in | Loads when | Use it for |
|---|---|---|---|
| Command | `commands/*.md` | You type `/playbook:<name>`, or ask in plain words | A multi-step procedure with a clear start and end |
| Skill | `skills/<name>/SKILL.md` | A command, an agent or Claude decides it is relevant | Knowledge or discipline that many procedures share |
| Agent | `agents/*.md` | A command or the orchestrator spawns it | A role that needs its own model, effort or tool limits |
| Hook | `crates/cli/src/hooks/`, wired in `settings.json` | A tool call, a prompt or a session event | A rule that must hold even when nobody remembers it |
| Output style | `output-styles/*.md` | You select it | How replies read, not what work gets done |

The split follows one question: who has to remember to do it?

- A **hook** runs without anyone remembering. Safety rules go here (block a destructive `rm`, strip AI attribution from a commit). A rule that depends on the model choosing to follow it is a request, not a guard.
- A **command** is something you ask for. It holds the order of steps, so the order is the same on every run.
- A **skill** is shared knowledge. A review discipline or the writing rules would otherwise be copied into every command that needs them, and the copies would drift. A skill is written once and loaded by name.
- An **agent** is a role with a fixed shape. Add one only when a plain prompt cannot give you a restricted tool set, a pinned model, or identical behaviour on every run.

## Why skills carry no model and no effort

A skill is knowledge loaded into whoever uses it. If a skill set `effort: low`, it would override the choice of the command or agent that loaded it. A review run at `high` that loads a skill pinned to `low` would think less because of a reference page. So skills never set `model` or `effort`. The caller decides.

## Why agents exist at all

Commands could do everything inline. Agents exist for three reasons:

1. **Structural limits.** The read-only reviewers (`reviewer`, `critic`, `fact-checker`, `test-reviewer`, `analyst`, `cheap-checker`, `review-triage`) have no `Edit`, `Write` or `Bash` tool. The `writer` is read-only too, with `Read` and `Skill` only. You can verify in the file that a reviewer cannot change the code, and `playbook agents check` enforces it in CI. A prompt saying "do not edit" cannot do that.
2. **Cost control.** Mechanical work (git steps, gathering history, applying an approved diff) goes to Haiku at low effort. The expensive model is kept for costly mistakes.
3. **Context control.** An agent's long output stays out of the main conversation. Only its result comes back.

The full list of agents, their tools and how a command binds to one is in [Authoring agents](../authoring/02-authoring-agents.md).

## Why each model is where it is

The rule is: spend on judgment, save on routine.

- **Sonnet** is the session default and covers most coding. `implementer` and `critic` use it because they write code or check plans, and Sonnet at medium effort did as well as high in `playbook eval bench`.
- **Haiku** runs `git`, `collector`, `cheap-checker`, `review-triage`, `fact-checker`, `test-reviewer`, `analyst` and `writer`. The `git`, `collector`, `cheap-checker`, `review-triage` and `writer` agents apply a decision someone else made, or classify with a safe fallback, and a wrong answer is cheap to detect and rerun. The `fact-checker`, `test-reviewer` and `analyst` ran on Haiku after the same bench showed no meaningful loss on real playbook code.
- **Opus** runs design (`/playbook:plan`, `/playbook:adr`), every `reviewer` and the `auditor`. A missed review finding or a weak design costs far more than the extra tokens.

Plugin files name only the alias. `playbook cc` resolves each to the 5.5 model and passes the previous generation as a fallback. The table, the overrides (`models.<alias>`) and the routing details, including the hook that nudges design prompts toward Opus, are in [Model routing and memory](../internals/02-model-routing-and-memory.md).

## Why effort is a second dial

Model sets how capable the work is. Effort sets how long it thinks. They are separate because the right pairing differs by task:

- A fixed procedure (commit, push, open a PR) needs a cheap model at **low** effort. Thinking harder does not help.
- A review needs a strong model. Opus at **medium** found as much as at high on the reviewer bench, so the reviewer ships at medium and a risky diff escalates to `high` through the `effort` parameter of the `Agent` call.
- Design and orchestration commands (`plan`, `adr`, `review`, `pr-comments`, `learn`, `implement`, `fix`) also ship at **medium**. The maintainer chose that on 2026-10-11 using the reviewer, implementer and critic results. No bench covers the design commands themselves. The `auditor` stays at **high** ([ADR-0023](../adr/0023-commands-at-medium.md)).
- A small diff does not need an `xhigh` review, and a risky one might.

The rule behind the current values: **lower effort where the work is mechanical or already decided, and never where a missed finding is costly.** A model that is wrong costs a rerun. A reviewer that stops looking costs a production bug. The table of every value and its reason is in [Effort policy](../internals/02-model-routing-and-memory.md#effort-policy).

### Why an agent's effort is a parameter and not a separate agent

The `Agent` tool takes an `effort` per call when a skill asks for it, and the `delegating-subagents` skill does. A role that needs two efforts (a small diff and a risky one) keeps one definition. Earlier releases rendered a variant of every agent per tier into a throwaway plugin, and a spawn by variant name skipped the ceiling check. One parameter on the base agent is the single place that sets the effort, so `effort.max` and `effort.agents.<name>` cap it before the spawn (see [ADR 0017](../adr/0017-per-component-effort-ceilings.md)).

| Command or key | What it does |
|---|---|
| `playbook effort resolve` | Tells the orchestrator which `effort` to pass, because the user's effort ceilings decide it |
| `playbook effort choose` | With `effort.auto` on, picks the level from the diff |

The orchestrator then picks by diff size and risk, following the `delegating-subagents` skill.

### Who decides the effort

Playbook sets the defaults, in each file. You set the ceiling. Playbook has its own, the config key `effort.max` (same values as Claude Code's `maxEffortLevel`), set with `playbook effort <level>`. Claude Code has one too, `maxEffortLevel`, and Claude Code always has the last word. The lower of the two applies:

| Playbook | Claude Code | Result |
|---|---|---|
| `xhigh` | `max` | `xhigh` applies to what the binary caps (the effort passed to a spawned agent and the launcher's default effort), because `xhigh` is below `max` |
| `xhigh` | `medium` | `medium` applies. Playbook never goes above Claude Code |
| `auto` (the default) | any | Playbook adds no ceiling. Claude Code's applies if it has one |

A ceiling never raises anything. An agent that ships at `low` still runs at `low`.

Playbook only reads Claude Code's `maxEffortLevel` (from the user, project and local settings files) and never writes it. Where Claude Code's exists, Claude Code enforces it and playbook does not fight it. Where it does not, the binary applies playbook's ceiling itself, and the launcher passes no `--settings`.

Playbook's ceiling caps:

- the effort an orchestrator passes when it spawns an agent
- the default effort the launcher adds
- what `playbook effort` and `playbook doctor check` report

It does not cap a command or skill that ships its own `effort`, and a spawn that skips the resolve step is not capped. It does not cap a level you state for the session (`--effort`, `effortLevel`, `CLAUDE_CODE_EFFORT_LEVEL`, `/effort`). Playbook cannot cap those because the only mechanism for that is Claude Code's `maxEffortLevel` setting, and playbook never writes a Claude Code settings file. A session started without the launcher gets no playbook ceiling. Only Claude Code's own `maxEffortLevel` bounds `/effort` and `--effort`.

The launcher moves `CLAUDE_CODE_EFFORT_LEVEL` to `--effort` in the session it starts (one stderr line says so):

- Claude Code lets that variable override a skill's or subagent's own `effort`, while `--effort` does not (checked against the docs and with a `${CLAUDE_EFFORT}` probe skill on 2.1.296). With the variable at `max`, a playbook skill set to `low` still runs at `low` and the main session runs at `max`.
- A value that is not a level, such as `auto`, is dropped.
- A `--effort` you pass wins over the variable.
- `playbook effort` with no level shows both values and the winner, and `--json` prints the same for scripts.

Two more rules sit on top of the ceiling:

- **Per component.** `effort.agents.<name>`, `effort.commands.<name>` and `effort.skills.<name>` cap one piece. The lowest ceiling wins, and nothing is raised above what the file ships with. See [ADR-0017](../adr/0017-per-component-effort-ceilings.md).
- **Per model.** A component on Haiku never runs above its cap, because measured cost grows faster than pass rate. `playbook doctor models` and `playbook effort list` show the result.

`playbook route <kind>` is the shared rule for which model, effort and agent a task goes to, and when to ask you first (`routing.escalate`). `playbook eval bench` measures a role across models and efforts, so the values come from data.

## Why hooks are in Rust and wired by `setup`

Hooks are the safety layer. They must be fast, must not depend on a script on disk that an update could break, and must fail safe. They are subcommands of one binary (`playbook hook <name>`). `playbook setup` wires them into `settings.json`, and `playbook doctor check` checks they are wired. See [Launcher and hooks](../internals/01-launcher-and-hooks.md).

## Why the docs live in layers

| Layer | Answers |
|---|---|
| README | What is this, and how do I install and start? |
| Guides | How do I do a task end to end? |
| Reference | Where do I look up a key, a flag or a hook? |
| Authoring | How do I add or change a piece? |
| Internals | How does it work underneath, and why is it built this way? (this page, the system prompt, memory) |
| ADRs | Why did we choose this over the alternatives, and when? |

For the why, start here, follow the links, then check the ADR in `docs/adr/` that names the decision.

## Where to find a setting

| You want to change | Look at |
|---|---|
| Which model a command or agent uses | The `model:` line in its file, then [Model routing](../internals/02-model-routing-and-memory.md) |
| How hard it thinks | The `effort:` line in its file, or ask `playbook effort resolve agents <name>` for the `effort` to pass |
| The most effort anything may use | `playbook effort <level>` |
| How far the PR flow goes | [Config keys](../reference/config-keys.md) |
| Whether playbook asks or decides | `playbook mode`, and the [auto mode guide](../guides/04-auto-mode.md) |
| What a hook blocks | [Launcher and hooks](../internals/01-launcher-and-hooks.md) |
