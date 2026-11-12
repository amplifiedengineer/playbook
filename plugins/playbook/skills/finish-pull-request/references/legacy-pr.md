# Legacy pr-create steps

## Contents

- Argument flags
- Worktree guard and Step 0: mode and skill rules
- Step 1: prepare
- Steps 2 and 3: title and body
- Step 4: push and create

These are the steps `/playbook:pr-create` followed before it ran `playbook pr prepare --json` inline and one `playbook deliver` call. The command points here only when shell execution is disabled by policy (the inline block prints `[shell command execution disabled by policy]`). Tell the user once to upgrade (`brew upgrade playbook` or `playbook upgrade`), then follow the steps below with normal `Bash` calls.

# Create Pull Request

**You are already the executing subagent for this command: run the steps below directly and never call the Skill tool to start it again.**

Push the current branch and open a pull request. The title is a conventional-commit summary, the body follows the team template, and both obey `playbook:engineering-standards` (readiness, size) and `playbook:writing-style` (voice, banned words, no dashes). A PR opens as a **draft** by default, controlled by the `pr.draft` setting (`true`, the default, or `false`): `--ready` does not publish it immediately, it marks it for promotion to ready once `/playbook:finish-pull-request`'s self-review passes, since a human should never be the first reviewer of unreviewed code. With `pr.draft` set to `false`, the PR opens ready for review instead.

This creates a **new** PR. If one already exists for the branch, this stops and points you at `/playbook:pr-comments` or `/playbook:review --quick`.

## Run this now

Execute the steps below immediately, end to end, running every command for real. Do **not** narrate a plan, summarize `git status`, offer a numbered menu, or ask "what would you like me to do?" / "proceed? [Y/n]". There is **no confirmation gate**.

The mechanical work (base and ticket detection, pre-flight checks, diff gathering, push, PR creation, base check) is two steps, prepare (Step 1) and push and create (Step 4). Each uses a tool when the session's tool map (printed at session start) lists it, and plain git and gh steps otherwise. Your job is the judgment in between: read the diff, then write the title and the body. Readiness problems (uncommitted work, no tests) print as warnings and never pause. Only the hard aborts (on the base branch, nothing ahead of base, an existing PR) stop the run.

This command is built to run in an isolated subagent (`context: fork`) so the diff and drafting stay out of the main context. When it forks, your final message is the only thing the main conversation sees, so end with a concise outcome summary (the PR URL, title, base, and draft state). If you are instead reading this in the main conversation, run it here exactly the same way; do not wait for a fork and do not defer to the user.

## Argument flags

Read these from `$ARGUMENTS` once and remember them. `--base`, `--ticket`, and `--dir` apply to Steps 1 and 4, and `--ready` is read by `/playbook:finish-pull-request`.

- `--ready` → promote the PR to ready once `/playbook:finish-pull-request`'s self-review passes, instead of leaving it a draft. Does NOT change how the PR opens: that is decided only by `pr.draft`.
- `--base <branch>` → override the base branch. Use it in Steps 1 and 4.
- `--ticket <ID>` → force the ticket key, skipping auto-detect (`none` turns every ticket rule off). Use it in Steps 1 and 4.
- `--issue <N>` → the GitHub issue this PR handles. Use it in Step 4.
- `--dir <path>` → publish the branch checked out in this directory. A caller that names a directory or branch outside the shell's current directory (for example `/playbook:implement` publishing a Segment that lives in a git worktree) passes it here. Use it in Steps 1 and 4 (pass it to a tool, or `cd` into it at the top of every plain shell block). A forked shell can reset to the main checkout between calls, so without `--dir` the command acts on whatever repo the shell is in.
- `--auto` or `--ask` → set the run mode for this run. Step 0 reads it.
- `--help` → print the argument-hint line from this file's frontmatter and stop.

There is no confirmation flag or gate: the command always runs end to end.

## Execution rules

1. Run every command for real. Do not simulate output; use the real result to drive the next step.
2. Do not assume git state, diff contents, or `gh` output. Check them.
3. Copy what Step 1 and Step 4 print. Never recompute or restate a value one of them already computed.
4. Never run destructive git commands (`reset --hard`, `push --force`, `clean -f`) or skip hooks (`--no-verify`).
5. Derive the title and body from the actual diff and commit log, never from the branch name alone or from memory.
6. Pass the PR body with `--body-file`, never inline, to preserve formatting.

## Worktree guard (first)

Before anything else, run the worktree guard in the `playbook:delegating-subagents` skill's `references/worktree-guard.md` (invoke the skill and read the file from the skill base directory it shows). The caller gives "Worktree root: <path>, branch: <name>" in this command's prompt. On a mismatch stop with its `WRONG_DIRECTORY` line and open nothing.

