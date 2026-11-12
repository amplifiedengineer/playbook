---
description: Deprecated, use /playbook:pr-create. Removed in 1.0.0.
disable-model-invocation: true
allowed-tools: Bash(playbook pr prepare --json --args-stdin:*), Bash(playbook pr create:*), Bash(playbook:*), Read, Skill
argument-hint: "[--help] [--ready] [--base <branch>] [--ticket <ID>] [--issue <N>] [--dir <path>] [--auto] [--ask]"
---

Print this line first: `/playbook:create-pull-request is deprecated and will be removed in 1.0.0. Use /playbook:pr-create.`

Then read `${CLAUDE_PLUGIN_ROOT}/commands/pr-create.md` and follow it with these arguments: `$ARGUMENTS`. Its inline `!` block has not run for you, so run that command with `Bash` yourself.
