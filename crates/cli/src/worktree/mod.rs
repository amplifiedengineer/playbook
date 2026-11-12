// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Classifies a worktree by matching its path against playbook's known
//! creation conventions (`playbook wu`, the Agent tool, `playbook cc launch worktree`, and
//! review worktrees), then decides whether an already-classified worktree is
//! safe to reap.

pub(crate) mod pr_state;
pub mod review;

use crate::common::git;
use crate::common::paths::playbook_root_from;
use crate::common::repo_slug;
use crate::common::time::parse_iso8601_utc_epoch;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Which convention created a worktree, matched purely by its path shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Convention {
    Wu,
    AgentTool,
    CcLauncher,
    Review,
    Unmanaged,
}

/// Classifies `worktree_path` against each known creation convention's path
/// shape, resolved relative to `home`.
pub fn classify(worktree_path: &Path, home: &Path) -> Convention {
    if is_wu_convention(worktree_path, home) {
        Convention::Wu
    } else if is_agent_tool_convention(worktree_path) {
        Convention::AgentTool
    } else if is_cc_launcher_convention(worktree_path) {
        Convention::CcLauncher
    } else if is_review_convention(worktree_path) {
        Convention::Review
    } else {
        Convention::Unmanaged
    }
}

/// `playbook_root_from(home)/repos/<owner>/<repo>/worktrees/*/*`, or the older
/// `.../repos/<owner>/<repo>/*/worktrees/*/*` with a worktree-id segment. The
/// owner/repo prefix comes from `repo_slug()`, never from `worktree_id()`:
/// the worktree-id segment differs between two worktrees of the same repo, so
/// a literal compare would miss every worktree but the one that created it.
fn is_wu_convention(path: &Path, home: &Path) -> bool {
    let slug = repo_slug();
    let Some((owner, repo)) = slug.split_once('/') else {
        return false;
    };
    let prefix = playbook_root_from(home)
        .join("repos")
        .join(owner)
        .join(repo);
    let Ok(rest) = path.strip_prefix(&prefix) else {
        return false;
    };
    let segments: Vec<_> = rest.components().collect();
    let under = |i: usize| {
        segments
            .get(i)
            .is_some_and(|s| s.as_os_str() == "worktrees")
    };
    (segments.len() == 3 && under(0)) || (segments.len() == 4 && under(1))
}

/// `.claude/worktrees/agent-*` anywhere in the path, matching the Agent
/// tool's launch convention regardless of which repo root it sits under.
fn is_agent_tool_convention(path: &Path) -> bool {
    let segments: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    segments
        .windows(3)
        .any(|w| w[0] == ".claude" && w[1] == "worktrees" && w[2].starts_with("agent-"))
}

/// `<main-worktree-parent>/.worktrees/<repo>/<branch>`, the default
/// `_wt_resolve_base` shape. Derived from the MAIN worktree entry of `git
/// worktree list --porcelain`, never the calling process's own toplevel: a
/// sweep invoked from inside the just-created worktree would otherwise
/// resolve its own parent instead of the launcher's base directory.
fn is_cc_launcher_convention(path: &Path) -> bool {
    let Some(porcelain) = git_stdout(&["worktree", "list", "--porcelain"]) else {
        return false;
    };
    let Some(main) = crate::cc::worktree::main_worktree(&porcelain) else {
        return false;
    };
    launcher_base(Path::new(&main)).is_some_and(|base| is_direct_child(path, &base))
}

/// The folder the worktree hook puts this repo's worktrees in,
/// `<main-worktree-parent>/.worktrees/<repo>` or under `WORKTREE_BASE_DIR`.
pub(crate) fn launcher_base(main_root: &Path) -> Option<PathBuf> {
    let parent = main_root.parent()?;
    let configured = std::env::var("WORKTREE_BASE_DIR").ok();
    Some(crate::cc::worktree::resolve_base(
        main_root,
        parent,
        configured.as_deref(),
    ))
}

/// `review-worktrees/<pr>-<sha>` under the git common dir, per
/// `review::setup`.
pub(crate) fn is_review_convention(path: &Path) -> bool {
    let Some(common_dir) = git_stdout(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
    else {
        return false;
    };
    let base = PathBuf::from(common_dir).join("review-worktrees");
    is_direct_child(path, &base)
}

/// True if `path` is exactly one path component below `base`.
fn is_direct_child(path: &Path, base: &Path) -> bool {
    matches!(path.strip_prefix(base), Ok(rest) if rest.components().count() == 1)
}

/// Compares `worktree_head_commit` against `origin/<default_branch>` to
/// decide whether the WU worktree's work already reached the default branch.
pub fn wu_worktree_landed(
    worktree_head_commit: &str,
    default_branch: &str,
    repo_root: &Path,
) -> bool {
    let upstream = format!("origin/{default_branch}");
    git::run(
        Some(repo_root),
        &[
            "merge-base",
            "--is-ancestor",
            worktree_head_commit,
            &upstream,
        ],
        git::SLOW_TIMEOUT,
    )
    .is_some_and(|out| out.status.success())
}

/// Whether a branch's pull request is merged or open, the richer signal
/// [`named_branch_landed`] needs beyond a flat open-or-not list. `None`
/// stands for no PR found (or `gh` unavailable), which falls back to local
/// git history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Merged,
    Open,
}

/// Reads the pull request state of a branch in a repo, for the sweep. Called
/// from the parallel judging step, so it must be `Sync`.
pub type PrLookup<'a> = &'a (dyn Fn(&Path, &str) -> Option<PrState> + Sync);

/// Seconds in a day, for converting a day count into the age
/// comparison [`named_branch_landed`] runs against a commit timestamp.
const SECS_PER_DAY: i64 = 86_400;

/// Whether a named-branch worktree's branch already landed: true for a
/// merged PR, false for an open one, and otherwise a local fallback: merged
/// into `default_branch` or stale past `stale_after_days`, with the
/// threshold itself counting as stale.
pub fn named_branch_landed(
    branch: &str,
    default_branch: &str,
    stale_after_days: i64,
    repo_root: &Path,
    now_epoch: i64,
    pr_state: Option<PrState>,
) -> bool {
    match pr_state {
        Some(PrState::Merged) => return true,
        Some(PrState::Open) => return false,
        None => {}
    }

    let merged = crate::cc::worktree::merged_branches(repo_root, default_branch);
    if crate::cc::worktree::contains_line(&merged, branch) {
        return true;
    }

    let Some(commit_epoch) = branch_commit_epoch(branch, repo_root) else {
        return false;
    };
    now_epoch - commit_epoch >= stale_after_days * SECS_PER_DAY
}

