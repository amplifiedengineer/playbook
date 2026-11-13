# Slash commands

Every `/playbook:*` command in one table. For the binary's own commands, run `playbook commands` or see the [CLI and config reference](cli-and-config.md).

| Command | What it does |
|---|---|
| `/playbook:setup` | Wires the guards, seeds `settings.json`, installs what you choose. Safe to repeat. |
| `/playbook:doctor` | Checks the seven layers and prints a hint for each miss. |
| `/playbook:plan` | Idea to a verified, parallel-safe plan for `/playbook:implement`. |
| `/playbook:implement` | Runs a plan or ADR blueprint with subagents and TDD. `--auto` opens a PR. |
| `/playbook:fix` | One small bug: failing test, smallest fix, one PR. Hands off to `plan` if it grows. |
| `/playbook:adr` | Writes a fact-checked Architecture Decision Record to `docs/adr/`. |
| `/playbook:commit` | Writes the message from the staged diff, commits signed, pushes. |
| `/playbook:pr-create` | Opens a PR with pre-flight checks and the team template. |
| `/playbook:review` | The one review command. Picks a quick or a deep review (or takes `--quick` or `--deep`), says why in one line, then runs it. `--quick` is a one-pass review posted as a pending GitHub review. `--deep` is parallel specialist reviewers, consolidated and fact-checked. |
| `/playbook:pr-comments` | Walks unresolved review threads, fixes or drafts replies. |
| `/playbook:learn` | Reads the repo, PRs and tickets, then writes distilled facts to memory. |
| `/playbook:audit` | Read-only audit with severity-rated findings and a task plan. |
| `/playbook:handoff` | Writes a handoff note (state, decisions, next steps, open questions) and saves it for the next session. Use it before `/clear` or when you stop. |
| `/playbook:handoff load` | Loads the last handoff for this directory. `--all` loads every waiting one. |

A command that was renamed or removed is listed in [Renamed and removed commands](../renamed.md).

Reviews are stack aware. When a PR sits in a stack, `/playbook:review` (through the review it runs) always asks whether to review the whole stack or one PR (`playbook pr stack` finds it, see [ADR-0019](../adr/0019-pr-stack-review.md)).

Skills load on demand from the `playbook` plugin in `plugins/playbook`: review and research discipline, engineering standards, writing style, subagent delegation and debugging. To add your own, see [Commands, skills and hooks](../authoring/01-commands-skills-hooks.md).
