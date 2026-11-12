// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook segment unit start|integrate|resume` against scratch git repos
//! and scratch worktrees. No signing key exists in the scratch config, so the
//! commit path runs unsigned.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const WU: &str = "WU-1";
const SEGMENT_BRANCH: &str = "feat/plan-s1-unit";
const LISTED: &str = "listed.rs";
const UNLISTED: &str = "unlisted.rs";
const CONFLICT_MARKER: &str = ".playbook-conflict-stop";
const MESSAGE: &str = "feat(unit): add the listed file";

struct Repo {
    root: PathBuf,
    work: PathBuf,
    worktree: PathBuf,
    base: String,
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit_file(dir: &Path, file: &str, body: &str, subject: &str) {
    fs::write(dir.join(file), body).expect("write file");
    git_in(dir, &["add", "--", file]);
    git_in(dir, &["commit", "-q", "-m", subject]);
}

impl Repo {
    /// A repo on the Segment branch with one commit; `worktree` is where a
    /// unit worktree would go, not yet created.
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "playbook-segment-unit-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        let work = root.join("work");
        fs::create_dir_all(&work).expect("work dir");
        let root = root.canonicalize().expect("scratch dir resolves");
        let work = root.join("work");
        git_in(&work, &["init", "-q", "-b", SEGMENT_BRANCH]);
        commit_file(&work, "seed.txt", "seed\n", "seed");
        let base = git_in(&work, &["rev-parse", "HEAD"]);
        let worktree = root.join("worktrees").join("wu-1");
        Self {
            root,
            work,
            worktree,
            base,
        }
    }

    fn unit(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["segment", "unit"])
            .args(args)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("playbook runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }

    fn start(&self) -> (i32, String, String) {
        self.unit(&[
            "start",
            WU,
            "--base",
            &self.base,
            "--path",
            self.worktree.to_str().unwrap(),
            "--dir",
            self.work.to_str().unwrap(),
        ])
    }

    fn integrate(&self, files: &[&str]) -> (i32, String, String) {
        self.integrate_with(files, &[])
    }

    fn integrate_with(&self, files: &[&str], extra: &[&str]) -> (i32, String, String) {
        let mut args = vec![
            "integrate",
            WU,
            "--base",
            &self.base,
            "--path",
            self.worktree.to_str().unwrap(),
            "--dir",
            self.work.to_str().unwrap(),
            "--branch",
            SEGMENT_BRANCH,
            "-m",
            MESSAGE,
            "--files",
        ];
        args.extend(files);
        args.extend(extra);
        self.unit(&args)
    }

    fn resume(&self) -> (i32, String, String) {
        self.unit(&[
            "resume",
            WU,
            "--base",
            &self.base,
            "--dir",
            self.worktree.to_str().unwrap(),
        ])
    }
}

#[test]
fn start_adds_a_worktree_at_the_base_and_prints_its_path() {
    // Arrange
    let repo = Repo::new("start");

    // Act
    let (code, out, err) = repo.start();

    // Assert
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        format!("worktree={} base={}", repo.worktree.display(), repo.base)
    );
    assert_eq!(git_in(&repo.worktree, &["rev-parse", "HEAD"]), repo.base);
    assert!(repo.worktree.join("seed.txt").is_file());
}

#[test]
fn start_twice_reports_the_same_worktree() {
    // Arrange
    let repo = Repo::new("start-twice");
    repo.start();

    // Act
    let (code, out, err) = repo.start();

    // Assert
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("worktree="), "{out}");
}

#[test]
fn integrate_commits_only_the_listed_files_and_removes_the_worktree() {
    // Arrange
    let repo = Repo::new("integrate");
    repo.start();
    commit_file(&repo.worktree, LISTED, "listed\n", "wip(WU-1): red - s1");
    commit_file(&repo.worktree, UNLISTED, "other\n", "wip(WU-1): green - s1");

    // Act
    let (code, out, err) = repo.integrate(&[LISTED]);

    // Assert
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("integrated="), "{out}");
    assert_eq!(
        git_in(
            &repo.work,
            &["rev-list", "--count", &format!("{}..HEAD", repo.base)]
        ),
        "1"
    );
    assert_eq!(git_in(&repo.work, &["log", "-1", "--format=%s"]), MESSAGE);
    assert_eq!(
        git_in(&repo.work, &["show", "--name-only", "--format=", "HEAD"]),
        LISTED
    );
    assert!(git_in(&repo.work, &["log", "-1", "--format=%B"]).contains("Signed-off-by:"));
    assert!(!repo.worktree.exists());
}

