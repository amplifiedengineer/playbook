// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr merge` through the binary with a scripted `gh` on `PATH`: one
//! row per verdict, the merge method fallback, `--no-wait`, and the legacy
//! `pr land` spelling. No test reaches the network.

#[path = "support/isolated.rs"]
mod isolated;

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const PR: &str = "7";
const HEAD: &str = "abc";
const LOG_FILE: &str = "log";

/// The `gh` the tests put on `PATH`. Its answers come from `FAKE_*`
/// variables, and every call is appended to `$FAKE_DIR/log`.
const FAKE_GH: &str = r#"#!/bin/bash
d="$FAKE_DIR"
echo "$*" >> "$d/log"
case "$1 $2" in
  "pr view")
    [ -n "$FAKE_VIEW_FAIL" ] && exit 1
    state="${FAKE_STATE:-OPEN}"
    [ -n "$FAKE_MERGES_ON_ARM" ] && [ -f "$d/armed" ] && state=MERGED
    printf '{"state":"%s","isDraft":%s,"headRefOid":"abc","baseRefName":"main","mergeStateStatus":"%s","reviewDecision":"%s","autoMergeRequest":null}' \
      "$state" "${FAKE_DRAFT:-false}" "${FAKE_MERGE_STATE:-CLEAN}" "${FAKE_REVIEW:-}";;
  "pr checks")
    if [ -n "$FAKE_CHECKS" ]; then printf '%s' "$FAKE_CHECKS"
    else printf '[{"name":"t","workflow":"ci","bucket":"%s","startedAt":"2026-01-01T00:00:01Z"}]' "${FAKE_BUCKET:-pass}"; fi;;
  "pr list") printf '[]';;
  "repo view") printf '{"squashMergeAllowed":%s,"mergeCommitAllowed":true,"rebaseMergeAllowed":false}' "${FAKE_SQUASH:-true}";;
  "pr merge")
    if [ -n "$FAKE_NEEDS_METHOD" ]; then
      case "$*" in *--squash*|*--merge*|*--rebase*) ;; *) echo "merge method is required" >&2; exit 1;; esac
    fi
    [ -n "$FAKE_MERGE_FAIL" ] && { echo "auto-merge is not allowed" >&2; exit 1; }
    touch "$d/armed"; echo armed;;
  *) echo "unexpected gh $*" >&2; exit 1;;
esac
"#;

