// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr review post` and `pr review submit` through the binary, with
//! a scripted `gh` on `PATH` that logs every call and the review payload it
//! was given. Nothing here reaches the network.

#[path = "support/isolated.rs"]
mod isolated;

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

const PR: &str = "7";
const REVIEW_ID: &str = "99";
const REVIEW_URL: &str = "https://github.com/o/r/pull/7#pullrequestreview-99";
const GH_FAILED: i32 = 5;
const REFUSED: i32 = 1;

fn fixture() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT")).join("crates/cli/tests/fixtures/review_post")
}

/// A scratch home with a fake `gh` whose review reply carries `state`.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(state: &str) -> Sandbox {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "pb-review-post-{}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        let script = format!(
            r#"#!/bin/sh
echo "$*" >> '{root}/gh.log'
case "$1 $2" in
  "repo view") echo o/r ;;
  "api -X")
    prev=""
    for a in "$@"; do
      if [ "$prev" = "--input" ]; then cp "$a" '{root}/payload.json'; fi
      prev="$a"
    done
    echo '{{"id":{id},"state":"{state}","html_url":"{url}"}}' ;;
  *) exit 1 ;;
esac
"#,
            root = root.display(),
            id = REVIEW_ID,
            url = REVIEW_URL
        );
        let gh = root.join("bin/gh");
        fs::write(&gh, script).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        Sandbox { root }
    }

    fn run(&self, args: &[&str]) -> Output {
        isolated::playbook_in(&self.root)
            .args(["pr", "review"])
            .args(args)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .output()
            .unwrap()
    }

    fn post(&self, extra: &[&str]) -> Output {
        let findings = fixture().join("findings.json");
        self.run(
            &[
                &["post", PR, "--findings", findings.to_str().unwrap()],
                extra,
            ]
            .concat(),
        )
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.join("gh.log")).unwrap_or_default()
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn post_creates_a_pending_review_from_the_findings_file() {
    // Arrange
    let sandbox = Sandbox::new("PENDING");

    // Act
    let out = sandbox.post(&[]);

    // Assert
    assert!(out.status.success(), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(text.contains(&format!("id={REVIEW_ID}")), "{text}");
    assert!(text.contains(&format!("url={REVIEW_URL}")), "{text}");
    assert!(sandbox.log().contains(&format!(
        "api -X POST /repos/o/r/pulls/{PR}/reviews --input"
    )));
    let sent: Value =
        serde_json::from_str(&fs::read_to_string(sandbox.root.join("payload.json")).unwrap())
            .unwrap();
    let wanted: Value =
        serde_json::from_str(&fs::read_to_string(fixture().join("findings.json")).unwrap())
            .unwrap();
    assert_eq!(sent, wanted);
}

#[test]
fn post_json_carries_the_id_and_url() {
    // Arrange
    let sandbox = Sandbox::new("PENDING");

    // Act
    let out = sandbox.post(&["--json"]);

    // Assert
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["id"], 99);
    assert_eq!(v["state"], "PENDING");
    assert_eq!(v["url"], REVIEW_URL);
    assert_eq!(v["comments"], 2);
}

#[test]
fn post_refuses_a_reply_that_is_not_pending() {
    // Arrange
    let sandbox = Sandbox::new("COMMENTED");

    // Act
    let out = sandbox.post(&[]);

    // Assert
    assert_eq!(out.status.code(), Some(GH_FAILED));
}

#[test]
fn post_json_reports_a_non_pending_reply_as_code_five_and_exits_zero() {
    // Arrange
    let sandbox = Sandbox::new("COMMENTED");

    // Act
    let out = sandbox.post(&["--json"]);

    // Assert
    assert!(out.status.success());
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["code"], GH_FAILED);
}

#[test]
fn post_refuses_a_findings_file_with_no_comments() {
    // Arrange
    let sandbox = Sandbox::new("PENDING");
    let empty = sandbox.root.join("empty.json");
    fs::write(&empty, r#"{"commit_id":"abc","comments":[]}"#).unwrap();

    // Act
    let out = sandbox.run(&["post", PR, "--findings", empty.to_str().unwrap()]);

    // Assert
    assert_eq!(out.status.code(), Some(REFUSED));
    assert!(!sandbox.log().contains("api -X POST"));
}

#[test]
fn submit_sends_the_event_and_confirms_the_review_left_pending() {
    // Arrange
    let sandbox = Sandbox::new("COMMENTED");

    // Act
    let out = sandbox.run(&["submit", PR, REVIEW_ID, "--event", "request-changes"]);

    // Assert
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(sandbox.log().contains(&format!(
        "api -X POST /repos/o/r/pulls/{PR}/reviews/{REVIEW_ID}/events -f event=REQUEST_CHANGES"
    )));
    assert!(stdout(&out).contains("state=COMMENTED"));
}

#[test]
fn submit_sends_the_body_when_given_and_lgtm_for_a_bare_approve() {
    // Arrange
    let with_body = Sandbox::new("COMMENTED");
    let bare_approve = Sandbox::new("APPROVED");

    // Act
    with_body.run(&[
        "submit",
        PR,
        REVIEW_ID,
        "--event",
        "comment",
        "--body",
        "Looks fine",
    ]);
    bare_approve.run(&["submit", PR, REVIEW_ID, "--event", "approve"]);

    // Assert
    assert!(with_body.log().contains("-f body=Looks fine"));
    assert!(bare_approve.log().contains("event=APPROVE"));
    assert!(bare_approve.log().contains("-f body=LGTM"));
}

#[test]
fn submit_fails_with_code_five_when_the_review_stays_pending() {
    // Arrange
    let sandbox = Sandbox::new("PENDING");

    // Act
    let out = sandbox.run(&["submit", PR, REVIEW_ID, "--event", "comment"]);

    // Assert
    assert_eq!(out.status.code(), Some(GH_FAILED));
}