/// Unix seconds of `branch`'s last commit, or `None` if the branch or its
/// history cannot be read.
fn branch_commit_epoch(branch: &str, repo_root: &Path) -> Option<i64> {
    git::output_slow(repo_root, &["log", "-1", "--format=%ct", branch])?
        .parse()
        .ok()
}

/// Whether a review-convention worktree already landed, judged by its lock
/// state rather than git history: a review worktree carries no branch or PR
/// to query, only a lock file `git worktree add`/`lock` may or may not have
/// reached yet.
pub fn review_worktree_landed(
    is_locked: bool,
    lock_owner_pid: Option<u32>,
    lock_age_secs: i64,
    never_locked_grace_secs: i64,
) -> bool {
    if !is_locked {
        return lock_age_secs >= never_locked_grace_secs;
    }
    // `review::setup`'s lock reason carries the SETUP process's own pid,
    // and that process exits right after locking, so this pid is dead for
    // every review, active ones included, not only finished ones: pid
    // liveness alone is never a safe "still in use" signal here. Requiring
    // the lock to also be older than a real TTL, not just the short
    // never-locked grace window, keeps an active review (which can run for
    // minutes, e.g. a full check-suite run) from being reaped the instant a
    // sweep happens to run. Review setup and the daily sweep both use
    // `REVIEW_LOCK_TTL_SECS`.
    let stale_enough = lock_age_secs >= REVIEW_LOCK_TTL_SECS;
    match lock_owner_pid {
        Some(pid) => !pid_is_alive(pid) && stale_enough,
        None => stale_enough,
    }
}

/// 24 hours, the one limit for review setup and the daily sweep: the floor a locked review worktree's lock age must clear before
/// [`review_worktree_landed`] treats it as safe to reap, since a dead lock
/// pid is not itself a safe signal for this convention.
pub const REVIEW_LOCK_TTL_SECS: i64 = 86_400;

/// Whether `pid` still refers to a running process, checked via `kill -0`
/// rather than a signal-handling crate dependency (none already in this
/// crate covers it). Any non-success exit, including permission denied,
/// reads as not alive. Runs directly rather than through
/// [`run_with_timeout`], unlike this module's git and `gh` calls: a local
/// syscall carries no hang risk to guard against.
pub fn pid_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Extracts a pid from a lock reason string, trying `review::setup`'s
/// flat `pid=<n>` shape first, then the Agent tool's `pid <n>` shape. `None`
/// if neither is found: the Agent-tool format is not a documented contract
/// anywhere in this repo, so it is never assumed to be stable.
pub fn parse_lock_pid(lock_reason: &str) -> Option<u32> {
    parse_pid_after(lock_reason, "pid=").or_else(|| parse_pid_after(lock_reason, "pid "))
}

/// Digits immediately following the first occurrence of `marker`, or `None`
/// if `marker` is absent or is not followed by at least one digit.
fn parse_pid_after(lock_reason: &str, marker: &str) -> Option<u32> {
    let after_marker = &lock_reason[lock_reason.find(marker)? + marker.len()..];
    after_marker
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// `git <args>` against the CALLING process's current directory, trimmed.
/// `None` on any failure, including a timeout. Mirrors `src/cc/worktree.rs`'s
/// `git_stdout`, but without a `-C <repo_root>`: `classify` has no repo root
/// of its own, only the caller's cwd.
fn git_stdout(args: &[&str]) -> Option<String> {
    git::trimmed(git::run(None, args, git::SLOW_TIMEOUT))
}

/// One worktree from `git worktree list --porcelain`'s output: its path,
/// checked-out `HEAD` commit, branch name (absent when detached), and lock
/// reason (absent when unlocked).
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub lock_reason: Option<String>,
}

/// Removes the data folder databases of the worktree that was at `path`, once
/// it is gone. Two paths can slugify to one worktree id, so nothing is
/// removed while a still-registered worktree has the same id.
fn remove_data_of(repo_root: &Path, home: &Path, repo_slug: Option<&str>, path: &Path) {
    let (Some(slug), Some(live)) = (repo_slug, live_worktree_ids(repo_root)) else {
        return;
    };
    let id = crate::common::paths::worktree_id_for(path);
    if live.contains(&id) {
        return;
    }
    let data = crate::common::data::data_dir_from(home);
    crate::common::data::remove_worktree_dbs(&data, slug, &id);
    // Best effort: a store that is busy or damaged keeps its row, and the
    // dead-root rule of the next index run drops it.
    let _ = playbook_code::forget_worktree(&data, slug, &id);
}

/// The worktree ids of every worktree registered against `repo_root`, or
/// `None` when git cannot say (so nothing is reported as orphaned).
pub fn live_worktree_ids(repo_root: &Path) -> Option<Vec<String>> {
    let entries = list_worktrees(repo_root).ok()?;
    Some(
        entries
            .iter()
            .map(|e| crate::common::paths::worktree_id_for(&e.path))
            .collect(),
    )
}

