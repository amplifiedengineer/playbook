// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr checks` through the binary with a scripted `gh` on `PATH`:
//! the failure triage fields, `--watch` and its timeout, `--fail-fast` and the
//! `--required` marks. No test reaches the network.

#[path = "support/isolated.rs"]
mod isolated;

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

const PR: &str = "7";
const RUN_ID: u64 = 111;
const JOB_ID: u64 = 222;
const SETUP_STEP: &str = "Set up job";
const TEST_STEP: &str = "Run cargo test";
const CODE_CHECK_FAILED: i64 = 6;
const CODE_TIMEOUT: i64 = 7;

/// A `gh` that prints `checks.json` for `pr checks` (`required.json` when
/// `--required` is given and the file exists) and `jobs.json` for `api`.
const FAKE_GH: &str = r#"#!/bin/sh
d="$FAKE_DIR"
echo "$*" >> "$d/log"
case "$1 $2" in
  "pr checks")
    if [ -f "$d/stderr.txt" ]; then cat "$d/stderr.txt" >&2; exit 1; fi
    case "$*" in
      *--required*) if [ -f "$d/required.json" ]; then cat "$d/required.json"; else cat "$d/checks.json"; fi;;
      *) cat "$d/checks.json";;
    esac
    exit 8;;
  "api repos/"*) cat "$d/jobs.json";;
esac
"#;

struct Rig {
    root: PathBuf,
}

