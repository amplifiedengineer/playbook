// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr ci-wait`, `land-wait` and `merge`: the polling loops and the
//! merge wrappers that `commands/implement.md` used to run as bash blocks.
//! The loops take their inputs and clock as parameters, so tests run them
//! without a network or a sleep.

use crate::common::proc::run_with_timeout;
use serde_json::Value;
use std::process::Command;
use std::time::{Duration, Instant};

const GH_TIMEOUT: Duration = Duration::from_secs(60);
const CHECK_FIELDS: &str = "name,state,bucket,link,startedAt,workflow";

/// Check counts from one `gh pr checks` read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub total: usize,
    pub pending: usize,
    pub fail: usize,
    pub cancel: usize,
    /// Cancelled runs that a later run of the same check replaced.
    pub superseded: usize,
}

/// The checks of one read with each check reduced to its latest run.
///
/// Runs group by `(workflow, name)`. A group of several runs keeps the one
/// with the latest `startedAt`, and the older ones count as superseded when
/// they were cancelled, so a cancelled run that a later run replaced is not a
/// failure. A group where any run has no `startedAt` cannot be ordered, so
/// every run counts as it did before.
pub fn latest_checks(json: &str) -> (Vec<Value>, usize) {
    let Ok(Value::Array(all)) = serde_json::from_str::<Value>(json) else {
        return (Vec::new(), 0);
    };
    let key = |c: &Value| {
        let f = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        (f("workflow"), f("name"))
    };
    // A queued re-run can print the zero time; it is the newest run, not the oldest.
    let started = |c: &Value| {
        c.get("startedAt").and_then(Value::as_str).map(|t| {
            if t.starts_with("0001-") {
                "9999".to_string()
            } else {
                t.to_string()
            }
        })
    };
    let mut groups: Vec<((String, String), Vec<Value>)> = Vec::new();
    for check in all {
        let k = key(&check);
        match groups.iter_mut().find(|(g, _)| *g == k) {
            Some((_, runs)) => runs.push(check),
            None => groups.push((k, vec![check])),
        }
    }
    let mut kept = Vec::new();
    let mut superseded = 0;
    for (_, runs) in groups {
        if runs.len() == 1 || runs.iter().any(|r| started(r).is_none()) {
            kept.extend(runs);
            continue;
        }
        let Some(latest) = runs.iter().filter_map(started).max() else {
            kept.extend(runs);
            continue;
        };
        let mut chosen = false;
        for run in runs {
            let is_latest = started(&run).as_deref() == Some(latest.as_str()) && !chosen;
            if is_latest {
                chosen = true;
                kept.push(run);
            } else if run.get("bucket").and_then(Value::as_str) == Some("cancel") {
                superseded += 1;
            }
        }
    }
    (kept, superseded)
}

impl Counts {
    pub fn from_json(json: &str) -> Counts {
        let (kept, superseded) = latest_checks(json);
        let n = |bucket: &str| {
            kept.iter()
                .filter(|c| c.get("bucket").and_then(Value::as_str) == Some(bucket))
                .count()
        };
        Counts {
            total: kept.len(),
            pending: n("pending"),
            fail: n("fail"),
            cancel: n("cancel"),
            superseded,
        }
    }
}

/// The words of the check verdict, shared by `ci-wait` and `pr checks`.
pub mod verdict {
    pub const NONE: &str = "NONE";
    pub const FAIL: &str = "FAIL";
    pub const CANCELLED: &str = "CANCELLED";
    pub const PASS: &str = "PASS";
    pub const TIMEOUT: &str = "TIMEOUT";
}

/// The verdict for one read, or `None` to keep waiting. With `fail_fast` a
/// failure wins over pending checks; without it every check must settle
/// first. Cancelled only counts once nothing is pending.
pub fn settled(c: Counts, fail_fast: bool) -> Option<&'static str> {
    if c.total == 0 {
        Some(verdict::NONE)
    } else if c.fail > 0 && (fail_fast || c.pending == 0) {
        Some(verdict::FAIL)
    } else if c.pending > 0 {
        None
    } else if c.cancel > 0 {
        Some(verdict::CANCELLED)
    } else {
        Some(verdict::PASS)
    }
}