/// Lists every worktree registered against `repo_root`. Aborts rather than
/// treating a failed or unparseable command as "nothing to sweep": a broken
/// `.git` state must stop the sweep, not proceed as if no worktrees existed.
pub(crate) fn list_worktrees(repo_root: &Path) -> Result<Vec<WorktreeEntry>, String> {
    let out = git::run(
        Some(repo_root),
        &["worktree", "list", "--porcelain"],
        git::SLOW_TIMEOUT,
    )
    .ok_or_else(|| "git worktree list --porcelain timed out or failed to run".to_string())?;
    if !out.status.success() {
        return Err(format!(
            "git worktree list --porcelain failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    parse_porcelain(&stdout)
        .ok_or_else(|| "git worktree list --porcelain produced unparseable output".to_string())
}

/// Splits porcelain output into one [`WorktreeEntry`] per `worktree ` block.
/// `None` when a line appears before any `worktree ` header, or no block was
/// found at all: either shape means this is not the output this parser was
/// built against, and a caller must abort rather than guess.
fn parse_porcelain(porcelain: &str) -> Option<Vec<WorktreeEntry>> {
    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    for line in porcelain.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(WorktreeEntry {
                path: PathBuf::from(path),
                head: None,
                branch: None,
                lock_reason: None,
            });
            continue;
        }
        let Some(entry) = current.as_mut() else {
            if line.trim().is_empty() {
                continue;
            }
            return None;
        };
        if let Some(sha) = line.strip_prefix("HEAD ") {
            entry.head = Some(sha.to_string());
        } else if let Some(reference) = line.strip_prefix("branch ") {
            entry.branch = Some(reference.trim_start_matches("refs/heads/").to_string());
        } else if line == "locked" {
            entry.lock_reason = Some(String::new());
        } else if let Some(reason) = line.strip_prefix("locked ") {
            entry.lock_reason = Some(reason.to_string());
        }
    }
    if let Some(entry) = current.take() {
        entries.push(entry);
    }
    (!entries.is_empty()).then_some(entries)
}

/// Days since the last commit after which the sweep treats an agent or
/// launcher branch as landed.
pub const STALE_AFTER_DAYS: i64 = 30;
/// Days a worktree stopped on a cherry-pick conflict is kept, counted from
/// the marker's timestamp.
pub const CONFLICT_GRACE_PERIOD_DAYS: i64 = 90;

/// `worktreeCleanup.enabled`, resolved once per `sweep`/`remove` invocation
/// rather than per worktree inside the loop.
pub struct SweepPolicy {
    pub enabled: bool,
}

/// Resolves [`SweepPolicy`] via `config::resolve`, propagating a malformed
/// tier file as an error rather than falling back to a default: a broken
/// config file must stop the sweep, not silently run under default policy.
pub fn resolve_policy(
    home: &Path,
    repo_slug: Option<&str>,
) -> Result<SweepPolicy, crate::config::ConfigError> {
    let (enabled, _) = crate::config::resolve("worktreeCleanup.enabled", home, repo_slug)?;
    Ok(SweepPolicy {
        enabled: require_bool("worktreeCleanup.enabled", &enabled)?,
    })
}

/// A hand-edited or externally-written tier file can carry a `Value` of the
/// wrong type for a key `config::resolve` never itself type-checks: fail
/// closed with [`ConfigError::WrongType`] rather than silently defaulting,
/// since a wrong-typed `worktreeCleanup.enabled` defaulting to `true` would
/// re-open the destructive gate the value was meant to close.
fn require_bool(key: &str, value: &serde_json::Value) -> Result<bool, crate::config::ConfigError> {
    value
        .as_bool()
        .ok_or_else(|| crate::config::ConfigError::WrongType {
            key: key.to_string(),
            expected: "bool",
        })
}

/// The marker filename left inside a Wu-convention worktree when a merge
/// conflict stopped its landed check and a human must resolve it.
const CONFLICT_MARKER_FILE: &str = ".playbook-conflict-stop";

/// A worktree just added but not yet locked needs a moment before `git
/// worktree lock` runs, matching the gap between `review::setup`'s own
/// `add` and `lock` calls.
pub const NEVER_LOCKED_GRACE_SECS: i64 = 60;

/// Unix seconds the conflict-STOP marker inside `worktree_path` was written
/// at, or `None` when the marker is absent or its second line does not parse
/// as an ISO-8601 UTC timestamp.
fn read_conflict_marker_epoch(worktree_path: &Path) -> Option<i64> {
    let raw = std::fs::read_to_string(worktree_path.join(CONFLICT_MARKER_FILE)).ok()?;
    let timestamp = raw.lines().nth(1)?;
    parse_iso8601_utc_epoch(timestamp.trim())
}

/// The bare name of the repo's actual default branch, reusing
/// `cc::worktree::base_branch`'s `origin/HEAD`-then-common-names resolution
/// rather than reading whatever branch the calling worktree happens to be
/// on: the sweep can run from a feature branch (the normal state mid-Work
/// Unit), and comparing against that branch instead of the real default
/// would misjudge every named-branch and Wu-convention worktree.
fn default_branch(repo_root: &Path) -> Option<String> {
    crate::cc::worktree::base_branch(repo_root)
        .strip_prefix("origin/")
        .map(str::to_string)
}

/// The repo a judgment runs against and its default branch, resolved once.
struct RepoRef<'a> {
    root: &'a Path,
    default: Option<&'a str>,
    pr_lookup: Option<PrLookup<'a>>,
}

/// Whether judging a worktree of this kind compares against the default
/// branch (a review worktree is judged by its lock and age alone).
fn needs_default_branch(convention: Convention) -> bool {
    matches!(
        convention,
        Convention::Wu | Convention::AgentTool | Convention::CcLauncher
    )
}

/// Unix seconds `worktree_path`'s own linked-worktree `.git` file was
/// written, a stand-in for when the worktree itself was created: `git
/// worktree add` writes that file once and never touches it again, unlike
/// the checked-out commit's own date, which reflects whoever authored it
/// (often long before this worktree existed) and would make the never-locked
/// grace window in [`review_worktree_landed`] never actually apply.
fn worktree_created_epoch(worktree_path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(worktree_path.join(".git"))
        .ok()?
        .modified()
        .ok()?;
    crate::common::time::epoch_secs_of(modified)
}

/// Whether a worktree was found locked, and by which pid, per `git worktree
/// list --porcelain`'s own `locked` line for that entry.
struct LockState {
    is_locked: bool,
    owner_pid: Option<u32>,
    /// The `ts=<epoch>` the lock reason carries, when it has a readable one.
    ts: Option<i64>,
}

fn lock_state(entry: &WorktreeEntry) -> LockState {
    match &entry.lock_reason {
        Some(reason) => LockState {
            is_locked: true,
            owner_pid: parse_lock_pid(reason),
            ts: lock_ts(reason),
        },
        None => LockState {
            is_locked: false,
            owner_pid: None,
            ts: None,
        },
    }
}

