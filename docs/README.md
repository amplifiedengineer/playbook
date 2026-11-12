# Playbook docs

A map of the Playbook docs, for people who use Claude Code and want to know where to look. The [README](../README.md) has the short pitch and the install commands.

## Start here

- [Get started](get-started.md): install Playbook and open your first pull request.
- [Concepts](concepts.md): the plugin, the binary, commands, skills, agents, hooks and memory in one page.
- [Recipes](recipes/README.md): short how-tos for the common tasks, each checked in CI.
- [Install](guides/00-install.md): every route, the Homebrew tap, upgrade and uninstall.
- [What setup installs](guides/05-what-setup-installs.md): every change `playbook setup` makes, and how to undo each.

## Features

| Feature | What it does | Read |
|---|---|---|
| Plan to PR | Plan with `/playbook:plan`, build with `/playbook:implement` | [Guide](guides/01-plan-and-implement.md) |
| Review | Commit, review and answer feedback | [Guide](guides/02-review-and-pr-flow.md) |
| Code search | One index per repo, searched with `playbook code` | [Page](code-search.md), [recipe](recipes/search-the-code.md) |
| Document reading | A PDF or Office file as text, for fewer tokens | [Guide](guides/06-reading-documents.md), [recipe](recipes/read-a-document.md) |
| pbtop | The Agent Budget Dashboard | [Page](pbtop.md) |
| Safety guards | The rm guard and the opt-in policy guard | [rm guard](guides/07-rm-guard.md), [policy guard](guides/08-policy-guard.md) |
| Memory | Facts that sessions recall, per repo, org and global | [Guide](guides/15-memory.md) |
| Launcher | `playbook cc`, worktrees and the `pb` alias | [Guide](guides/09-launcher.md) |
| Usage sync | An opt-in preview that shares usage between your own machines with `playbook sync` | [Guide](guides/17-sync.md) |

More guides: [decisions and memory](guides/03-decisions-and-memory.md), [auto mode](guides/04-auto-mode.md), [telemetry](guides/10-telemetry.md), [settings](guides/12-settings.md), [security defaults](guides/13-security-defaults.md), [models and effort](guides/14-models-and-effort.md) and the [watch acceptance run](guides/16-watch-acceptance-run.md).

## Look it up

- [CLI and config reference](reference/cli-and-config.md): every command, flag and config key, generated from the code.
- [Config keys](reference/config-keys.md), [hook reference](reference/hooks.md), [slash commands](reference/commands.md) and [layers](reference/layers.md).
- [Policy table](reference/policy-table.md), [Codex support](reference/codex-support.md), [CLI help text style](reference/cli-help-style.md) and [repository layout](reference/repository-layout.md).

## How it works

[Authoring](authoring/01-commands-skills-hooks.md) shows how to add your own pieces, and [agents](authoring/02-authoring-agents.md) covers the agent schema. The [decision records](adr/README.md) explain the design choices. The internals pages, in order:

1. [Launcher and hooks](internals/01-launcher-and-hooks.md)
2. [Model routing and memory](internals/02-model-routing-and-memory.md)
3. [Worktree engine](internals/03-worktree.md)
4. [Agent Budget Dashboard](internals/04-usage-dashboard.md)
5. [Headless mode](internals/05-headless-mode.md)
6. [Release channels](internals/06-release-channels.md)
7. [Migrations](internals/07-migrations.md)
8. [State store](internals/08-state-store.md)
9. [Sessions](internals/09-sessions.md)
10. [Document conversion benchmark](internals/10-document-conversion-benchmark.md)
11. [Data folder](internals/11-data-folder.md)
12. [The system prompt](internals/12-system-prompt.md)
13. [The memory system](internals/13-memory-system.md)
14. [Why the pieces are shaped this way](internals/14-why-the-pieces-are-shaped-this-way.md)
15. [Codex skills](internals/15-codex-skills.md)
16. [PR-Agent and the review pipeline](internals/16-pr-agent-and-the-review-pipeline.md)
17. [Cleanup, tombstones and gc](internals/17-gc-and-tombstones.md)
18. [CLA and DCO checks](internals/18-cla-and-dco-checks.md)
19. [The pbdoc helper](internals/19-pbdoc-helper.md) and [the SQLite store crate](internals/19-sqlite-store.md)
20. [Code index evaluation](internals/20-code-index-evaluation.md)
