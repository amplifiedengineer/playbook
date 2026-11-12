---
description: Deprecated, use /playbook:commit. Removed in 1.0.0.
disable-model-invocation: true
allowed-tools: Bash(playbook commit run --json --args-stdin:*), Bash(playbook:*), Bash(git:*), Read, Skill
argument-hint: "[--all|-A] [--update|-u] [--amend|-a] [--no-signoff] [--auto] [--ask]"
---

Print this line first: `/playbook:commit-and-push is deprecated and will be removed in 1.0.0. Use /playbook:commit.`

Then read `${CLAUDE_PLUGIN_ROOT}/commands/commit.md` and follow it with these arguments: `$ARGUMENTS`. Its inline `!` block has not run for you, so run that command with `Bash` yourself.