/// Poll until a verdict or `timeout`. `read` returns the checks JSON,
/// `emit` prints a progress line, `sleep` waits between reads. Returns the
/// verdict word and the JSON of the last read.
pub fn wait_checks(
    timeout: Duration,
    fail_fast: bool,
    read: &mut dyn FnMut() -> String,
    emit: &mut dyn FnMut(String),
    sleep: &mut dyn FnMut(),
) -> (&'static str, String) {
    let start = Instant::now();
    loop {
        let json = read();
        let c = Counts::from_json(&json);
        emit(format!(
            "checks total={} pending={} fail={} cancel={} superseded={}",
            c.total, c.pending, c.fail, c.cancel, c.superseded
        ));
        if let Some(v) = settled(c, fail_fast) {
            return (v, json);
        }
        if start.elapsed() >= timeout {
            return (verdict::TIMEOUT, json);
        }
        sleep();
    }
}

/// Poll until a verdict or `timeout`, stopping at the first failure. Returns
/// the verdict word.
pub fn ci_wait(
    timeout: Duration,
    read: &mut dyn FnMut() -> String,
    emit: &mut dyn FnMut(String),
    sleep: &mut dyn FnMut(),
) -> &'static str {
    wait_checks(timeout, true, read, emit, sleep).0
}

/// One PR status line, `state mergeState review armed`, tab separated, as the
/// land loop reads it.
fn land_line(view: &Value) -> String {
    let s = |k: &str| view.get(k).and_then(Value::as_str).unwrap_or("");
    let review = match s("reviewDecision") {
        "" => "-",
        r => r,
    };
    let armed = if view.get("autoMergeRequest").is_some_and(|v| !v.is_null()) {
        "armed"
    } else {
        "-"
    };
    format!(
        "{}\t{}\t{}\t{}",
        s("state"),
        s("mergeStateStatus"),
        review,
        armed
    )
}

/// The verdict for one status line, or `None` to keep waiting.
fn land_verdict(line: &str) -> Option<&'static str> {
    if line.starts_with("MERGED") {
        Some("MERGED")
    } else if line.contains("REVIEW_REQUIRED") {
        Some("REVIEW_GATE")
    } else if line.contains("CHANGES_REQUESTED") {
        Some("CHANGES_REQUESTED")
    } else if line.contains("DIRTY") || line.contains("BEHIND") {
        Some("RESTATE")
    } else {
        None
    }
}

/// Poll until the PR lands, hits a gate, or `timeout`.
pub fn land_wait(
    timeout: Duration,
    read: &mut dyn FnMut() -> String,
    emit: &mut dyn FnMut(String),
    sleep: &mut dyn FnMut(),
) -> &'static str {
    let start = Instant::now();
    loop {
        let line = read();
        emit(line.clone());
        if let Some(v) = land_verdict(&line) {
            return v;
        }
        if start.elapsed() >= timeout {
            return "TIMEOUT";
        }
        sleep();
    }
}

