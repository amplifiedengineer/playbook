---
description: Use when a branch is ready to open as a pull request. Runs pre-flight checks and opens a draft PR with a conventional title.
allowed-tools: Bash(playbook pr prepare --json --args-stdin:*), Bash(playbook pr create:*), Bash(playbook:*), Read, Skill
argument-hint: "[--help] [--ready] [--base <branch>] [--ticket <ID>] [--issue <N>] [--dir <path>] [--auto] [--ask]"
---

# Create Pull Request

```!
playbook pr prepare --json --args-stdin <<'PLAYBOOK_ARGS_END_7Q2XK9'
$ARGUMENTS
PLAYBOOK_ARGS_END_7Q2XK9
```

The block above is the pre-flight report, one JSON object. It exits 0 whatever happened, so read `verdict`, not the exit code. Your job is the judgment: write the title and the body, then make one `playbook pr create` call. A caller that names "Worktree root: <path>, branch: <name>" is handled by the block. `--help` means print this file's `argument-hint` and stop.

## Read the report

- `PR_EXISTS`: say "A PR already exists: <existing_pr>. Use /playbook:pr-comments or /playbook:review." and stop.
- `WRONG_DIRECTORY` or `REFUSED`: print `reason` and stop. Nothing was pushed.
- `READY`: continue. Print `checks` (dirty, tests) as warnings and never pause on them. Print `fetch_warning` when it is set.
- `[shell command execution disabled by policy]` or no JSON at all: tell the user to run `brew upgrade playbook` or `playbook upgrade`, then follow `${CLAUDE_PLUGIN_ROOT}/skills/finish-pull-request/references/legacy-pr.md`.

## Draft the title and body

Use `commit_log`, `diff_stat` and what you know about the change. Read `diff_file` only when you did not write the change. The text follows `rules` in the report (`rules.writing`, `rules.prs`, `rules.github`, `rules.readiness`): plain words, no dashes, no AI attribution.

- Title: `type(scope): summary`, at most 72 characters, imperative, no period. The type comes from what the files are (docs only is `docs`, tests only `test`, CI only `ci`, a dependency bump `build` or `chore`, a restructure `refactor`). A `ticket` is the scope on a stacked PR and on every commit.
- Body, at most `rules.max_body_chars` characters. First line: the `ticket` key when there is one. Then `## Summary` (the only required section, at most 3 short sentences, ending with `Closes #N` or `Refs #N` when an `issue` is set), then `## Notes for reviewers` and `## Related work` only when they have content.

## Deliver

One call. Pass `--auto` when `mode` is `auto`, `--ticket` and `--issue` when the report has them, and nothing about draft or ready (the `pr.draft` setting decides):

```bash
playbook pr create --json --dir "<root>" --branch "<branch>" --title "<title>" --body-file - --base "<base>" <<'PRBODY_EOF'
<the body>
PRBODY_EOF
```

- `verdict` `DELIVERED`: print the `url`, the title, the base and whether it is a draft. List `warnings`.
- `REFUSED`: nothing was pushed. `problems` lists every rule that failed, each with a `rule`, a `where` and a `message`. When every `rule` is a text rule (`title-length`, `title-format`, `dash`, `attribution`, `prose`, `template`, `body-length`, `ticket-line`, `ticket-scope`, `issue`), fix all of them and call once more. A second refusal stops: print the list. For any other rule (`staged`, `merge-commit`, `commit-type`, a parked branch, a diverged remote) the text is not the cause: print the problems as they are and stop.
- Any other verdict (`FAILED`, `CONFLICT`, `NOTHING_TO_DO`): print `error` and stop. A run that failed after the push can be repeated, it reuses the pushed branch.

Unless the caller said a review already covered the code, end with "Next: invoke playbook:finish-pull-request for this PR.", adding `--ready` when it was passed. `/playbook:implement` is such a caller. In auto mode that skill skips the self-review and says so.
