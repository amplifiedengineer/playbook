// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Decision and path logic for `playbook cc launch worktree`, ported from the retired shell launcher.
//!
//! Final slice of WU-17: every decision and git-driving action `_wt_main`
//! takes is now ported. What is left outside this file is CLI wiring:
//! argument parsing, message and exit-code presentation, how [`housekeep`]
//! gets backgrounded, and the `--ai-resolve` spawn path (see their own doc
//! comments for why each is deliberately deferred rather than guessed at).

use crate::common::git;
use crate::common::run_with_timeout;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

/// For the helper tools (`bash`, `lsof`, `gh`) this file runs besides git.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Never collapse worktrees into the repo root, which `WORKTREE_BASE_DIR="."`
/// would otherwise do.
const DEFAULT_BASE_DIR: &str = ".worktrees";

#[derive(Debug, PartialEq)]
pub enum ConflictAction {
    Spawn,
    Abort,
}

/// What to do when a rebase conflicts and AI-resolve is available.
///
/// Silent mode spawns without asking. Otherwise only an interactive terminal
/// may consent, and a non-tty aborts rather than assuming yes: the alternative
/// is an unattended run rewriting someone's conflicts.
pub fn conflict_action(silent: bool, is_tty: bool, answer: &str) -> ConflictAction {
    if silent {
        return ConflictAction::Spawn;
    }
    if !is_tty {
        return ConflictAction::Abort;
    }
    // Empty means the user pressed return on a default-yes prompt.
    match answer.trim().to_ascii_lowercase().as_str() {
        "" | "y" | "yes" => ConflictAction::Spawn,
        _ => ConflictAction::Abort,
    }
}

/// The directory holding this repo's worktrees: `<base>/<repo>`.
///
/// A relative base sits under the repo's parent, an absolute one is used as is.
/// The `<repo>` leaf is what stops same-named branches in sibling repos from
/// colliding.
pub fn resolve_base(repo_root: &Path, repo_parent: &Path, configured: Option<&str>) -> PathBuf {
    let repo_name = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let base = match configured {
        Some(b) if !b.is_empty() && b != "." => b,
        _ => DEFAULT_BASE_DIR,
    };

    if base.starts_with('/') {
        Path::new(base).join(repo_name)
    } else {
        repo_parent.join(base).join(repo_name)
    }
}

/// Where the `.env` lives, relative to the repo root, or `None`.
///
/// `Some(".")` means the repo root itself. Without an explicit hint it looks
/// one level down, matching the shell's `find -mindepth 2 -maxdepth 2`.
pub fn find_env_base(repo_root: &Path, hint: Option<&str>) -> Option<String> {
    if let Some(hint) = hint.filter(|h| !h.is_empty()) {
        if hint == "." && repo_root.join(".env").is_file() {
            return Some(".".to_string());
        }
        if repo_root.join(hint).join(".env").is_file() {
            return Some(hint.to_string());
        }
        return None;
    }

    if repo_root.join(".env").is_file() {
        return Some(".".to_string());
    }

    // One level down only, and sorted so the answer does not depend on
    // readdir order the way the shell's `head -n1` did.
    let mut candidates: Vec<String> = std::fs::read_dir(repo_root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join(".env").is_file())
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect();
    candidates.sort();
    candidates.into_iter().next()
}

#[derive(Debug, PartialEq)]
pub enum EnvCopy {
    Copied(PathBuf),
    NoEnvConfigured,
    SourceMissing,
    /// The reason this guard exists. Reported, never silent.
    RefusedNotGitignored(String),
}

/// Copies the `.env` into a new worktree, but ONLY when it is gitignored in the
/// source repo.
///
/// This is a secret-leak control, not a convenience. A tracked `.env` copied
/// into a fresh worktree is a file `git add` will happily stage, so an
/// unignored one is refused and the refusal is reported. No-clobber: an
/// existing destination file is left alone.
pub fn copy_env(repo_root: &Path, dest: &Path, env_base: Option<&str>) -> EnvCopy {
    let Some(env_base) = env_base.filter(|b| !b.is_empty()) else {
        return EnvCopy::NoEnvConfigured;
    };
    let rel = if env_base == "." {
        ".env".to_string()
    } else {
        format!("{env_base}/.env")
    };
    let src = repo_root.join(&rel);
    if !src.is_file() {
        return EnvCopy::SourceMissing;
    }
    if !is_gitignored(repo_root, &rel) {
        return EnvCopy::RefusedNotGitignored(rel);
    }

    let target = dest.join(&rel);
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if target.exists() {
        // cp -n: an existing file is never overwritten.
        return EnvCopy::Copied(target);
    }
    match std::fs::copy(&src, &target) {
        Ok(_) => EnvCopy::Copied(target),
        Err(_) => EnvCopy::SourceMissing,
    }
}

/// `git check-ignore -q`, where exit 0 means the path is ignored.
///
/// A git failure or timeout reads as NOT ignored, which refuses the copy. That
/// direction is deliberate: the failure mode of guessing wrong here is staging
/// a secret.
fn is_gitignored(repo_root: &Path, rel: &str) -> bool {
    git::ok(repo_root, &["check-ignore", "-q", rel])
}

/// Branches tried when the remote publishes no `origin/HEAD`, in order.
const BASE_BRANCH_CANDIDATES: [&str; 4] = ["main", "master", "trunk", "develop"];

/// Last resort when the remote is unreachable and no candidate exists locally.
const BASE_BRANCH_FALLBACK: &str = "master";

const SECS_PER_DAY: i64 = 86_400;

/// Cleanup runs at most this often per repo. Kept as its own name so callers
/// reference the rate limit rather than SECS_PER_DAY directly: retuning this
/// must not move the staleness cutoff, which is a separate decision that
/// happens to share the same number today.
const CLEANUP_INTERVAL_SECS: i64 = SECS_PER_DAY;

/// The base branch as a remote-tracking ref, for example `origin/main`.
///
/// Prefers what the remote itself publishes as `origin/HEAD`, then the common
/// names, so a repo whose default is `trunk` is not silently rebased onto a
/// `main` that does not exist.
pub fn base_branch(repo_root: &Path) -> String {
    let published = git::raw(
        repo_root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .map(|s| s.trim().trim_start_matches("origin/").to_string())
    .filter(|s| !s.is_empty());

    if let Some(name) = published {
        return format!("origin/{name}");
    }

    for candidate in BASE_BRANCH_CANDIDATES {
        let reference = format!("refs/remotes/origin/{candidate}");
        if git::ok(repo_root, &["show-ref", "--verify", "--quiet", &reference]) {
            return format!("origin/{candidate}");
        }
    }
    format!("origin/{BASE_BRANCH_FALLBACK}")
}

/// `_wt_main`'s own fallback (worktree.sh:272) when `origin/HEAD` is
/// unpublished. Named separately from [`BASE_BRANCH_FALLBACK`] on purpose:
/// same string today, but it belongs to a different function with a
/// different contract (see [`main_base_ref`]'s doc), and this constant
/// exists so that stays true even if the two values are ever tuned apart.
const MAIN_BASE_REF_FALLBACK: &str = "master";

/// The bare base branch name `_wt_main` computes directly (worktree.sh:271-272),
/// e.g. `main`, never `origin/main`.
///
/// THIS IS A SECOND, DIFFERENTLY-SHAPED NOTION OF "THE BASE BRANCH," and the
/// difference is deliberate, not an oversight to reconcile. [`base_branch`]
/// (used by `_wt_make`, worktree.sh:72-88) returns a remote-tracking ref
/// PREFIXED with `origin/`, and falls back through four candidate names
/// (`main`, `master`, `trunk`, `develop`) before settling on `origin/master`.
/// This function returns the BARE name with no prefix, checks only
/// `origin/HEAD`, and on that being unpublished falls straight to a bare
/// `master` with no candidate loop at all. The two shapes are consumed
/// differently too: `_wt_main` builds `"$REMOTE/$BASE_REF"` itself from this
/// bare value (worktree.sh:274, and again by `_wt_maybe_rebase`), whereas
/// `base_branch`'s result is already a complete ref. Do not unify the two or
/// route this through `base_branch` plus a prefix strip: that would also
/// wrongly pull in the four-candidate fallback this one never had.
pub fn main_base_ref(repo_root: &Path) -> String {
    git::raw(
        repo_root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .map(|s| s.trim().trim_start_matches("origin/").to_string())
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| MAIN_BASE_REF_FALLBACK.to_string())
}

/// Fetches `base_ref` and `branch` from `remote` before `_wt_main` decides
/// how to source the worktree (worktree.sh:274-275).
///
/// Falls back to fetching `base_ref` alone when the combined fetch fails,
/// since a brand-new branch that does not exist on the remote yet fails the
/// combined form, but the base ref still needs fetching regardless. The
/// whole call tolerates failure (the shell's trailing `|| true`): an
/// unreachable remote just means the rest of the run works with whatever
/// refs already exist locally. Shares [`REBASE_TIMEOUT`] rather than the 5s
/// `GIT_TIMEOUT` other calls in this file use, since this is a network fetch
/// like `rebase_onto`'s, not a local read.
pub fn initial_fetch(repo_root: &Path, remote: &str, base_ref: &str, branch: &str) {
    let combined = ["fetch", remote, "--quiet", base_ref, branch];
    if git::run(Some(repo_root), &combined, REBASE_TIMEOUT).is_some_and(|o| o.status.success()) {
        return;
    }
    let _ = git::run(
        Some(repo_root),
        &["fetch", remote, "--quiet", base_ref],
        REBASE_TIMEOUT,
    );
}

/// Everything the staleness decision needs, gathered by the caller so the
/// decision itself stays pure and exhaustively testable.
pub struct WorktreeStatus<'a> {
    pub path: &'a Path,
    pub branch: &'a str,
    /// The worktree this run is creating, which must never be reaped.
    pub is_target: bool,
    /// Something is cwd'd into it.
    pub in_use: bool,
    pub has_open_pr: bool,
    pub merged_into_base: bool,
    /// Unix seconds of the last commit.
    pub last_commit_epoch: i64,
}

