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
nums=($FAKE_STACK)
# A per-PR override (FAKE_<NAME>_<pr>) wins over the global one.
var() {
  local v="$1_$2"
  if [ -n "${!v}" ]; then printf '%s' "${!v}"; else printf '%s' "$3"; fi
}
state_of() {
  local s
  s="$(var FAKE_STATE "$1" "${FAKE_STATE:-OPEN}")"
  if [ -n "$FAKE_STACK" ] && [ -f "$d/enqueued" ] && [ "$FAKE_STUCK" != "$1" ]; then s=MERGED; fi
  printf '%s' "$s"
}
base_of() {
  local b=main prev= x
  for x in "${nums[@]}"; do
    if [ "$x" = "$1" ] && [ -n "$prev" ]; then b="b$prev"; fi
    prev=$x
  done
  printf '%s' "$b"
}
case "$1 $2" in
  "pr view")
    [ -n "$FAKE_VIEW_FAIL" ] && exit 1
    n="$3"
    state="$(state_of "$n")"
    [ -n "$FAKE_MERGES_ON_ARM" ] && [ -f "$d/armed" ] && state=MERGED
    auto=null
    if [ "$FAKE_STALE_AUTO" = "$n" ] && [ ! -f "$d/disabled" ]; then auto='{"enabledBy":{"login":"x"}}'; fi
    printf '{"number":%s,"headRefName":"b%s","state":"%s","isDraft":%s,"headRefOid":"abc","baseRefName":"%s","mergeStateStatus":"%s","reviewDecision":"%s","autoMergeRequest":%s}' \
      "$n" "$n" "$state" "$(var FAKE_DRAFT "$n" "${FAKE_DRAFT:-false}")" "$(base_of "$n")" \
      "$(var FAKE_MERGE_STATE "$n" "${FAKE_MERGE_STATE:-CLEAN}")" "${FAKE_REVIEW:-}" "$auto";;
  "pr checks")
    if [ -n "$FAKE_CHECKS" ]; then printf '%s' "$FAKE_CHECKS"
    else printf '[{"name":"t","workflow":"ci","bucket":"%s","startedAt":"2026-01-01T00:00:01Z"}]' "$(var FAKE_BUCKET "$3" "${FAKE_BUCKET:-pass}")"; fi;;
  "pr list")
    case "$*" in
      *--base*) printf '[]';;
      *) out=""
         for x in "${nums[@]}"; do
           [ "$(state_of "$x")" = OPEN ] && out="$out${out:+,}{\"headRefName\":\"b$x\",\"isCrossRepository\":false}"
         done
         printf '[%s]' "$out";;
    esac;;
  "repo view")
    case "$*" in
      *nameWithOwner*) printf '{"nameWithOwner":"o/r","defaultBranchRef":{"name":"main"}}';;
      *) printf '{"squashMergeAllowed":%s,"mergeCommitAllowed":true,"rebaseMergeAllowed":false}' "${FAKE_SQUASH:-true}";;
    esac;;
  "api graphql")
    if [ -z "$FAKE_STACK" ]; then
      printf '{"data":{"repository":{"pullRequest":{"stack":null}}}}'
    else
      nodes=""; i=1
      for x in "${nums[@]}"; do
        nodes="$nodes${nodes:+,}{\"position\":$i,\"pullRequest\":{\"number\":$x,\"state\":\"$(state_of "$x")\",\"isDraft\":false,\"baseRefName\":\"$(base_of "$x")\",\"headRefName\":\"b$x\",\"headRefOid\":\"abc\"}}"
        i=$((i+1))
      done
      printf '{"data":{"repository":{"pullRequest":{"stack":{"entries":{"nodes":[%s]}}}}}}' "$nodes"
    fi;;
  "api -X")
    case "$*" in *merge_action=merge_queue*sha=abc*) ;; *) echo "bad merge-async body" >&2; exit 1;; esac
    if [ -n "$FAKE_STALE_AUTO" ] && [ ! -f "$d/disabled" ]; then
      echo "422 auto-merge is enabled" >&2; exit 1
    fi
    [ -n "$FAKE_ENQUEUE_FAIL" ] && { echo "merge queue is not enabled" >&2; exit 1; }
    touch "$d/enqueued"; echo '{"job_id":"j"}';;
  "pr merge")
    case "$*" in *--disable-auto*) touch "$d/disabled"; echo disabled; exit 0;; esac
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

const STACK: &str = "5 6 7";
const MERGE_ASYNC: &str = "pulls/7/merge-async";

/// One stacked fixture: the env that shapes it and what the merge reports.
struct StackRow {
    name: &'static str,
    env: &'static [(&'static str, &'static str)],
    line: &'static str,
    code: i32,
}

const STACK_STOPS: &[StackRow] = &[
    StackRow {
        name: "a red middle PR",
        env: &[("FAKE_BUCKET_6", "fail")],
        line: "MERGE=FAILED reason=a required check failed blocked_by=#6",
        code: 6,
    },
    StackRow {
        name: "a draft bottom PR",
        env: &[("FAKE_DRAFT_5", "true")],
        line: "MERGE=GATE reason=the PR is a draft blocked_by=#5",
        code: 3,
    },
    StackRow {
        name: "a middle PR that needs a rebase",
        env: &[("FAKE_MERGE_STATE_6", "DIRTY")],
        line: "MERGE=GATE reason=the branch needs a rebase blocked_by=#6",
        code: 3,
    },
];

