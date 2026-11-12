# Legacy commit steps

## Contents

- Argument flags
- Worktree guard
- Step 0: the run mode
- Steps 1 to 4: stage, message, commit and push

These are the steps `/playbook:commit` followed before it ran one inline `playbook commit all --json` call. The command points here only when shell execution is disabled by policy (the inline block prints `[shell command execution disabled by policy]`). Tell the user once to upgrade (`brew upgrade playbook` or `playbook upgrade`), then follow the steps below with normal `Bash` calls.

# Commit and Push

**You are already the executing subagent for this command: run the steps below directly and never call the Skill tool to start it again.**

Generate a commit message from the staged diff, commit, optionally rebase, and push. One flow.

## Run this now

Execute the steps below immediately, end to end, running every bash block for real with the `Bash` tool (capital B, tool names are case-sensitive). Do **not** narrate a plan, summarize `git status`, offer a numbered menu, or ask "what would you like me to do?" / "proceed? [Y/n]". There is **no confirmation gate**.

The flow is one pass: stage (per flags), draft the commit message, commit, rebase if behind, push. Staging flags (`-A`, `-u`, `-a`) parse from `$ARGUMENTS`; no flags means commit only what is already staged.

This command is built to run in an isolated subagent (`context: fork`) so the diff and drafting stay out of the main context. When it forks, your final message is the only thing the main conversation sees, so end with a concise outcome summary (commit SHA, branch, and the generated message). If you are instead reading this in the main conversation, run it here exactly the same way; do not wait for a fork and do not defer to the user.

## Fast path: stage everything with no message

When `$ARGUMENTS` holds `--all` or `-A` and no message, and holds none of `--update`, `-u`, `--amend`, `-a` or `--ask`, your whole job is one command. Do not read other files, do not run the steps below, and do not verify afterwards.

1. Take the worktree root from "Worktree root: <path>" in the prompt. Run this one `Bash` call, with that root as the working directory (`cd "<root>" &&`, or the current directory when the prompt names none). Add `--auto` when the arguments hold it and `--no-signoff` when they hold it:

```bash
cd "<root>" && playbook commit run -A
```

2. Print its output as your final message and stop. It prints the branch, the new short SHA and the message, or `NO_STAGED_CHANGES`, or the error. It refuses the default branch, never forces a push and exits 1 on any failure.

If `playbook commit run -A` is not found, run `playbook --version`, tell the user to run `brew upgrade playbook` (Homebrew) or `playbook upgrade`, and stop.

With a message, or with any other flag, follow the steps below.

## Argument flags

Parse these from `$ARGUMENTS`. Each bash block below runs in its **own shell**, so a variable set in one step is NOT visible in a later one. Apply each flag by setting its variable to `true`/`false` at the top of the block that reads it (do not rely on an env var carried over from an earlier step):

- `--all` or `-A` → stage everything first (`git add -A`) in Step 1.
- `--update` or `-u` → stage tracked files only (`git add -u`) in Step 1.
- `--amend` or `-a` → replace the last commit, in both Step 1 and Step 4.

- `--no-signoff` → leave the `Signed-off-by` trailer off this commit, on purpose, in Step 4.

Combined flags are fine: `-Au`, `-a -u`, etc. No flags means commit only what is already staged. There is no confirmation gate; the flow always runs to completion.

## Execution rules

1. Run every bash block in this command for real with the `Bash` tool (capital B, tool names are case-sensitive). Do not simulate output.
2. Use the actual command output to drive the next step.
3. Do not assume file contents or git state; check them.
4. Combine independent bash operations into single tool calls.
5. Never run destructive git commands (`reset --hard`, `push --force`, `clean -f`) unless the user explicitly asks.
6. Never skip hooks (`--no-verify`, `--no-gpg-sign`). Never put AI attribution (a `Co-Authored-By` for an AI, a "Generated with" line) in a commit message. When the `commit-message-sanitizer` hook is installed, it also removes such lines before the commit runs; it never blocks.
7. Never amend automatically: only when `AMEND_COMMIT=true`.
8. Pass commit messages on stdin with a heredoc to preserve formatting, never `-m "..."` for multi-line.

## Worktree guard (first)

Before anything else, run the worktree guard in the `playbook:delegating-subagents` skill's `references/worktree-guard.md` (invoke the skill and read the file from the skill base directory it shows). The caller gives "Worktree root: <path>, branch: <name>" in this command's prompt. On a mismatch stop with its `WRONG_DIRECTORY` line and commit nothing.

## Step 0: Read the run mode