/// Whether a worktree may be removed.
///
/// The four skips come first and are absolute: the target of this run, anything
/// in use, a branch with an open pull request, and a detached or unreadable
/// HEAD. Only then does merged-or-old decide. Ordering matters, since an old
/// branch with an open PR is still wanted.
pub fn is_stale(status: &WorktreeStatus, now_epoch: i64) -> bool {
    if status.is_target || status.in_use || status.has_open_pr || status.branch.is_empty() {
        return false;
    }
    if status.merged_into_base {
        return true;
    }
    let cutoff = now_epoch - (crate::worktree::STALE_AFTER_DAYS * SECS_PER_DAY);
    status.last_commit_epoch < cutoff
}

/// Whether the daily cleanup is due, given the marker's mtime.
///
/// `None` means the marker is absent, which counts as due: a repo that has
/// never been cleaned should be.
pub fn cleanup_due(marker_mtime_epoch: Option<i64>, now_epoch: i64) -> bool {
    match marker_mtime_epoch {
        None => true,
        Some(stamped) => now_epoch - stamped >= CLEANUP_INTERVAL_SECS,
    }
}

/// Directory and filename prefix for the per-repo cleanup rate-limit marker
/// (worktree.sh:203): `/tmp/.git-wt-cleanup-<hash>`.
const CLEANUP_MARKER_DIR: &str = "/tmp";
const CLEANUP_MARKER_PREFIX: &str = ".git-wt-cleanup-";

/// Directory and filename prefix for the per-repo fetch-cache marker
/// (worktree.sh:270): `/tmp/.git-fetch-<hash>`. This slice only computes
/// where the marker lives; touching it belongs to the background
/// housekeeping block (`touch "$FETCH_CACHE"`, worktree.sh:373), a later
/// slice's job.
const FETCH_CACHE_MARKER_DIR: &str = "/tmp";
const FETCH_CACHE_MARKER_PREFIX: &str = ".git-fetch-";

/// Ports `_wt_cleanup_stale` (worktree.sh:201-242): the outer entry point.
///
/// Gathers the two inputs that reach outside this process, a real
/// `/tmp/.git-wt-cleanup-*` marker path and a real `gh` call, then delegates
/// the actual git-driving work to [`cleanup_stale_with`]. That split exists
/// for testability: [`cleanup_stale_with`] is what the test suite calls
/// directly, with a scratch marker and an injected PR list, so no test
/// depends on `gh` being installed and authenticated or touches this
/// machine's real markers.
///
/// RETIRED as the active cleanup path: [`crate::gc::worktrees::sweep_locked`] is what
/// production now calls. Nothing in this binary calls this function; kept
/// only for the tests exercising [`cleanup_stale_with`] directly.
pub fn cleanup_stale(repo_root: &Path, target: &Path, now_epoch: i64) -> usize {
    let Some(marker) = cleanup_marker_path(repo_root) else {
        return 0;
    };
    let open_prs = open_pr_branches(repo_root);
    cleanup_stale_with(repo_root, target, &marker, &open_prs, now_epoch)
}