struct Rig {
    root: PathBuf,
    env: Vec<(&'static str, String)>,
}

impl Rig {
    fn new(env: &[(&'static str, &str)]) -> Rig {
        let root = isolated::fresh_home();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let gh = bin.join("gh");
        fs::write(&gh, FAKE_GH).unwrap();
        Command::new("chmod").arg("+x").arg(&gh).status().unwrap();
        Rig {
            root,
            env: env.iter().map(|(k, v)| (*k, v.to_string())).collect(),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
        isolated::isolate(&mut cmd, &self.root);
        cmd.args(args)
            .current_dir(&self.root)
            .env("PATH", path)
            .env("FAKE_DIR", &self.root);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.output().expect("playbook spawns")
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.join(LOG_FILE)).unwrap_or_default()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn args_for<'a>(sub: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "pr",
        sub,
        PR,
        "--settle",
        "0",
        "--interval",
        "0",
        "--timeout",
        "1",
    ];
    args.extend_from_slice(extra);
    args
}

/// One verdict: the fixture that produces it and what the command reports.
struct Row {
    name: &'static str,
    env: &'static [(&'static str, &'static str)],
    verdict: &'static str,
    reason: &'static str,
    next: &'static str,
    code: i32,
}

const ROWS: &[Row] = &[
    Row {
        name: "merged",
        env: &[("FAKE_STATE", "MERGED")],
        verdict: "MERGED",
        reason: "merged",
        next: "done",
        code: 0,
    },
    Row {
        name: "gate on a required review",
        env: &[("FAKE_REVIEW", "REVIEW_REQUIRED")],
        verdict: "GATE",
        reason: "review required",
        next: "ask",
        code: 3,
    },
    Row {
        name: "gate on a branch that needs a rebase",
        env: &[("FAKE_MERGE_STATE", "BEHIND")],
        verdict: "GATE",
        reason: "the branch needs a rebase",
        next: "rebase",
        code: 3,
    },
    Row {
        name: "gh cannot read the PR",
        env: &[("FAKE_VIEW_FAIL", "1")],
        verdict: "FAILED",
        reason: "could not read the PR",
        next: "ask",
        code: 5,
    },
    Row {
        name: "a required check fails",
        env: &[("FAKE_BUCKET", "fail")],
        verdict: "FAILED",
        reason: "a required check failed",
        next: "fix-ci",
        code: 6,
    },
    Row {
        name: "closed",
        env: &[("FAKE_STATE", "CLOSED")],
        verdict: "CLOSED",
        reason: "the PR is closed",
        next: "ask",
        code: 6,
    },
    Row {
        name: "armed but not merged in time",
        env: &[],
        verdict: "TIMEOUT",
        reason: "still waiting after the timeout",
        next: "wait",
        code: 7,
    },
    Row {
        name: "auto-merge does not queue",
        env: &[("FAKE_MERGE_FAIL", "1")],
        verdict: "NOT_QUEUED",
        reason: "auto-merge did not queue",
        next: "ask",
        code: 8,
    },
];

#[test]
fn each_verdict_prints_its_line_and_exits_with_its_code() {
    for row in ROWS {
        // Arrange
        let rig = Rig::new(row.env);
        let args = args_for("merge", &[]);

        // Act
        let out = rig.run(&args);

        // Assert
        let text = stdout(&out);
        let first = text.lines().next().unwrap_or_default();
        assert_eq!(
            first,
            format!("MERGE={} reason={}", row.verdict, row.reason),
            "{}: {text}",
            row.name
        );
        assert_eq!(out.status.code(), Some(row.code), "{}: {text}", row.name);
    }
}

#[test]
fn each_verdict_under_json_carries_reason_and_next_and_exits_zero() {
    for row in ROWS {
        // Arrange
        let rig = Rig::new(row.env);
        let args = args_for("merge", &["--json"]);

        // Act
        let out = rig.run(&args);

        // Assert
        assert_eq!(out.status.code(), Some(0), "{}", row.name);
        let v: Value = serde_json::from_str(&stdout(&out))
            .unwrap_or_else(|e| panic!("{}: {e}: {}", row.name, stdout(&out)));
        assert_eq!(v["verdict"], row.verdict, "{}", row.name);
        assert_eq!(v["reason"], row.reason, "{}", row.name);
        assert_eq!(v["next"], row.next, "{}", row.name);
        assert_eq!(v["code"], row.code, "{}", row.name);
    }
}

#[test]
fn a_cancelled_run_replaced_by_a_green_one_merges() {
    // Arrange
    let checks = r#"[{"name":"t","workflow":"ci","bucket":"cancel","startedAt":"2026-01-01T00:00:01Z"},{"name":"t","workflow":"ci","bucket":"pass","startedAt":"2026-01-01T00:00:02Z"}]"#;
    let rig = Rig::new(&[("FAKE_CHECKS", checks), ("FAKE_MERGES_ON_ARM", "1")]);

    // Act
    let out = rig.run(&args_for("merge", &[]));

    // Assert
    assert!(stdout(&out).starts_with("MERGE=MERGED"), "{}", stdout(&out));
    assert_eq!(out.status.code(), Some(0));
    assert!(rig
        .log()
        .contains(&format!("pr merge {PR} --auto --match-head-commit {HEAD}")));
}

#[test]
fn a_repo_that_refuses_squash_falls_back_to_the_next_method() {
    // Arrange
    let rig = Rig::new(&[
        ("FAKE_NEEDS_METHOD", "1"),
        ("FAKE_SQUASH", "false"),
        ("FAKE_MERGES_ON_ARM", "1"),
    ]);

    // Act
    let out = rig.run(&args_for("merge", &[]));

    // Assert
    assert!(stdout(&out).starts_with("MERGE=MERGED"), "{}", stdout(&out));
    assert!(rig.log().contains(&format!(
        "pr merge {PR} --auto --merge --match-head-commit {HEAD}"
    )));
}

#[test]
fn no_wait_prints_the_merge_rc_line_and_pins_no_head_by_default() {
    // Arrange
    let rig = Rig::new(&[]);

    // Act
    let out = rig.run(&["pr", "merge", PR, "--no-wait"]);

    // Assert
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).starts_with("merge_rc=0\n"), "{}", stdout(&out));
    let log = rig.log();
    assert!(log.contains(&format!("pr merge {PR} --auto")), "{log}");
    assert!(!log.contains("--match-head-commit"), "{log}");
}

#[test]
fn no_wait_arms_and_reports_armed_under_json() {
    // Arrange
    let rig = Rig::new(&[]);

    // Act
    let out = rig.run(&["pr", "merge", PR, "--no-wait", "--json"]);

    // Assert
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["verdict"], "ARMED");
    assert_eq!(v["next"], "wait");
    assert_eq!(v["code"], 0);
}

#[test]
fn no_wait_with_match_head_pins_the_head() {
    // Arrange
    let rig = Rig::new(&[]);

    // Act
    rig.run(&["pr", "merge", PR, "--no-wait", "--match-head", "def456"]);

    // Assert
    let log = rig.log();
    assert!(
        log.contains(&format!("pr merge {PR} --auto --match-head-commit def456")),
        "{log}"
    );
}

#[test]
fn admin_keeps_the_admin_squash() {
    // Arrange
    let rig = Rig::new(&[]);

    // Act
    let out = rig.run(&["pr", "merge", PR, "--admin"]);

    // Assert
    assert!(stdout(&out).starts_with("admin_rc=0\n"), "{}", stdout(&out));
    assert!(rig
        .log()
        .contains(&format!("pr merge {PR} --admin --squash")));
}

#[test]
fn match_head_without_no_wait_or_admin_is_a_usage_error() {
    // Arrange
    let rig = Rig::new(&[]);

    // Act
    let out = rig.run(&["pr", "merge", PR, "--match-head", HEAD]);

    // Assert
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn pr_land_keeps_its_land_lines_and_codes() {
    for (env, line, code) in [
        (&[("FAKE_STATE", "MERGED")][..], "LAND=MERGED", 0),
        (&[("FAKE_BUCKET", "fail")][..], "LAND=FAIL", 6),
        (&[("FAKE_MERGE_FAIL", "1")][..], "LAND=NOT_QUEUED", 8),
        (&[][..], "LAND=STILL_WAITING", 7),
    ] {
        // Arrange
        let rig = Rig::new(env);

        // Act
        let out = rig.run(&args_for("land", &[]));

        // Assert
        assert!(stdout(&out).starts_with(line), "{line}: {}", stdout(&out));
        assert_eq!(out.status.code(), Some(code), "{line}");
    }
}
