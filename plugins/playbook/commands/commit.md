---
description: Use when committing staged changes and pushing. Stages, formats, makes a signed commit, and pushes.
allowed-tools: Bash(playbook commit run --json --args-stdin:*), Bash(playbook:*), Bash(git:*), Read, Skill
argument-hint: "[--all|-A] [--update|-u] [--amend|-a] [--no-signoff] [--auto] [--ask]"
---

# Commit and Push

```!
playbook commit run --json --args-stdin <<'PLAYBOOK_ARGS_END_7Q2XK9'
$ARGUMENTS
PLAYBOOK_ARGS_END_7Q2XK9
```

The block above ran the whole flow: guard, stage, message, commit, rebase and push. Read the JSON report and print the outcome in a few lines. Make no tool call.

- `PUSHED`: print `Pushed <commit> to origin/<branch>` and the `message`. List each `warnings` entry on its own line.
- `NO_STAGED_CHANGES`: say there is nothing staged, and that `-A` stages everything.
- `PARKED`: say the push would need a force and auto mode never forces. The commit is local only; a person decides how to publish it.
- `CONFLICT`: print `error` and tell the user to run the `git rebase` it names by hand.
- `WRONG_DIRECTORY`: print `error`. Nothing was committed.
- `REFUSED` or `FAILED`: print `error` as it is. Nothing was pushed.

With `--json` the process always exits 0, so read `verdict`, never the exit code.

## If the block did not run

- The output is `[shell command execution disabled by policy]`: tell the user to run `brew upgrade playbook` or `playbook upgrade`, then follow `${CLAUDE_PLUGIN_ROOT}/skills/finish-pull-request/references/legacy-commit.md` with normal `Bash` calls.
- The caller wants a drafted message in its own words: use `playbook commit prepare` to read the diff, write the message, and pass it with `-m`.

Other flows: `/playbook:pr-create` pushes the branch and opens its pull request.
