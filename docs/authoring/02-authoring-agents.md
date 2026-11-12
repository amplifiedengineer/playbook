# Authoring Agents

Agents are the fourth extension point alongside commands, skills, and hooks. An agent is a pinned system prompt plus a fixed model tier and a fixed tool set, auto-discovered from `agents/`, no registry to update elsewhere. This page is the sibling to [Commands, skills, and hooks](01-commands-skills-hooks.md); read that first if you have not, it covers the other three extension points.

## What an agent is, and when to add one

Drop a file at `agents/<name>.md` and it registers itself: no plugin manifest entry, no separate index to update.

Add one when the role needs a structural property an inline prompt cannot express: a restricted tool set, a pinned model tier, or behaviour that must stay identical across every run. Do not add one for a prompt you could inline in a command or a skill instead; a command's body or a skill's `SKILL.md` already gives you that, without the extra file.

## Frontmatter schema

Every agent file opens with five keys:

| Key | Allowed values |
|---|---|
| `name` | Kebab-case, must match the filename (`agents/reviewer.md` declares `name: reviewer`). |
| `description` | When the orchestrator should spawn it, what it returns, and a closing "Not for general-purpose work." line that keeps it out of the generic picker. |
| `tools` | Comma-separated allowlist, the smallest set the role needs. |
| `model` | One of `haiku`, `sonnet`, `opus`. |
| `effort` | One of `low`, `medium`, `high`, `xhigh`, `max`. |

`agents/auditor.md`, `agents/implementer.md`, and `agents/reviewer.md` are real, filled-in examples of this shape.

## Fresh agents, not forks

An `Agent` call without `subagent_type` forks the parent's whole history. Every command, skill and prompt that dispatches a subagent names a `subagent_type` and writes a stand-alone brief (goal, inputs by path, output file, constraints). The template and the per-site classification are in `skills/delegating-subagents/references/briefs.md`. `crates/plugin-checks/tests/fresh_subagent_briefs.rs` pins the wording.

Reviewer and implementer briefs also carry the diff or numbered file excerpts, the scoped test command and a stated limit to read beyond them only when needed, so those agents skip exploratory reads. Every agent's output contract also caps the returned message at about 300 words (status, findings with `file:line`, output file path) so the parent context grows less; the report file is never capped. New agents copy that sentence from an existing one. An agent prompt that tells the agent to read every file it cites defeats that; keep the guardrail worded as in `agents/reviewer.md`.

## The two binding mechanisms

A command binds to an agent one of two ways. Both are real and in use.

**Whole-command fork.** The command's own frontmatter carries `context: fork` and `agent: <name>`, so the entire command body runs inside that agent. Its final message is the only thing the main conversation sees. Pick this when the whole command is the agent's job. Start the body, right under the title, with one line saying the reader is already the executing subagent and must not call the Skill tool again; `crates/plugin-checks/tests/fork_commands_agent.rs` enforces it.

```yaml
context: fork
agent: playbook:auditor
```

That is what `commands/audit.md` adds to its frontmatter to route to `playbook:auditor`. A plugin agent needs its `playbook:` prefix here, as it does for `subagent_type`: a bare `agent: critic` or `agent: auditor` does not resolve, and the fork silently runs as a general purpose agent on the session's own model (measured: Sonnet at $0.117 a commit against $0.007 on a Haiku agent).

**Inline spawn.** The command keeps `Agent` in its `allowed-tools` and spawns the agent directly with `subagent_type: <name>`, often several at once. Pick this when the command orchestrates and the agent does one scoped piece of the work, or when you need a swarm.

```
subagent_type: playbook:reviewer
```

`skills/grounding-review/references/quick-pass.md` spawns one `reviewer` for the whole diff in a single pass. `skills/grounding-review/references/deep-pass.md` spawns several `reviewer` subagents in parallel, one per review lens.

## Model and tool policy

Name a tier alias (`haiku`, `sonnet`, `opus`) and never a model id. `playbook cc` resolves the alias to the 5.5 model and passes the previous generation as a fallback.

Pick the cheapest tier that does the job. `git` runs on `haiku` for mechanical work, `implementer` on `sonnet`, and `reviewer` and `auditor` on `opus`, where a missed finding costs more than the tokens. The full policy is in [Model routing and memory](../internals/02-model-routing-and-memory.md). When you add or change an agent, update the policy table in [`docs/reference/policy-table.md`](../reference/policy-table.md) too, because a test compares it with the file.

Grant the smallest `tools` allowlist the role needs, and say so in the `description`. There are two read-only tiers, and the wording you pick decides which one the lint applies:

- **`Structurally read-only`**: no `Edit`, no `Write`, no `NotebookEdit`, and no `Bash`. The agent reads and greps, nothing else. `reviewer` is the example.
- **`read-only`**: no `Edit`, `Write`, or `NotebookEdit`, but `Bash` is allowed for non-mutating shell like `git log` or `find`. `auditor` is the example: it audits a whole repo, so it needs shell, and it still never writes.