/// The `ts=<epoch>` value inside a lock reason, if any.
fn lock_ts(reason: &str) -> Option<i64> {
    let rest = &reason[reason.find("ts=")? + 3..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// How old a review worktree is: from the lock's `ts=` when it has a readable
/// one, else from when the worktree was created. Zero when neither is known,
/// which keeps it.
pub fn review_age_secs(lock_ts: Option<i64>, created: Option<i64>, now_epoch: i64) -> i64 {
    lock_ts.or(created).map_or(0, |started| now_epoch - started)
}

/// A worktree younger than this is never treated as landed, whatever git says:
/// a brand new worktree sits at a commit that is already an ancestor of the
/// default branch, so "merged" is true before any work exists.
const FRESH_WORKTREE_GRACE_SECS: i64 = 3_600;

/// Whether `branch` has a commit of its own: its tip differs from the commit
/// it was created at, read from the oldest reflog entry. A branch with no
/// reflog (or an unreadable one) counts as having none, the safe answer,
/// because "no own commits" blocks the merged signal.
fn branch_has_own_commits(repo_root: &Path, branch: &str) -> bool {
    let reflog = git::output_slow(
        repo_root,
        &[
            "reflog",
            "show",
            "--format=%H",
            &format!("refs/heads/{branch}"),
        ],
    );
    let tip = git::output_slow(repo_root, &["rev-parse", &format!("refs/heads/{branch}")]);
    match (reflog, tip) {
        (Some(log), Some(tip)) => log
            .lines()
            .last()
            .is_some_and(|oldest| oldest.trim() != tip.trim()),
        _ => false,
    }
}

/// The reasons a worktree must not count as landed yet, however its commits
/// compare to the default branch: too new, or a named branch that has never
/// had a commit of its own and has not sat idle for [`STALE_AFTER_DAYS`].
fn too_new_to_be_landed(
    entry: &WorktreeEntry,
    repo_root: &Path,
    now_epoch: i64,
) -> Option<&'static str> {
    let age = worktree_created_epoch(&entry.path).map(|created| now_epoch - created);
    if age.is_some_and(|age| age < FRESH_WORKTREE_GRACE_SECS) {
        return Some("created less than an hour ago");
    }
    if let Some(branch) = entry.branch.as_deref() {
        if !branch_has_own_commits(repo_root, branch)
            && age.is_none_or(|age| age < STALE_AFTER_DAYS * SECS_PER_DAY)
        {
            return Some("its branch has no commits of its own yet");
        }
    }
    None
}

/// Applies whichever landed-signal function matches `convention`:
/// `wu_worktree_landed` for `Wu` (overridden by a present conflict-STOP
/// marker's grace period), `named_branch_landed` for `AgentTool`/
/// `CcLauncher`, and `review_worktree_landed` for `Review`.
/// `never_locked_grace_secs` guards a background scan against the narrow
/// add-then-lock race a review worktree can be caught in; it does not apply
/// when the caller named this exact path (`remove`), so that path passes 0
/// instead of [`NEVER_LOCKED_GRACE_SECS`].
fn is_landed(
    entry: &WorktreeEntry,
    convention: Convention,
    lock: &LockState,
    repo: &RepoRef<'_>,
    now_epoch: i64,
    never_locked_grace_secs: i64,
) -> bool {
    let (repo_root, default) = (repo.root, repo.default);
    if matches!(
        convention,
        Convention::Wu | Convention::AgentTool | Convention::CcLauncher
    ) && !entry.path.join(CONFLICT_MARKER_FILE).exists()
        && too_new_to_be_landed(entry, repo_root, now_epoch).is_some()
    {
        return false;
    }
    match convention {
        Convention::Wu => wu_landed(entry, repo_root, default, now_epoch),
        Convention::AgentTool | Convention::CcLauncher => match entry.branch.as_deref() {
            Some(branch) => match default {
                Some(default) => named_branch_landed(
                    branch,
                    default,
                    STALE_AFTER_DAYS,
                    repo_root,
                    now_epoch,
                    repo.pr_lookup.and_then(|lookup| lookup(repo_root, branch)),
                ),
                None => false,
            },
            None => false,
        },
        Convention::Review => {
            let worktree_age_secs =
                review_age_secs(lock.ts, worktree_created_epoch(&entry.path), now_epoch);
            review_worktree_landed(
                lock.is_locked,
                lock.owner_pid,
                worktree_age_secs,
                never_locked_grace_secs,
            )
        }
        Convention::Unmanaged => false,
    }
}

/// A present conflict-STOP marker overrides the normal commit-reachability
/// check with a grace period measured from the marker's own timestamp, using
/// this module's stale-after inclusive-boundary convention: the
/// boundary itself already counts as landed. A present-but-unparseable
/// marker fails closed (not landed) rather than falling through to the
/// reachability check: the marker's whole purpose is to stop that check
/// from firing, so a malformed timestamp must never silently defeat it.
fn wu_landed(
    entry: &WorktreeEntry,
    repo_root: &Path,
    default: Option<&str>,
    now_epoch: i64,
) -> bool {
    let marker_path = entry.path.join(CONFLICT_MARKER_FILE);
    if marker_path.exists() {
        return match read_conflict_marker_epoch(&entry.path) {
            Some(marker_epoch) => {
                let age = now_epoch - marker_epoch;
                age >= CONFLICT_GRACE_PERIOD_DAYS * SECS_PER_DAY
            }
            None => false,
        };
    }
    let (Some(head), Some(branch)) = (entry.head.as_deref(), default) else {
        return false;
    };
    wu_worktree_landed(head, branch, repo_root)
}

/// Removes `path` via `git worktree remove`, doubling `--force` when the
/// caller has already confirmed the lock is held by a dead process: a live
/// lock is never overridden, only a stale one left behind by a process that
/// no longer exists.
pub(crate) fn remove_worktree(
    repo_root: &Path,
    path: &Path,
    force_twice: bool,
) -> Result<(), String> {
    let path_str = path.to_string_lossy().into_owned();
    let mut args: Vec<&str> = vec!["worktree", "remove", "--force"];
    if force_twice {
        args.push("--force");
    }
    args.push(&path_str);
    match git::run(Some(repo_root), &args, git::SLOW_TIMEOUT) {
        Some(out) if out.status.success() => Ok(()),
        Some(out) => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        None => Err("git worktree remove timed out or failed to run".to_string()),
    }
}

/// Whether `worktree_path` has uncommitted changes: a plain `git status
/// --porcelain` from inside it, non-empty meaning dirty. `remove_worktree`
/// always passes `--force`, which overrides `git worktree remove`'s own
/// refusal to touch a dirty tree, so this check stands in for that refusal
/// before removal is even attempted: a landed commit-reachability check
/// says nothing about uncommitted work sitting on top of it, and a
/// just-created worktree mid-Work-Unit is landed (a commit is its own
/// ancestor) with its actual work still uncommitted.
fn worktree_is_dirty(worktree_path: &Path) -> bool {
    match git::output_slow(worktree_path, &["status", "--porcelain"]) {
        // The conflict-STOP marker is written directly into the worktree and
        // never committed, so its own status line must not count as dirty:
        // that would block removal once its own grace period expires, the
        // one thing the marker exists to allow.
        Some(status) => status
            .lines()
            .any(|line| line.get(3..) != Some(CONFLICT_MARKER_FILE)),
        // Can't confirm clean: treat as dirty rather than risk removing
        // work `git status` itself couldn't be asked about.
        None => true,
    }
}

/// What a report line says after `worktree <path>: `, one text per outcome.
pub(crate) const REPORT_NOT_LANDED: &str = "not landed, skipping";
pub(crate) const REPORT_IN_USE: &str = "a process is using it, skipping";
pub(crate) const REPORT_DIRTY: &str = "has uncommitted changes, skipping";
pub(crate) const REPORT_LOCK_NO_OWNER: &str = "locked with no readable owner pid, skipping";
pub(crate) const REPORT_LOCKED_LIVE: &str = "locked by a live process, skipping";
pub(crate) const REPORT_WOULD_REMOVE: &str = "would remove (dry run)";
pub(crate) const REPORT_REMOVED: &str = "removed";
/// The line for a repo whose `worktreeCleanup.enabled` is false.
pub(crate) const REPORT_CLEANUP_DISABLED: &str =
    "worktree remove: worktreeCleanup.enabled is false, nothing to do";

/// The tail of a report line for a worktree that was removed.
fn removed_suffix() -> String {
    format!(": {REPORT_REMOVED}")
}

/// The text of a report line for a removal that failed, after the path.
const REPORT_FAILED_MARKER: &str = ": failed to remove";

/// Says whether a process still has `path` open, kept behind a closure so a
/// test never runs the host `lsof`.
type InUse<'a> = &'a dyn Fn(&Path) -> bool;

/// Decides one worktree's fate and, unless `dry_run`, acts on it, returning
/// a report line: "not landed", "has uncommitted changes", "locked by a live
/// process", "would remove", or the outcome of the actual removal.
fn decide_and_report(
    entry: &WorktreeEntry,
    landed: bool,
    lock: &LockState,
    dry_run: bool,
    in_use: InUse<'_>,
    remove: Remover,
) -> String {
    let path = entry.path.display();
    if !landed {
        return format!("worktree {path}: {REPORT_NOT_LANDED}");
    }
    if in_use(&entry.path) {
        return format!("worktree {path}: {REPORT_IN_USE}");
    }
    if worktree_is_dirty(&entry.path) {
        return format!("worktree {path}: {REPORT_DIRTY}");
    }
    if lock.is_locked && lock.owner_pid.is_none() {
        return format!("worktree {path}: {REPORT_LOCK_NO_OWNER}");
    }
    let (should_remove, force_twice) = match (lock.is_locked, lock.owner_pid) {
        (false, _) => (true, false),
        (true, Some(pid)) if !pid_is_alive(pid) => (true, true),
        (true, _) => (false, false),
    };
    if !should_remove {
        return format!("worktree {path}: {REPORT_LOCKED_LIVE}");
    }
    if dry_run {
        return format!("worktree {path}: {REPORT_WOULD_REMOVE}");
    }
    match remove(&entry.path, force_twice) {
        Ok(()) => format!("worktree {path}: {REPORT_REMOVED}"),
        Err(err) => format!("worktree {path}: failed to remove: {err}"),
    }
}

/// Applies the same list -> classify -> check-landed -> check-lock -> decide
/// -> act sequence as [`sweep`], scoped to exactly the worktree entry at
/// `target_path` rather than every registered worktree, and with no
/// never-locked grace window (see [`is_landed`]): the caller named this path
/// directly, so the background-scan race that window guards against does not
/// apply. Errs if `target_path` is not itself a registered worktree,
/// comparing canonicalized paths (falling back to the raw path on either side
/// if it no longer exists, so an already-deleted-but-still-registered
/// worktree still matches) so a relative argument or a trailing slash cannot
/// cause a false "not found". Also errs on anything other than a clean
/// removal (not landed, dirty, locked, or a failed `git worktree remove`),
/// since unlike `sweep`'s per-entry report list, `remove` is a single
/// targeted command with no other channel to signal that nothing happened.
pub fn remove(
    repo_root: &Path,
    home: &Path,
    repo_slug: Option<&str>,
    target_path: &Path,
    now_epoch: i64,
) -> Result<String, String> {
    match remove_with(
        repo_root,
        home,
        repo_slug,
        target_path,
        now_epoch,
        Some(&pr_state::gh_pr_state),
        &crate::cc::worktree::is_in_use,
    )? {
        Removal::Nothing(text) => Ok(text),
        Removal::Removed(report) => Ok(report),
    }
}

/// The outcome of a [`remove_with`] that did not fail.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Removal {
    /// Nothing was removed: cleanup is turned off, or the path is still there
    /// after the removal ran. Carries the text to show.
    Nothing(String),
    /// The worktree is gone and its data with it; carries the report line.
    Removed(String),
}

