# Without the binary

Read this file only when the session's tool map lacks an intent. A recipe names the intent, and each section below is that intent's fallback. The heading is the intent id.

## Contents

`run-mode`, `config-get`, `route-model`, `pr-stack`, `pr-review-triage`, `effort-resolve`, `effort-choose`, `atlassian-check`, `pr-comments`, `pr-reply`, `handoff-template`, `Where we are now`, `Decisions made`, `Next steps`, `Open questions`, `handoff-facts`, `handoff-check`, `handoff-save`, `handoff-show`

## run-mode

If `$ARGUMENTS` has both `--auto` and `--ask`, stop with one line: "--auto and --ask conflict; pass one." If it has `--auto`, the mode is `auto`. In every other case the mode is `ask`, the default.

## config-get

Use the defaults of the key the recipe names. For `fix.maxFiles` the default is 3, and for `fix.maxLines` it is 500.

## route-model

Spawn the agent the recipe names, with the model its agent file pins.

## pr-stack

Detect the stack by hand as `skills/grounding-review/references/stack-review.md` section 1 describes.

## pr-review-triage

Read `skills/grounding-review/references/review-routing.md` in this plugin and apply Part 1 by hand with plain `git` and `gh`. For the whole stack, Part 1 runs on the union of the open PRs' files. The same applies when the command fails, for example an older binary without `--scope` that exits 2.

## effort-resolve

Spawn the base agent with no `effort`, and say in your report that no ceiling was checked.

## effort-choose

Skip the automatic choice and use the effort table in the `playbook:delegating-subagents` skill.

## atlassian-check

The `playbook:atlassian` skill needs `acli` 1.3.39 or newer. Run `acli --version` and compare the `X.Y.Z` in `acli version X.Y.Z-stable` with 1.3.39. No output or no `X.Y.Z` means unreadable; a lower version means too old. Run `command -v twg` to see whether `twg` is present.

## pr-comments

Resolve the PR and fetch its comments by hand. The steps need `gh`.

1. `PR_NUMBER`: the number from the arguments, else `gh pr view --json number -q .number`. If neither gives one, stop with `error: no PR for current branch; pass a PR number`.
2. `OWNER/NAME`: `gh repo view --json nameWithOwner -q .nameWithOwner`. `HEAD_SHA`: `gh pr view "$PR_NUMBER" --json headRefOid -q .headRefOid`. `ME`: `gh api /user -q .login`.
3. Write the review threads to `/tmp/pr-comments-<PR>-threads.json`:

   ```bash
   gh api graphql -F owner="$OWNER" -F name="$NAME" -F pr="$PR_NUMBER" -f query='
     query($owner: String!, $name: String!, $pr: Int!) {
       repository(owner: $owner, name: $name) {
         pullRequest(number: $pr) {
           reviewThreads(first: 100) {
             nodes {
               id isResolved isOutdated path line originalLine
               comments(first: 50) {
                 nodes { id databaseId author { login } body url createdAt }
               }
             }
           }
         }
       }
     }' > "/tmp/pr-comments-$PR_NUMBER-threads.json"
   ```

4. Write the PR-level comments to `/tmp/pr-comments-<PR>-issues.json` with `gh api "/repos/$OWNER/$NAME/issues/$PR_NUMBER/comments" --paginate`.

## pr-reply

Check the file yourself: it must not be empty, hold an em or en dash, or carry AI attribution. Then post it from the file, which needs no shell quoting:

```bash
# Inline review-thread reply (the databaseId of the thread's first comment)
gh api -X POST "/repos/$OWNER/$NAME/pulls/$PR_NUMBER/comments/$DATABASE_ID/replies" -F body=@<that file> -q .html_url
# PR-level comment reply
gh api -X POST "/repos/$OWNER/$NAME/issues/$PR_NUMBER/comments" -F body=@<that file> -q .html_url
```

Shell-quoting gotcha for `gh api -f body=`: apostrophes in double quotes are literal (`"I don't"` is correct, NEVER escape to `"I don\'t"` which posts as `don'''t`). If the body contains complex quoting, write it to a temp file and use `-F body=@/tmp/reply.txt`.

## handoff-template

Use the template below. Keep the sections in this order and skip one with nothing real in it. Write for a reader who has not seen this conversation. Decisions and facts only: no tool narration, chronology, praise or speculation. Use short SHAs, path:line, exact versions and ISO dates. No emojis, no em or en dashes, no preamble, no closing offer. Keep each section under about 10 lines.

```markdown
# Session Handoff - <specific one-line topic>

## Where we are now
- <fact>

## Decisions made
- <chosen path>. Why: <reason>.

## Next steps
1. <action>. Why now: <reason>.

## Open questions
- <gap>. Surfaced by: <decision or task>.
```

At a clean stopping point, "Next steps" is the single line `Clean stop. No pending work.` "Open questions" is the only optional section.

## handoff-facts

In that directory run `git rev-parse --show-toplevel`, `git branch --show-current`, `git log -1 --format='%h %s'` and `git status --short`, and write bullets for the worktree, the branch (or `(detached)`), HEAD, and the working tree (clean, or the count of changed files with up to 20 listed). Add the last lint, typecheck, test and build results you know of.

## handoff-check

Confirm the first three sections are present, in order and not empty, no other `##` heading exists, no `<placeholder>` from the template is left, and no line has an em or en dash. Fix every problem and check again.

## handoff-save

- Folder: `~/.config/playbook/repos/<owner>/<repo>/handoff/`. `<owner>/<repo>` is the output of `git remote get-url origin` with the scheme, any `user@`, the host and a trailing `.git` stripped, so `git@github.com:acme/api.git` and `https://github.com/acme/api.git` both give `acme/api`. With no `origin` remote, use `~/.config/playbook/runtime/handoff/`. Create the folder with `mkdir -p -m 700`.
- File: `<slug>-<epoch>-<pid>.md`. `<slug>` is the absolute directory path with every character that is not a letter or digit turned into `-` (`printf '%s' "$PWD" | tr -c 'A-Za-z0-9' '-'`), `<epoch>` is `date +%s` and `<pid>` is `$$`. Write it through the same quoted heredoc.

## handoff-show

- Folder: `~/.config/playbook/repos/<owner>/<repo>/handoff/`, where `<owner>/<repo>` is `git remote get-url origin` with the scheme, any `user@`, the host and a trailing `.git` stripped. With no `origin` remote, use `~/.config/playbook/runtime/handoff/`.
- Slug: `printf '%s' "$PWD" | tr -c 'A-Za-z0-9' '-'`.
- Run `ls -t <folder> 2>/dev/null` (no shell glob, since zsh aborts on an unmatched glob), keep the names that start with `<slug>-` or `<slug>_`, and read the first file whose name after the slug is `-<epoch>-<pid>.md` or `_<hash>_<epoch>_<pid>.md` (every such file with `--all`); a longer name belongs to a child or sibling directory. Run `playbook handoff show` as well: it prints a handoff that a session start already loaded and gc keeps for its grace period; say it was already loaded. Skip a file older than 14 days.