Pick the strict wording whenever the role genuinely needs no shell. `playbook agents check` reads the tier off the `description` and checks the `tools` list against it, so the claim and the allowlist cannot drift apart.

A **write-capable** agent claims neither tier and holds `Edit`, `Write`, and `Bash` because writing code is its job. `implementer` is the example: `/playbook:implement` spawns it for the RED, GREEN, and REFACTOR steps and the `--no-tdd` and `--no-tests` paths. The lint does not forbid write tools, it only forbids them when a read-only claim contradicts them, so a write-capable agent passes as long as its description does not say read-only.

## The guardrail template

Copy `docs/authoring/agent-template.md.tmpl` into `agents/<name>.md` rather than copying an existing agent. The template lives outside `agents/` on purpose so it never registers as a live agent itself, and copying a real one risks carrying over guardrails or scope that don't fit the new role.

Every agent carries these invariants from the template:

- The `## Non-negotiable guardrails` heading itself, so the section is easy to find and to lint for.
- The grounding rule: read a file before citing it, quote exact code with `file:line`, tag anything unverified.
- The output contract: the final message must be the exact deliverable the caller asked for, nothing wrapped around it.
- The no-dash rule: no em dashes or en dashes anywhere the agent writes.

The template also carries a read-only invariant and a zero-attribution invariant. Keep the read-only one only if the agent's `tools` list is actually read-only, and drop it otherwise.

## Parametrize or split

[ADR 0003: Purpose-built subagents over generic fallbacks](../adr/0003-purpose-built-subagents.md) sets the rule: parametrize one agent with a focus parameter when the variants are near-identical, split into separate agents when the discipline genuinely diverges.

`reviewer` is the real parametrized example on disk: it takes a lens (`logic`, `test`, `security`, `data`, `types`, `perf`, or a conditional lens) and the same agent covers every review site across the quick and deep passes of `review`.

ADR 0003 works the same rule through a second example. A `critic` takes a focus parameter (`premise`, `plan`, `decision`, `pre-exec`) and covers several commands, because the role is one adversarial pass and the focus just flips the stance. A `fact-checker` and a `test-reviewer` split apart instead, because one follows the `playbook:grounding-research` discipline and the other follows `playbook:engineering-standards`, checklists too different to share one file. Same rule, opposite answers, because the question is whether the discipline diverges, not whether the wording does.

The trade-off in one line each: parametrizing keeps behaviour consistent and the file count low. Splitting keeps each checklist honest, at the cost of another file to maintain.

## Effort per spawn

An agent file sets the effort the agent runs at by default. When an orchestrator spawns one role at different difficulty (a one-line docs diff and a 200 KB security diff), it keeps the one base agent and passes a different `effort` on the `Agent` call. The `delegating-subagents` skill tells it to, because the tool only takes the parameter when a skill asks.

- There are no variant agents and nothing to register. A new agent file needs no table entry.
- `playbook effort resolve agents <name> --json` reports `effort`, the shipped effort lowered by every ceiling, and `ceiling`. The orchestrator never passes a level above `ceiling`.
- Claude Code 2.1.296 or later applies the parameter to plugin agents. An older version ignores it and the agent runs at its file's effort.
- Spawn the base agent with its prefix, for example `playbook:reviewer`.
- Older releases rendered `<base>-<tier>` variants such as `reviewer-low`. The launcher now removes any leftover `playbook-variants-<pid>` folder from the temp dir, and the retired `agents.variants` setting is ignored.

## The check

`playbook agents check` validates every `agents/*.md` and runs in `rust-ci` (`.github/workflows/rust-ci.yml`) against `plugins/playbook/agents`. The template lives under `docs/authoring/`, so it isn't scanned at all. The validator still skips a file named `_TEMPLATE.md` inside `agents/`, which keeps a stray copy from failing the lint.

It enforces, per file:

- Real frontmatter: an opening and a closing `---`.
- The five required keys: `name`, `description`, `tools`, `model`, `effort`.
- `name` matches the filename.
- `model` is one of `haiku`, `sonnet`, `opus`.
- `effort` is one of `low`, `medium`, `high`, `xhigh`, `max`.
- Every entry in `tools` is a known tool name, regardless of read-only tier.
- If `description` says `structurally read-only`, `tools` holds none of `Edit`, `Write`, `NotebookEdit`, `Bash`. If it says plain `read-only`, `tools` holds none of `Edit`, `Write`, `NotebookEdit`, and `Bash` is allowed.
- A `## Non-negotiable guardrails` heading is present. Under that heading only, a no-dash clause, a grounding clause, and a zero AI attribution clause are each present somewhere.

Run it before committing a new or edited agent:

```bash
playbook agents check plugins/playbook/agents
```

It reports every offending file at once rather than stopping at the first one.

## See also

- [Commands, skills, and hooks](01-commands-skills-hooks.md): the other three extension points, commands, skills, and hooks.
- [ADR 0003: Purpose-built subagents over generic fallbacks](../adr/0003-purpose-built-subagents.md): the decision this page documents.
- [Model routing and memory](../internals/02-model-routing-and-memory.md): the model tier policy in full.
- [Docs index](../README.md)
