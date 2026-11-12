# Sessions

`playbook sessions` lists your recent agent sessions in one table, so you can see which ones still matter. It is read only. The usage terminal view shows the same records (key `p`). Design: [ADR-0024](../adr/0024-session-management-view-and-lifecycle.md), stage 1.

```
playbook sessions
playbook sessions --json
```

## Columns

| Column | Where it comes from |
|---|---|
| State | `working` (a message in the last 2 minutes), `idle`, `stale` (no activity for 3 days), `done` (the pull request of the branch is merged or closed). |
| Session | The first 8 characters of the session id. |
| Repo, Branch | The newest message of the session in the usage store. |
| Worktree | Whether the directory the session started in is a linked git worktree, with the playbook convention that made it (`cc` for `playbook cc launch worktree`, `wu`, `agent`, `review`), `main checkout`, or `gone` when the folder no longer exists. |
| Link | The PR of the branch from `gh pr list --head`, or `issue #N?` when only the branch name has a number (the `?` marks a guess). A session on `main` has no PR link. |
| Last | Time since the last message. |
| Context | Input plus cache read plus cache write tokens of the newest request, the size of the prompt the model last read. |
| Cost | The whole session's cost, as in `playbook usage`. |

Reads: the same store as `playbook usage` (new history is ingested first), the first transcript lines of each session for its working directory, `git` for the worktree and `git status`, and `gh` for the PR. `gh` and `git` calls have timeouts. A missing `gh` or no network leaves the link empty.

## The sessions view

In `playbook usage`, press `p` for a full screen table of the same records, in the usage theme and in the chosen time zone. Keys: `j` and `k` move, `v` changes the order (newest activity, grouped by repo, or by state with the ones that need attention first), `r` reads again now, `p` or `Esc` goes back, `q` quits.

The view reads again every 30 seconds, off the drawing loop, so a slow `gh` never freezes the screen. The selected session's suggestion shows under the table. The view has no key that closes, removes or stops anything.

## Sweep dry run

```
playbook sessions sweep --dry-run
```

For each finished session, one line says what a sweep would do with its worktree. The verdict comes from the same guards as `playbook worktree remove`: the branch landed, the tree is clean, no live lock and no process using it. A session with activity in the last 15 minutes is left alone, a stale one is listed but not swept, and one with no worktree is only told to close. The flag is required, because only the dry run exists. It removes nothing, stops no process and closes no tab or pane (ADR-0024, stage 4).

## What it never does

- It writes nothing, removes nothing and stops nothing.
- A finished or stale session gets a printed suggestion. When its worktree has uncommitted changes, the suggestion starts with `!` and says nothing is removed.
- It stores and prints no prompt text. The `--json` output holds ids, names, paths, counts and times (epoch seconds, UTC).
- `waiting` is not shown yet. Claude Code only reports it through the `Notification` hook, which playbook does not use yet (ADR-0024, stage 3).

## Limits

- The working directory is the first one the transcript records. A session that moves into a worktree later shows where it started.
- `working` is a guess from the last message time. A session that is thinking and has not written a message looks idle.
- Codex sessions show repo, branch, context and cost, but no directory, so no worktree or PR.

The list covers the last 7 days.