/// [`remove`] with the pull request lookup and the in-use check passed in, so
/// a caller or test decides what `gh` and `lsof` would have said.
pub(crate) fn remove_with(
    repo_root: &Path,
    home: &Path,
    repo_slug: Option<&str>,
    target_path: &Path,
    now_epoch: i64,
    lookup: Option<PrLookup<'_>>,
    in_use: InUse<'_>,
) -> Result<Removal, String> {
    let probes = Probes { lookup, in_use };
    let target = Target {
        repo_root,
        home,
        repo_slug,
        path: target_path,
    };
    let (entry, report) = decide_target_with(&target, now_epoch, false, &probes)?;
    let Some(entry) = entry else {
        return Ok(Removal::Nothing(report));
    };
    if report.ends_with(&removed_suffix()) {
        if entry.path.exists() {
            return Ok(Removal::Nothing(report));
        }
        remove_data_of(repo_root, home, repo_slug, &entry.path);
        Ok(Removal::Removed(report))
    } else {
        Err(report.trim_start_matches("worktree ").to_string())
    }
}

/// What [`remove`] would do with `target_path`, as one report line, with
/// nothing removed: "would remove (dry run)", the guard that keeps it, or why
/// it is not a worktree the sweep manages.
pub(crate) fn would_remove(
    repo_root: &Path,
    home: &Path,
    repo_slug: Option<&str>,
    target_path: &Path,
    now_epoch: i64,
) -> String {
    would_remove_with(
        repo_root,
        home,
        repo_slug,
        target_path,
        now_epoch,
        Some(&pr_state::gh_pr_state),
        &crate::cc::worktree::is_in_use,
    )
}

