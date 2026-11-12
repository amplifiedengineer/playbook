// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr create` runs the deliver engine: it commits what is staged,
//! prints the deliver report and arms auto-merge on request. The binary runs
//! against a scratch repo with a bare `origin` and a fake `gh` on `PATH`.

#[path = "support/deliver_gh.rs"]
mod deliver_gh;
#[path = "support/isolated.rs"]
mod isolated;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const COMMIT_MESSAGE: &str = "feat(x): add b";
const PR_BODY: &str = "## Summary\n\nadds b quickly\n";
const DELIVERED_LINE: &str = "Delivered: feat/x -> main";
const ARMED_CALL: &str = "pr merge 7 --auto";

struct Rig {
    root: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let root = isolated::fresh_home();
        let rig = Rig { root };
        for d in ["home", "work", "origin.git", "fake"] {
            fs::create_dir_all(rig.root.join(d)).unwrap();
        }
        let gh = rig.fake().join("gh");
        fs::write(&gh, deliver_gh::FAKE_GH).unwrap();
        run(&rig.fake(), "chmod", &["+x", gh.to_str().unwrap()]);
        run(
            &rig.origin(),
            "git",
            &["init", "-q", "--bare", "-b", "main"],
        );
        rig.git(&["init", "-q", "-b", "main"]);
        rig.git(&["remote", "add", "origin", rig.origin().to_str().unwrap()]);
        rig.write("a.txt", "one\n");
        rig.git(&["add", "-A"]);
        rig.git(&["commit", "-q", "-m", "chore: seed"]);
        rig.git(&["push", "-q", "-u", "origin", "main"]);
        rig.git(&["remote", "set-head", "origin", "main"]);
        rig.git(&["checkout", "-q", "-b", "feat/x"]);
        rig
    }

    fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    fn origin(&self) -> PathBuf {
        self.root.join("origin.git")
    }

    fn fake(&self) -> PathBuf {
        self.root.join("fake")
    }

    fn git(&self, args: &[&str]) -> String {
        run(&self.work(), "git", args)
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.work().join(name), text).unwrap();
    }

    fn gh_log(&self) -> String {
        fs::read_to_string(self.fake().join("log")).unwrap_or_default()
    }

    fn pr_create(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.fake().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
        isolated::isolate(&mut cmd, &self.root.join("home"));
        cmd.args(["pr", "create"])
            .args(args)
            .current_dir(self.work())
            .env("PATH", path)
            .env("FAKE_GH_DIR", self.fake())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t.test")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t.test")
            .output()
            .expect("playbook spawns")
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run(dir: &Path, program: &str, args: &[&str]) -> String {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t.test")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t.test")
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn staged_files_are_committed_before_the_push_and_the_deliver_report_prints() {
    // Arrange
    let rig = Rig::new();
    rig.write("b.txt", "new\n");
    rig.git(&["add", "b.txt"]);
    let seed = rig.git(&["rev-parse", "HEAD"]);

    // Act
    let out = rig.pr_create(&["--message", COMMIT_MESSAGE]);

    // Assert
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text(&out).contains(DELIVERED_LINE), "{}", text(&out));
    assert_eq!(rig.git(&["log", "-1", "--format=%s"]), COMMIT_MESSAGE);
    assert_eq!(
        rig.git(&["rev-list", "--count", &format!("{seed}..HEAD")]),
        "1"
    );
    let pushed = run(&rig.origin(), "git", &["rev-parse", "refs/heads/feat/x"]);
    assert_eq!(pushed, rig.git(&["rev-parse", "HEAD"]));
    assert!(rig.gh_log().contains("pr create"));
}

#[test]
fn with_nothing_staged_no_commit_is_made_and_the_pr_opens() {
    // Arrange
    let rig = Rig::new();
    rig.write("b.txt", "new\n");
    rig.git(&["add", "b.txt"]);
    rig.git(&["commit", "-q", "-m", COMMIT_MESSAGE]);
    let head = rig.git(&["rev-parse", "HEAD"]);
    rig.write("body.md", PR_BODY);
    let body_file = rig.work().join("body.md");

    // Act
    let out = rig.pr_create(&[
        "--title",
        COMMIT_MESSAGE,
        "--body-file",
        body_file.to_str().unwrap(),
    ]);

    // Assert
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text(&out).contains(DELIVERED_LINE), "{}", text(&out));
    assert_eq!(rig.git(&["rev-parse", "HEAD"]), head);
    assert!(rig.gh_log().contains("pr create"));
}

#[test]
fn auto_merge_arms_the_pr_after_it_is_ready() {
    // Arrange
    let rig = Rig::new();
    rig.write("b.txt", "new\n");
    rig.git(&["add", "b.txt"]);

    // Act
    let out = rig.pr_create(&["--message", COMMIT_MESSAGE, "--ready", "--auto-merge"]);

    // Assert
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(rig.gh_log().contains(ARMED_CALL), "{}", rig.gh_log());
}