## Step 0: Read the run mode and load the skill rules (MUST run before drafting title or body)

**Read the run mode first.** Intent: mode-status. If the session's tool map lists it, use that command, with `--json`, adding `--flag auto` or `--flag ask` when the arguments hold `--auto` or `--ask` (check for both first and stop on a conflict). It reports `mode` (`ask` or `auto`), `source`, `hook_mode` and `warning`; if `warning` is not empty, print it once. If it fails, do not stop and do not retry: run in ask mode and print one line. Otherwise do this: if the arguments hold both `--auto` and `--ask`, stop with one line: "--auto and --ask conflict; pass one." `--auto` means auto mode. Anything else means ask mode, the default.

- **`ask` mode:** behave exactly as this file describes.
- **`auto` mode:** follow the Auto path below.

### Auto path

In auto mode, `/playbook:finish-pull-request` skips the self-review. When `/playbook:implement` opened this PR, its review already covered the code. Any other caller gets no review there, and the finish skill's final report says that no self-review ran. If `--ready` was passed, it still promotes the draft. The `pr.review.fix` and `pr.merge.enabled` settings still apply in auto mode, except that the finish skill never merges a PR whose code no review covered.

**Then load the skill rules.** This step needs four things: `playbook:writing-style`'s voice/banned-words/dash rules, its "When creating PRs" guidance (`references/commits-and-prs.md`), its "Prohibited GitHub Content" rules (`references/github-prohibited.md`; the PR title and body are posted to GitHub, so these apply), and `playbook:engineering-standards`' PR readiness criteria (used in Step 2). Reading each full skill file to get a subset that small is most of Step 0's own cost, so extract only those sections. Intent: pr-rules. If the session's tool map lists it, use that command. It checks each extracted file for a marker string, so a renamed heading fails loudly instead of silently pulling in everything through the end of the file. On success it prints `Skill sections extracted and verified:` and four paths; read all four with the Read tool now. On failure it prints `ERROR: ...`. On any `ERROR:` (an old binary says 'skill reference not found' or 'no plugin root'), run `playbook --version` first; if it is older than 0.26.0, tell the user to run `brew upgrade playbook` (Homebrew) or `playbook upgrade`. Then, and also if the tool map does not list it, do this: from the skill base directory each skill shows when you invoke it (`playbook:writing-style`, `playbook:engineering-standards`), read `writing-style/SKILL.md` up to its `## Loading the references` heading, `writing-style/references/commits-and-prs.md` from `## Pull requests` to the end of the file, all of `writing-style/references/github-prohibited.md`, and the `engineering-standards/SKILL.md` text from `### Readiness` up to `### Review Comments`.