/// Ports the loop body of `_wt_cleanup_stale` (worktree.sh:201-242): given a
/// marker path and an open-PR list, decides and executes removals. Returns
/// how many worktrees were removed.
///
/// THIS FUNCTION DELETES BRANCHES AND WORKTREES. Every skip in [`is_stale`]
/// is a safety control, not a nicety, and the ordering below, remove first
/// and only delete the branch once that succeeds, is one too: a branch is
/// never destroyed while its worktree still exists on disk.
pub fn cleanup_stale_with(
    repo_root: &Path,
    target: &Path,
    marker: &Path,
    open_prs: &[String],
    now_epoch: i64,
) -> usize {
    if !cleanup_due(marker_mtime_epoch(marker), now_epoch) {
        return 0;
    }
    // Touched BEFORE any removal work (worktree.sh:206): a crash partway
    // through still rate-limits the next run, rather than retrying the same
    // destructive pass on every invocation until one finally finishes.
    touch_marker(marker, now_epoch);

    let base = base_branch(repo_root);
    let merged = merged_branches(repo_root, &base);

    let Some(porcelain) = git::raw(repo_root, &["worktree", "list", "--porcelain"]) else {
        return 0;
    };

    let mut removed = 0;
    for path in cleanup_candidates(&porcelain) {
        let wt_path = Path::new(&path);
        let branch = git::raw(wt_path, &["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let commit_epoch = git::raw(wt_path, &["log", "-1", "--format=%ct"])
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);

        let status = WorktreeStatus {
            path: wt_path,
            branch: &branch,
            is_target: wt_path == target,
            in_use: is_in_use(wt_path),
            has_open_pr: contains_line(open_prs, &branch),
            merged_into_base: contains_line(&merged, &branch),
            last_commit_epoch: commit_epoch,
        };
        if !is_stale(&status, now_epoch) {
            continue;
        }

        if !git::ok(repo_root, &["worktree", "remove", "--force", path.as_str()]) {
            // The shell's `|| continue` (worktree.sh:236): a failed remove
            // must never be followed by a branch delete, so a branch is
            // never destroyed while its worktree still exists on disk.
            continue;
        }
        removed += 1;
        if branch != "HEAD" {
            let _ = git::ok(repo_root, &["branch", "-D", branch.as_str()]);
        }
    }

    let _ = git::ok(repo_root, &["worktree", "prune"]);
    removed
}

/// The default cleanup rate-limit marker path for `repo_root`
/// (worktree.sh:203). `None` when the hash could not be computed at all (no
/// `bash`, or it timed out).
///
/// Divergence: the shell in that state still proceeds, with an un-suffixed,
/// effectively repo-agnostic marker (`/tmp/.git-wt-cleanup-`). This port
/// instead treats an unknown hash as "cannot safely rate-limit" and skips the
/// run entirely, per this file's "when in doubt, keep" rule for anything
/// that decides what gets deleted.
fn cleanup_marker_path(repo_root: &Path) -> Option<PathBuf> {
    hashed_marker_path(CLEANUP_MARKER_DIR, CLEANUP_MARKER_PREFIX, repo_root)
}

/// `<dir>/<prefix><hash-of-repo_root>`, the shape both the cleanup marker
/// above and the fetch-cache marker below share. Factored out so the two
/// stay byte-for-byte consistent rather than each hand-rolling its own
/// `format!`; behaviour of the existing cleanup marker is unchanged, only
/// where its path construction lives.
fn hashed_marker_path(dir: &str, prefix: &str, repo_root: &Path) -> Option<PathBuf> {
    let hash = wt_hash(&repo_root.to_string_lossy())?;
    Some(Path::new(dir).join(format!("{prefix}{hash}")))
}

/// The fetch-cache marker path `_wt_main` computes at worktree.sh:270. See
/// [`FETCH_CACHE_MARKER_DIR`]'s doc for why nothing reads or writes it yet.
pub fn fetch_cache_marker_path(repo_root: &Path) -> Option<PathBuf> {
    hashed_marker_path(FETCH_CACHE_MARKER_DIR, FETCH_CACHE_MARKER_PREFIX, repo_root)
}

/// The shell's exact `_wt_hash` body (worktree.sh:69). `<<<` is a bash/zsh
/// here-string, so this runs under `bash`, matching this file's own
/// "sourceable in bash and zsh" contract, not `/bin/sh` (often dash on
/// Linux, which rejects `<<<`).
///
/// macOS's `md5 -q` prints the full 32-character digest; the Linux fallback
/// pipes through `cut -c1-8` and keeps only 8, so the two platforms produce
/// cache filenames of different lengths for the same repo. That quirk is
/// preserved on purpose, not normalized away.
const WT_HASH_SCRIPT: &str =
    r#"printf '%s\n' "$1" | md5 -q 2>/dev/null || md5sum <<< "$1" | cut -c1-8"#;

/// Shells out to compute the shell's cache-marker hash of `input`, rather
/// than hashing in Rust: the crate graph is deliberately clap + serde only,
/// with no hashing crate, and shelling out guarantees byte-for-byte parity
/// with the shell, digest length included (see [`WT_HASH_SCRIPT`]'s doc). If
/// this port and a still-installed shell ever computed the hash differently,
/// they would write to two different marker files for the same repo during
/// the coexistence period, and the once-a-day cleanup would effectively run
/// twice as often, once per implementation.
fn wt_hash(input: &str) -> Option<String> {
    let mut command = Command::new("bash");
    command.arg("-c").arg(WT_HASH_SCRIPT).arg("bash").arg(input);
    let out = run_with_timeout(&mut command, GIT_TIMEOUT)?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Unix seconds of `marker`'s mtime, or `None` when it does not exist or is
/// unreadable. Feeds directly into [`cleanup_due`], which already treats
/// `None` as due, so this does not need to replicate the shell's
/// `stat || stat || echo 0` fallback chain (worktree.sh:204): a missing
/// marker and an unreadable one both mean "we do not know when this last
/// ran," and that reads as due either way.
fn marker_mtime_epoch(marker: &Path) -> Option<i64> {
    let modified = std::fs::metadata(marker).ok()?.modified().ok()?;
    Some(
        modified
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    )
}

/// Updates `marker`'s mtime, creating it first if absent, matching the
/// shell's `touch "$cache"` (worktree.sh:206). Writes the current epoch
/// rather than merely opening the file: POSIX only guarantees an mtime bump
/// on `truncate` when the size actually changes, but a `write()` updates it
/// unconditionally, so writing is the reliable "touch."
fn touch_marker(marker: &Path, now_epoch: i64) {
    let _ = std::fs::write(marker, now_epoch.to_string());
}

/// The `worktree` field paths from `git worktree list --porcelain`, in
/// listed order. The first is always the main worktree.
fn worktree_paths(porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .filter_map(|line| line.strip_prefix("worktree ").map(str::to_string))
        .collect()
}

/// The worktree paths eligible for cleanup: every entry from
/// `worktree_paths` EXCEPT the first, which is always the main worktree
/// (worktree.sh:218's `awk '$1=="worktree" && c++>0{print $2}'`) and must
/// never be a cleanup candidate. Getting this off by one would delete the
/// user's main checkout.
///
/// Pulled out as its own function, rather than an inline `.skip(1)` in
/// [`cleanup_stale_with`], so this exact decision has a direct unit test: a
/// live-git test cannot tell this bug apart from correct behaviour, since
/// `git worktree remove` already refuses to remove the main working tree on
/// its own, no matter what this function passes it.
pub fn cleanup_candidates(porcelain: &str) -> Vec<String> {
    worktree_paths(porcelain).into_iter().skip(1).collect()
}

/// The main worktree's path: the FIRST entry from `git worktree list
/// --porcelain` (worktree.sh:258's `awk '$1=="worktree" && !f{print $2;
/// f=1}'`), what `_wt_main` `cd`'s into before anything else.
///
/// The exact counterpart of [`cleanup_candidates`]: one keeps the first
/// entry, the other drops it, and both are built on the same
/// [`worktree_paths`] so a change to how paths are parsed cannot put the
/// pair out of sync with each other.
pub fn main_worktree(porcelain: &str) -> Option<String> {
    worktree_paths(porcelain).into_iter().next()
}

/// `git branch --merged <base>` (worktree.sh:213), with the shell's
/// `sed 's/^[[:space:]*+]*//'` prefix-strip applied: `*` marks the branch
/// checked out in the worktree this command ran from, `+` marks one checked
/// out in another linked worktree, and either can mix with leading spaces.
pub(crate) fn merged_branches(repo_root: &Path, base: &str) -> Vec<String> {
    git::raw(repo_root, &["branch", "--merged", base])
        .map(|out| {
            out.lines()
                .map(|line| strip_branch_marker(line).to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn strip_branch_marker(line: &str) -> &str {
    line.trim_start_matches(|c: char| c.is_whitespace() || c == '*' || c == '+')
}

/// Whole-line, fixed-string membership, matching the shell's `grep -qxF`
/// (`-x` whole line, `-F` fixed string): worktree.sh:225 (open PRs) and
/// worktree.sh:228 (merged branches). A substring match here would spare or
/// reap the wrong branch, e.g. an open-PR entry of `feat-two` must not spare
/// a branch named `feat`.
pub(crate) fn contains_line(haystack: &[String], needle: &str) -> bool {
    !needle.is_empty() && haystack.iter().any(|line| line == needle)
}

/// Whether some process's cwd is inside `path`, mirroring `lsof -d cwd +c0`
/// piped to a plain, non-anchored `grep -qF` (worktree.sh:220). Absent
/// `lsof` reads as "not in use": the shell's own `command -v lsof` gate means
/// a machine without it never treats any worktree as busy.
pub(crate) fn is_in_use(path: &Path) -> bool {
    if !command_exists("lsof") {
        return false;
    }
    let mut command = Command::new("lsof");
    command.args(["-d", "cwd", "+c0"]);
    let Some(out) = run_with_timeout(&mut command, GIT_TIMEOUT) else {
        return false;
    };
    let path_str = path.to_string_lossy();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|line| line.contains(path_str.as_ref()))
}

/// The current user's open PR head branches (worktree.sh:214-216), or empty
/// when `gh` is absent or the call fails. Gathered here, in the outer
/// [`cleanup_stale`], rather than in [`cleanup_stale_with`], so tests can
/// inject the list directly instead of depending on `gh` being installed and
/// authenticated.
fn open_pr_branches(repo_root: &Path) -> Vec<String> {
    if !command_exists("gh") {
        return Vec::new();
    }
    let mut command = Command::new("gh");
    command.current_dir(repo_root).args([
        "pr",
        "list",
        "--state",
        "open",
        "--author",
        "@me",
        "--limit",
        "200",
        "--json",
        "headRefName",
        "--jq",
        ".[].headRefName",
    ]);
    match run_with_timeout(&mut command, GIT_TIMEOUT) {
        Some(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether the new worktree can reuse the main checkout's `node_modules`.
///
/// Requires a `package.json` in the worktree, and a lockfile plus installed
/// modules in the source, then that the two lockfiles are byte-identical.
///
/// The shell hashed both lockfiles with sha256sum or shasum and compared the
/// digests, which also meant skipping the reuse entirely when neither tool was
/// present. Comparing the bytes answers the same question, is strictly
/// stronger than a digest, and drops both the external dependency and that
/// silent skip.
pub fn node_modules_reusable(worktree: &Path, repo_root: &Path) -> bool {
    if !worktree.join("package.json").is_file() {
        return false;
    }
    let source_lock = repo_root.join("package-lock.json");
    if !source_lock.is_file() || !repo_root.join("node_modules").is_dir() {
        return false;
    }

    // A worktree without its own lockfile inherits the source's, which is the
    // copy the shell made before comparing, so that case matches by definition.
    let worktree_lock = worktree.join("package-lock.json");
    if !worktree_lock.is_file() {
        return true;
    }
    match (std::fs::read(&source_lock), std::fs::read(&worktree_lock)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// A `node_modules` tree can hold tens of thousands of files, and the copy
/// ladder's last resort (`cp -R`, once copy-on-write and reflink both fail)
/// walks every one of them. Generous on purpose: a short timeout here would
/// abort a real, still-progressing copy rather than a stuck one.
const NODE_MODULES_COPY_TIMEOUT: Duration = Duration::from_secs(120);

/// Bounds the best-effort `npm install --prefer-offline` refresh after
/// cloning. It only has to reconcile an already-installed tree rather than
/// fetch one from scratch, so this is far shorter than the copy timeout: a
/// machine with no network should fail fast instead of stalling worktree
/// setup.
const NPM_INSTALL_TIMEOUT: Duration = Duration::from_secs(15);

/// Ports the imperative half of `_wt_node_modules` (worktree.sh:174-197):
/// once [`node_modules_reusable`] says the reuse is safe, this performs it.
///
/// Order mirrors the shell with one deliberate exception. The shell seeds a
/// missing worktree lockfile BEFORE its hash comparison runs (worktree.sh:184);
/// this instead decides first and seeds only when the decision is `true`.
/// The two orders are provably equivalent: `node_modules_reusable` already
/// re-checks every precondition the shell checks before its own seed line, so
/// a `false` decision means the shell would never have reached that line
/// either, and a `true` decision comes only from a lockfile that already
/// matches (nothing to seed) or one that is missing (seeded now, and a copy
/// of an identical file always matches). Deciding first also means a
/// worktree that fails a precondition is never touched, which a literal
/// seed-first port would get wrong for a worktree with no `package.json` at
/// all: the shell returns before ever reaching its seed line, but seeding
/// unconditionally here would still write a lockfile into a non-node
/// worktree.
///
/// The shell's diagnostic ("Cloned node_modules (copy-on-write)") is left to
/// the caller, since printing is not this module's job; that message also
/// claims copy-on-write unconditionally even when the ladder below fell
/// through to the plain `cp -R`, a pre-existing inaccuracy this port does not
/// fix. The best-effort `npm install --prefer-offline --no-audit --no-fund`
/// refresh that follows it is kept, since it changes what ends up on disk
/// rather than just announcing it, and the shell runs it regardless of
/// whether the copy ladder actually succeeded.
pub fn reuse_node_modules(worktree: &Path, repo_root: &Path) -> bool {
    if !node_modules_reusable(worktree, repo_root) {
        return false;
    }

    let worktree_lock = worktree.join("package-lock.json");
    if !worktree_lock.is_file() {
        // Best-effort, matching the shell's `2>/dev/null || true`: a failed
        // copy must not undo a reuse that was otherwise judged safe.
        let _ = std::fs::copy(repo_root.join("package-lock.json"), &worktree_lock);
    }

    let source = repo_root.join("node_modules");
    let dest = worktree.join("node_modules");
    // `-e`, not `-d` (worktree.sh:191): a plain FILE named `node_modules` is
    // removed too, not just a stray directory.
    remove_node_modules(&dest);

    let copied = clone_node_modules(&source, &dest);

    if command_exists("npm") {
        let mut command = Command::new("npm");
        command.current_dir(worktree).args([
            "install",
            "--prefer-offline",
            "--no-audit",
            "--no-fund",
        ]);
        let _ = run_with_timeout(&mut command, NPM_INSTALL_TIMEOUT);
    }

    copied
}

/// The shell's three-tier `cp` fallback (worktree.sh:192-194): a macOS
/// copy-on-write clone, then a GNU reflink copy, then a plain recursive copy.
/// Each retry removes whatever the previous attempt left behind first, since
/// the shell repeats `rm -rf node_modules` before every retry, not just once:
/// a `cp` that fails partway can still leave a partial destination that would
/// make the next attempt fail on "already exists" instead of actually
/// retrying.
fn clone_node_modules(source: &Path, dest: &Path) -> bool {
    if run_cp(&["-cR"], source, dest) {
        return true;
    }
    remove_node_modules(dest);
    if run_cp(&["-R", "--reflink=auto"], source, dest) {
        return true;
    }
    remove_node_modules(dest);
    run_cp(&["-R"], source, dest)
}

/// One `cp` attempt from the ladder above.
///
/// The shell suppresses stderr on the first two tiers and lets the third
/// flow to the terminal, so a real failure is visible only once the cheaper
/// fallbacks are exhausted. This port has no output channel of its own here
/// (`run_with_timeout` always captures both streams and nothing forwards
/// them), so that distinction has no observable effect in this function;
/// noted rather than silently dropped.
fn run_cp(args: &[&str], source: &Path, dest: &Path) -> bool {
    let mut command = Command::new("cp");
    command.args(args).arg(source).arg(dest);
    matches!(
        run_with_timeout(&mut command, NODE_MODULES_COPY_TIMEOUT),
        Some(o) if o.status.success()
    )
}

/// Removes whatever is at `path`, mirroring `rm -rf`: a no-op when nothing is
/// there, and correct for either a directory or a plain file.
fn remove_node_modules(path: &Path) {
    if path.is_dir() {
        let _ = std::fs::remove_dir_all(path);
    } else if path.exists() {
        let _ = std::fs::remove_file(path);
    }
}

/// A stand-in for the shell's `command -v name`: true when spawning `name
/// --version` succeeds at all. `--version` is near-universal and returns
/// almost instantly, so this shares the install's timeout rather than
/// needing its own.
fn command_exists(name: &str) -> bool {
    let mut command = Command::new(name);
    command.arg("--version");
    run_with_timeout(&mut command, NPM_INSTALL_TIMEOUT).is_some()
}

/// Whether the main worktree is dirty enough to need an auto-stash, the
/// gate at worktree.sh:278.
///
/// Two checks, not one: `git diff-index --quiet HEAD --` catches changes
/// against HEAD (staged or unstaged), and `git diff --quiet` catches changes
/// against the index. Either alone would still flag a plain unstaged edit,
/// but only the first sees a staged-but-uncommitted change whose working
/// tree content happens to already match the index, so collapsing to one
/// check would miss that case.
pub fn needs_stash(repo_root: &Path) -> bool {
    !git::ok(repo_root, &["diff-index", "--quiet", "HEAD", "--"])
        || !git::ok(repo_root, &["diff", "--quiet"])
}

/// The message `_wt_main` tags its auto-stash with (worktree.sh:279), so
/// `restore_stash`'s pop targets exactly the entry this pushed.
const AUTO_STASH_MESSAGE: &str = "worktree: auto-stash";

/// Auto-stashes a dirty main worktree, worktree.sh:279's
/// `git stash push -m "worktree: auto-stash" --quiet`. Returns whether a
/// stash was actually created.
///
/// `git stash push` exits 0 even when there is nothing to stash ("No local
/// changes to save" is not an error to git), so a bare exit-code check would
/// wrongly report success on a clean tree. That never happens in the shell,
/// which only calls this after [`needs_stash`] has already confirmed the
/// tree is dirty, but this function is a public, independently callable
/// port, and its return value is exactly what tells a caller whether a later
/// `git stash pop` (see [`restore_stash`]) is popping a stash this call
/// pushed versus an unrelated one that predates it. So this compares the
/// stash count before and after, and only reports success when it actually
/// grew.
pub fn auto_stash(repo_root: &Path) -> bool {
    let before = stash_count(repo_root);
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo_root)
        .args(["stash", "push", "-m", AUTO_STASH_MESSAGE, "--quiet"]);
    let succeeded =
        matches!(run_with_timeout(&mut command, GIT_TIMEOUT), Some(o) if o.status.success());
    succeeded && stash_count(repo_root) > before
}

fn stash_count(repo_root: &Path) -> usize {
    git::raw(repo_root, &["stash", "list"])
        .map(|out| out.lines().filter(|line| !line.is_empty()).count())
        .unwrap_or(0)
}

/// Ports `_wt_restore_stash` (worktree.sh:248-251): pops the auto-stash taken
/// on the main worktree before this run began, best-effort. `stash_applied`
/// stands in for the shell's `STASH_APPLIED` global, passed in explicitly
/// rather than read from process state.
pub fn restore_stash(main_worktree: &Path, stash_applied: bool) {
    if !stash_applied {
        return;
    }
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(main_worktree)
        .args(["stash", "pop", "--quiet"]);
    // The shell's `|| true`: a failed pop (nothing to pop, a conflict) is not
    // this function's problem to report.
    let _ = run_with_timeout(&mut command, GIT_TIMEOUT);
}

/// The worktree path already checked out on `branch`, from
/// `git worktree list --porcelain`.
///
/// The recovery path when `git worktree add` fails: a branch can only live in
/// one worktree, so the usual cause is that it is already checked out
/// somewhere, and reporting that beats a bare failure.
pub fn worktree_for_branch(porcelain: &str, branch: &str) -> Option<String> {
    let wanted = format!("refs/heads/{branch}");
    let mut current: Option<&str> = None;
    for line in porcelain.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(path);
        } else if let Some(found) = line.strip_prefix("branch ") {
            if found == wanted {
                return current.map(|p| p.to_string());
            }
        }
    }
    None
}

/// The `git worktree add` argument list, in the shell's exact order.
///
/// The shell builds `[-b <new_branch>] <dest> <ref>` once (worktree.sh:104-106),
/// then only prepends `-f` on the retry (`git worktree add -f
/// "${cmd_args[@]}"`, worktree.sh:111), so `-f` lands BEFORE `-b`, not after.
/// A line-for-line port exists precisely so this order is not left to guessing.
pub fn worktree_add_args(
    dest: &Path,
    reference: &str,
    new_branch: Option<&str>,
    force: bool,
) -> Vec<OsString> {
    let mut args = Vec::new();
    if force {
        args.push(OsString::from("-f"));
    }
    if let Some(branch) = named_branch(new_branch) {
        args.push(OsString::from("-b"));
        args.push(OsString::from(branch));
    }
    args.push(dest.as_os_str().to_os_string());
    args.push(OsString::from(reference));
    args
}

/// The branch name for the fallback lookup once both `git worktree add`
/// attempts fail: the shell's `refs/heads/${new_branch:-$ref}`
/// (worktree.sh:113), minus the `refs/heads/` prefix that `worktree_for_branch`
/// already adds.
///
/// When a new branch is being created, that new branch is what to look up.
/// Otherwise the REFERENCE ITSELF is looked up as though it were a local
/// branch name, which only resolves when it happens to name one, e.g. a plain
/// branch rather than a remote-tracking ref like `origin/main`. That mismatch
/// is a genuine quirk of the shell, preserved here deliberately rather than
/// fixed.
pub fn fallback_lookup_branch<'a>(reference: &'a str, new_branch: Option<&'a str>) -> &'a str {
    named_branch(new_branch).unwrap_or(reference)
}

/// Collapses an empty branch name to `None`.
///
/// Both shell tests that read `$new_branch` treat empty and unset as the same
/// thing: `[[ -n "$new_branch" ]]` gates the `-b` flag, and `${new_branch:-$ref}`
/// falls back on empty as well as unset. `Option<&str>` draws that line
/// differently, so without this an empty name would pass `-b ""` to git and
/// look up an empty branch, neither of which the shell can do.
fn named_branch(new_branch: Option<&str>) -> Option<&str> {
    new_branch.filter(|b| !b.is_empty())
}

/// Outcome of [`create_worktree`].
#[derive(Debug, PartialEq)]
pub enum CreateOutcome {
    /// A fresh worktree was created at the given path.
    Created(PathBuf),
    /// Nothing was created: the wanted branch was already checked out at the
    /// given path, discovered by the fallback lookup.
    AlreadyAt(PathBuf),
    /// Both `git worktree add` attempts failed and no fallback path resolved.
    Failed,
}

/// Creates a worktree, recovering from a stale registration if needed.
///
/// Mirrors the shell's three-tier ladder in `_wt_create_worktree`
/// (worktree.sh:102-124): try a plain `git worktree add`; on failure run
/// `worktree prune` then `worktree repair`, each allowed to fail (the shell's
/// `|| true`) since they are best-effort recovery, not preconditions; retry
/// with `-f`; and on a second failure, fall back to whatever worktree already
/// holds the branch via [`worktree_for_branch`], only accepting it when that
/// worktree's directory still exists (the shell's `-d "$existing"`).
///
/// The shell prints two diagnostics here ("branch already at $existing", "git
/// worktree add failed"); this function returns the outcome instead and
/// leaves emitting messages to the caller, so that omission is a documented
/// divergence rather than a silently dropped one.
pub fn create_worktree(
    repo_root: &Path,
    dest: &Path,
    reference: &str,
    new_branch: Option<&str>,
) -> CreateOutcome {
    if run_worktree_add(repo_root, dest, reference, new_branch, false) {
        return CreateOutcome::Created(dest.to_path_buf());
    }

    let _ = git::ok(repo_root, &["worktree", "prune"]);
    let _ = git::ok(repo_root, &["worktree", "repair"]);

    if run_worktree_add(repo_root, dest, reference, new_branch, true) {
        return CreateOutcome::Created(dest.to_path_buf());
    }

    let Some(porcelain) = git::raw(repo_root, &["worktree", "list", "--porcelain"]) else {
        return CreateOutcome::Failed;
    };
    let lookup_branch = fallback_lookup_branch(reference, new_branch);
    if let Some(existing) = worktree_for_branch(&porcelain, lookup_branch) {
        let path = PathBuf::from(existing);
        if path.is_dir() {
            return CreateOutcome::AlreadyAt(path);
        }
    }
    CreateOutcome::Failed
}

/// What a new worktree checks out, and whether that means creating a branch.
#[derive(Debug, PartialEq)]
pub struct MakePlan {
    /// The ref `git worktree add` checks out.
    pub reference: String,
    /// `Some` when the branch does not exist yet and `-b` must create it.
    pub new_branch: Option<String>,
    /// Whether to clear the new branch's inherited upstream afterwards.
    pub unset_upstream: bool,
}

/// Ports the branch-source choice in `_wt_make` (worktree.sh:390-401): check
/// out an existing local branch, else start one from its remote counterpart,
/// else start one from the base ref.
///
/// Only the third case unsets the upstream (worktree.sh:398). Branching off
/// the base ref makes git inherit the BASE's tracking, so without this a later
/// bare `git push` on the new branch would target the base branch. The other
/// two cases already have correct tracking, so clearing it there would break
/// them.
pub fn make_plan(
    branch: &str,
    remote: &str,
    base: &str,
    local_exists: bool,
    remote_exists: bool,
) -> MakePlan {
    if local_exists {
        return MakePlan {
            reference: branch.to_string(),
            new_branch: None,
            unset_upstream: false,
        };
    }
    if remote_exists {
        return MakePlan {
            reference: format!("refs/remotes/{remote}/{branch}"),
            new_branch: Some(branch.to_string()),
            unset_upstream: false,
        };
    }
    MakePlan {
        reference: base.to_string(),
        new_branch: Some(branch.to_string()),
        unset_upstream: true,
    }
}

/// Creates the worktree for `branch`, choosing its source via [`make_plan`].
///
/// The two `show-ref --verify` probes use fully qualified refs so a tag or a
/// remote ref sharing the branch's name cannot be mistaken for a local branch.
fn make_worktree(repo_root: &Path, target: &Path, branch: &str, remote: &str) -> CreateOutcome {
    let local_exists = git::ok(
        repo_root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    );
    let remote_exists = git::ok(
        repo_root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{remote}/{branch}"),
        ],
    );
    let plan = make_plan(
        branch,
        remote,
        &base_branch(repo_root),
        local_exists,
        remote_exists,
    );

    let outcome = create_worktree(
        repo_root,
        target,
        &plan.reference,
        plan.new_branch.as_deref(),
    );

    // Only after a successful create, since the shell's `|| return $?` means a
    // failed create never reaches the unset.
    if plan.unset_upstream && !matches!(outcome, CreateOutcome::Failed) {
        let _ = git::ok(repo_root, &["branch", "--unset-upstream", branch]);
    }
    outcome
}

/// Runs `git worktree add` with [`worktree_add_args`], reporting only success.
fn run_worktree_add(
    repo_root: &Path,
    dest: &Path,
    reference: &str,
    new_branch: Option<&str>,
    force: bool,
) -> bool {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo_root).arg("worktree").arg("add");
    command.args(worktree_add_args(dest, reference, new_branch, force));
    matches!(run_with_timeout(&mut command, GIT_TIMEOUT), Some(o) if o.status.success())
}

/// What the launcher found at the target path, gathered by the caller so the
/// decision stays pure.
pub struct TargetState<'a> {
    pub target_exists: bool,
    /// Registered with `git worktree list`, as opposed to a stray directory.
    pub registered: bool,
    /// `None` means detached or unreadable HEAD.
    pub current_branch: Option<&'a str>,
    pub wanted_branch: &'a str,
    /// Whether the branch currently checked out there still exists on the remote.
    pub current_branch_on_remote: bool,
    /// A different worktree already holding the wanted branch, and whether its
    /// directory is still present.
    pub existing_for_wanted: Option<(&'a str, bool)>,
}

#[derive(Debug, PartialEq)]
pub enum WorktreePlan {
    /// Detached HEAD in a registered worktree: abort any rebase or merge and
    /// check the branch back out. The caller re-evaluates afterwards, since
    /// recovery can fail and fall through to a rebuild.
    RecoverDetached,
    /// A directory git does not know about. Prune, remove, recreate.
    CleanOrphanAndCreate,
    /// Already on the wanted branch: fetch and fast-forward in place.
    ReuseTarget,
    /// The occupying branch still exists on the remote, so it is unfinished
    /// work. Refuse rather than destroy it.
    RefuseOccupied(String),
    /// The occupying branch is gone from the remote, so it was merged or
    /// deleted and the worktree can be recycled.
    RecycleTarget(String),
    /// The wanted branch is checked out in another worktree that still exists.
    ReuseExisting(String),
    /// Git has a worktree registered for the branch but the directory is gone.
    PruneStaleAndCreate,
    Create,
}

/// Chooses the recovery path for the target worktree.
///
/// The ordering is the whole point and mirrors the shell. In particular
/// `RefuseOccupied` comes before `RecycleTarget`: recycling removes the
/// worktree and deletes its branch, so a branch that still exists on the remote
/// must stop the operation rather than be destroyed. Getting that pair the
/// wrong way round would silently discard unfinished work.
pub fn plan_for_target(state: &TargetState) -> WorktreePlan {
    if state.target_exists {
        return match (state.current_branch, state.registered) {
            (None, true) => WorktreePlan::RecoverDetached,
            (None, false) => WorktreePlan::CleanOrphanAndCreate,
            (Some(current), _) if current == state.wanted_branch => WorktreePlan::ReuseTarget,
            (Some(current), _) if state.current_branch_on_remote => {
                WorktreePlan::RefuseOccupied(current.to_string())
            }
            (Some(current), _) => WorktreePlan::RecycleTarget(current.to_string()),
        };
    }

    match state.existing_for_wanted {
        Some((path, true)) => WorktreePlan::ReuseExisting(path.to_string()),
        Some((_, false)) => WorktreePlan::PruneStaleAndCreate,
        None => WorktreePlan::Create,
    }
}

/// Minimum letters in a JIRA project key, so a branch like `a-1` is not
/// mistaken for one.
const MIN_JIRA_PREFIX_LEN: usize = 2;

/// Trims whitespace and carriage returns from a branch name.
///
/// The CR matters: a branch name pasted from a Windows-authored ticket or a CI
/// variable carries one, and git rejects it while the error names a branch that
/// looks identical to the one requested.
pub fn sanitize_branch(raw: &str) -> String {
    raw.replace('\r', "").trim().to_string()
}

/// Whether `branch` is a syntactically valid git branch name:
/// `git check-ref-format --branch <branch>` (worktree.sh:263).
///
/// Delegates to git rather than hand-rolling the ref-name grammar (no
/// leading `-`, no `..`, no trailing `/`, no embedded whitespace, and more):
/// git owns those rules, and a re-implementation here would just be a second
/// copy of them to keep in sync.
pub fn valid_branch_name(repo_root: &Path, branch: &str) -> bool {
    git::ok(repo_root, &["check-ref-format", "--branch", branch])
}

/// The first JIRA-style key in the branch, uppercased.
///
/// Matches the shell's case-insensitive `[A-Z]{2,}-[0-9]+`, so `fix/abc-45`
/// yields `ABC-45`. Scanned by hand rather than with a regex, since the crate
/// graph is deliberately clap plus serde.
pub fn jira_key(branch: &str) -> Option<String> {
    let bytes = branch.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let letters = i - start;
        if letters < MIN_JIRA_PREFIX_LEN || i >= bytes.len() || bytes[i] != b'-' {
            continue;
        }
        let dash = i;
        i += 1;
        let digits_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i > digits_start {
            return Some(branch[start..i].to_uppercase());
        }
        i = dash + 1;
    }
    None
}

/// Everything after the final slash, matching `${BRANCH##*/}`.
pub fn branch_leaf(branch: &str) -> &str {
    branch.rsplit('/').next().unwrap_or(branch)
}

/// The folder name a branch gets under the worktree root.
///
/// A JIRA key wins, so `feature/PROJ-1/spike` and `fix/PROJ-1` share one
/// folder, which is the point: one ticket, one worktree. `occupied_by` is the
/// branch already checked out in that key's folder, if any. When it is a
/// DIFFERENT branch the key is already taken, so this falls back to the leaf
/// rather than colliding.
pub fn folder_for_branch(branch: &str, occupied_by: Option<&str>) -> String {
    match jira_key(branch) {
        Some(key) => match occupied_by {
            Some(other) if other != branch => branch_leaf(branch).to_string(),
            _ => key,
        },
        None => branch_leaf(branch).to_string(),
    }
}

/// Branches that never auto-rebase, even when you authored their last commit.
///
/// Without this list, the ownership heuristic below would happily rebase
/// `develop` or a `release/*` onto base, because whoever committed last looks
/// like the owner. Shared history is not personal history.
/// Slices rather than fixed-size arrays so adding a name stays a one-line edit
/// with no length to keep in sync.
const PROTECTED_BRANCHES: &[&str] = &[
    "main", "master", "trunk", "develop", "dev", "staging", "release", "hotfix",
];

/// Prefixes whose whole namespace is protected. `release` and `hotfix` appear
/// in both lists: bare, and as the root of a namespace.
const PROTECTED_PREFIXES: &[&str] = &["release/", "hotfix/"];

fn is_protected(branch: &str) -> bool {
    PROTECTED_BRANCHES.contains(&branch) || PROTECTED_PREFIXES.iter().any(|p| branch.starts_with(p))
}

/// What the rebase decision needs, gathered by the caller.
pub struct RebaseContext<'a> {
    pub current_branch: &'a str,
    /// `git config user.name`, empty when unset.
    pub git_user: &'a str,
    /// Author of the branch's last commit.
    pub branch_author: &'a str,
    /// GitHub login, empty when `gh` is absent or unauthenticated.
    pub gh_user: &'a str,
    /// The branch the launcher was asked for, which may embed the login.
    pub wanted_branch: &'a str,
    pub base_ref: &'a str,
}

/// Whether this branch may be auto-rebased onto base.
///
/// Protection is checked FIRST and is absolute. Only then does ownership apply,
/// via either the last commit's author or a login embedded in the branch name.
/// Rebasing rewrites history, so every uncertain case declines.
pub fn should_rebase(ctx: &RebaseContext) -> bool {
    if is_protected(ctx.current_branch) {
        return false;
    }
    // Detached HEAD, or already sitting on base: nothing to rebase onto.
    if ctx.current_branch == ctx.base_ref || ctx.current_branch == "HEAD" {
        return false;
    }

    let authored_by_me = !ctx.git_user.is_empty() && ctx.branch_author == ctx.git_user;
    let named_for_me = !ctx.gh_user.is_empty() && ctx.wanted_branch.contains(ctx.gh_user);
    authored_by_me || named_for_me
}

/// The `git rebase` argument list, worktree.sh:427-429.
///
/// `--rebase-merges` is appended only when the branch actually carries merge
/// commits against upstream: rewriting merge topology a linear history
/// doesn't have would be pointless churn. Argument ORDER matches the shell's
/// array construction exactly.
pub fn rebase_args(upstream: &str, has_merge_commits: bool) -> Vec<&str> {
    let mut args = vec![upstream, "--quiet"];
    if has_merge_commits {
        args.push("--rebase-merges");
    }
    args
}

/// How long `git fetch` and `git rebase` may run before being killed.
///
/// Both touch the network or replay an unbounded number of commits, unlike
/// every other git call in this file, which only reads local state; the 5s
/// `GIT_TIMEOUT` is sized for that, not this.
///
/// Divergence: the shell caps neither, so any value here can cut short a call
/// the shell would let finish. Sized high because the failure mode is quiet
/// rather than loud: a timed-out fetch is indistinguishable from a failed
/// one, and both are tolerated, so the rebase would then run against a STALE
/// `<remote>/<base_ref>`. A cap tight enough to fire on a large repo or a
/// slow link would silently rebase onto the wrong base. This is a bound on
/// genuinely hung processes only, not a latency budget.
const REBASE_TIMEOUT: Duration = Duration::from_secs(300);

/// Outcome of attempting to bring the current branch up to date with base.
#[derive(Debug, PartialEq)]
pub enum RebaseOutcome {
    /// HEAD already contains `<remote>/<base_ref>`; nothing was rebased.
    UpToDate,
    /// The rebase completed cleanly.
    Rebased,
    /// The rebase failed and is STILL IN PROGRESS on disk.
    ///
    /// Mirrors the instant worktree.sh:431 finds `git rebase` failed: at that
    /// point the shell has not yet decided between spawning the AI resolver
    /// and aborting, so this port stops at the same spot and hands that
    /// decision to the caller. A caller that drops this outcome on the floor
    /// leaves the worktree mid-rebase, an actually broken state rather than a
    /// resolved one; it MUST eventually call [`abort_rebase`], directly or
    /// after a failed resolution attempt.
    Conflicted,
}

/// Fetches `base_ref` from `remote` and rebases the current branch onto it,
/// worktree.sh:424-431.
///
/// A failed fetch (offline, unreachable remote) is not fatal: the shell's
/// `|| true` lets the ancestry check and rebase proceed against whatever
/// `<remote>/<base_ref>` already exists locally.
pub fn rebase_onto(worktree: &Path, remote: &str, base_ref: &str) -> RebaseOutcome {
    let mut fetch = Command::new("git");
    fetch
        .arg("-C")
        .arg(worktree)
        .args(["fetch", remote, base_ref, "--quiet"]);
    let _ = run_with_timeout(&mut fetch, REBASE_TIMEOUT);

    let upstream = format!("{remote}/{base_ref}");
    if git::ok(
        worktree,
        &["merge-base", "--is-ancestor", &upstream, "HEAD"],
    ) {
        return RebaseOutcome::UpToDate;
    }

    let range = format!("{upstream}..HEAD");
    let has_merge_commits = git::raw(worktree, &["log", "--merges", "--oneline", &range])
        .is_some_and(|out| !out.trim().is_empty());

    let mut rebase = Command::new("git");
    rebase
        .arg("-C")
        .arg(worktree)
        .arg("rebase")
        .args(rebase_args(&upstream, has_merge_commits));
    match run_with_timeout(&mut rebase, REBASE_TIMEOUT) {
        Some(out) if out.status.success() => RebaseOutcome::Rebased,
        _ => RebaseOutcome::Conflicted,
    }
}

/// Aborts an in-progress rebase.
///
/// Matches the shell's `git rebase --abort 2>/dev/null || true`, used at all
/// three of its call sites: failing because there was no rebase in progress
/// is not an error worth reporting.
pub fn abort_rebase(worktree: &Path) {
    let mut command = Command::new("git");
    command.arg("-C").arg(worktree).args(["rebase", "--abort"]);
    let _ = run_with_timeout(&mut command, GIT_TIMEOUT);
}

/// The detached-HEAD safety net, worktree.sh:453-458.
///
/// Runs after a rebase attempt, successful or not: if HEAD somehow ended up
/// detached, abort whatever rebase might still be in progress and restore a
/// real branch. `current_branch` (what the worktree started on) is tried
/// FIRST, `wanted_branch` (what the launcher was asked for) only as a
/// fallback once that checkout fails. The shell's `||` chain fixes that
/// order and it is load-bearing: collapsing it to "try either" would recover
/// onto the wrong branch whenever both exist.
///
/// Returns whether HEAD was actually detached and this had to act. The shell
/// also prints "HEAD detached after rebase. Aborting and restoring." here;
/// that is left to the caller, which is what the return value is for.
pub fn recover_detached_head(worktree: &Path, current_branch: &str, wanted_branch: &str) -> bool {
    let detached = git::raw(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])
        .is_some_and(|head| head.trim() == "HEAD");
    if !detached {
        return false;
    }
    abort_rebase(worktree);
    if !git::ok(worktree, &["checkout", current_branch]) {
        git::ok(worktree, &["checkout", wanted_branch]);
    }
    true
}

/// Ports `_wt_maybe_rebase` (worktree.sh:404-459), wiring the already-ported
/// decision and actions: [`should_rebase`] gates whether to run at all,
/// [`rebase_onto`] does the fetch-and-rebase, and a conflict is always
/// aborted here.
///
/// THE AI-RESOLVE PATH IS DEFERRED, not ported: worktree.sh:432-446 spawns
/// `claude -p`, reads a prompt answer from stdin, and decides via
/// [`conflict_action`] (already ported) whether to spawn it. None of that
/// belongs in a pure git-driving function, so on `RebaseOutcome::Conflicted`
/// this always takes the shell's NON-AI branch (worktree.sh:447-450): abort
/// via [`abort_rebase`]. A caller that wants `--ai-resolve` support offers it
/// BEFORE calling this, then decides for itself whether to still call this
/// (skip it, since the conflict is already resolved) or fall through to the
/// same abort this does. Either way, a conflict is never left both
/// unresolved and unaborted.
///
/// Returns `None` when [`should_rebase`] declined (worktree.sh's several
/// early `return 0`s, worktree.sh:415, :421, :422), which also means the
/// detached-HEAD safety net (worktree.sh:453-458) never runs: the shell only
/// reaches it after an actual rebase attempt. `Some(UpToDate)` is the same
/// story one gate later, worktree.sh:425's own early `return 0`; only
/// `Some(Rebased)` and `Some(Conflicted)` reach that safety net here, exactly
/// as they do in the shell.
pub fn maybe_rebase(ctx: &RebaseContext, worktree: &Path, remote: &str) -> Option<RebaseOutcome> {
    if !should_rebase(ctx) {
        return None;
    }
    let outcome = rebase_onto(worktree, remote, ctx.base_ref);
    match outcome {
        RebaseOutcome::UpToDate => {}
        RebaseOutcome::Rebased | RebaseOutcome::Conflicted => {
            if outcome == RebaseOutcome::Conflicted {
                abort_rebase(worktree);
            }
            recover_detached_head(worktree, ctx.current_branch, ctx.wanted_branch);
        }
    }
    Some(outcome)
}

#[derive(Debug, PartialEq)]
pub enum UpstreamAction {
    /// Tracking is already correct.
    None,
    /// The branch exists on the remote, so point at it without pushing.
    SetTracking,
    /// The branch is remote-only-absent and pushing is allowed.
    PushAndTrack,
    /// Absent remotely and `--no-push` was given, so leave it local.
    SkipNoPush,
}

pub fn upstream_action(
    current_upstream: Option<&str>,
    expected: &str,
    exists_on_remote: bool,
    no_push: bool,
) -> UpstreamAction {
    if current_upstream == Some(expected) {
        return UpstreamAction::None;
    }
    if exists_on_remote {
        return UpstreamAction::SetTracking;
    }
    if no_push {
        return UpstreamAction::SkipNoPush;
    }
    UpstreamAction::PushAndTrack
}

/// Whether the remote publishes `branch`, reading a failed `ls-remote` as
/// "not published".
///
/// Correct for the upstream decision only, where the shell's own fallthrough
/// on a failed call is to push (worktree.sh:131-136). Do NOT use it for
/// anything destructive; see [`remote_may_still_have_branch`].
fn ls_remote_heads_has_branch(repo_root: &Path, remote: &str, branch: &str) -> bool {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo_root)
        .args(["ls-remote", "--heads", remote, branch]);
    match run_with_timeout(&mut command, REBASE_TIMEOUT) {
        Some(out) if out.status.success() => {
            !String::from_utf8_lossy(&out.stdout).trim().is_empty()
        }
        _ => false,
    }
}

/// Whether the branch occupying the target might still be live on the remote,
/// which is the question that decides refuse-versus-recycle.
///
/// Answers TRUE when `ls-remote` cannot tell us, not just when the branch is
/// present. "Absent" and "could not ask" are different states and only the
/// first is safe to act on, because the destructive branch of that decision
/// force-deletes the branch. An unreachable remote, expired credentials or a
/// timeout therefore refuse rather than reap.
///
/// This deliberately DIVERGES from the retired shell, which piped `ls-remote`
/// into `grep -q .` and so read a failed call as "absent" and recycled.
fn remote_may_still_have_branch(repo_root: &Path, remote: &str, branch: &str) -> bool {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo_root)
        .args(["ls-remote", "--heads", remote, branch]);
    match run_with_timeout(&mut command, REBASE_TIMEOUT) {
        Some(out) if out.status.success() => {
            !String::from_utf8_lossy(&out.stdout).trim().is_empty()
        }
        // Could not ask. Assume live and keep the worktree.
        _ => true,
    }
}