/// [`would_remove`] with the pull request lookup and the in-use check passed
/// in.
pub(crate) fn would_remove_with(
    repo_root: &Path,
    home: &Path,
    repo_slug: Option<&str>,
    target_path: &Path,
    now_epoch: i64,
    lookup: Option<PrLookup<'_>>,
    in_use: InUse<'_>,
) -> String {
    let probes = Probes { lookup, in_use };
    let target = Target {
        repo_root,
        home,
        repo_slug,
        path: target_path,
    };
    match decide_target_with(&target, now_epoch, true, &probes) {
        Ok((_, report)) => report,
        Err(err) => err,
    }
}

/// The worktree a single-target command names, and where to look for it.
struct Target<'a> {
    repo_root: &'a Path,
    home: &'a Path,
    repo_slug: Option<&'a str>,
    path: &'a Path,
}

/// The two outside facts a verdict reads: the pull request state of a branch
/// and whether a process uses a worktree.
struct Probes<'a> {
    lookup: Option<PrLookup<'a>>,
    in_use: InUse<'a>,
}

/// The shared core of [`remove`] and [`would_remove`]: finds the entry, then
/// decides and, unless `dry_run`, acts. `None` for the entry means cleanup is
/// turned off and the line says so.
fn decide_target_with(
    target: &Target<'_>,
    now_epoch: i64,
    dry_run: bool,
    probes: &Probes<'_>,
) -> Result<(Option<WorktreeEntry>, String), String> {
    let Target {
        repo_root,
        home,
        repo_slug,
        path: target_path,
    } = *target;
    let policy = resolve_policy(home, repo_slug).map_err(|err| err.to_string())?;
    if !policy.enabled {
        return Ok((None, REPORT_CLEANUP_DISABLED.to_string()));
    }
    let entries = list_worktrees(repo_root)?;
    let target = target_path
        .canonicalize()
        .unwrap_or_else(|_| target_path.to_path_buf());
    let entry = entries
        .into_iter()
        .find(|entry| {
            entry
                .path
                .canonicalize()
                .unwrap_or_else(|_| entry.path.clone())
                == target
        })
        .ok_or_else(|| format!("{} is not a registered git worktree", target_path.display()))?;

    let convention = classify(&entry.path, home);
    if convention == Convention::Unmanaged {
        return Err(format!(
            "{} is not a playbook-managed worktree",
            target_path.display()
        ));
    }
    let lock = lock_state(&entry);
    let default = needs_default_branch(convention)
        .then(|| default_branch(repo_root))
        .flatten();
    let repo = RepoRef {
        root: repo_root,
        default: default.as_deref(),
        pr_lookup: probes.lookup,
    };
    let landed = is_landed(&entry, convention, &lock, &repo, now_epoch, 0);
    let report = decide_and_report(
        &entry,
        landed,
        &lock,
        dry_run,
        probes.in_use,
        &|path, force_twice| remove_worktree(repo_root, path, force_twice),
    );
    Ok((Some(entry), report))
}

/// Appends one line per removal to `~/.config/playbook/worktree-sweep.log`
/// so a user can see what the sweep took and why. Best effort: a write
/// failure never stops the sweep.
fn log_removal(home: &Path, now_epoch: i64, line: &str) {
    use std::io::Write;
    let dir = home.join(".config").join("playbook");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("worktree-sweep.log"))
    {
        let _ = writeln!(file, "{now_epoch} {line} (landed, clean and idle)");
    }
}

/// Scans every worktree registered against `repo_root`, classifies each,
/// applies the matching landed-signal check, and removes what has landed
/// and is not locked by a live process, per the sequence: list, classify,
/// check landed (conflict marker first for `Wu`), check lock, decide, act.
/// A named branch counts as landed when its pull request is merged (read with
/// `gh`), which catches squash merges that git history cannot see.
///
/// This only reports. The gc worktree store is the one caller that removes, so
/// every removal goes through its guard.
pub fn sweep_dry_run(
    repo_root: &Path,
    home: &Path,
    repo_slug: Option<&str>,
    now_epoch: i64,
) -> Result<Vec<String>, String> {
    sweep_with(
        repo_root,
        home,
        repo_slug,
        true,
        now_epoch,
        Some(&pr_state::gh_pr_state),
    )
}

/// What a sweep may do beyond judging.
#[derive(Clone, Copy)]
pub struct SweepOpts<'a> {
    pub dry_run: bool,
    pub now_epoch: i64,
    /// Judge only the worktrees below this folder.
    pub only_under: Option<&'a Path>,
}

/// Removes one worktree: the path and whether the lock needs a doubled
/// `--force`.
pub type Remover<'a> = &'a dyn Fn(&Path, bool) -> Result<(), String>;

/// [`sweep_dry_run`] with the pull request lookup injected and removal done
/// by `git worktree remove` directly; `None` judges by local git history
/// alone. For tests; production removal goes through [`sweep_removing`].
pub fn sweep_with(
    repo_root: &Path,
    home: &Path,
    repo_slug: Option<&str>,
    dry_run: bool,
    now_epoch: i64,
    pr_lookup: Option<PrLookup<'_>>,
) -> Result<Vec<String>, String> {
    sweep_removing(
        (repo_root, home, repo_slug),
        SweepOpts {
            dry_run,
            now_epoch,
            only_under: None,
        },
        pr_lookup,
        &|path, force_twice| remove_worktree(repo_root, path, force_twice),
    )
}