/// Stdout of `gh` whatever its exit status. `gh pr checks` exits non-zero
/// while checks fail but still prints the JSON, so the status is ignored.
pub(crate) fn gh_stdout(args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("gh");
    cmd.args(args);
    let out = run_with_timeout(&mut cmd, GH_TIMEOUT)?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn checks_args(pr: &str, all: bool) -> Vec<&str> {
    let mut args = vec!["pr", "checks", pr];
    if !all {
        args.push("--required");
    }
    args.extend(["--json", CHECK_FIELDS]);
    args
}

/// The checks of `pr` as `gh` prints them, `[]` when unreadable. Only the
/// required ones unless `all`.
pub fn read_checks(pr: &str, all: bool) -> String {
    gh_stdout(&checks_args(pr, all))
        .filter(|t| serde_json::from_str::<Value>(t).is_ok())
        .unwrap_or_else(|| "[]".to_string())
}

/// What `gh pr checks` says on stderr when a PR has no checks to list.
const NO_CHECKS_REPORTED: &str = "checks reported on";

/// Like `read_checks`, but an unreadable PR is an error with gh's message,
/// and "no checks reported" is an empty list.
pub fn try_read_checks(pr: &str, all: bool) -> Result<String, String> {
    let mut cmd = Command::new("gh");
    cmd.args(checks_args(pr, all));
    let out = run_with_timeout(&mut cmd, GH_TIMEOUT)
        .ok_or_else(|| "gh did not finish (missing, or timed out)".to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if serde_json::from_str::<Value>(&stdout).is_ok() {
        Ok(stdout)
    } else if stderr.contains(NO_CHECKS_REPORTED) {
        Ok("[]".to_string())
    } else {
        Err(stderr.trim().to_string())
    }
}

/// The land status line of `pr`, empty when unreadable.
pub fn read_land_line(pr: &str) -> String {
    gh_stdout(&[
        "pr",
        "view",
        pr,
        "--json",
        "state,mergeStateStatus,reviewDecision,autoMergeRequest",
    ])
    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    .map(|v| land_line(&v))
    .unwrap_or_default()
}

/// The `gh` arguments of a merge: `--auto`, or `--admin --squash`. A
/// `match_head` commit makes `gh` refuse when the PR head has moved.
pub fn merge_args<'a>(pr: &'a str, admin: bool, match_head: Option<&'a str>) -> Vec<&'a str> {
    let mut args = vec!["pr", "merge", pr];
    if admin {
        args.extend(["--admin", "--squash"]);
    } else {
        args.push("--auto");
    }
    if let Some(oid) = match_head {
        args.extend(["--match-head-commit", oid]);
    }
    args
}

/// `gh pr merge` with `--auto`, or `--admin --squash`: the exit code and gh's
/// combined output.
pub fn merge(pr: &str, admin: bool, match_head: Option<&str>) -> (i32, String) {
    let args = merge_args(pr, admin, match_head);
    let mut cmd = Command::new("gh");
    cmd.args(&args);
    match run_with_timeout(&mut cmd, GH_TIMEOUT) {
        Some(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.code().unwrap_or(-1), text.trim_end().to_string())
        }
        None => (-1, "gh did not finish (missing, or timed out)".to_string()),
    }
}

/// The two lines the command file reads: `<label>_rc=<code>` then the output.
pub fn merge_text(admin: bool, code: i32, output: &str) -> String {
    let label = if admin { "admin" } else { "merge" };
    format!("{label}_rc={code}\n{output}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn checks(buckets: &[&str]) -> String {
        serde_json::to_string(
            &buckets
                .iter()
                .map(|b| json!({"name":"c","bucket":b}))
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    fn verdict_of(buckets: &[&str]) -> Option<&'static str> {
        settled(Counts::from_json(&checks(buckets)), true)
    }

    fn run(workflow: &str, bucket: &str, at: Option<&str>) -> Value {
        let mut v = json!({"name": "build", "workflow": workflow, "bucket": bucket});
        if let Some(at) = at {
            v["startedAt"] = json!(at);
        }
        v
    }

    fn counts_of(runs: Vec<Value>) -> Counts {
        Counts::from_json(&serde_json::to_string(&runs).unwrap())
    }

    #[test]
    fn without_fail_fast_a_failure_waits_for_the_pending_checks() {
        let c = Counts::from_json(&checks(&["fail", "pending"]));

        assert_eq!(settled(c, true), Some("FAIL"));
        assert_eq!(settled(c, false), None);
        assert_eq!(
            settled(Counts::from_json(&checks(&["fail", "pass"])), false),
            Some("FAIL")
        );
    }

    #[test]
    fn a_cancel_replaced_by_a_later_pass_is_superseded() {
        let c = counts_of(vec![
            run("ci", "cancel", Some("2026-01-01T00:00:01Z")),
            run("ci", "pass", Some("2026-01-01T00:00:02Z")),
        ]);
        assert_eq!((c.total, c.cancel, c.superseded), (1, 0, 1));
        assert_eq!(settled(c, true), Some("PASS"));
    }

    #[test]
    fn a_queued_rerun_with_the_zero_time_beats_an_older_cancel() {
        let c = counts_of(vec![
            run("ci", "cancel", Some("2026-01-01T00:00:01Z")),
            run("ci", "pending", Some("0001-01-01T00:00:00Z")),
        ]);
        assert_eq!((c.total, c.pending, c.cancel, c.superseded), (1, 1, 0, 1));
        assert_eq!(settled(c, true), None);
    }

    #[test]
    fn a_cancel_after_a_pass_is_cancelled() {
        let c = counts_of(vec![
            run("ci", "pass", Some("2026-01-01T00:00:01Z")),
            run("ci", "cancel", Some("2026-01-01T00:00:02Z")),
        ]);
        assert_eq!((c.total, c.cancel, c.superseded), (1, 1, 0));
        assert_eq!(settled(c, true), Some("CANCELLED"));
    }

    #[test]
    fn a_lone_cancel_is_cancelled() {
        let c = counts_of(vec![run("ci", "cancel", Some("2026-01-01T00:00:01Z"))]);
        assert_eq!(settled(c, true), Some("CANCELLED"));
    }

    #[test]
    fn runs_without_a_start_time_keep_the_old_counting() {
        let c = counts_of(vec![run("ci", "cancel", None), run("ci", "pass", None)]);
        assert_eq!((c.total, c.cancel, c.superseded), (2, 1, 0));
        assert_eq!(settled(c, true), Some("CANCELLED"));
    }

    #[test]
    fn the_same_name_in_another_workflow_is_a_different_check() {
        let c = counts_of(vec![
            run("a", "cancel", Some("2026-01-01T00:00:01Z")),
            run("b", "pass", Some("2026-01-01T00:00:02Z")),
        ]);
        assert_eq!((c.total, c.cancel), (2, 1));
    }

    #[test]
    fn a_merge_pins_the_head_only_when_asked() {
        assert_eq!(merge_args("7", false, None), ["pr", "merge", "7", "--auto"]);
        assert_eq!(
            merge_args("7", false, Some("abc")),
            ["pr", "merge", "7", "--auto", "--match-head-commit", "abc"]
        );
        assert_eq!(
            merge_args("7", true, None),
            ["pr", "merge", "7", "--admin", "--squash"]
        );
    }

    #[test]
    fn no_checks_is_none() {
        assert_eq!(verdict_of(&[]), Some("NONE"));
    }

    #[test]
    fn a_failure_wins_over_pending() {
        assert_eq!(verdict_of(&["pass", "fail", "pending"]), Some("FAIL"));
    }

    #[test]
    fn cancelled_waits_for_pending_to_finish() {
        assert_eq!(verdict_of(&["cancel", "pending"]), None);
        assert_eq!(verdict_of(&["cancel", "pass"]), Some("CANCELLED"));
    }

    #[test]
    fn all_passed_is_pass_and_pending_keeps_waiting() {
        assert_eq!(verdict_of(&["pass", "skipping"]), Some("PASS"));
        assert_eq!(verdict_of(&["pass", "pending"]), None);
    }

    #[test]
    fn the_loop_reports_each_read_then_the_verdict() {
        let mut reads = vec![checks(&["pending"]), checks(&["pass"])].into_iter();
        let mut lines = Vec::new();
        let v = ci_wait(
            Duration::from_secs(60),
            &mut || reads.next().unwrap(),
            &mut |l| lines.push(l),
            &mut || {},
        );
        assert_eq!(v, "PASS");
        assert_eq!(
            lines,
            vec![
                "checks total=1 pending=1 fail=0 cancel=0 superseded=0",
                "checks total=1 pending=0 fail=0 cancel=0 superseded=0"
            ]
        );
    }

    #[test]
    fn the_loop_times_out() {
        let v = ci_wait(
            Duration::ZERO,
            &mut || checks(&["pending"]),
            &mut |_| {},
            &mut || {},
        );
        assert_eq!(v, "TIMEOUT");
    }

    #[test]
    fn the_land_line_has_four_tab_separated_fields() {
        let v = json!({"state":"OPEN","mergeStateStatus":"CLEAN","reviewDecision":null,"autoMergeRequest":{"x":1}});
        assert_eq!(land_line(&v), "OPEN\tCLEAN\t-\tarmed");
        let v = json!({"state":"OPEN","mergeStateStatus":"BLOCKED","reviewDecision":"APPROVED","autoMergeRequest":null});
        assert_eq!(land_line(&v), "OPEN\tBLOCKED\tAPPROVED\t-");
    }

    #[test]
    fn land_verdicts_follow_the_documented_order() {
        assert_eq!(land_verdict("MERGED\tCLEAN\t-\t-"), Some("MERGED"));
        assert_eq!(
            land_verdict("OPEN\tBLOCKED\tREVIEW_REQUIRED\t-"),
            Some("REVIEW_GATE")
        );
        assert_eq!(
            land_verdict("OPEN\tBLOCKED\tCHANGES_REQUESTED\t-"),
            Some("CHANGES_REQUESTED")
        );
        assert_eq!(land_verdict("OPEN\tDIRTY\t-\t-"), Some("RESTATE"));
        assert_eq!(land_verdict("OPEN\tBEHIND\t-\tarmed"), Some("RESTATE"));
        assert_eq!(land_verdict("OPEN\tBLOCKED\t-\tarmed"), None);
    }

    #[test]
    fn the_land_loop_stops_on_the_first_verdict() {
        let mut reads = vec![
            "OPEN\tBLOCKED\t-\tarmed".to_string(),
            "MERGED\tCLEAN\t-\t-".to_string(),
        ]
        .into_iter();
        let mut n = 0;
        let v = land_wait(
            Duration::from_secs(60),
            &mut || reads.next().unwrap(),
            &mut |_| {},
            &mut || n += 1,
        );
        assert_eq!((v, n), ("MERGED", 1));
    }
}