/// Executes `_wt_setup_upstream` (worktree.sh:127-138) via the already-ported
/// [`upstream_action`] decision. Every branch tolerates failure, including a
/// failed push: the shell's `_wt_die "failed to create upstream" 6`
/// (worktree.sh:136) belongs to whatever eventually surfaces this to a user,
/// not to housekeeping running in the background where nothing reads that
/// exit code anyway.
fn apply_upstream_action(worktree: &Path, remote: &str, branch: &str, no_push: bool) {
    let expected = format!("{remote}/{branch}");
    let current = git::raw(
        worktree,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .map(|s| s.trim().to_string());
    let exists_on_remote = ls_remote_heads_has_branch(worktree, remote, branch);

    match upstream_action(current.as_deref(), &expected, exists_on_remote, no_push) {
        UpstreamAction::None | UpstreamAction::SkipNoPush => {}
        UpstreamAction::SetTracking => {
            let _ = git::ok(
                worktree,
                &["branch", "--set-upstream-to", &expected, branch],
            );
        }
        UpstreamAction::PushAndTrack => {
            let mut push = Command::new("git");
            push.arg("-C")
                .arg(worktree)
                .args(["push", "-u", remote, branch]);
            let _ = run_with_timeout(&mut push, REBASE_TIMEOUT);
        }
    }
}

/// Static identity for one [`housekeep`] run: which worktree, which repo, and
/// how to reach the remote. Gathered by the caller, like every other
/// `*Context` in this file, so `housekeep` itself stays a plain function of
/// its inputs.
pub struct HousekeepContext<'a> {
    pub worktree: &'a Path,
    pub repo_root: &'a Path,
    pub remote: &'a str,
    pub branch: &'a str,
    pub no_push: bool,
}