Together they carry the same rules Step 0 has always needed: voice, banned words, the dash rule, "When creating PRs" guidance, the Prohibited GitHub Content rules, and the PR readiness rules enforced in Step 2. The `github-review-comments` and `review-examples` references of writing-style are deliberately not extracted: they govern review comments and replies, so they do not apply to a PR title or body. Also read the `phrases` and `structures` references (`references/<name>.md` in the writing-style skill's folder) and apply them to the title and body. If any extraction fails or a file cannot be found, invoke the full skill via the Skill tool for that one file rather than proceeding without its rules.

The PR title and body are read by another engineer, so they follow `playbook:writing-style`: simple English, plain and direct, the same register as chat.

## Step 1: Prepare (version check, pre-flight, diff, ticket)

Resolve the branch and the base, run the pre-flight checks, gather the diff and detect the ticket. Intent: pr-prepare. If the session's tool map lists it, use that command with only the flags the caller gave (`--base`, `--ticket`, `--dir`). It resolves the base (flag, then the repo default, then `main`), refuses to run on the base branch, or with nothing ahead of it, and prints:

- `branch=`, `state_dir=`, `base=<branch> (source: ...)`, and `commits_ahead= changed_lines= dirty_files= test_files_touched=`.
- A `VERDICT` line each for dirty files and tests.
- The diff stat and commit log, then `diff_file=<path>` and `ticket=<ID or empty>`.

If it prints `A PR already exists: <url>` it exited 0: report that and stop. Any other non-zero exit is a hard stop: report its message and stop.

Otherwise do this, and print the same lines yourself:

1. Branch: `git branch --show-current`. If `gh pr view <branch> --json url,state` shows an `OPEN` PR, print "A PR already exists: <url>. Use /playbook:pr-comments or /playbook:review instead." and stop.
2. Base: `--base`, else `gh repo view --json defaultBranchRef -q .defaultBranchRef.name`, else `git symbolic-ref refs/remotes/origin/HEAD` without `refs/remotes/origin/`, else `main`. If the branch is the base, stop: "on the base branch (<base>); create a feature branch first".
3. Run `git fetch origin <base> --quiet`. If it fails, say the results use the local ref.
4. `git rev-list --count origin/<base>..HEAD` is the commits ahead. At 0, stop: "nothing ahead of <base>; there is nothing to open a PR for".
5. Changed lines are the insertions plus deletions in `git diff --shortstat origin/<base>...HEAD`.
6. Print one `VERDICT` line each:
   - dirty: `WARN` with the count of `git status --porcelain` lines that will NOT be in the PR, else `OK`.
   - tests: `NONE` when no file in `git diff --name-only origin/<base>...HEAD` matches `.test.`, `.spec.`, `_test.`, `test_`, a `test/` or `tests/` or `__tests__/` folder, and no added Rust line holds `#[test]` or `#[cfg(test)]`; else `OK` with the count.
7. Make a state folder with `mktemp -d` and print it as `state_dir=`. Write `git diff origin/<base>...HEAD` to `<state_dir>/pr-diff.txt` and print it as `diff_file=`. Print `git diff --stat origin/<base>...HEAD` and `git log origin/<base>..HEAD --format='%h %s'`.
8. Ticket: `--ticket` as given (it may be `none`), else the plan or Segment ticket, else the first match of `[A-Z][A-Z0-9]+-[0-9]+` in the branch name, else a key in the scope of the commit subjects, else empty. Never invent a key. Print it as `ticket=`.

**Copy every `VERDICT` line verbatim into the readiness block.** Do not recompute or paraphrase them. Every `VERDICT` is non-blocking: print them and move on without pausing.

Read the file named by `diff_file=` with the Read tool. This is the source of truth for the title and body. If it is large, read it in chunks; do not skip it.

If `ticket=` holds a real key (not empty, not `none`), the key goes on the very first line of the body and in the scope of the title (Steps 2 and 3). If empty or `none`, no ticket rule applies: omit it without asking.

## Step 2: Generate the title (conventional commits)

Derive the title from the diff and commit log gathered in Step 1. Read the `playbook:writing-style` skill's `references/conventional-commits.md` (invoke the skill and read the file from the skill base directory it shows; if you cannot find it, follow the usual types (`feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`)) before writing it, and follow it for the title only.

- Format: `type(scope): summary`. With a ticket key, the key is the scope: `feat(PROJ-123): add retry to the sync job`. Without one, use a module name as the scope, e.g. `feat(auth): add SSO retry logic`, or no scope.
- In a stacked PR set, every PR title and every commit carries the key. Squash merge turns the title into the commit message, so the title follows the same format.
- Types: `feat`, `fix`, `style`, `refactor`, `perf`, `docs`, `test`, `build`, `ci`, `chore`, `revert`. Pick the type from what the files in the diff are, not from words in it: only docs files is `docs`, only tests is `test`, only CI files is `ci`, a dependency bump (lock files, or manifests next to a lock file) is `build` or `chore`, and a change that restructures code and keeps behavior is `refactor`, not `fix`.
- Imperative mood ("add", not "added" or "adds"), no trailing period.
- **Length: 72 characters maximum**, counting the entire line including the `type(scope):` prefix. This is a hard limit, not a target. If the draft exceeds it, tighten the summary (cut filler, shorten wording; drop the scope only when it is a module name, never when it holds the ticket key) until it fits; never open a PR with a title over 72 characters. Step 4 re-checks the length and refuses a longer title before it pushes anything.
- The summary states the **effect** of the change, not a list of files.

## Step 3: Generate the body (MANDATORY template)

The body is short. Fill this template exactly, in this order, and drop every optional part that has nothing to say. Follow `playbook:writing-style`: plain words, active voice, no filler openers, no em or en dashes.

```markdown
PROJ-123

## Summary

<What changed and why, in at most 3 short sentences. No file lists, no restating the title.>

## Notes for reviewers

- <Optional. Risks, decisions, or how to verify. Drop the whole section when empty.>

## Related work

- <Optional. Links to the other PRs of a stack. Drop the whole section when empty.>
```

Rules:

1. **First line:** the ticket key (shape `[A-Z][A-Z0-9]+-[0-9]+`) when Step 1 printed one, as a link when the base URL is known from config or memory. No key means no line, and the body starts at `## Summary`.
2. **GitHub issue:** when `--issue`, the plan or the branch names an issue, end the Summary with `Closes #N` if the PR fully resolves it, or `Refs #N` if it only advances it.
3. **Summary:** the only required section. It says what changed and why in at most 3 short sentences.
4. **Length:** aim for about 500 characters and stay under 1200. A reviewer reads the diff, so do not describe it line by line.
5. **Leave out:** a "What Changed" or "Test plan" section, checklists of obvious steps, file lists, emoji headers, hedging, marketing words, an "N/A" section, and any "generated by" footer.
6. The body carries no AI attribution and no em or en dashes outside code. Step 4 checks the title, the body and every commit message, and refuses to push if it finds a problem.

A correct body:

```markdown
PROJ-123

## Summary

The sync job now retries up to three times when the queue is busy. Before, one busy reply dropped the whole run. Closes #41
```

## Step 4: Push and create

The PR opens as a draft unless the `pr.draft` setting is `false` (default `true`). `--ready` is not used here: `/playbook:finish-pull-request` reads it, after the self-review, to decide whether to promote a draft.

Write the finished body to `<state_dir>/pr-body.md` (the `state_dir=` value from Step 1) through a quoted heredoc. Always target the branch Step 1 printed on its `base=` line, so the PR targets the base the title and body were written against:

```bash
cat > "<state_dir>/pr-body.md" <<'PRBODY_EOF'
<the filled template goes here>
PRBODY_EOF
```

Then push and open the PR. Intent: pr-create. If the session's tool map lists it, use that command with the title, `--body-file "<state_dir>/pr-body.md"`, `--base <base from Step 1>`, `--ticket` and `--issue` when known, and `--dir` when given. It reads `pr.draft` itself, so do not add a draft flag. It checks the body against the template and length cap, the issue clause, the ticket on the first line, the ticket in the title and commit scopes (a stacked PR is refused without it), the title length and the attribution and dash rules, then pushes, confirms the remote carries your HEAD, opens the PR (or reuses the open one) and corrects its base. If it refuses with a list of problems, nothing was pushed or created: fix the title or body (or amend the named commit) and run it again. Otherwise do this, stopping at the first failure:

1. Refuse when anything is staged (`git diff --cached --quiet` fails): report the staged files and stop, and do not rewrite the title or body.
2. Check by hand: the title is at most 72 characters; the body follows Step 3 and is under 1200 characters; the key (when known) is on the first body line and in the scope of the title and every commit; a handled issue has its `Closes #N` or `Refs #N`; the title and body have no em or en dash outside code; the title, the body and every message in `git log origin/<base>..HEAD` carry no AI attribution. If any fails, fix the title or body (or say which commit to amend) and check again. This check is advisory: no tool backs it.
3. Read `pr.draft`. Intent: config-get. If the tool map lists it, use that command. Otherwise use the default, `true`.
4. If `git rev-list --count HEAD..origin/<base>` is above 0, run `git rebase --autostash origin/<base>`. On a conflict, run `git rebase --abort`, list the files, and stop.
5. Push with `git push -u origin HEAD:refs/heads/<branch>`. After the rebase in item 4 the push needs a force: first check that `git merge-base --is-ancestor <remote sha> <HEAD before the rebase>` passes, where the remote sha is `git rev-parse origin/<branch>` after a `git fetch origin <branch>`, and then push with `--force-with-lease=refs/heads/<branch>:<remote sha>`. If the check fails, stop and say the remote branch has commits this branch lacks. A rejected push stops the run before any PR exists.
6. Confirm `git ls-remote origin refs/heads/<branch>` shows the SHA of `git rev-parse HEAD`. If not, stop: the push did not land.
7. If `gh pr view <branch> --json url,state` shows an `OPEN` PR, reuse it. Else run `gh pr create --title "<title>" --body-file "<state_dir>/pr-body.md" --base <base>`, adding `--draft` when `pr.draft` is `true`.
8. If `gh pr view <branch> --json baseRefName -q .baseRefName` is not `<base>`, run `gh pr edit <branch> --base <base>`.
9. Read the URL, number and draft state with `gh pr view <branch> --json url,number,isDraft`.

If a run fails after the PR already exists, running it again reuses the open PR.

Report the PR URL and a one-line summary (title, base, and whether it opened as a draft), and say whether `--ready` was passed. Unless the caller said the code was already reviewed, end the report with the line "Next: invoke playbook:finish-pull-request for this PR." and add "--ready" to it when `--ready` was passed. `/playbook:implement` is such a caller. This forked agent cannot run the self-review, fix, promote and merge work: the orchestrating session that receives this report does, through that skill.