/// The sweep proper: every removal goes through `remove`, which the caller
/// supplies so it can refuse or log.
pub fn sweep_removing(
    (repo_root, home, repo_slug): (&Path, &Path, Option<&str>),
    opts: SweepOpts<'_>,
    pr_lookup: Option<PrLookup<'_>>,
    remove: Remover<'_>,
) -> Result<Vec<String>, String> {
    let SweepOpts {
        dry_run,
        now_epoch,
        only_under,
    } = opts;
    let policy = resolve_policy(home, repo_slug).map_err(|err| err.to_string())?;
    if !policy.enabled {
        return Ok(vec![
            "worktree sweep: worktreeCleanup.enabled is false, nothing to do".to_string(),
        ]);
    }

    let entries = list_worktrees(repo_root)?;
    // `repo_root` is the caller's own cwd, which may itself be a linked
    // worktree (the eager trigger this backs runs from inside the
    // just-created worktree, not the main checkout), so skipping only
    // position 0 (the main worktree, per `git worktree list`'s documented
    // ordering) is not enough to protect it.
    let caller_worktree = repo_root.canonicalize().ok();
    // The first entry is always the main worktree; sweeping it would
    // destroy the caller's own checkout.
    let candidates: Vec<(WorktreeEntry, Convention)> = entries
        .into_iter()
        .skip(1)
        .filter(|entry| caller_worktree.as_deref() != entry.path.canonicalize().ok().as_deref())
        .filter(|entry| only_under.is_none_or(|base| entry.path.starts_with(base)))
        .filter_map(|entry| {
            let convention = classify(&entry.path, home);
            (convention != Convention::Unmanaged).then_some((entry, convention))
        })
        .collect();

    // Judging a worktree only reads git state, and each judgment waits on a
    // few `git` children, so judge them side by side. The default branch is
    // resolved once for all of them. Acting (dirty check, removal, log) stays
    // sequential and in list order, because `git worktree remove` takes locks
    // and the report order must not change.
    let default = candidates
        .iter()
        .any(|(_, convention)| needs_default_branch(*convention))
        .then(|| default_branch(repo_root))
        .flatten();
    let repo = RepoRef {
        root: repo_root,
        default: default.as_deref(),
        pr_lookup,
    };
    let judged = crate::common::par::map(
        &candidates,
        crate::common::par::MAX_CONCURRENT,
        |(entry, convention)| {
            let lock = lock_state(entry);
            let landed = is_landed(
                entry,
                *convention,
                &lock,
                &repo,
                now_epoch,
                NEVER_LOCKED_GRACE_SECS,
            );
            (lock, landed)
        },
    );
    let mut report = Vec::new();
    for ((entry, _), (lock, landed)) in candidates.iter().zip(judged) {
        let line = decide_and_report(
            entry,
            landed,
            &lock,
            dry_run,
            &crate::cc::worktree::is_in_use,
            remove,
        );
        if !dry_run && (line.ends_with(&removed_suffix()) || line.contains(REPORT_FAILED_MARKER)) {
            log_removal(home, now_epoch, &line);
        }
        if !dry_run && line.ends_with(&removed_suffix()) {
            remove_data_of(repo_root, home, repo_slug, &entry.path);
        }
        report.push(line);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// The time of every fixture commit but the feature one, which is dated to
    /// its worktree's creation. No test reads the clock for a commit age.
    const FIXTURE_COMMIT_EPOCH: i64 = 1_700_000_000;
    const BRANCH: &str = "feat/landed-by-squash";
    const SECS_PER_TEST_DAY: i64 = 86_400;
    /// Past the one hour grace but inside `staleAfterDays` (30 by default).
    const TEN_DAYS: i64 = 10 * SECS_PER_TEST_DAY;
    /// Past `staleAfterDays`.
    const FORTY_DAYS: i64 = 40 * SECS_PER_TEST_DAY;
    /// Inside the one hour grace.
    const TEN_MINUTES: i64 = 600;

    static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn merged(_: &Path, _: &str) -> Option<PrState> {
        Some(PrState::Merged)
    }
    fn open(_: &Path, _: &str) -> Option<PrState> {
        Some(PrState::Open)
    }
    fn idle(_: &Path) -> bool {
        false
    }
    fn busy(_: &Path) -> bool {
        true
    }

    fn git_in(dir: &Path, args: &[&str]) {
        git_at(dir, args, FIXTURE_COMMIT_EPOCH);
    }

    fn git_at(dir: &Path, args: &[&str], epoch: i64) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_DATE", format!("{epoch} +0000"))
            .env("GIT_COMMITTER_DATE", format!("{epoch} +0000"))
            .output()
            .expect("git should spawn");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repository with a bare `origin`, and an agent-tool convention
    /// worktree on a branch with one commit of its own.
    struct Fixture {
        root: PathBuf,
        repo: PathBuf,
        home: PathBuf,
        worktree: PathBuf,
        /// The branch's last commit time, in Unix seconds.
        commit_epoch: i64,
    }

    impl Fixture {
        fn new() -> Self {
            let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "playbook-decide-target-{}-{n}",
                crate::testing::run_id()
            ));
            std::fs::create_dir_all(&root).expect("create fixture root");
            let root = root.canonicalize().expect("fixture root resolves");
            let (bare, repo, home) = (
                root.join("origin.git"),
                root.join("repo"),
                root.join("home"),
            );
            for dir in [&bare, &repo, &home] {
                std::fs::create_dir_all(dir).expect("create fixture dir");
            }
            git_in(&bare, &["init", "-q", "--bare", "-b", "main"]);
            git_in(&repo, &["init", "-q", "-b", "main"]);
            git_in(
                &repo,
                &["remote", "add", "origin", bare.to_str().expect("utf8")],
            );
            std::fs::write(repo.join("README.md"), "seed\n").expect("seed file");
            git_in(&repo, &["add", "."]);
            git_in(&repo, &["commit", "-q", "-m", "seed"]);
            git_in(&repo, &["push", "-q", "-u", "origin", "main"]);
            git_in(&repo, &["remote", "set-head", "origin", "main"]);

            let worktree = repo.join(".claude").join("worktrees").join("agent-landed");
            std::fs::create_dir_all(worktree.parent().expect("parent")).expect("mkdir");
            git_in(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    BRANCH,
                    worktree.to_str().expect("utf8"),
                    "main",
                ],
            );
            std::fs::write(worktree.join("feature.txt"), "work\n").expect("feature file");
            // The grace period reads the folder's creation time, so the
            // feature commit is dated to that same instant: its age is then
            // exactly the offset a test adds to `commit_epoch`.
            let commit_epoch = worktree_created_epoch(&worktree).expect("creation time reads");
            git_in(&worktree, &["add", "."]);
            git_at(&worktree, &["commit", "-q", "-m", "feature"], commit_epoch);
            let worktree = worktree.canonicalize().expect("worktree resolves");
            Self {
                root,
                repo,
                home,
                worktree,
                commit_epoch,
            }
        }

        /// Lands the branch the way a squash merge does: the default branch
        /// gets a different commit, so the branch tip never becomes reachable.
        fn squash_merge(&self) {
            std::fs::write(self.repo.join("feature.txt"), "work, squashed\n").expect("squash file");
            git_in(&self.repo, &["add", "feature.txt"]);
            git_in(&self.repo, &["commit", "-q", "-m", "squash"]);
            git_in(&self.repo, &["push", "-q", "origin", "main"]);
        }

        /// Lands the branch with a fast-forward, so `git branch --merged` sees it.
        fn fast_forward_merge(&self) {
            git_in(&self.repo, &["merge", "-q", "--ff-only", BRANCH]);
            git_in(&self.repo, &["push", "-q", "origin", "main"]);
        }

        fn dry_run(
            &self,
            now_epoch: i64,
            lookup: Option<PrLookup<'_>>,
            in_use: InUse<'_>,
        ) -> String {
            would_remove_with(
                &self.repo,
                &self.home,
                None,
                &self.worktree,
                now_epoch,
                lookup,
                in_use,
            )
        }

        fn line(&self, text: &str) -> String {
            format!("worktree {}: {text}", self.worktree.display())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn decide_target_with_calls_a_squash_merged_branch_landed() {
        // Arrange
        let f = Fixture::new();
        f.squash_merge();
        let now = f.commit_epoch + TEN_DAYS;

        // Act
        let line = f.dry_run(now, Some(&merged), &idle);

        // Assert
        assert_eq!(line, f.line(REPORT_WOULD_REMOVE));
    }

    #[test]
    fn an_open_pr_keeps_the_branch_even_past_the_stale_age() {
        // Arrange
        let f = Fixture::new();
        let now = f.commit_epoch + FORTY_DAYS;

        // Act
        let line = f.dry_run(now, Some(&open), &idle);

        // Assert
        assert_eq!(line, f.line(REPORT_NOT_LANDED));
    }

    #[test]
    fn no_lookup_keeps_a_squash_merged_branch() {
        // Arrange
        let f = Fixture::new();
        f.squash_merge();
        let now = f.commit_epoch + TEN_DAYS;

        // Act
        let line = f.dry_run(now, None, &idle);

        // Assert
        assert_eq!(line, f.line(REPORT_NOT_LANDED));
    }

    #[test]
    fn with_no_pr_the_old_local_verdicts_hold() {
        // Arrange: (merged by git, seconds past the last commit, expected line)
        let landed = "would remove (dry run)";
        let kept = "not landed, skipping";
        let cases = [
            (true, TEN_DAYS, landed),
            (true, TEN_MINUTES, kept),
            (false, TEN_DAYS, kept),
            (false, TEN_MINUTES, kept),
            (false, FORTY_DAYS, landed),
        ];
        for (is_merged, after_commit, expected) in cases {
            let f = Fixture::new();
            if is_merged {
                f.fast_forward_merge();
            }

            // Act
            let line = f.dry_run(f.commit_epoch + after_commit, None, &idle);

            // Assert
            assert_eq!(
                line,
                f.line(expected),
                "merged={is_merged} after={after_commit}"
            );
        }
    }

    #[test]
    fn remove_with_a_merged_pr_deletes_a_squash_merged_worktree() {
        // Arrange
        let f = Fixture::new();
        f.squash_merge();

        // Act
        let result = remove_with(
            &f.repo,
            &f.home,
            None,
            &f.worktree,
            f.commit_epoch + TEN_DAYS,
            Some(&merged),
            &idle,
        );

        // Assert
        assert_eq!(result, Ok(Removal::Removed(f.line(REPORT_REMOVED))));
        assert!(!f.worktree.exists());
    }

    #[test]
    fn remove_with_cleanup_disabled_does_nothing() {
        // Arrange
        let f = Fixture::new();
        crate::config::write::set(
            crate::config::write::Tier::Global,
            "worktreeCleanup.enabled",
            &serde_json::Value::Bool(false),
            &f.home,
            None,
        )
        .expect("disable cleanup");

        // Act
        let result = remove_with(
            &f.repo,
            &f.home,
            None,
            &f.worktree,
            f.commit_epoch + FORTY_DAYS,
            Some(&merged),
            &idle,
        );

        // Assert
        assert_eq!(
            result,
            Ok(Removal::Nothing(REPORT_CLEANUP_DISABLED.to_string()))
        );
        assert!(f.worktree.exists());
    }

    #[test]
    fn remove_with_an_unregistered_path_names_it() {
        // Arrange
        let f = Fixture::new();
        let stranger = f.root.join("not-a-worktree");

        // Act
        let result = remove_with(
            &f.repo,
            &f.home,
            None,
            &stranger,
            f.commit_epoch,
            None,
            &idle,
        );

        // Assert
        assert_eq!(
            result,
            Err(format!(
                "{} is not a registered git worktree",
                stranger.display()
            ))
        );
    }

    #[test]
    fn a_worktree_in_use_is_kept_and_an_idle_one_is_not() {
        // Arrange
        let f = Fixture::new();
        f.squash_merge();
        let now = f.commit_epoch + TEN_DAYS;

        // Act
        let in_use = f.dry_run(now, Some(&merged), &busy);
        let idle_line = f.dry_run(now, Some(&merged), &idle);

        // Assert
        assert_eq!(in_use, f.line(REPORT_IN_USE));
        assert_eq!(idle_line, f.line(REPORT_WOULD_REMOVE));
    }

    #[test]
    fn the_sweep_thresholds_are_built_in_whatever_is_stored() {
        // Arrange
        let home = crate::testing::scratch_root("worktree-thresholds-fixed");
        let root = crate::common::paths::playbook_root_from(&home);
        for key in [
            "worktreeCleanup.staleAfterDays",
            "worktreeCleanup.conflictGracePeriodDays",
        ] {
            crate::config::store::put(
                &root,
                crate::config::write::Tier::Global,
                None,
                key,
                &serde_json::json!(3),
            )
            .unwrap();
        }

        // Act
        let policy = resolve_policy(&home, None).unwrap();

        // Assert
        assert!(policy.enabled);
        assert_eq!(STALE_AFTER_DAYS, 30);
        assert_eq!(CONFLICT_GRACE_PERIOD_DAYS, 90);
    }
}