/// Ports the BODY of `_wt_main`'s background housekeeping block
/// (worktree.sh:372-384): a full fetch, a local upstream sync, a
/// fast-forward pull, stale-registration pruning, upstream tracking/push,
/// `node_modules` reuse, and the daily stale-worktree cleanup.
///
/// THE SHELL RUNS THIS DETACHED. It backgrounds the block in a subshell,
/// redirects all three streams, and `disown`s it so the work outlives
/// `_wt_main` returning and the parent shell exiting (worktree.sh:371-386).
/// A Rust process that returns from `main` takes its whole process tree with
/// it, so there is a real design decision here (a detached child process? a
/// thread the CLI declines to join? something else?) and it is not this
/// slice's to make. This function is the block's BODY only, run
/// synchronously and left for whatever wires the `playbook cc launch worktree` CLI together
/// to background however it decides is right. That deferral is deliberate,
/// not an oversight.
///
/// Every step tolerates failure, matching the shell's `|| true` on each one:
/// housekeeping is best-effort, and it already runs after the worktree the
/// user asked for is ready and usable. The two marker paths and the open-PR
/// list are caller-supplied rather than computed here (the real paths are
/// [`fetch_cache_marker_path`] and [`cleanup_stale`]'s own marker, and a real
/// PR list needs a real `gh pr list`), for the same reason [`cleanup_stale`]
/// itself splits from [`cleanup_stale_with`]: so tests can inject scratch
/// values instead of touching this machine's real `/tmp` markers or
/// depending on `gh` being installed and authenticated.
///
/// Returns how many stale worktrees [`cleanup_stale_with`] removed.
pub fn housekeep(
    target: &HousekeepContext,
    fetch_cache_marker: &Path,
    cleanup_marker: &Path,
    open_prs: &[String],
    now_epoch: i64,
) -> usize {
    let mut fetch = Command::new("git");
    fetch
        .arg("-C")
        .arg(target.worktree)
        .args(["fetch", target.remote, "--prune", "--quiet"]);
    if matches!(run_with_timeout(&mut fetch, REBASE_TIMEOUT), Some(o) if o.status.success()) {
        touch_marker(fetch_cache_marker, now_epoch);
    }

    // Two upstream checks back to back, on purpose: this one reads only the
    // LOCAL remote-tracking ref (`show-ref`, worktree.sh:376),
    // `apply_upstream_action` below re-checks with a live `git ls-remote`
    // (worktree.sh:131). The shell runs both in sequence too; preserved
    // rather than collapsed into one.
    let expected = format!("{}/{}", target.remote, target.branch);
    let expected_ref = format!("refs/remotes/{expected}");
    let remote_ref_registered = git::ok(
        target.worktree,
        &["show-ref", "--verify", "--quiet", &expected_ref],
    );
    let current_upstream = git::raw(
        target.worktree,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .map(|s| s.trim().to_string());

    if remote_ref_registered && current_upstream.as_deref() != Some(expected.as_str()) {
        let _ = git::ok(
            target.worktree,
            &["branch", "--set-upstream-to", &expected, target.branch],
        );
    }
    if remote_ref_registered {
        let mut pull = Command::new("git");
        pull.arg("-C")
            .arg(target.worktree)
            .args(["pull", "--ff-only", "--quiet"]);
        let _ = run_with_timeout(&mut pull, REBASE_TIMEOUT);
    }

    let _ = git::ok(target.worktree, &["worktree", "prune"]);

    apply_upstream_action(
        target.worktree,
        target.remote,
        target.branch,
        target.no_push,
    );
    reuse_node_modules(target.worktree, target.repo_root);
    // Every caller in this binary hands this a marker freshly touched this
    // instant (`worktree_run.rs`'s `run_housekeep`), so this call always
    // reads as not-yet-due and no-ops: it is not the active stale-worktree
    // path any more, `crate::gc::worktrees::sweep_locked` is, called separately by that
    // same caller.
    cleanup_stale_with(
        target.repo_root,
        target.worktree,
        cleanup_marker,
        open_prs,
        now_epoch,
    )
}

/// If `worktree` is a genuinely detached HEAD right after creation, checks
/// `branch` back out: worktree.sh:360-363. Prefers `checkout -B branch
/// HEAD`, which also repoints a branch ref that happens to already exist
/// elsewhere; only falls back to a plain `checkout branch` when that fails.
/// Both tolerate failure, matching the shell's trailing `|| true`.
///
/// Returns whether HEAD was actually detached (and this therefore acted),
/// not whether the checkout succeeded: that is what a caller needs to know
/// to decide whether to print the shell's "Worktree created detached.
/// Attaching to $BRANCH..." message (worktree.sh:361), left to the caller
/// like every other diagnostic in this file.
///
/// A DIFFERENT function from [`recover_detached_head`] (worktree.sh:453-458):
/// that one runs after a rebase attempt, aborts any rebase in progress
/// first, and tries the pre-rebase branch before the wanted one as its
/// fallback. This one runs right after worktree creation, aborts nothing,
/// and only ever tries `branch`. Both exist in the shell, at different
/// moments, doing different things; do not merge them.
pub fn attach_if_detached(worktree: &Path, branch: &str) -> bool {
    let detached = git::raw(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])
        .is_some_and(|head| head.trim() == "HEAD");
    if !detached {
        return false;
    }
    if !git::ok(worktree, &["checkout", "-B", branch, "HEAD"]) {
        let _ = git::ok(worktree, &["checkout", branch]);
    }
    true
}

