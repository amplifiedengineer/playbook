// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr ci-wait`, `land-wait`, `merge`, `plans` and `path --create`
//! against a fake `gh` on PATH. They replace the bash blocks of
//! `commands/implement.md`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("pb-wait-{tag}-{}-{n}", playbook::testing::run_id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        Sandbox { root }
    }

    /// A fake `gh` whose body is `script`, a POSIX shell fragment.
    fn gh(&self, script: &str) {
        let f = self.root.join("bin/gh");
        fs::write(&f, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .env("HOME", self.root.join("home"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .current_dir(&self.root)
            .output()
            .unwrap()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn ci_wait_reads_the_json_even_when_gh_exits_nonzero() {
    let s = Sandbox::new("fail");
    s.gh(r#"echo '[{"name":"a","bucket":"fail"},{"name":"b","bucket":"pass"}]'; exit 8"#);
    let o = s.run(&["pr", "ci-wait", "7", "--timeout", "5", "--interval", "0"]);
    let t = out(&o);
    assert!(
        t.contains("checks total=2 pending=0 fail=1 cancel=0"),
        "{t}"
    );
    assert!(t.contains("CI_VERDICT=FAIL"), "{t}");
}

#[test]
fn ci_wait_passes_and_reports_none_for_no_checks() {
    let s = Sandbox::new("pass");
    s.gh(r#"echo '[{"name":"a","bucket":"pass"}]'"#);
    assert!(out(&s.run(&["pr", "ci-wait", "7", "--interval", "0"])).contains("CI_VERDICT=PASS"));
    s.gh("echo '[]'");
    assert!(out(&s.run(&["pr", "ci-wait", "7", "--interval", "0"])).contains("CI_VERDICT=NONE"));
}

#[test]
fn ci_wait_with_an_unreadable_gh_reads_as_no_checks() {
    let s = Sandbox::new("broken");
    s.gh("echo 'not json'; exit 1");
    assert!(out(&s.run(&["pr", "ci-wait", "7", "--interval", "0"])).contains("CI_VERDICT=NONE"));
}

#[test]
fn ci_wait_times_out_while_checks_stay_pending() {
    let s = Sandbox::new("timeout");
    s.gh(r#"echo '[{"name":"a","bucket":"pending"}]'"#);
    let o = s.run(&["pr", "ci-wait", "7", "--timeout", "0", "--interval", "0"]);
    assert!(out(&o).contains("CI_VERDICT=TIMEOUT"), "{}", out(&o));
}

#[test]
fn ci_wait_reads_required_checks_by_default_and_all_on_request() {
    let s = Sandbox::new("all");
    s.gh(
        r#"case "$*" in *--required*) echo '[]';; *) echo '[{"name":"a","bucket":"pass"}]';; esac"#,
    );
    let t = out(&s.run(&["pr", "ci-wait", "7", "--interval", "0"]));
    assert!(t.contains("CI_VERDICT=NONE"), "{t}");
    let t = out(&s.run(&[
        "pr",
        "ci-wait",
        "7",
        "--all",
        "--settle",
        "0",
        "--interval",
        "0",
    ]));
    assert!(t.contains("CI_VERDICT=PASS"), "{t}");
}

#[test]
fn merge_passes_the_head_commit_to_gh() {
    let s = Sandbox::new("match-head");
    s.gh(r#"echo "$@""#);
    let t = out(&s.run(&["pr", "merge", "7", "--no-wait", "--match-head", "abc123"]));
    assert!(
        t.contains("pr merge 7 --auto --match-head-commit abc123"),
        "{t}"
    );
}

#[test]
fn land_wait_ends_on_merged() {
    let s = Sandbox::new("land");
    s.gh(r#"echo '{"state":"MERGED","mergeStateStatus":"CLEAN","reviewDecision":null,"autoMergeRequest":null}'"#);
    let t = out(&s.run(&["pr", "land-wait", "7", "--interval", "0"]));
    assert!(t.contains("MERGED\tCLEAN\t-\t-"), "{t}");
    assert!(t.contains("LAND_VERDICT=MERGED"), "{t}");
}

#[test]
fn land_wait_names_each_gate() {
    for (state, review, verdict) in [
        ("BLOCKED", "REVIEW_REQUIRED", "REVIEW_GATE"),
        ("BLOCKED", "CHANGES_REQUESTED", "CHANGES_REQUESTED"),
        ("DIRTY", "", "RESTATE"),
    ] {
        let s = Sandbox::new("gate");
        let review = if review.is_empty() {
            "null".to_string()
        } else {
            format!("\"{review}\"")
        };
        s.gh(&format!(
            r#"echo '{{"state":"OPEN","mergeStateStatus":"{state}","reviewDecision":{review},"autoMergeRequest":null}}'"#
        ));
        let t = out(&s.run(&["pr", "land-wait", "7", "--interval", "0"]));
        assert!(t.contains(&format!("LAND_VERDICT={verdict}")), "{t}");
    }
}

#[test]
fn merge_prints_the_exit_code_and_gh_output_and_exits_zero() {
    let s = Sandbox::new("merge");
    s.gh(r#"echo "queued"; echo "warn" >&2; exit 1"#);
    let o = s.run(&["pr", "merge", "7", "--no-wait"]);
    assert!(o.status.success());
    let t = out(&o);
    assert!(t.starts_with("merge_rc=1\n"), "{t}");
    assert!(t.contains("queued") && t.contains("warn"), "{t}");
    s.gh(r#"echo "$@""#);
    let t = out(&s.run(&["pr", "merge", "7", "--admin"]));
    assert!(
        t.starts_with("admin_rc=0\n") && t.contains("--admin --squash"),
        "{t}"
    );
}

fn repo(s: &Sandbox) -> PathBuf {
    let r = s.root.join("repo");
    fs::create_dir_all(&r).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", "https://github.com/o/r.git"],
    ] {
        assert!(Command::new("git")
            .args(&args)
            .current_dir(&r)
            .status()
            .unwrap()
            .success());
    }
    r
}

#[test]
fn plans_lists_nothing_then_a_blueprint() {
    let s = Sandbox::new("plans");
    let r = repo(&s);
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .env("HOME", s.root.join("home"))
            .current_dir(&r)
            .output()
            .unwrap()
    };
    assert_eq!(out(&run(&["plans"])).trim(), "NO_PLANS");
    fs::create_dir_all(r.join("docs/adr")).unwrap();
    fs::write(
        r.join("docs/adr/0001-a-blueprint.md"),
        "# Do a thing\nStatus: accepted\n",
    )
    .unwrap();
    let t = out(&run(&["plans"]));
    assert_eq!(
        t.trim(),
        "docs/adr/0001-a-blueprint.md\t[accepted]\tDo a thing"
    );
    let j: serde_json::Value = serde_json::from_str(&out(&run(&["plans", "--json"]))).unwrap();
    assert_eq!(j[0]["status"], "accepted");
}

#[test]
fn path_create_makes_the_folder_and_without_it_does_not() {
    let s = Sandbox::new("path");
    let r = repo(&s);
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .env("HOME", s.root.join("home"))
            .current_dir(&r)
            .output()
            .unwrap()
    };
    let plain = out(&run(&["path", "implement"])).trim().to_string();
    assert!(!PathBuf::from(&plain).exists());
    let made = out(&run(&["path", "implement", "--create"]))
        .trim()
        .to_string();
    assert_eq!(plain, made);
    assert!(PathBuf::from(&made).is_dir());
}
