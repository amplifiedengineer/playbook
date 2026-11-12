// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `playbook worktree sweep`/`remove`, spawning the
//! real compiled binary against real scratch git repos and real `git
//! worktree add` calls, matching `tests/config_cli.rs`'s convention for
//! CLI-layer subcommands and `tests/worktree_sweep.rs`'s convention for
//! building convention-specific worktree fixtures.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "playbook-worktree-cli-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("scratch dir should be creatable");
    dir
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git command should spawn")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git(repo, args).stdout)
        .trim()
        .to_string()
}

/// A repo with one seed commit, dated `commit_epoch`, and an `origin` remote
/// so `repo_slug()` resolves to `<owner>/<repo>`.
fn init_repo_at_epoch(dir: &Path, remote_url: &str, commit_epoch: i64) {
    fs::create_dir_all(dir).expect("create repo dir");
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "T"],
        vec!["remote", "add", "origin", remote_url],
    ] {
        git_ok(dir, &args);
    }
    fs::write(dir.join("README.md"), "seed\n").expect("write seed file");
    git_ok(dir, &["add", "."]);
    let date = format!("{commit_epoch} +0000");
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-q", "-m", "seed"])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .expect("git commit should spawn");
    assert!(
        out.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Backdates the worktree's `.git` file, the creation time the sweep reads,
/// so a fixture reads as a long-lived worktree. Brand new worktrees are
/// never treated as landed (see `worktree_sweep_keeps_new_work.rs`).
fn age_worktree(path: &Path, days: u64) {
    let target = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
    let file = fs::File::open(path.join(".git")).expect("open .git file");
    file.set_modified(target).expect("backdate worktree");
}

fn add_worktree(repo_root: &Path, dest: &Path, branch: &str) -> PathBuf {
    fs::create_dir_all(dest.parent().expect("dest has a parent")).expect("mkdir dest parent");
    git_ok(
        repo_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            dest.to_str().expect("utf8 path"),
            "main",
        ],
    );
    let path = dest.canonicalize().expect("worktree should resolve");
    age_worktree(&path, 40);
    path
}

fn add_detached_worktree(repo_root: &Path, dest: &Path) -> PathBuf {
    fs::create_dir_all(dest.parent().expect("dest has a parent")).expect("mkdir dest parent");
    git_ok(
        repo_root,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            dest.to_str().expect("utf8 path"),
            "main",
        ],
    );
    let path = dest.canonicalize().expect("worktree should resolve");
    age_worktree(&path, 40);
    path
}