/// Outcome of [`prepare_worktree`].
#[derive(Debug, PartialEq)]
pub enum WorktreeOutcome {
    /// Ready to use, plus what [`copy_env`] decided about the `.env` copy,
    /// so a caller can still report a refusal even though it did not stop
    /// the run.
    Ready(PathBuf, EnvCopy),
    /// worktree.sh:332-335: the branch already occupying the target still
    /// exists on the remote. NOTHING WAS REMOVED. The caller owns the
    /// message and exit code (5).
    Refused(String),
    /// [`create_worktree`]'s whole ladder failed and no fallback resolved
    /// either. The caller owns the message and exit code (3, surfaced
    /// through `_wt_main`'s `|| return $?`).
    CreateFailed,
}

/// The branch and registration status of the worktree at `target`, combining
/// worktree.sh:300-301's two separate `awk` passes over the same
/// `git worktree list --porcelain` capture into one scan: nothing can change
/// between the shell's two invocations of the same read-only command, so
/// reusing one parse changes nothing observable.
fn target_worktree_info(porcelain: &str, target: &Path) -> (Option<String>, bool) {
    let target_str = target.to_string_lossy();
    let mut in_target = false;
    let mut registered = false;
    let mut branch = None;
    for line in porcelain.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            in_target = path == target_str.as_ref();
            registered = registered || in_target;
        } else if in_target {
            if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.trim_start_matches("refs/heads/").to_string());
            }
        }
    }
    (branch, registered)
}