fn stacked_env<'a>(extra: &[(&'static str, &'a str)]) -> Vec<(&'static str, &'a str)> {
    let mut env = vec![("FAKE_STACK", STACK)];
    env.extend_from_slice(extra);
    env
}

#[test]
fn a_green_three_pr_stack_enqueues_the_top_once_and_waits_for_all() {
    // Arrange
    let rig = Rig::new(&stacked_env(&[]));

    // Act
    let out = rig.run(&args_for("merge", &[]));

    // Assert
    assert!(stdout(&out).starts_with("MERGE=MERGED"), "{}", stdout(&out));
    assert_eq!(out.status.code(), Some(0));
    let log = rig.log();
    assert_eq!(log.matches("merge-async").count(), 1, "{log}");
    let call = log.lines().find(|l| l.contains("merge-async")).unwrap();
    assert!(call.contains(MERGE_ASYNC), "{call}");
    assert!(call.contains("merge_action=merge_queue"), "{call}");
    assert!(call.contains(&format!("sha={HEAD}")), "{call}");
    assert!(!log.contains("--auto"), "{log}");
    let after = log.split("merge-async").nth(1).unwrap();
    for pr in ["5", "6", "7"] {
        assert!(after.contains(&format!("pr view {pr} ")), "{pr}: {log}");
    }
}

#[test]
fn a_stack_that_fails_a_gate_stops_naming_the_pr_and_never_enqueues() {
    for row in STACK_STOPS {
        // Arrange
        let rig = Rig::new(&stacked_env(row.env));

        // Act
        let out = rig.run(&args_for("merge", &[]));

        // Assert
        let text = stdout(&out);
        assert_eq!(text.lines().next(), Some(row.line), "{}: {text}", row.name);
        assert_eq!(out.status.code(), Some(row.code), "{}", row.name);
        assert!(!rig.log().contains("merge-async"), "{}", row.name);
    }
}

#[test]
fn a_stale_auto_merge_is_disabled_before_the_enqueue() {
    // Arrange
    let rig = Rig::new(&stacked_env(&[("FAKE_STALE_AUTO", "7")]));

    // Act
    let out = rig.run(&args_for("merge", &[]));

    // Assert
    assert!(stdout(&out).starts_with("MERGE=MERGED"), "{}", stdout(&out));
    let log = rig.log();
    let disable = log.find(&format!("pr merge {PR} --disable-auto")).unwrap();
    let enqueue = log.find("merge-async").unwrap();
    assert!(disable < enqueue, "{log}");
}

#[test]
fn admin_on_a_stack_is_a_usage_error() {
    // Arrange
    let rig = Rig::new(&stacked_env(&[]));

    // Act
    let out = rig.run(&["pr", "merge", PR, "--admin"]);

    // Assert
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("--admin") && err.contains("stack"), "{err}");
    let log = rig.log();
    assert!(
        !log.contains("--admin") && !log.contains("merge-async"),
        "{log}"
    );
}

#[test]
fn no_wait_on_a_stack_enqueues_and_reports_armed() {
    for (extra, json) in [(&[][..], false), (&["--json"][..], true)] {
        // Arrange
        let rig = Rig::new(&stacked_env(&[]));
        let mut args = vec!["pr", "merge", PR, "--no-wait"];
        args.extend_from_slice(extra);

        // Act
        let out = rig.run(&args);

        // Assert
        assert_eq!(out.status.code(), Some(0));
        let log = rig.log();
        assert_eq!(log.matches("merge-async").count(), 1, "{log}");
        assert!(!log.contains("--auto"), "{log}");
        if json {
            let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
            assert_eq!(v["verdict"], "ARMED");
        } else {
            assert!(stdout(&out).starts_with("MERGE=ARMED"), "{}", stdout(&out));
        }
    }
}

#[test]
fn a_pr_that_is_not_stacked_never_calls_merge_async() {
    // Arrange
    let rig = Rig::new(&[("FAKE_MERGES_ON_ARM", "1")]);

    // Act
    let out = rig.run(&args_for("merge", &[]));

    // Assert
    assert!(stdout(&out).starts_with("MERGE=MERGED"), "{}", stdout(&out));
    assert!(!rig.log().contains("merge-async"), "{}", rig.log());
}

#[test]
fn a_stack_that_never_finishes_merging_times_out_naming_the_stuck_pr() {
    // Arrange
    let rig = Rig::new(&stacked_env(&[("FAKE_STUCK", "6")]));

    // Act
    let out = rig.run(&args_for("merge", &[]));

    // Assert
    assert_eq!(
        stdout(&out).lines().next(),
        Some("MERGE=TIMEOUT reason=still waiting after the timeout blocked_by=#6")
    );
    assert_eq!(out.status.code(), Some(7));
}