/// Run the real compiled binary with `cwd` and `$HOME` pinned to scratch
/// locations, so it never touches the developer's real config.
fn run_playbook(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("PATH", common::path_with_failing_gh(home))
        .output()
        .expect("playbook binary should spawn")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn still_registered(repo_root: &Path, worktree_path: &Path) -> bool {
    let porcelain = git_stdout(repo_root, &["worktree", "list", "--porcelain"]);
    porcelain.contains(&worktree_path.to_string_lossy().into_owned())
}

/// Asserts `worktree_path` was both removed from disk, deregistered from
/// `git worktree list`, and named in the sweep's own report.
fn assert_removed_and_reported(repo_root: &Path, worktree_path: &Path, out: &Output) {
    assert!(out.status.success(), "stderr: {}", stderr_of(out));
    assert!(
        fs::symlink_metadata(worktree_path).is_err(),
        "worktree directory should have been removed: {}",
        worktree_path.display()
    );
    assert!(
        !still_registered(repo_root, worktree_path),
        "worktree should no longer be a registered git worktree"
    );
    let stdout = stdout_of(out);
    assert!(
        stdout.contains(&worktree_path.to_string_lossy().into_owned()),
        "sweep should report the removed path, got: {stdout}"
    );
    assert!(
        stdout.to_lowercase().contains("removed"),
        "sweep should report the worktree as removed, got: {stdout}"
    );
}

#[test]
fn sweep_removes_a_landed_unlocked_wu_convention_worktree() {
    // Arrange: a Wu-convention worktree checked out at the repo's only
    // commit, with `origin/main` pointing at that same commit, so
    // `wu_worktree_landed` sees it as already reachable from the default
    // branch.
    let container = scratch("wu").canonicalize().expect("container resolves");
    let repo_root = container.join("repo");
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        1_700_000_000,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let head = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    git_ok(
        &repo_root,
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");

    let wu_path = add_detached_worktree(
        &repo_root,
        &home
            .join(".config")
            .join("playbook")
            .join("repos")
            .join("acme")
            .join("widgets")
            .join("wt-abc123")
            .join("worktrees")
            .join("plan-slug")
            .join("wu-1"),
    );

    // Act
    let out = run_playbook(&repo_root, &home, &["worktree", "sweep"]);

    // Assert
    assert_removed_and_reported(&repo_root, &wu_path, &out);

    let _ = fs::remove_dir_all(&container);
}

#[test]
fn sweep_dry_run_reports_without_removing_a_landed_unlocked_wu_convention_worktree() {
    // Arrange: same landed, unlocked Wu-convention fixture as
    // `sweep_removes_a_landed_unlocked_wu_convention_worktree`.
    let container = scratch("wu-dry-run")
        .canonicalize()
        .expect("container resolves");
    let repo_root = container.join("repo");
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        1_700_000_000,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let head = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    git_ok(
        &repo_root,
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");

    let wu_path = add_detached_worktree(
        &repo_root,
        &home
            .join(".config")
            .join("playbook")
            .join("repos")
            .join("acme")
            .join("widgets")
            .join("wt-abc123")
            .join("worktrees")
            .join("plan-slug")
            .join("wu-1"),
    );

    // Act
    let out = run_playbook(&repo_root, &home, &["worktree", "sweep", "--dry-run"]);

    // Assert: nothing removed, but reported as a dry-run candidate.
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert!(
        fs::symlink_metadata(&wu_path).is_ok(),
        "worktree directory should still exist under dry-run: {}",
        wu_path.display()
    );
    assert!(
        still_registered(&repo_root, &wu_path),
        "worktree should still be a registered git worktree under dry-run"
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("would remove (dry run)"),
        "sweep --dry-run should report the dry-run wording, got: {stdout}"
    );
    assert!(
        stdout.contains(&wu_path.to_string_lossy().into_owned()),
        "sweep --dry-run should report the candidate path, got: {stdout}"
    );

    let _ = fs::remove_dir_all(&container);
}

#[test]
fn sweep_leaves_an_unlanded_wu_convention_worktree_alone() {
    // Arrange: a Wu-convention worktree on its own branch with a commit that
    // is never merged into `origin/main`, so `wu_worktree_landed` sees it as
    // unreachable from the default branch.
    let container = scratch("wu-unlanded")
        .canonicalize()
        .expect("container resolves");
    let repo_root = container.join("repo");
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        1_700_000_000,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let head = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    git_ok(
        &repo_root,
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");

    let wu_path = add_worktree(
        &repo_root,
        &home
            .join(".config")
            .join("playbook")
            .join("repos")
            .join("acme")
            .join("widgets")
            .join("wt-def456")
            .join("worktrees")
            .join("plan-slug")
            .join("wu-2"),
        "wu-2-unlanded",
    );
    fs::write(wu_path.join("unmerged.txt"), "work in progress\n")
        .expect("write unmerged file in worktree");
    git_ok(&wu_path, &["add", "."]);
    git_ok(&wu_path, &["commit", "-q", "-m", "unmerged work"]);

    // Act
    let out = run_playbook(&repo_root, &home, &["worktree", "sweep"]);

    // Assert: left alone, on disk, still registered, reported as not landed.
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert!(
        fs::symlink_metadata(&wu_path).is_ok(),
        "unlanded worktree directory should still exist: {}",
        wu_path.display()
    );
    assert!(
        still_registered(&repo_root, &wu_path),
        "unlanded worktree should still be a registered git worktree"
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("not landed, skipping"),
        "sweep should report the unlanded worktree as not landed, got: {stdout}"
    );

    let _ = fs::remove_dir_all(&container);
}

#[test]
fn sweep_leaves_a_worktree_locked_by_a_live_pid_alone() {
    // Arrange: the same landed, unlocked Wu-convention fixture as
    // `sweep_removes_a_landed_unlocked_wu_convention_worktree`, then locked
    // with a reason naming a real running child process's pid, standing in
    // for a worktree another process still holds open.
    let container = scratch("wu-live-lock")
        .canonicalize()
        .expect("container resolves");
    let repo_root = container.join("repo");
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        1_700_000_000,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let head = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    git_ok(
        &repo_root,
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");

    let wu_path = add_detached_worktree(
        &repo_root,
        &home
            .join(".config")
            .join("playbook")
            .join("repos")
            .join("acme")
            .join("widgets")
            .join("wt-abc123")
            .join("worktrees")
            .join("plan-slug")
            .join("wu-1"),
    );

    let mut live_child = Command::new("sleep")
        .arg("10")
        .spawn()
        .expect("sleep child should spawn");
    let live_pid = live_child.id();
    git_ok(
        &repo_root,
        &[
            "worktree",
            "lock",
            wu_path.to_str().expect("utf8 path"),
            "--reason",
            &format!("pid={live_pid}"),
        ],
    );

    // Act
    let out = run_playbook(&repo_root, &home, &["worktree", "sweep"]);

    live_child.kill().expect("kill live child");
    live_child.wait().expect("wait for killed live child");

    // Assert: left alone, on disk, still registered and still locked.
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert!(
        fs::symlink_metadata(&wu_path).is_ok(),
        "worktree locked by a live pid should still exist: {}",
        wu_path.display()
    );
    assert!(
        still_registered(&repo_root, &wu_path),
        "worktree locked by a live pid should still be a registered git worktree"
    );
    let porcelain = git_stdout(&repo_root, &["worktree", "list", "--porcelain"]);
    assert!(
        porcelain.contains("locked"),
        "worktree should still be listed as locked, got: {porcelain}"
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("locked by a live process, skipping"),
        "sweep should report the live-locked worktree as skipped, got: {stdout}"
    );

    let _ = fs::remove_dir_all(&container);
}

#[test]
fn sweep_removes_a_landed_unlocked_agent_tool_convention_worktree() {
    // Arrange: an AgentTool-convention worktree on a branch whose tip equals
    // `main`'s tip, so `git branch --merged <default>` already lists it as
    // merged. The seed commit is recent (well under staleAfterDays), so the
    // only way this can read as landed is through the merged-branch check
    // itself, not the age fallback.
    let container = scratch("agent-tool")
        .canonicalize()
        .expect("container resolves");
    let repo_root = container.join("repo");
    let recent_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should read after the unix epoch")
        .as_secs() as i64
        - 60;
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        recent_epoch,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let head = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    git_ok(
        &repo_root,
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");

    let agent_tool_path = add_worktree(
        &repo_root,
        &repo_root.join(".claude").join("worktrees").join("agent-99"),
        "agent-branch",
    );

    // Act
    let out = run_playbook(&repo_root, &home, &["worktree", "sweep"]);

    // Assert
    assert_removed_and_reported(&repo_root, &agent_tool_path, &out);

    let _ = fs::remove_dir_all(&container);
}

#[test]
fn sweep_removes_a_landed_unlocked_cc_launcher_convention_worktree() {
    // Arrange: a CcLauncher-convention worktree
    // (`<main-worktree-parent>/.worktrees/<repo>/<branch>`) on a branch
    // whose tip equals `main`'s tip, exercising the merged-branch check the
    // same way the AgentTool fixture does (recent seed commit, so the age
    // fallback can't be what makes this pass).
    let container = scratch("cc-launcher")
        .canonicalize()
        .expect("container resolves");
    let repo_root = container.join("repo");
    let recent_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should read after the unix epoch")
        .as_secs() as i64
        - 60;
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        recent_epoch,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let head = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    git_ok(
        &repo_root,
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");

    let cc_launcher_path = add_worktree(
        &repo_root,
        &container
            .join(".worktrees")
            .join("repo")
            .join("feature-branch"),
        "feature-branch",
    );

    // Act
    let out = run_playbook(&repo_root, &home, &["worktree", "sweep"]);

    // Assert
    assert_removed_and_reported(&repo_root, &cc_launcher_path, &out);

    let _ = fs::remove_dir_all(&container);
}

#[test]
fn sweep_removes_a_worktree_locked_by_a_dead_pid_via_double_force() {
    // Arrange: the same landed, unlocked Wu-convention fixture as
    // `sweep_removes_a_landed_unlocked_wu_convention_worktree`, then locked
    // with a reason naming a just-reaped child process's pid, standing in
    // for a lock left behind by a process that no longer exists. A locked
    // worktree normally refuses `git worktree remove` without at least one
    // `--force`, and refuses again with only one `--force`, so successful
    // removal here is itself evidence the double-force override fired.
    let container = scratch("wu-dead-lock")
        .canonicalize()
        .expect("container resolves");
    let repo_root = container.join("repo");
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        1_700_000_000,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let head = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    git_ok(
        &repo_root,
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");

    let wu_path = add_detached_worktree(
        &repo_root,
        &home
            .join(".config")
            .join("playbook")
            .join("repos")
            .join("acme")
            .join("widgets")
            .join("wt-abc123")
            .join("worktrees")
            .join("plan-slug")
            .join("wu-1"),
    );

    let mut dead_child = Command::new("true")
        .spawn()
        .expect("true child should spawn");
    let dead_pid = dead_child.id();
    dead_child
        .wait()
        .expect("wait for true child should reap it");
    git_ok(
        &repo_root,
        &[
            "worktree",
            "lock",
            wu_path.to_str().expect("utf8 path"),
            "--reason",
            &format!("pid={dead_pid}"),
        ],
    );

    // Act
    let out = run_playbook(&repo_root, &home, &["worktree", "sweep"]);

    // Assert
    assert_removed_and_reported(&repo_root, &wu_path, &out);

    let _ = fs::remove_dir_all(&container);
}

/// Formats `epoch_secs` as `YYYY-MM-DDTHH:MM:SSZ`, trying BSD `date -u -r`
/// then GNU `date -u -d @`, the same fallback order
/// `cc::sessions::local_timestamp` uses to render an arbitrary past epoch.
fn iso8601_utc(epoch_secs: i64) -> String {
    format_epoch(epoch_secs, "+%Y-%m-%dT%H:%M:%SZ")
}

/// Formats `epoch_secs` with `format_spec`, trying BSD `date -u -r` then GNU
/// `date -u -d`, the fallback order shared by every real-clock test helper
/// in this file.
fn format_epoch(epoch_secs: i64, format_spec: &str) -> String {
    for args in [
        vec!["-u".to_string(), "-r".to_string(), epoch_secs.to_string()],
        vec!["-u".to_string(), "-d".to_string(), format!("@{epoch_secs}")],
    ] {
        if let Ok(out) = Command::new("date").args(&args).arg(format_spec).output() {
            if out.status.success() {
                return String::from_utf8_lossy(&out.stdout).trim().to_string();
            }
        }
    }
    panic!("date should format epoch {epoch_secs} on either BSD or GNU");
}

/// Same as [`format_epoch`], without `-u`: formats in the local timezone
/// instead of UTC.
/// Backdates `path`'s mtime to `epoch_secs`, standing in for a worktree
/// created a while ago. Sets it directly via `File::set_modified` rather than
/// round-tripping through `date`/`touch -t`'s local-wall-clock string
/// representation, which is ambiguous during a DST fall-back hour and could
/// shift the mtime by an hour; this matches the pattern already used in
/// `tests/hooks_session.rs`.
fn touch_at_epoch(path: &Path, epoch_secs: i64) {
    let target = std::time::UNIX_EPOCH + std::time::Duration::from_secs(epoch_secs as u64);
    let file = fs::File::open(path).expect("open file to backdate");
    file.set_modified(target).expect("set mtime");
}

fn write_conflict_marker(worktree_path: &Path, epoch_secs: i64) {
    fs::write(
        worktree_path.join(".playbook-conflict-stop"),
        format!(
            "merge conflict during rebase\n{}\n",
            iso8601_utc(epoch_secs)
        ),
    )
    .expect("write conflict-STOP marker");
}

/// A Wu-convention worktree whose own commit is already reachable from
/// `origin/main`, the same landed fixture
/// `sweep_removes_a_landed_unlocked_wu_convention_worktree` exercises, so a
/// present conflict-STOP marker's grace period is the only thing left to
/// decide the outcome, not the underlying commit.
fn build_landed_wu_fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let container = scratch(tag).canonicalize().expect("container resolves");
    let repo_root = container.join("repo");
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        1_700_000_000,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let head = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    git_ok(
        &repo_root,
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");

    let wu_path = add_detached_worktree(
        &repo_root,
        &home
            .join(".config")
            .join("playbook")
            .join("repos")
            .join("acme")
            .join("widgets")
            .join("wt-abc123")
            .join("worktrees")
            .join("plan-slug")
            .join("wu-1"),
    );

    (container, repo_root, home, wu_path)
}

enum ConflictMarkerExpectation {
    LeftAlone,
    Removed,
}

#[test]
fn sweep_applies_conflict_stop_marker_grace_period_boundary() {
    // Arrange: the real current time, since the compiled binary reads its
    // own `now_epoch` from the real clock with no injection point, so each
    // marker's timestamp is computed relative to whenever this test runs
    // rather than a fixed constant.
    const GRACE_PERIOD_DAYS: i64 = 90;
    const SECS_PER_DAY: i64 = 86_400;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should read after the unix epoch")
        .as_secs() as i64;

    let cases = [
        ("fresh", now, ConflictMarkerExpectation::LeftAlone),
        (
            "at-grace-boundary",
            now - GRACE_PERIOD_DAYS * SECS_PER_DAY,
            ConflictMarkerExpectation::Removed,
        ),
        (
            "past-grace-period",
            now - (GRACE_PERIOD_DAYS + 1) * SECS_PER_DAY,
            ConflictMarkerExpectation::Removed,
        ),
    ];

    for (tag, marker_epoch, expected) in cases {
        let (container, repo_root, home, wu_path) =
            build_landed_wu_fixture(&format!("conflict-marker-{tag}"));
        write_conflict_marker(&wu_path, marker_epoch);

        // Act
        let out = run_playbook(&repo_root, &home, &["worktree", "sweep"]);

        // Assert
        match expected {
            ConflictMarkerExpectation::LeftAlone => {
                assert!(
                    out.status.success(),
                    "case {tag}, stderr: {}",
                    stderr_of(&out)
                );
                assert!(
                    fs::symlink_metadata(&wu_path).is_ok(),
                    "case {tag}: worktree within the conflict-STOP grace period should still exist: {}",
                    wu_path.display()
                );
                assert!(
                    still_registered(&repo_root, &wu_path),
                    "case {tag}: worktree within the conflict-STOP grace period should still be registered"
                );
                let stdout = stdout_of(&out);
                assert!(
                    stdout.contains("not landed, skipping"),
                    "case {tag}: sweep should report the marker as still within grace, got: {stdout}"
                );
            }
            ConflictMarkerExpectation::Removed => {
                assert_removed_and_reported(&repo_root, &wu_path, &out);
            }
        }

        let _ = fs::remove_dir_all(&container);
    }
}

#[test]
fn sweep_removes_a_landed_unlocked_review_convention_worktree() {
    // Arrange: a Review-convention worktree that was never locked, whose own
    // `.git` file (the linked-worktree pointer `git worktree add` writes
    // once) is backdated well past the never-locked grace window, standing
    // in for a review session whose worktree has sat idle that long. The
    // checked-out commit's own date is irrelevant here: the landed check
    // reads the worktree's own creation time, not its commit's.
    let container = scratch("review")
        .canonicalize()
        .expect("container resolves");
    let repo_root = container.join("repo");
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        1_700_000_000,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");

    let common_dir = git_stdout(
        &repo_root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let review_path = add_detached_worktree(
        &repo_root,
        &Path::new(&common_dir)
            .join("review-worktrees")
            .join("42-abc1234"),
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should read after the unix epoch")
        .as_secs() as i64;
    touch_at_epoch(&review_path.join(".git"), now - 400 * 86_400);

    // Act
    let out = run_playbook(&repo_root, &home, &["worktree", "sweep"]);

    // Assert
    assert_removed_and_reported(&repo_root, &review_path, &out);

    let _ = fs::remove_dir_all(&container);
}

/// Builds an unlocked Review-convention worktree fixture whose own creation
/// time (its `.git` file's mtime, the signal `review_worktree_landed`'s
/// unlocked path actually reads) is backdated by `worktree_age_secs`, so only
/// that age varies between cases, not the lock state.
fn build_review_fixture(tag: &str, worktree_age_secs: i64) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let container = scratch(tag).canonicalize().expect("container resolves");
    let repo_root = container.join("repo");
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        1_700_000_000,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");

    let common_dir = git_stdout(
        &repo_root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let review_path = add_detached_worktree(
        &repo_root,
        &Path::new(&common_dir)
            .join("review-worktrees")
            .join("42-abc1234"),
    );
    // The landed check reads the worktree's own creation time (its `.git`
    // file's mtime), not its checked-out commit's date, so the fixture
    // backdates that file directly to control the age the sweep sees.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should read after the unix epoch")
        .as_secs() as i64;
    touch_at_epoch(&review_path.join(".git"), now - worktree_age_secs);

    (container, repo_root, home, review_path)
}

enum ReviewGraceExpectation {
    LeftAlone,
    Removed,
}

#[test]
fn sweep_applies_never_locked_grace_window_to_unlocked_review_convention_worktree() {
    // Arrange: `build_review_fixture` backdates the worktree's own creation
    // time, since the compiled binary reads its own `now_epoch` from the
    // real clock with no injection point.
    let cases = [
        ("young", 10, ReviewGraceExpectation::LeftAlone),
        ("old", 3600, ReviewGraceExpectation::Removed),
    ];

    for (tag, worktree_age_secs, expected) in cases {
        let (container, repo_root, home, review_path) =
            build_review_fixture(&format!("review-grace-{tag}"), worktree_age_secs);

        // Act
        let out = run_playbook(&repo_root, &home, &["worktree", "sweep"]);

        // Assert
        match expected {
            ReviewGraceExpectation::LeftAlone => {
                assert!(
                    out.status.success(),
                    "case {tag}, stderr: {}",
                    stderr_of(&out)
                );
                assert!(
                    fs::symlink_metadata(&review_path).is_ok(),
                    "case {tag}: worktree younger than the never-locked grace window should still exist: {}",
                    review_path.display()
                );
                assert!(
                    still_registered(&repo_root, &review_path),
                    "case {tag}: worktree younger than the never-locked grace window should still be registered"
                );
                let stdout = stdout_of(&out);
                assert!(
                    stdout.contains("not landed, skipping"),
                    "case {tag}: sweep should report the young worktree as not landed, got: {stdout}"
                );
            }
            ReviewGraceExpectation::Removed => {
                assert_removed_and_reported(&repo_root, &review_path, &out);
            }
        }

        let _ = fs::remove_dir_all(&container);
    }
}

#[test]
fn sweep_never_removes_the_worktree_it_is_invoked_from() {
    // Arrange: a landed, unlocked Wu-convention worktree, otherwise
    // removable by every other rule the sweep applies, but invoked with cwd
    // set to that same worktree, standing in for the eager trigger's own
    // cwd (inside the just-created worktree, not the main checkout).
    let container = scratch("caller-self")
        .canonicalize()
        .expect("container resolves");
    let repo_root = container.join("repo");
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        1_700_000_000,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let head = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    git_ok(
        &repo_root,
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");

    let wu_path = add_detached_worktree(
        &repo_root,
        &home
            .join(".config")
            .join("playbook")
            .join("repos")
            .join("acme")
            .join("widgets")
            .join("wt-abc123")
            .join("worktrees")
            .join("plan-slug")
            .join("wu-1"),
    );

    // Act: invoked from inside the worktree itself, not the main checkout.
    let out = run_playbook(&wu_path, &home, &["worktree", "sweep"]);

    // Assert: left alone, on disk, still registered, never named in the
    // report.
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert!(
        fs::symlink_metadata(&wu_path).is_ok(),
        "the worktree the sweep runs from should never be removed: {}",
        wu_path.display()
    );
    assert!(
        still_registered(&repo_root, &wu_path),
        "the worktree the sweep runs from should still be a registered git worktree"
    );
    let stdout = stdout_of(&out);
    assert!(
        !stdout.contains(&wu_path.to_string_lossy().into_owned()),
        "the sweep should not report on the worktree it runs from at all, got: {stdout}"
    );

    let _ = fs::remove_dir_all(&container);
}

#[test]
fn sweep_no_ops_when_worktree_cleanup_is_disabled() {
    // Arrange: the same landed, unlocked, otherwise-removable Wu-convention
    // fixture scenario 1 exercises, then the policy disabled at the repo
    // tier the same way `tests/config_cli.rs` exercises `config set`.
    let (container, repo_root, home, wu_path) = build_landed_wu_fixture("sweep-disabled");
    let set_out = run_playbook(
        &repo_root,
        &home,
        &["config", "set", "worktree.cleanup.enabled", "false"],
    );
    assert!(set_out.status.success(), "stderr: {}", stderr_of(&set_out));

    // Act
    let out = run_playbook(&repo_root, &home, &["worktree", "sweep"]);

    // Assert: left alone, on disk, still registered, policy reported.
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert!(
        fs::symlink_metadata(&wu_path).is_ok(),
        "worktree directory should still exist while the policy is disabled: {}",
        wu_path.display()
    );
    assert!(
        still_registered(&repo_root, &wu_path),
        "worktree should still be registered while the policy is disabled"
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("worktree.cleanup.enabled is false"),
        "sweep should report the policy as disabled, got: {stdout}"
    );

    let _ = fs::remove_dir_all(&container);
}

#[test]
fn remove_no_ops_when_worktree_cleanup_is_disabled() {
    // Arrange: an otherwise-removable Wu-convention fixture, then the policy
    // disabled at the repo tier, matching the sweep-side case above so the
    // same setting is proven to gate both subcommands.
    let (container, repo_root, home, wu_path) = build_landed_wu_fixture("remove-disabled");
    let set_out = run_playbook(
        &repo_root,
        &home,
        &["config", "set", "worktree.cleanup.enabled", "false"],
    );
    assert!(set_out.status.success(), "stderr: {}", stderr_of(&set_out));

    // Act
    let out = run_playbook(
        &repo_root,
        &home,
        &["worktree", "remove", wu_path.to_str().expect("utf8 path")],
    );

    // Assert: left alone, on disk, still registered, policy reported.
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert!(
        fs::symlink_metadata(&wu_path).is_ok(),
        "worktree directory should still exist while the policy is disabled: {}",
        wu_path.display()
    );
    assert!(
        still_registered(&repo_root, &wu_path),
        "worktree should still be registered while the policy is disabled"
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("worktree.cleanup.enabled is false"),
        "remove should report the policy as disabled, got: {stdout}"
    );

    let _ = fs::remove_dir_all(&container);
}

#[test]
fn sweep_aborts_when_git_worktree_list_fails() {
    // Arrange: a scratch directory that is not a git repository at all (no
    // `.git`, no parent directory that is one either), so `git worktree
    // list --porcelain` itself exits non-zero rather than merely returning
    // an empty worktree list.
    let container = scratch("no-git")
        .canonicalize()
        .expect("container resolves");
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");

    // Act
    let out = run_playbook(&container, &home, &["worktree", "sweep"]);

    // Assert: aborts cleanly with a non-zero exit and an error on stderr,
    // nothing printed as a removal candidate on stdout.
    assert!(
        !out.status.success(),
        "sweep should exit non-zero against a non-git directory"
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("git worktree list --porcelain failed"),
        "sweep should report the listing failure specifically, got: {stderr}"
    );
    assert!(
        stdout_of(&out).is_empty(),
        "sweep should not report any removal candidates when the listing itself failed"
    );

    let _ = fs::remove_dir_all(&container);
}

/// The repo tier's config file path for the `acme/widgets` fixture repo
/// `build_landed_wu_fixture` seeds, matching `config::repo_config_path`'s
/// layout (`tests/config_cli.rs`'s `repo_config_path` pins the same shape
/// for the `test-owner/test-repo` fixture there).
fn repo_config_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("playbook")
        .join("repos")
        .join("acme")
        .join("widgets")
        .join(".config")
        .join("config.json")
}

#[test]
fn sweep_aborts_on_a_malformed_repo_tier_config_file() {
    // Arrange: the same landed, unlocked, otherwise-removable Wu-convention
    // fixture scenario 1 exercises, then the repo tier's config file
    // corrupted with malformed JSON, standing in for a hand-edited or
    // partially-written tier file.
    let (container, repo_root, home, wu_path) = build_landed_wu_fixture("sweep-malformed-config");
    let config_path = repo_config_path(&home);
    fs::create_dir_all(config_path.parent().expect("config path has a parent"))
        .expect("create repo tier config dir");
    fs::write(&config_path, "{not valid json").expect("write malformed config file");

    // Act
    let out = run_playbook(&repo_root, &home, &["worktree", "sweep"]);

    // Assert: aborts rather than proceeding under default policy values.
    assert!(
        !out.status.success(),
        "sweep should exit non-zero against a malformed config tier file"
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("not a valid JSON object"),
        "sweep should report the malformed config reason specifically, got: {stderr}"
    );
    assert!(
        stdout_of(&out).is_empty(),
        "sweep should not report any removal candidates when config resolution itself failed"
    );
    assert!(
        fs::symlink_metadata(&wu_path).is_ok(),
        "worktree should not be removed when the sweep aborted on a malformed config file: {}",
        wu_path.display()
    );
    assert!(
        still_registered(&repo_root, &wu_path),
        "worktree should still be registered when the sweep aborted on a malformed config file"
    );

    let _ = fs::remove_dir_all(&container);
}

#[test]
fn remove_on_a_path_that_is_not_a_registered_worktree_errors_clearly() {
    // Arrange: a scratch directory that exists on disk but was never
    // registered via `git worktree add`, standing in for any path that is
    // simply not a worktree.
    let container = scratch("remove-not-registered")
        .canonicalize()
        .expect("container resolves");
    let repo_root = container.join("repo");
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        1_700_000_000,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");
    let not_a_worktree = container.join("not-a-worktree");
    fs::create_dir_all(&not_a_worktree).expect("create non-worktree dir");

    // Act
    let out = run_playbook(
        &repo_root,
        &home,
        &[
            "worktree",
            "remove",
            not_a_worktree.to_str().expect("utf8 path"),
        ],
    );

    // Assert: non-zero exit and a clear error naming the path.
    assert!(
        !out.status.success(),
        "remove should exit non-zero against a path that is not a registered git worktree"
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("worktree remove:") && stderr.contains("is not a registered git worktree"),
        "remove should report a clear error naming the path, got: {stderr}"
    );

    let _ = fs::remove_dir_all(&container);
}

#[test]
fn remove_actually_removes_a_landed_unlocked_worktree() {
    // Arrange: the same landed, unlocked Wu-convention fixture scenario 1
    // exercises for `sweep`, proving `remove` performs the full removal path
    // end to end rather than only the disabled-policy or not-found cases.
    let (container, repo_root, home, wu_path) = build_landed_wu_fixture("remove-landed");

    // Act
    let out = run_playbook(
        &repo_root,
        &home,
        &["worktree", "remove", wu_path.to_str().expect("utf8 path")],
    );

    // Assert
    assert_removed_and_reported(&repo_root, &wu_path, &out);

    let _ = fs::remove_dir_all(&container);
}

#[test]
fn remove_refuses_an_unlanded_worktree_and_exits_nonzero() {
    // Arrange: a Wu-convention worktree on its own branch with a commit that
    // is never merged into `origin/main`, the same unlanded fixture `sweep`
    // leaves alone, proving `remove` refuses it too rather than only
    // covering the disabled-policy, not-found, and landed-removal cases.
    let container = scratch("remove-unlanded")
        .canonicalize()
        .expect("container resolves");
    let repo_root = container.join("repo");
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        1_700_000_000,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let head = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    git_ok(
        &repo_root,
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");

    let wu_path = add_worktree(
        &repo_root,
        &home
            .join(".config")
            .join("playbook")
            .join("repos")
            .join("acme")
            .join("widgets")
            .join("wt-def456")
            .join("worktrees")
            .join("plan-slug")
            .join("wu-2"),
        "wu-2-unlanded",
    );
    fs::write(wu_path.join("unmerged.txt"), "work in progress\n")
        .expect("write unmerged file in worktree");
    git_ok(&wu_path, &["add", "."]);
    git_ok(&wu_path, &["commit", "-q", "-m", "unmerged work"]);

    // Act
    let out = run_playbook(
        &repo_root,
        &home,
        &["worktree", "remove", wu_path.to_str().expect("utf8 path")],
    );

    // Assert: refused, exits non-zero, left on disk and still registered.
    assert!(
        !out.status.success(),
        "remove should exit non-zero for an unlanded worktree"
    );
    assert!(
        fs::symlink_metadata(&wu_path).is_ok(),
        "unlanded worktree should not be removed: {}",
        wu_path.display()
    );
    assert!(
        still_registered(&repo_root, &wu_path),
        "unlanded worktree should still be a registered git worktree"
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("not landed, skipping"),
        "remove should report the unlanded worktree as not landed, got: {stderr}"
    );

    let _ = fs::remove_dir_all(&container);
}

#[test]
fn worktree_sweep_alias_reports_busy() {
    // Arrange: a landed Wu-convention worktree and a gc run in progress.
    let container = scratch("busy").canonicalize().expect("container resolves");
    let repo_root = container.join("repo");
    init_repo_at_epoch(
        &repo_root,
        "https://github.com/acme/widgets.git",
        1_700_000_000,
    );
    let repo_root = repo_root.canonicalize().expect("repo root resolves");
    let head = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    git_ok(
        &repo_root,
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    let home = container.join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let home = home.canonicalize().expect("home resolves");
    let wu_path = add_detached_worktree(
        &repo_root,
        &home.join(".config/playbook/repos/acme/widgets/wt-abc123/worktrees/plan-slug/wu-1"),
    );
    fs::create_dir_all(home.join(".config/playbook/gc/.lock")).expect("hand made lock");

    // Act
    let out = run_playbook(&repo_root, &home, &["worktree", "sweep"]);

    // Assert
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("gc is running"));
    assert!(wu_path.exists(), "a busy gc run leaves the worktree alone");

    let _ = fs::remove_dir_all(&container);
}
