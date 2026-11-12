// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook review triage merge` and `review size`: the triage fail-open
//! merge and the reviewer-count rule the review commands call.

#[path = "support/isolated.rs"]
mod isolated;

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn playbook(args: &[&str], stdin: Option<&str>) -> (i32, String, String) {
    let mut child = isolated::playbook()
        .args(args)
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("playbook");
    if let Some(text) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    } else {
        drop(child.stdin.take());
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

#[test]
fn triage_merge_reads_stdin_and_completes_the_map() {
    let reply = r#"{"security":{"tier":"skip","reason":"docs only"},"logic":{"tier":"bogus","reason":"x"}}"#;
    let (code, out, _) = playbook(
        &[
            "review",
            "triage",
            "merge",
            "--lenses",
            "security,logic,perf",
            "--tiers",
            "-",
        ],
        Some(reply),
    );
    assert_eq!(code, 0);
    let (json, summary) = out.split_once('\n').unwrap();
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(v["security"]["tier"], "skip");
    assert_eq!(v["logic"]["tier"], "full-lens");
    assert_eq!(v["perf"]["tier"], "full-lens");
    assert_eq!(
        summary,
        "Triage: security=skip, logic=full-lens, perf=full-lens"
    );
}

#[test]
fn triage_merge_with_no_reply_runs_everything_in_full() {
    let (code, out, _) = playbook(&["review", "triage", "merge", "--lenses", "a,b"], None);
    assert_eq!(code, 0);
    assert!(out.ends_with("Triage: a=full-lens, b=full-lens"), "{out}");
}

#[test]
fn triage_merge_reads_a_file_and_a_missing_file_is_a_silent_triage() {
    let dir = std::env::temp_dir().join(format!("pb-triage-merge-{}", playbook::testing::run_id()));
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("t.json");
    fs::write(&file, r#"{"a":{"tier":"cheap-check","reason":"r"}}"#).unwrap();
    let (_, out, _) = playbook(
        &[
            "review",
            "triage",
            "merge",
            "--lenses",
            "a",
            "--tiers",
            file.to_str().unwrap(),
        ],
        None,
    );
    assert!(out.ends_with("Triage: a=cheap-check"), "{out}");
    let (code, out, _) = playbook(
        &[
            "review",
            "triage",
            "merge",
            "--lenses",
            "a",
            "--tiers",
            "/no/such/file",
        ],
        None,
    );
    assert_eq!(code, 0);
    assert!(out.ends_with("Triage: a=full-lens"), "{out}");
}

#[test]
fn triage_merge_force_overrides_one_lens_and_a_bad_spec_exits_2() {
    let reply = r#"{"tests":{"tier":"full-lens","reason":"x"}}"#;
    let (_, out, _) = playbook(
        &[
            "review",
            "triage",
            "merge",
            "--lenses",
            "tests,scope",
            "--tiers",
            "-",
            "--force",
            "tests=skip:no new tests by explicit user choice",
        ],
        Some(reply),
    );
    assert!(
        out.contains("no new tests by explicit user choice"),
        "{out}"
    );
    assert!(
        out.ends_with("Triage: tests=skip, scope=full-lens"),
        "{out}"
    );

    let (code, _, err) = playbook(
        &[
            "review",
            "triage",
            "merge",
            "--lenses",
            "a",
            "--force",
            "a=maybe:x",
        ],
        None,
    );
    assert_eq!(code, 2);
    assert!(err.contains("bad --force"), "{err}");
}

fn repo_with_change(lines: usize) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pb-review-size-{}-{lines}",
        playbook::testing::run_id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "commit.gpgsign", "false"]);
    fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["switch", "-q", "-c", "work"]);
    fs::write(dir.join("a.txt"), "x\n".repeat(lines)).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "work"]);
    dir
}

#[test]
fn size_picks_one_reviewer_up_to_150_changed_lines_and_the_swarm_above() {
    // 149 added + 1 deleted = 150: still one reviewer.
    let small = repo_with_change(149);
    let (code, out, _) = playbook(
        &[
            "review",
            "size",
            "--base",
            "main",
            "--dir",
            small.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 0);
    assert_eq!(out, "lines=150 files=1 path=single");

    let big = repo_with_change(150);
    let (_, out, _) = playbook(
        &[
            "review",
            "size",
            "--base",
            "main",
            "--dir",
            big.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(out, "lines=151 files=1 path=swarm");
}

#[test]
fn size_with_all_lenses_always_takes_the_swarm_and_a_bad_base_fails() {
    let small = repo_with_change(3);
    let dir = small.to_str().unwrap();
    let (_, out, _) = playbook(
        &[
            "review",
            "size",
            "--base",
            "main",
            "--dir",
            dir,
            "--all-lenses",
        ],
        None,
    );
    assert_eq!(out, "lines=4 files=1 path=swarm");
    let (code, _, err) = playbook(
        &["review", "size", "--base", "no-such-ref", "--dir", dir],
        None,
    );
    assert_eq!(code, 1);
    assert!(
        err.starts_with("error: git diff no-such-ref...HEAD failed"),
        "{err}"
    );
}
