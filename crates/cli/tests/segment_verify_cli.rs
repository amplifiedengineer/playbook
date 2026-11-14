// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook segment verify` against a bare remote and clones in scratch
//! directories. The remote is a local path, so nothing touches the network.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const DEFAULT: &str = "main";
const BRANCH: &str = "feat/plan-s1-unit";
const CHANGED: &str = "changed.txt";
const UNTOUCHED: &str = "untouched.txt";

struct Fixture {
    root: PathBuf,
    /// The clone `segment verify` runs in.
    work: PathBuf,
    /// A second clone standing in for whoever merges on the remote.
    merger: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
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
}

fn commit(dir: &Path, file: &str, body: &str) {
    fs::write(dir.join(file), body).expect("write file");
    git(dir, &["add", "--", file]);
    git(dir, &["commit", "-q", "-m", &format!("edit {file}")]);
}

impl Fixture {
    /// `main` holds both files on the remote; the work clone has the Segment
    /// branch with `CHANGED` edited and not yet merged.
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "playbook-segment-verify-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        fs::create_dir_all(&root).expect("scratch dir");
        let root = root.canonicalize().expect("scratch dir resolves");
        let remote = root.join("origin.git");
        let work = root.join("work");
        let merger = root.join("merger");
        git(
            &root,
            &["init", "-q", "--bare", "-b", DEFAULT, "origin.git"],
        );
        git(&root, &["clone", "-q", remote.to_str().unwrap(), "work"]);
        git(&work, &["switch", "-q", "-c", DEFAULT]);
        commit(&work, CHANGED, "one\n");
        commit(&work, UNTOUCHED, "same\n");
        git(&work, &["push", "-q", "origin", DEFAULT]);
        git(&root, &["clone", "-q", remote.to_str().unwrap(), "merger"]);
        git(&work, &["switch", "-q", "-c", BRANCH]);
        commit(&work, CHANGED, "two\n");
        Self { root, work, merger }
    }

    /// Land the Segment change on the remote default branch as a squash,
    /// from the other clone, so the work clone must fetch to see it.
    fn merge_on_remote(&self) {
        commit(&self.merger, CHANGED, "two\n");
        git(&self.merger, &["push", "-q", "origin", DEFAULT]);
    }

    fn verify(&self, files: &[&str], json: bool) -> (i32, String) {
        let mut args = vec!["segment", "verify", "--branch", BRANCH, "--files"];
        args.extend_from_slice(files);
        if json {
            args.push("--json");
        }
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("playbook runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )
    }
}

#[test]
fn a_merged_branch_matches_after_the_fetch() {
    // Arrange
    let fx = Fixture::new("merged");
    fx.merge_on_remote();

    // Act
    let (code, out) = fx.verify(&[CHANGED, UNTOUCHED], false);

    // Assert
    assert_eq!(code, 0, "{out}");
    assert_eq!(out, "match=true");
}

#[test]
fn an_unmerged_branch_names_the_files_that_differ() {
    // Arrange
    let fx = Fixture::new("unmerged");

    // Act
    let (code, out) = fx.verify(&[CHANGED, UNTOUCHED], false);

    // Assert
    assert_eq!(code, 1, "{out}");
    assert_eq!(out, format!("match=false differing={CHANGED}"));
}

#[test]
fn only_the_listed_files_are_compared() {
    // Arrange
    let fx = Fixture::new("scoped");

    // Act
    let (code, out) = fx.verify(&[UNTOUCHED], false);

    // Assert
    assert_eq!(code, 0, "{out}");
    assert_eq!(out, "match=true");
}

#[test]
fn json_carries_the_verdict_and_exits_zero_even_when_files_differ() {
    // Arrange
    let fx = Fixture::new("json");

    // Act
    let (code, out) = fx.verify(&[CHANGED], true);

    // Assert
    assert_eq!(code, 0, "{out}");
    let v: Value = serde_json::from_str(&out).expect("envelope is JSON");
    assert_eq!(v["ok"], false);
    assert_eq!(v["code"], 1);
    assert_eq!(v["matches"], false);
    assert_eq!(v["differing"], serde_json::json!([CHANGED]));
}

#[test]
fn a_missing_branch_is_an_error() {
    // Arrange
    let fx = Fixture::new("nobranch");

    // Act
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args([
            "segment",
            "verify",
            "--branch",
            "feat/none",
            "--files",
            CHANGED,
        ])
        .current_dir(&fx.work)
        .env("HOME", &fx.root)
        .output()
        .expect("playbook runs");

    // Assert
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("feat/none"));
}