impl Rig {
    fn new(checks: &str) -> Rig {
        let root = isolated::fresh_home();
        fs::create_dir_all(root.join("bin")).unwrap();
        let gh = root.join("bin/gh");
        fs::write(&gh, FAKE_GH).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        let rig = Rig { root };
        rig.file("checks.json", checks);
        rig.file("jobs.json", r#"{"jobs":[]}"#);
        rig
    }

    fn file(&self, name: &str, body: &str) {
        fs::write(self.root.join(name), body).unwrap();
    }

    fn run(&self, extra: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
        isolated::isolate(&mut cmd, &self.root);
        cmd.args(["pr", "checks", PR])
            .args(extra)
            .current_dir(&self.root)
            .env("PATH", path)
            .env("FAKE_DIR", &self.root);
        cmd.output().expect("playbook spawns")
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.join("log")).unwrap_or_default()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn check(name: &str, bucket: &str, with_link: bool) -> String {
    let link = if with_link {
        format!(r#","link":"https://github.com/o/r/actions/runs/{RUN_ID}/job/{JOB_ID}""#)
    } else {
        String::new()
    };
    format!(
        r#"{{"name":"{name}","workflow":"ci","bucket":"{bucket}","state":"{}","startedAt":"2026-01-01T00:00:01Z"{link}}}"#,
        bucket.to_uppercase()
    )
}

fn jobs_with_steps(steps: &[(&str, &str)]) -> String {
    let steps: Vec<String> = steps
        .iter()
        .map(|(name, conclusion)| format!(r#"{{"name":"{name}","conclusion":"{conclusion}"}}"#))
        .collect();
    format!(
        r#"{{"jobs":[{{"id":{JOB_ID},"name":"build","conclusion":"failure","steps":[{}]}}]}}"#,
        steps.join(",")
    )
}

fn json_of(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).expect("stdout is one JSON value")
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn a_cancelled_job_is_a_transient_failure_without_a_step() {
    // Arrange
    let rig = Rig::new(&format!("[{}]", check("build", "cancel", true)));

    // Act
    let o = rig.run(&["--json"]);

    // Assert
    let v = json_of(&o);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(v["code"], CODE_CHECK_FAILED);
    assert_eq!(v["checks"].as_array().unwrap().len(), 1);
    assert_eq!(v["failed"][0]["run_id"], RUN_ID);
    assert_eq!(v["failed"][0]["step"], "");
    assert_eq!(v["failed"][0]["transient"], true);
}

#[test]
fn a_failed_setup_step_is_transient() {
    // Arrange
    let rig = Rig::new(&format!("[{}]", check("build", "fail", true)));
    rig.file(
        "jobs.json",
        &jobs_with_steps(&[(SETUP_STEP, "failure"), (TEST_STEP, "skipped")]),
    );

    // Act
    let v = json_of(&rig.run(&["--json"]));

    // Assert
    assert_eq!(v["code"], CODE_CHECK_FAILED);
    assert_eq!(v["failed"][0]["run_id"], RUN_ID);
    assert_eq!(v["failed"][0]["step"], SETUP_STEP);
    assert_eq!(v["failed"][0]["transient"], true);
}

#[test]
fn a_real_failure_names_its_step_and_is_not_transient() {
    // Arrange
    let rig = Rig::new(&format!("[{}]", check("build", "fail", true)));
    rig.file(
        "jobs.json",
        &jobs_with_steps(&[(SETUP_STEP, "success"), (TEST_STEP, "failure")]),
    );

    // Act
    let v = json_of(&rig.run(&["--json"]));

    // Assert
    assert_eq!(v["failed"][0]["name"], "build");
    assert_eq!(v["failed"][0]["run_id"], RUN_ID);
    assert_eq!(v["failed"][0]["step"], TEST_STEP);
    assert_eq!(v["failed"][0]["transient"], false);
}

#[test]
fn all_green_lists_the_checks_and_no_failures() {
    // Arrange
    let rig = Rig::new(&format!("[{}]", check("build", "pass", true)));

    // Act
    let o = rig.run(&["--json"]);

    // Assert
    let v = json_of(&o);
    assert_eq!(
        (v["ok"].clone(), v["code"].clone()),
        (true.into(), 0.into())
    );
    assert_eq!(v["verdict"], "PASS");
    assert_eq!(v["checks"][0]["name"], "build");
    assert_eq!(v["checks"][0]["state"], "PASS");
    assert!(v["failed"].as_array().unwrap().is_empty());
}

#[test]
fn plain_mode_prints_the_verdict_and_exits_with_the_check_failed_code() {
    // Arrange
    let rig = Rig::new(&format!("[{}]", check("build", "fail", true)));

    // Act
    let o = rig.run(&[]);

    // Assert
    assert_eq!(o.status.code(), Some(CODE_CHECK_FAILED as i32));
    assert!(text(&o).contains("CHECKS=FAIL"), "{}", text(&o));
}

#[test]
fn pending_checks_without_watch_exit_with_the_timeout_code() {
    // Arrange
    let rig = Rig::new(&format!("[{}]", check("build", "pending", false)));

    // Act
    let o = rig.run(&[]);

    // Assert
    assert_eq!(o.status.code(), Some(CODE_TIMEOUT as i32));
    assert!(text(&o).contains("CHECKS=PENDING"), "{}", text(&o));
}

#[test]
fn watch_past_the_timeout_exits_7_and_with_json_reports_code_7() {
    // Arrange
    let rig = Rig::new(&format!("[{}]", check("build", "pending", false)));
    let args = ["--watch", "--timeout", "1", "--interval", "0"];

    // Act
    let plain = rig.run(&args);
    let json = rig.run(&[&args[..], &["--json"]].concat());

    // Assert
    assert_eq!(plain.status.code(), Some(CODE_TIMEOUT as i32));
    assert!(text(&plain).contains("CHECKS=TIMEOUT"), "{}", text(&plain));
    let v = json_of(&json);
    assert_eq!(json.status.code(), Some(0));
    assert_eq!(v["code"], CODE_TIMEOUT);
    assert_eq!(v["verdict"], "TIMEOUT");
}

#[test]
fn fail_fast_stops_on_the_first_failure_while_others_are_pending() {
    // Arrange
    let rig = Rig::new(&format!(
        "[{},{}]",
        check("lint", "fail", false),
        check("test", "pending", false)
    ));
    let waiting = ["--watch", "--timeout", "1", "--interval", "0", "--json"];

    // Act
    let fast = rig.run(&[&waiting[..], &["--fail-fast"]].concat());
    let patient = rig.run(&waiting);

    // Assert
    assert_eq!(json_of(&fast)["verdict"], "FAIL");
    assert_eq!(json_of(&fast)["code"], CODE_CHECK_FAILED);
    assert_eq!(json_of(&patient)["verdict"], "TIMEOUT");
}

#[test]
fn required_marks_each_check_and_the_flag_narrows_the_read() {
    // Arrange
    let rig = Rig::new(&format!(
        "[{},{}]",
        check("lint", "pass", false),
        check("docs", "pass", false)
    ));
    rig.file(
        "required.json",
        &format!("[{}]", check("lint", "pass", false)),
    );

    // Act
    let all = json_of(&rig.run(&["--json"]));
    let only = json_of(&rig.run(&["--required", "--json"]));

    // Assert
    assert_eq!(all["checks"][0]["required"], true);
    assert_eq!(all["checks"][1]["required"], false);
    assert_eq!(only["checks"].as_array().unwrap().len(), 1);
    assert_eq!(only["checks"][0]["required"], true);
    assert!(
        rig.log().contains("pr checks 7 --required"),
        "{}",
        rig.log()
    );
}

#[test]
fn a_pr_gh_cannot_read_is_a_gh_failure_not_an_empty_list() {
    // Arrange
    let rig = Rig::new("[]");
    rig.file("stderr.txt", "GraphQL: Could not resolve to a PullRequest");

    // Act
    let plain = rig.run(&[]);
    let json = rig.run(&["--json"]);

    // Assert
    assert_eq!(plain.status.code(), Some(5));
    assert!(
        text(&plain).contains("CHECKS=UNREADABLE"),
        "{}",
        text(&plain)
    );
    assert_eq!(json_of(&json)["code"], 5);
    assert_eq!(json.status.code(), Some(0));
}

#[test]
fn no_checks_reported_is_an_empty_pass_not_a_failure() {
    // Arrange
    let rig = Rig::new("[]");
    rig.file(
        "stderr.txt",
        "no required checks reported on the 'x' branch",
    );

    // Act
    let o = rig.run(&["--required"]);

    // Assert
    assert_eq!(o.status.code(), Some(0));
    assert!(text(&o).contains("CHECKS=NONE"), "{}", text(&o));
}