#[test]
fn integrate_reports_a_cherry_pick_conflict_and_keeps_the_worktree() {
    // Arrange
    let repo = Repo::new("conflict");
    repo.start();
    commit_file(
        &repo.worktree,
        LISTED,
        "from the unit\n",
        "wip(WU-1): red - s1",
    );
    commit_file(&repo.work, LISTED, "from the branch\n", "branch moved on");

    // Act
    let (code, _out, err) = repo.integrate(&[LISTED]);

    // Assert
    assert_eq!(code, 1);
    assert!(err.contains("conflict"), "{err}");
    let marker = fs::read_to_string(repo.worktree.join(CONFLICT_MARKER)).expect("marker");
    assert_eq!(marker.lines().count(), 2, "{marker}");
    assert!(
        !repo.work.join(".git").join("CHERRY_PICK_HEAD").exists(),
        "the Segment branch checkout was left mid cherry-pick"
    );
    assert_eq!(
        fs::read_to_string(repo.work.join(LISTED)).unwrap(),
        "from the branch\n"
    );
}

#[test]
fn resume_with_no_checkpoint_starts_at_red() {
    // Arrange
    let repo = Repo::new("resume-none");
    repo.start();

    // Act
    let (code, out, err) = repo.resume();

    // Assert
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "last=none next=red");
}

#[test]
fn resume_after_green_says_refactor_for_the_same_scenario() {
    // Arrange
    let repo = Repo::new("resume-green");
    repo.start();
    commit_file(&repo.worktree, "a.rs", "a\n", "wip(WU-1): red - s2");
    commit_file(&repo.worktree, "b.rs", "b\n", "wip(WU-1): green - s2");

    // Act
    let (code, out, err) = repo.resume();

    // Assert
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "last=green scenario=s2 next=refactor");
}

#[test]
fn resume_after_refactor_says_red_and_ignores_other_units() {
    // Arrange
    let repo = Repo::new("resume-refactor");
    repo.start();
    commit_file(&repo.worktree, "a.rs", "a\n", "wip(WU-1): refactor - s1");
    commit_file(&repo.worktree, "b.rs", "b\n", "wip(WU-2): green - s9");

    // Act
    let (code, out, err) = repo.resume();

    // Assert
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "last=refactor scenario=s1 next=red");
}

#[test]
fn resume_json_wraps_the_report_in_the_envelope() {
    // Arrange
    let repo = Repo::new("resume-json");
    repo.start();
    commit_file(&repo.worktree, "a.rs", "a\n", "wip(WU-1): green - s2");

    // Act
    let (code, out, err) = repo.unit(&[
        "resume",
        WU,
        "--base",
        &repo.base,
        "--dir",
        repo.worktree.to_str().unwrap(),
        "--json",
    ]);

    // Assert
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["ok"], true);
    assert_eq!(v["code"], 0);
    assert_eq!(v["last"], "green");
    assert_eq!(v["scenario"], "s2");
    assert_eq!(v["next"], "refactor");
}

#[test]
fn integrate_json_reports_a_conflict_as_a_failed_envelope_with_exit_zero() {
    // Arrange
    let repo = Repo::new("conflict-json");
    repo.start();
    commit_file(
        &repo.worktree,
        LISTED,
        "from the unit\n",
        "wip(WU-1): red - s1",
    );
    commit_file(&repo.work, LISTED, "from the branch\n", "branch moved on");

    // Act
    let (code, out, _err) = repo.integrate_with(&[LISTED], &["--json"]);

    // Assert
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["ok"], false);
    assert_eq!(v["code"], 1);
    assert!(v["error"].as_str().unwrap().contains("conflict"));
}
