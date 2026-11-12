// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr create` through the binary: the branch guard, the body from
//! stdin and the JSON refusal shape.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(o.status.success(), "{args:?}");
}

fn repo(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "pb-deliver-cli-{tag}-{}",
        playbook::testing::run_id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let (work, origin) = (root.join("work"), root.join("origin.git"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&origin).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    git(&origin, &["init", "-q", "--bare", "-b", "main"]);
    git(&work, &["init", "-q", "-b", "main"]);
    git(
        &work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    std::fs::write(work.join("a.txt"), "a\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "init"]);
    git(&work, &["push", "-q", "-u", "origin", "main"]);
    git(&work, &["remote", "set-head", "origin", "main"]);
    git(&work, &["checkout", "-q", "-b", "feat/x"]);
    std::fs::write(work.join("b.txt"), "b\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "feat: add b"]);
    work
}

fn pr_create(work: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["pr", "create"])
        .args(args)
        .env("HOME", work.parent().unwrap().join("home"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .current_dir(work)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn parity_worktree_guard_a_branch_mismatch_is_wrong_directory_with_exit_3() {
    let work = repo("guard");

    let o = pr_create(&work, &["--branch", "other", "--json", "--dry-run"], "");

    assert_eq!(o.status.code(), Some(3));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["verdict"], "WRONG_DIRECTORY");
}

#[test]
fn the_body_is_read_from_stdin_and_a_refusal_lists_rule_ids() {
    let work = repo("stdin");

    let o = pr_create(
        &work,
        &[
            "--title",
            "feat: add b",
            "--body-file",
            "-",
            "--json",
            "--dry-run",
        ],
        "## Summary\n\nadds b \u{2014} quickly\n",
    );

    assert_eq!(
        o.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["verdict"], "REFUSED");
    assert_eq!(v["problems"][0]["rule"], "dash");
    assert_eq!(v["problems"][0]["where"], "body");
}