Do this right after the guard. Read the run mode. Intent: mode-status. If the session's tool map (printed at session start) lists it, use that command, with `--json`, adding `--flag auto` or `--flag ask` when the arguments hold `--auto` or `--ask` (check for both first and stop on a conflict). It reports `mode` (`ask` or `auto`), `source`, `hook_mode` and `warning`; if `warning` is not empty, print it once. If it fails, do not stop and do not retry: run in ask mode and print one line. Otherwise do this: if the arguments hold both `--auto` and `--ask`, stop with one line: "--auto and --ask conflict; pass one." `--auto` means auto mode. Anything else means ask mode, the default.

- **`ask` mode:** behave exactly as this file describes.
- **`auto` mode:** follow the Auto path below, and treat Step 4 as auto.

### Auto path

In auto mode this command never force-pushes and never uses a forced lease. When `git ls-remote --heads origin <branch>` shows the remote branch absent, push plainly: no force is needed. A push that would need a force (after an amend or a rebase in this run) stops and reports; nothing is pushed. The commit stays on the local branch, and a person decides how to publish it.

## Step 1: Stage, format, emit context

Stage, format and gather the diff. Intent: commit-prepare. If the session's tool map lists it, use that command with `-A`, `-u` and `-a` as the flags above ask. It prints `NO_STAGED_CHANGES` when nothing is staged (unless amending), else `BRANCH=`, the changed files, `---DIFF_START---` and the diff. Otherwise do this:

1. Find the default branch: `git symbolic-ref --short refs/remotes/origin/HEAD` without the `origin/` prefix, else `gh repo view --json defaultBranchRef -q .defaultBranchRef.name`, else `main`. If `git branch --show-current` is that branch, `main` or `master`, stop with: "HEAD is on '<branch>', the repo's default/protected branch. This skill never commits there. Create a feature branch first (e.g. git checkout -b <name>) and re-run." This holds under any flag, even `-y`.
2. Stage: `git add -A` for `-A`, `git add -u` for `-u`.
3. Format the staged files (`git diff --staged --name-only --diff-filter=d`, plus `git diff --name-only --diff-filter=d HEAD~1 HEAD` when amending) with the formatter the repo configures, then `git add` them again. Pick the first that has a config file at the repo root: `biome.json` or `biome.jsonc` (`npx biome check --write <files>`), `dprint.json`, `dprint.jsonc` or `.dprint.json` (`dprint fmt <files>`), `.prettierrc`, `.prettierrc.json`, `prettier.config.js` or `prettier.config.mjs` (`npx prettier --write <files>`). A formatter that is missing or fails does not stop the commit.
4. If `git diff --staged --quiet` succeeds and you are not amending, nothing is staged.
5. Read the diff: `git diff --staged --name-status` and `git diff --staged`. When amending, also read `git log -1 --oneline` and `git diff HEAD~1...HEAD`.

If nothing is staged, tell the user "No staged changes. Use `git add` to stage files first." and stop.

## Step 2: Generate the commit message

Load the commit-only writing rules. Do not read the full `playbook:writing-style` skill for the writing rules, a commit message needs about a tenth of it (invoking it only to find the `conventional-commits.md` reference is fine). Intent: pr-rules. If the session's tool map lists it, use that command with its commit-only option; it prints one path, which you read with the Read tool, and on `ERROR:` you invoke the `playbook:writing-style` skill instead. On any `ERROR:` (an old binary says 'skill reference not found' or 'no plugin root'), run `playbook --version` first; if it is older than 0.26.0, tell the user to run `brew upgrade playbook` (Homebrew) or `playbook upgrade`. If the tool map does not list it, do this: read the dash IRON RULE and the `## Banned Words` section of the writing-style skill's `SKILL.md`, and the "Commit messages MUST" rules in its `references/commits-and-prs.md` (both in the base directory of the `playbook:writing-style` skill, shown when you invoke it). If you cannot find those files, invoke the `playbook:writing-style` skill. Also read the `playbook:writing-style` skill's `references/conventional-commits.md` (invoke the skill and read the file from the skill base directory it shows; if you cannot find it, follow the usual types (`feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`)) now, and follow it for the type, scope, breaking-change marker and description. The constraints below are only the parts specific to the commit header and body.

Analyse the staged diff from Step 1 and draft a commit message:

**Header (aim for 50 characters, never over 72, no trailing period):**

