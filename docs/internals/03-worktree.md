# Worktree engine

`playbook cc launch worktree <branch>` creates or enters a git worktree, rebases it onto the base branch, and starts a session there. The engine is Rust: `playbook cc worktree` (`crates/cli/src/cc/worktree_run.rs`, `crates/cli/src/cc/worktree.rs`). Slow upkeep runs afterward in a detached process, so the session starts at once.

## What it does

In order:

1. Finds the repo's base branch through `origin/HEAD`, falling back to `main`, `master`, `trunk` or `develop`.
2. Stashes a dirty main worktree and restores it afterward, even when a later step fails.
3. Names the folder after the JIRA key in the branch (`PROJECT-1234-foo-bar` gives `PROJECT-1234/`), else the branch leaf.
4. Creates the worktree at `<repo-parent>/<base>/<repo>/<folder>`. `<base>` is `WORKTREE_BASE_DIR` (default `.worktrees`; a relative value sits under the repo parent, an absolute one is used as is). An existing worktree on the right branch is fast-forwarded instead.
5. Copies `.env` from the base repo without overwriting.
6. Rebases the branch onto the latest base when you authored it. Protected names (`main`, `master`, `trunk`, `develop`, `staging`, `release/*`, `hotfix/*`) are never rebased. A conflict aborts the rebase and leaves the worktree on the branch as created. AI conflict resolution is not wired in the binary yet (#446).
7. Hands the rest to the background.

## Background upkeep

A detached, silent process (`playbook cc housekeep`) runs these best-effort steps:

1. `git fetch --prune`.
2. Fix a stale upstream tracking ref, then fast-forward pull when the remote branch exists.
3. `git worktree prune`.
4. Set upstream: track the remote branch if it exists, else `git push -u <remote> <branch>`, which starts CI without a manual push. Skip the push with `--no-push` or `WORKTREE_NO_PUSH=1`.
5. Reuse `node_modules` (below).
6. `playbook worktree sweep`: remove worktrees of any creation convention that have landed and are not locked or in use. It is gated by `worktree.cleanup.enabled`, and a new worktree is protected from it. For a branch worktree, a merged pull request (read with `gh`, and only when it targeted the default branch and its head is the local branch tip) counts as landed, so a squash merge is caught; an open pull request keeps the worktree. Without `gh`, it falls back to local history and the idle age.

### node_modules

When `package-lock.json` hashes match between the base repo and the worktree, `node_modules` is cloned copy-on-write (`cp -cR` on APFS, `cp -R --reflink=auto` on GNU filesystems, plain `cp -R` as the last fallback). Each worktree gets an independent tree. Then a bounded `npm install --prefer-offline --no-audit --no-fund` reconciles extra packages. When the hashes differ, nothing is cloned.

## The daily sweep

Every removal in the sweep goes through the gc guard and takes the gc run lock, so a sweep and `playbook gc` never run at once. See [cleanup and tombstones](17-gc-and-tombstones.md). Review worktrees follow one rule in the sweep and in `worktree review setup`: unlocked after 60 seconds, locked after a dead owner pid and 24 hours.

`maybe_sweep_worktrees` (`crates/cli/src/hooks/session_init.rs`) also runs the sweep on every Claude Code `SessionStart`, at most once per 24 hours per repo (a row under `worktree-sweep/<repo slug>` in the [state store](08-state-store.md)). So a repo is swept when you create a worktree and again the first time a session opens each day. Both triggers call the same function. The overlap is deliberate.

A repo nobody opens is swept by neither trigger. That is a known limit: fixing it needs an OS scheduler, which nothing here uses.

Setting: `worktree.cleanup.enabled`. See [Config keys](../reference/config-keys.md). Two numbers are fixed in `crates/cli/src/worktree/mod.rs`: an agent or launcher branch with no pull request counts as landed after 30 days without a commit, and a worktree stopped on a cherry-pick conflict is kept for 90 days.

## Claude Code worktree hooks

`playbook hook worktree-create` and `playbook hook worktree-remove` are written into `~/.claude/settings.json` by `playbook setup` for the `WorktreeCreate` and `WorktreeRemove` events. They stop `claude --worktree`, `isolation: "worktree"` agents, and background sessions from creating worktrees under `.claude/worktrees/`.

**Create.** The hook reads `cwd` and `worktree_name` (or `name`) from stdin, finds the main worktree of the repo at `cwd`, and targets `<main-parent>/.worktrees/<repo>/<name>` (or the `WORKTREE_BASE_DIR` base, like the launcher). It uses the launcher's `main_worktree` and `resolve_base`.

- The name must be one safe path segment and `worktree-<name>` a valid branch name. Refused: empty names, `..`, slashes, a leading `-`, anything git rejects.
- A target that is already a registered worktree is reused. A registered entry whose folder is gone is pruned and re-added.
- Otherwise the hook runs `git worktree add -b worktree-<name> <target> <base>`, matching Claude Code's default branch name.
- The base is the payload's `base_commit`, then the cached `origin/HEAD`, then local `HEAD`.
- With no `base_commit`, the hook first refreshes `origin/HEAD`, like Claude Code. It fetches the default branch unless `FETCH_HEAD` or the `playbook-origin-fetch` stamp in the git directory is under 24 hours old. The fetch waits at most 5 seconds, never prompts for credentials, and keeps the cached ref on failure.
- A newly created worktree (not a reused one) gets the files that match a pattern in the main checkout's `.worktreeinclude` and are ignored by git, so local files such as `.env` follow it. Tracked files are never copied, existing files are never overwritten, symlinks are skipped, and a path that leaves the repo is ignored.
- On macOS, a newly created worktree also gets a copy-on-write clone of the main checkout's `target` folder when that folder exists and git ignores it. The clone shares disk blocks with the original until either side writes, so a new worktree reuses the compiled dependencies and spends little extra disk. On another platform, on a filesystem without clone support (anything but APFS), or when the copy fails, the worktree starts without a `target` and the hook still succeeds. Removing the worktree deletes its `target` with it; the main checkout's is never touched.
- Stdout carries only the absolute path. All git output goes to stderr.
- The path follows the launcher convention: `classify` reports `CcLauncher` and the sweep cleans it once its work has landed.

On any failure the hook says why on stderr and falls back to `<repo>/.claude/worktrees/<name>` with the same branch and base. It exits non-zero only if the fallback fails too.

**Remove.** The hook acts only on a registered worktree of the repo and never on the main worktree.

It keeps the worktree, prints a reason on stderr, and exits 0 when the tree has uncommitted or untracked changes, or when any commit is not reachable from a remote-tracking ref or the local default branch. Otherwise it runs `git worktree remove` without `--force`.

Then it deletes the branch only if its name starts with `worktree-` (so a launcher branch is never touched) and it passes the same reachability rule.

## Delivery from a worktree

`playbook pr create` (code in `crates/cli/src/deliver/`) never depends on the session's working directory. It runs `git` and `gh` with fixed argument vectors and the worktree as their directory, so a subagent that cannot change its cwd still delivers its branch. The module imports only `common/` helpers and its own `sys` adapter. The pre-commit step relies on the repo's own git hooks, which always run.

The base is `--base`, then the open PR's base, then the nearest open PR branch that is an ancestor of HEAD (a stacked PR), then the default branch.

## Claude Code 2.1.318

`playbook doctor check` warns when Claude Code 2.1.318 runs in a linked worktree. That release can refuse shell commands there. If one is refused, call ExitWorktree with action `keep`, then continue from the original directory.

## See also

- [Launcher and hooks](01-launcher-and-hooks.md): the `playbook cc` launcher that sets up the worktree.
- [Docs index](../README.md)