/// The `RecoverDetached` plan's actions (worktree.sh:304-311): abort any
/// rebase or merge in progress, then try to check `branch` back out,
/// falling back to creating it from the remote-tracking ref when a plain
/// checkout fails. Returns the branch actually checked out, or `None` when
/// HEAD is still detached afterwards.
///
/// Also `None` for the shell's `|| echo ""` case, an unreadable HEAD after
/// the checkout attempts above: a checkout that just ran successfully
/// against this exact worktree cannot leave it in a state `rev-parse` cannot
/// even read, so this collapses that theoretical branch into "recovery did
/// not land" rather than reproducing it as a third outcome the caller would
/// have to handle for something that cannot occur in practice.
///
/// A THIRD, DIFFERENT detached-HEAD recovery routine: not
/// [`attach_if_detached`] (runs once the worktree already exists in a
/// settled sense, never touches rebase/merge state) and not
/// [`recover_detached_head`] (runs after a rebase attempt, tries the
/// pre-rebase branch before the wanted one). All three exist in the shell,
/// at different moments, doing different things.
fn recover_detached_target(
    repo_root: &Path,
    target: &Path,
    branch: &str,
    remote: &str,
) -> Option<String> {
    abort_rebase(target);
    let _ = git::ok(target, &["merge", "--abort"]);
    if !git::ok(target, &["checkout", branch]) {
        let remote_ref = format!("refs/remotes/{remote}/{branch}");
        if git::ok(repo_root, &["show-ref", "--verify", "--quiet", &remote_ref]) {
            let _ = git::ok(target, &["checkout", "-b", branch, &remote_ref]);
        }
    }
    git::raw(target, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "HEAD")
}

