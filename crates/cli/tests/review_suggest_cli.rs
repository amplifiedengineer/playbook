// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook review suggest` on a local branch: no network, no `gh`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Run `program` with a scratch home, so neither the user's config nor their
/// global git settings reach a test.
fn run(dir: &Path, program: &str, args: &[&str]) -> Output {
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    Command::new(program)
        .args(args)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect(program)
}

/// A repo with `main`, then a `work` branch that writes each `(path, lines)`.
fn repo(name: &str, files: &[(&str, usize)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pb-suggest-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let d = dir.to_str().unwrap();
    let git = |args: &[&str]| {
        let all: Vec<&str> = ["-C", d].iter().chain(args).copied().collect();
        assert!(run(&dir, "git", &all).status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    for kv in [
        ["user.name", "t"],
        ["user.email", "t@example.com"],
        ["commit.gpgsign", "false"],
    ] {
        git(&["config", kv[0], kv[1]]);
    }
    fs::write(dir.join("base.txt"), "base\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["switch", "-q", "-c", "work"]);
    for (path, lines) in files {
        let full = dir.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, "x\n".repeat(*lines)).unwrap();
    }
    git(&["add", "-A"]);
    git(&["commit", "-q", "--allow-empty", "-m", "work"]);
    dir
}

fn suggest(dir: &Path, extra: &[&str]) -> (i32, String, String) {
    let mut args = vec!["review", "suggest", "--dir", dir.to_str().unwrap()];
    if !extra.iter().any(|a| a.starts_with("--base")) {
        args.extend(["--base", "main"]);
    }
    args.extend(extra);
    let out = run(dir, env!("CARGO_BIN_EXE_playbook"), &args);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

#[test]
fn a_workflow_change_is_a_high_confidence_deep_in_json() {
    let dir = repo("wf", &[(".github/workflows/ci.yml", 3)]);
    let (code, out, _) = suggest(&dir, &["--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        (v["recommendation"].as_str(), v["confidence"].as_str()),
        (Some("deep"), Some("high"))
    );
    assert_eq!(v["rules"][0], "workflows");
}

#[test]
fn a_small_change_is_quick_and_the_cut_is_not_a_config_key() {
    let dir = repo("cfg", &[("src/a.py", 10)]);
    let (_, out, _) = suggest(&dir, &[]);
    assert!(
        out.starts_with("recommendation=quick\nconfidence=high\n"),
        "{out}"
    );
    let set = run(
        &dir,
        env!("CARGO_BIN_EXE_playbook"),
        &[
            "config",
            "set",
            "--global",
            "review.suggest.quickLines",
            "5",
        ],
    );
    assert!(!set.status.success(), "a fixed key must not be settable");
    let (_, out, _) = suggest(&dir, &[]);
    assert!(
        out.starts_with("recommendation=quick\nconfidence=high\n"),
        "{out}"
    );
}

#[test]
fn no_changes_and_a_bad_base_fail_with_a_message() {
    let dir = repo("none", &[]);
    let (code, _, err) = suggest(&dir, &[]);
    assert_eq!((code, err.contains("no changed files")), (1, true), "{err}");
    let (code, _, err) = suggest(&dir, &["--base", "nope"]);
    assert_eq!((code, err.contains("not found")), (1, true), "{err}");
    let (code, _, err) = suggest(&dir, &["--base=-x"]);
    assert_eq!((code, err.contains("as an option")), (1, true), "{err}");
}

#[test]
fn the_text_answer_ends_with_the_scope() {
    let dir = repo("scope-line", &[("src/a.py", 10)]);
    let (code, out, _) = suggest(&dir, &[]);
    assert_eq!(code, 0);
    assert_eq!(out.lines().last(), Some("scope=this-pr"), "{out}");
}

#[test]
fn a_stack_scope_without_a_pr_number_fails_and_a_bad_scope_is_a_usage_error() {
    let dir = repo("scope-pr", &[("src/a.py", 10)]);
    let (code, _, err) = suggest(&dir, &["--scope", "stack"]);
    assert_eq!(
        (code, err.contains("--scope stack needs a PR number")),
        (1, true),
        "{err}"
    );
    let (code, _, err) = suggest(&dir, &["--scope", "bogus"]);
    assert_eq!((code, err.contains("bogus")), (2, true), "{err}");
}