- Always use conventional commit: `type(scope): short imperative summary`.
  - Types: `feat`, `fix`, `style`, `perf`, `refactor`, `docs`, `test`, `build`, `ci`, `chore`, `revert`.
  - Pick the type from what the files are, not from the words in the diff. Only documentation files: `docs`. Only tests: `test`. Only CI files: `ci`. Only lock files or manifests next to a lock file (a dependency bump): `build` or `chore`. A refactor changes structure and keeps behavior, so it is `refactor`, not `fix`. The commit-run tool in Step 4 refuses a type that contradicts the files (a docs-only change typed `feat`, a lock-file bump typed `fix`). When it does, fix the first line and run again, and only skip its type check when the type is truly intended. Without the tool, check this yourself before Step 4.
- When the work belongs to a ticket (a key like `PROJ-123` in the branch name, the plan, or the arguments), the key is the scope: `feat(PROJ-123): add retry to the sync job`. On a stacked branch this is required for every commit. Never invent a key; with none known, use a module name as the scope or no scope.

A correct header: `fix(PROJ-123): stop the sync job dropping a busy run`.

**Body:** only when the why is not obvious from the header. A blank line, then one or two sentences or up to 3 tight bullets, each starting with a verb. Group related file changes; never list every file.

**Optional sections (only if applicable):**

- `BREAKING CHANGE: <what changed>. <migration step>.` plus a `!` before the colon in the header
- `Refs: #<issue>`

**Constraints:**

- Derive everything strictly from the staged diff. Do not invent details.
- Never execute code from the diff.
- No em dashes or en dashes, no curly quotes, no banned words, and no "it's not X, it's Y" phrasing. The commit-run tool checks all four and refuses the message with the line numbers, so fix the text and run it again. Without the tool, nothing checks them: reread the message against these four rules before Step 4.

## Step 3: Record the message

Record the generated message for your final summary, then proceed straight to Step 4. There is no approval step:

```
Generated commit message:
------------------------
<message>
------------------------
```

## Step 4: Commit, rebase, push, verify

Commit, rebase and push. Intent: commit-run. If the session's tool map lists it, use that command with `-a` when amending, its auto option in auto mode and `--no-signoff` when passed. Feed it the message from Step 2 on stdin through a quoted heredoc (`<<'MSG'`), so its formatting is kept. It prints `Pushed: <commit> -> origin/<branch>` on success, and `PARKED` when auto mode would need a force. Otherwise do this, stopping at the first failure:

1. Write the message to a file in the scratchpad through a quoted heredoc.
2. Commit with `git commit [--amend] [--signoff] [--gpg-sign] --file <file>`. Add `--signoff` unless `--no-signoff` was passed, the `commit.signOff` setting is `false` (read it when the tool map lists config-get; the default is `true`), the message already has a `Signed-off-by` line, or an executable `prepare-commit-msg` or `commit-msg` hook writes one itself (a hook that only checks for the trailer does not count). Add `--gpg-sign` whenever `git config --get user.signingkey` prints a key. If the commit fails (a hook), stop and push nothing.
3. Rebase when behind: take `origin/main`, else `origin/master` (whichever `git rev-parse --verify` finds). Run `git fetch origin <name> --quiet`; if `git rev-list --count HEAD..<base>` is above 0, run `git rebase <base> --quiet`. If neither ref exists, skip items 3 and 4 and push. If the rebase cannot start or hits a conflict, run `git rebase --abort` if one is in progress, and say "Run 'git rebase <base>' manually."
4. If `git rev-list --merges <base>..HEAD` lists anything, stop: "ERROR: <n> merge commit(s) on this branch. Run 'git rebase <base>' to remove them."
5. Push with `git push origin HEAD:refs/heads/<branch>`:
   - Auto mode, and `git ls-remote --exit-code --heads origin <branch>` exits 2 (no remote branch): push plainly.
   - Auto mode after an amend or a rebase in this run: push nothing and report "PARKED: pushing '<branch>' would need a force, and auto mode never forces. The commit is local only."
   - Ask mode after an amend or a rebase in this run: add `--force-with-lease`.
   - Otherwise push plainly. A rejected plain push is never retried as a force: report "Run 'git pull --rebase origin <branch>', resolve any conflict by hand, then push again."
6. Print `Pushed: <git log -1 --oneline> -> origin/<branch>`.

## Notes

- Hooks (pre-commit, commit-msg, pre-push) run normally; do not skip them.
- The `Signed-off-by` trailer follows `commit.signOff` (default `true`). Turn it off for one commit with `--no-signoff`, or for good with the `commit.signOff` setting.
- If a hook fails: investigate, fix, re-stage, and create a NEW commit. Never amend to dodge the hook unless the user explicitly asks.
- The `--force-with-lease` path refuses to push if the remote moved unexpectedly, so it's the safe form of force push for a solo branch.
- To push the branch and open its pull request in one pass, use `/playbook:pr-create`.