/// `git worktree remove --force`, falling back to `rm -rf` plus `git
/// worktree prune` on failure: the identical fallback the shell repeats at
/// worktree.sh:314 and worktree.sh:337.
fn force_remove_worktree(repo_root: &Path, target: &Path) {
    let target_str = target.to_string_lossy();
    if !git::ok(
        repo_root,
        &["worktree", "remove", "--force", target_str.as_ref()],
    ) {
        let _ = std::fs::remove_dir_all(target);
        let _ = git::ok(repo_root, &["worktree", "prune"]);
    }
}

/// `_wt_make` plus `_wt_copy_env`, wrapped into a [`WorktreeOutcome`]:
/// worktree.sh:323-324, :339-340, and :352-353, the three places a fresh
/// worktree gets created and then has its `.env` copied.
fn create_fresh(
    repo_root: &Path,
    target: &Path,
    branch: &str,
    remote: &str,
    env_base: Option<&str>,
) -> WorktreeOutcome {
    match make_worktree(repo_root, target, branch, remote) {
        CreateOutcome::Failed => WorktreeOutcome::CreateFailed,
        CreateOutcome::Created(path) | CreateOutcome::AlreadyAt(path) => {
            let env_copy = copy_env(repo_root, &path, env_base);
            WorktreeOutcome::Ready(path, env_copy)
        }
    }
}

/// Fetches `branch` and fast-forwards `path` in place when the
/// remote-tracking ref exists, then copies `.env`: worktree.sh:326-330 and
/// :345-349, shared because both are the same "already have it, just
/// refresh it" action on two different paths. Also where `RecoverDetached`'s
/// successful-recovery fallthrough lands, since a recovered worktree is now
/// in exactly the state the shell's `current_branch == $BRANCH` arm handles.
fn refresh_existing(
    repo_root: &Path,
    path: &Path,
    branch: &str,
    remote: &str,
    env_base: Option<&str>,
) -> WorktreeOutcome {
    let mut fetch = Command::new("git");
    fetch
        .arg("-C")
        .arg(repo_root)
        .args(["fetch", remote, branch, "--quiet"]);
    let _ = run_with_timeout(&mut fetch, REBASE_TIMEOUT);

    let remote_ref = format!("refs/remotes/{remote}/{branch}");
    if git::ok(repo_root, &["show-ref", "--verify", "--quiet", &remote_ref]) {
        let mut pull = Command::new("git");
        pull.arg("-C")
            .arg(path)
            .args(["pull", "--ff-only", "--quiet"]);
        let _ = run_with_timeout(&mut pull, REBASE_TIMEOUT);
    }

    let env_copy = copy_env(repo_root, path, env_base);
    WorktreeOutcome::Ready(path.to_path_buf(), env_copy)
}

/// Wires [`plan_for_target`]'s decision to the already-ported actions it
/// dispatches to: worktree.sh:299-355, the whole "what to do with `target`"
/// block. `target` must already be the same absolute path git itself would
/// report (canonicalized by the caller if needed), since this compares it
/// against `git worktree list --porcelain` output byte for byte, exactly as
/// the shell's `awk -v t="$TARGET"` does.
///
/// Two things this function does NOT do, both left to the caller: print any
/// of the shell's informational messages (worktree.sh:304, :313, :320, :336,
/// :351), and decide the exit code for [`WorktreeOutcome::Refused`] or
/// [`WorktreeOutcome::CreateFailed`] (worktree.sh's exit codes 5 and 3).
pub fn prepare_worktree(
    repo_root: &Path,
    target: &Path,
    branch: &str,
    remote: &str,
    env_base: Option<&str>,
) -> WorktreeOutcome {
    let target_exists = target.is_dir();
    // A failed `worktree list` reads as empty, exactly like the shell's
    // `$(...)` capturing nothing on failure: nothing downstream treats that
    // as fatal, so this doesn't either.
    let porcelain = git::raw(repo_root, &["worktree", "list", "--porcelain"]).unwrap_or_default();

    if target_exists {
        let (current_branch, registered) = target_worktree_info(&porcelain, target);
        // Only asked when it can actually change the outcome (worktree.sh:332
        // asks it lazily too): a detached HEAD or the already-wanted branch
        // never reaches the refuse-vs-recycle fork this answers.
        //
        // "no" here is the DESTRUCTIVE answer: it recycles the worktree and
        // force-deletes the occupying branch. So this asks whether the branch
        // MIGHT still be live, not whether it is, and an `ls-remote` that
        // could not run counts as "might". Both implementations originally
        // read a failed call as "absent" and reaped on it; `worktree.sh`
        // carries the matching fix, so the two stay in parity.
        let current_branch_on_remote = match current_branch.as_deref() {
            Some(current) if current != branch => {
                remote_may_still_have_branch(repo_root, remote, current)
            }
            _ => false,
        };

        let state = TargetState {
            target_exists,
            registered,
            current_branch: current_branch.as_deref(),
            wanted_branch: branch,
            current_branch_on_remote,
            existing_for_wanted: None,
        };

        return match plan_for_target(&state) {
            WorktreePlan::RecoverDetached => {
                match recover_detached_target(repo_root, target, branch, remote) {
                    Some(_) => refresh_existing(repo_root, target, branch, remote, env_base),
                    None => {
                        force_remove_worktree(repo_root, target);
                        create_fresh(repo_root, target, branch, remote, env_base)
                    }
                }
            }
            WorktreePlan::CleanOrphanAndCreate => {
                let _ = git::ok(repo_root, &["worktree", "prune"]);
                let _ = std::fs::remove_dir_all(target);
                create_fresh(repo_root, target, branch, remote, env_base)
            }
            WorktreePlan::ReuseTarget => {
                refresh_existing(repo_root, target, branch, remote, env_base)
            }
            WorktreePlan::RefuseOccupied(current) => WorktreeOutcome::Refused(current),
            WorktreePlan::RecycleTarget(current) => {
                force_remove_worktree(repo_root, target);
                let _ = git::ok(repo_root, &["branch", "-D", &current]);
                create_fresh(repo_root, target, branch, remote, env_base)
            }
            WorktreePlan::ReuseExisting(_)
            | WorktreePlan::PruneStaleAndCreate
            | WorktreePlan::Create => {
                unreachable!("plan_for_target only returns these when target_exists is false")
            }
        };
    }

    let existing = worktree_for_branch(&porcelain, branch);
    let existing_is_dir = existing
        .as_deref()
        .map(|p| Path::new(p).is_dir())
        .unwrap_or(false);
    let state = TargetState {
        target_exists,
        registered: false,
        current_branch: None,
        wanted_branch: branch,
        current_branch_on_remote: false,
        existing_for_wanted: existing.as_deref().map(|p| (p, existing_is_dir)),
    };

    match plan_for_target(&state) {
        WorktreePlan::ReuseExisting(path) => {
            refresh_existing(repo_root, Path::new(&path), branch, remote, env_base)
        }
        WorktreePlan::PruneStaleAndCreate => {
            let _ = git::ok(repo_root, &["worktree", "prune"]);
            create_fresh(repo_root, target, branch, remote, env_base)
        }
        WorktreePlan::Create => create_fresh(repo_root, target, branch, remote, env_base),
        WorktreePlan::RecoverDetached
        | WorktreePlan::CleanOrphanAndCreate
        | WorktreePlan::ReuseTarget
        | WorktreePlan::RefuseOccupied(_)
        | WorktreePlan::RecycleTarget(_) => {
            unreachable!("plan_for_target only returns these when target_exists is true")
        }
    }
}

#[cfg(test)]
mod remote_reachability_tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git should spawn");
    }

    /// A repo with one commit plus a bare sibling acting as its remote. Named
    /// per test so parallel runs cannot collide, since there is no temp-dir
    /// crate in this deliberately minimal dependency graph.
    fn repo_with_remote(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("pb-lsremote-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let remote = base.join("remote.git");
        std::fs::create_dir_all(&repo).expect("scratch repo dir");
        Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&remote)
            .output()
            .expect("git init --bare");
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &[
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        (repo, remote)
    }

    #[test]
    fn a_branch_published_on_the_remote_blocks_recycling() {
        let (repo, _remote) = repo_with_remote("present");
        git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/live"]);
        assert!(remote_may_still_have_branch(&repo, "origin", "live"));
    }

    #[test]
    fn a_branch_genuinely_absent_allows_recycling() {
        let (repo, _remote) = repo_with_remote("absent");
        assert!(!remote_may_still_have_branch(&repo, "origin", "ghost"));
    }

    /// The regression this guards is data loss, not a wrong answer: "no" is
    /// what force-deletes the branch, so a remote that cannot be reached must
    /// never produce it. Both implementations previously read a failed
    /// `ls-remote` as "absent" and reaped on it.
    #[test]
    fn an_unreachable_remote_never_allows_recycling() {
        let (repo, _remote) = repo_with_remote("unreachable");
        git(
            &repo,
            &["remote", "set-url", "origin", "/nonexistent/nope.git"],
        );
        assert!(
            remote_may_still_have_branch(&repo, "origin", "ghost"),
            "an unanswerable lookup must not read as absent"
        );
    }

    /// The upstream decision keeps the opposite reading on failure, matching
    /// the shell's fallthrough to push (worktree.sh:131-136). Pinned so the
    /// two helpers are not collapsed into one.
    #[test]
    fn the_upstream_helper_still_reads_failure_as_not_published() {
        let (repo, _remote) = repo_with_remote("upstream");
        git(
            &repo,
            &["remote", "set-url", "origin", "/nonexistent/nope.git"],
        );
        assert!(!ls_remote_heads_has_branch(&repo, "origin", "ghost"));
    }
}
