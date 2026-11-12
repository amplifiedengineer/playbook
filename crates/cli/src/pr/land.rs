// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr land`: wait for a PR's checks, arm auto-merge pinned to the
//! head that was checked, and wait until it merges or a gate stops it. One
//! call replaces the wait, merge and confirm steps the finish skill ran as
//! several model turns.
//!
//! The core takes a [`LandGh`] and a clock, so tests script `gh` and never
//! sleep. It never passes `--admin` and never deletes the branch.
//!
//! Follow up: `pr status` (a separate change) defines its own trait
//! for the same `gh` reads. When both have merged, fold them into one.

use crate::pr::wait::latest_checks;
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

/// Exit codes, shared with `deliver`.
pub mod exit {
    pub const MERGED: i32 = 0;
    pub const REFUSED: i32 = 3;
    pub const CHECKS: i32 = 6;
    pub const WAITING: i32 = 7;
    pub const NOT_QUEUED: i32 = 8;
}

/// The verdict words of a land call.
pub mod verdict {
    pub const MERGED: &str = "MERGED";
    pub const FAIL: &str = "FAIL";
    pub const CLOSED: &str = "CLOSED";
    pub const STILL_WAITING: &str = "STILL_WAITING";
    pub const NOT_QUEUED: &str = "NOT_QUEUED";
    pub const DRAFT: &str = "DRAFT";
    pub const WAITING_ON_BASE: &str = "WAITING_ON_BASE";
    pub const HEAD_MOVED: &str = "HEAD_MOVED";
    pub const REVIEW_GATE: &str = "REVIEW_GATE";
    pub const CHANGES_REQUESTED: &str = "CHANGES_REQUESTED";
    pub const RESTATE: &str = "RESTATE";
}

/// The merge methods a repo allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Methods {
    pub squash: bool,
    pub merge: bool,
    pub rebase: bool,
}

/// The `gh` operations `land` needs, one method each.
pub trait LandGh {
    /// `gh pr view --json state,isDraft,headRefOid,baseRefName,mergeStateStatus,reviewDecision,autoMergeRequest`.
    fn pr_view(&self, pr: &str) -> Option<Value>;
    /// The required checks with `name,bucket,startedAt,workflow`; `None` when
    /// the read failed, which is not the same as having no checks.
    fn pr_checks(&self, pr: &str) -> Option<String>;
    /// `gh pr merge <pr> <args>`: the exit code and the combined output.
    fn pr_merge(&self, pr: &str, args: &[&str]) -> (i32, String);
    /// What the repo allows, `None` when unreadable.
    fn repo_merge_methods(&self) -> Option<Methods>;
    /// Head branch names of the open PRs.
    fn open_heads(&self) -> Vec<String>;
}

/// Limits of one call.
#[derive(Debug, Clone)]
pub struct Opts {
    /// Hard wall clock cap for the whole call.
    pub timeout: Duration,
    pub interval: Duration,
    /// Wait this long for the first checks to appear, on the first call only.
    pub settle: Duration,
    /// Test only: end the wait after this many polls instead of a timeout.
    pub max_polls: Option<usize>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            timeout: Duration::from_secs(480),
            interval: Duration::from_secs(20),
            settle: Duration::from_secs(120),
            max_polls: None,
        }
    }
}

/// What checks looked like when the wait ended.
#[derive(Debug, Default, Serialize, Clone, PartialEq, Eq)]
pub struct ChecksReport {
    /// `none` when the PR has no checks, else `read`.
    pub seen: String,
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub pending: usize,
    pub superseded_cancels: usize,
    pub failing: Vec<String>,
}

/// The result of a land call.
#[derive(Debug, Serialize)]
pub struct Report {
    pub verdict: String,
    pub pr: String,
    pub head: String,
    pub checks: ChecksReport,
    pub armed: bool,
    pub method: String,
    pub merge_output: String,
    pub state: String,
    pub merge_state: String,
    pub review_decision: String,
    pub elapsed_s: u64,
    pub next: String,
    pub exit_code: i32,
}

/// Values `gh` reports that the flow reads.
const STATE_MERGED: &str = "MERGED";
const STATE_CLOSED: &str = "CLOSED";
const STATE_DIRTY: &str = "DIRTY";
const STATE_BEHIND: &str = "BEHIND";
const REVIEW_REQUIRED: &str = "REVIEW_REQUIRED";
const CHANGES_REQUESTED: &str = "CHANGES_REQUESTED";

/// The exit code of a verdict.
pub fn exit_code(verdict: &str) -> i32 {
    match verdict {
        verdict::MERGED => exit::MERGED,
        verdict::FAIL | verdict::CLOSED => exit::CHECKS,
        verdict::STILL_WAITING => exit::WAITING,
        verdict::NOT_QUEUED => exit::NOT_QUEUED,
        _ => exit::REFUSED,
    }
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn summarize(json: &str) -> (ChecksReport, bool) {
    let (kept, superseded) = latest_checks(json);
    let bucket = |c: &Value| text(c, "bucket");
    let mut r = ChecksReport {
        seen: if kept.is_empty() { "none" } else { "read" }.into(),
        total: kept.len(),
        superseded_cancels: superseded,
        ..ChecksReport::default()
    };
    for c in &kept {
        match bucket(c).as_str() {
            "pass" | "skipping" => r.pass += 1,
            "fail" | "cancel" => {
                r.fail += 1;
                r.failing.push(text(c, "name"));
            }
            "pending" => r.pending += 1,
            _ => {}
        }
    }
    let done = r.total == 0 || r.pending == 0 || r.fail > 0;
    (r, done)
}

fn gate(view: &Value) -> Option<&'static str> {
    let review = text(view, "reviewDecision");
    let merge_state = text(view, "mergeStateStatus");
    if review == REVIEW_REQUIRED {
        Some(verdict::REVIEW_GATE)
    } else if review == CHANGES_REQUESTED {
        Some(verdict::CHANGES_REQUESTED)
    } else if merge_state == STATE_DIRTY || merge_state == STATE_BEHIND {
        Some(verdict::RESTATE)
    } else {
        None
    }
}

/// Runs the land flow. `now` reads the elapsed time, `sleep` waits.
pub fn land(
    gh: &dyn LandGh,
    pr: &str,
    opts: &Opts,
    now: &mut dyn FnMut() -> Duration,
    sleep: &mut dyn FnMut(Duration),
) -> Report {
    let mut r = Report {
        verdict: String::new(),
        pr: pr.into(),
        head: String::new(),
        checks: ChecksReport::default(),
        armed: false,
        method: String::new(),
        merge_output: String::new(),
        state: String::new(),
        merge_state: String::new(),
        review_decision: String::new(),
        elapsed_s: 0,
        next: String::new(),
        exit_code: 0,
    };
    let mut wait = Wait {
        opts,
        now,
        sleep,
        polls: 0,
    };
    let verdict = run(gh, pr, &mut wait, &mut r);
    r.verdict = verdict.into();
    r.exit_code = exit_code(verdict);
    r.elapsed_s = (wait.now)().as_secs();
    if verdict == verdict::STILL_WAITING {
        r.next = format!("playbook pr land {pr} --settle 0");
    }
    r
}

/// The clock, the sleeper and the poll count shared by the waiting steps.
struct Wait<'a> {
    opts: &'a Opts,
    now: &'a mut dyn FnMut() -> Duration,
    sleep: &'a mut dyn FnMut(Duration),
    polls: usize,
}

impl Wait<'_> {
    fn out_of_time(&mut self) -> bool {
        match self.opts.max_polls {
            Some(max) => self.polls >= max,
            None => (self.now)() >= self.opts.timeout,
        }
    }

    fn pause(&mut self) {
        (self.sleep)(self.opts.interval);
    }
}

fn record(r: &mut Report, view: &Value) {
    r.state = text(view, "state");
    r.merge_state = text(view, "mergeStateStatus");
    r.review_decision = text(view, "reviewDecision");
}

/// The steps in order: gate on the checked head, arm, wait for the merge. A
/// step that stops the flow returns its verdict as the error.
fn run(gh: &dyn LandGh, pr: &str, wait: &mut Wait, r: &mut Report) -> &'static str {
    let head = match gate_on_checks(gh, pr, wait, r) {
        Ok(head) => head,
        Err(verdict) => return verdict,
    };
    if let Err(verdict) = arm(gh, pr, &head, r) {
        return verdict;
    }
    await_merge(gh, pr, &head, wait, r)
}

/// Checks the PR can merge and returns the head that was checked: it is open,
/// not a draft, not stacked on another open PR, its required checks pass, the
/// head did not move, and no review or branch gate holds it.
fn gate_on_checks(
    gh: &dyn LandGh,
    pr: &str,
    wait: &mut Wait,
    r: &mut Report,
) -> Result<String, &'static str> {
    let Some(view) = gh.pr_view(pr) else {
        r.merge_output = "could not read the PR".into();
        return Err(verdict::FAIL);
    };
    record(r, &view);
    match r.state.as_str() {
        STATE_MERGED => return Err(verdict::MERGED),
        STATE_CLOSED => return Err(verdict::CLOSED),
        _ => {}
    }
    if view.get("isDraft").and_then(Value::as_bool) == Some(true) {
        return Err(verdict::DRAFT);
    }
    let base = text(&view, "baseRefName");
    if gh.open_heads().contains(&base) {
        return Err(verdict::WAITING_ON_BASE);
    }
    let head = text(&view, "headRefOid");
    r.head = head.clone();

    // The first read may find no checks because a fresh push has not
    // registered its runs yet: wait once for them, on the first call only.
    let mut settled = wait.opts.settle.is_zero();
    loop {
        wait.polls += 1;
        let Some(checks) = gh.pr_checks(pr) else {
            // A failed read is not "no checks": try again until the time is up.
            if wait.out_of_time() {
                return Err(verdict::STILL_WAITING);
            }
            wait.pause();
            continue;
        };
        let (summary, done) = summarize(&checks);
        let empty = summary.total == 0;
        r.checks = summary;
        if empty && !settled {
            settled = true;
            (wait.sleep)(wait.opts.settle);
            continue;
        }
        if done {
            break;
        }
        if wait.out_of_time() {
            return Err(verdict::STILL_WAITING);
        }
        wait.pause();
    }
    if r.checks.fail > 0 {
        return Err(verdict::FAIL);
    }

    let Some(view) = gh.pr_view(pr) else {
        return Err(verdict::STILL_WAITING);
    };
    record(r, &view);
    if text(&view, "headRefOid") != head {
        return Err(verdict::HEAD_MOVED);
    }
    match gate(&view) {
        Some(held) => Err(held),
        None => Ok(head),
    }
}

/// The merge method flags in the order they are tried.
fn method_flag(methods: Methods) -> Option<&'static str> {
    if methods.squash {
        Some("--squash")
    } else if methods.merge {
        Some("--merge")
    } else if methods.rebase {
        Some("--rebase")
    } else {
        None
    }
}

/// Arms auto-merge pinned to `head`, with no method flag first. A repo that
/// asks for a method gets the first one it allows.
fn arm(gh: &dyn LandGh, pr: &str, head: &str, r: &mut Report) -> Result<(), &'static str> {
    let (code, output) = gh.pr_merge(pr, &["--auto", "--match-head-commit", head]);
    let asks_for_method = code != 0 && output.to_lowercase().contains("merge method");
    let flag = asks_for_method
        .then(|| gh.repo_merge_methods().and_then(method_flag))
        .flatten();
    let (code, output, method) = match flag {
        Some(flag) => {
            let (c, o) = gh.pr_merge(pr, &["--auto", flag, "--match-head-commit", head]);
            (c, o, flag.trim_start_matches("--").to_string())
        }
        None if asks_for_method => (code, output, String::new()),
        None => (code, output, "auto".to_string()),
    };
    r.merge_output = output;
    if code != 0 {
        return Err(verdict::NOT_QUEUED);
    }
    r.armed = true;
    r.method = method;
    Ok(())
}

/// Polls until the PR merges, closes, moves its head or hits a gate.
fn await_merge(
    gh: &dyn LandGh,
    pr: &str,
    head: &str,
    wait: &mut Wait,
    r: &mut Report,
) -> &'static str {
    loop {
        let Some(view) = gh.pr_view(pr) else {
            // One failed read while armed is not a failed PR: read again.
            wait.polls += 1;
            if wait.out_of_time() {
                return verdict::STILL_WAITING;
            }
            wait.pause();
            continue;
        };
        record(r, &view);
        match r.state.as_str() {
            STATE_MERGED => return verdict::MERGED,
            STATE_CLOSED => return verdict::CLOSED,
            _ => {}
        }
        if text(&view, "headRefOid") != head {
            return verdict::HEAD_MOVED;
        }
        if let Some(held) = gate(&view) {
            return held;
        }
        wait.polls += 1;
        if wait.out_of_time() {
            return verdict::STILL_WAITING;
        }
        wait.pause();
    }
}

/// The real `gh`.
pub struct RealGh {
    pub dir: Option<std::path::PathBuf>,
}

const CALL_TIMEOUT: Duration = Duration::from_secs(20);

impl RealGh {
    fn gh(&self, args: &[&str]) -> Option<std::process::Output> {
        let mut cmd = std::process::Command::new("gh");
        cmd.args(args);
        if let Some(dir) = &self.dir {
            cmd.current_dir(dir);
        }
        crate::common::proc::run_with_timeout(&mut cmd, CALL_TIMEOUT)
    }

    fn stdout(&self, args: &[&str]) -> Option<String> {
        self.gh(args)
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    }
}

impl LandGh for RealGh {
    fn pr_view(&self, pr: &str) -> Option<Value> {
        let t = self.stdout(&[
            "pr",
            "view",
            pr,
            "--json",
            "state,isDraft,headRefOid,baseRefName,mergeStateStatus,reviewDecision,autoMergeRequest",
        ])?;
        serde_json::from_str(&t).ok()
    }

    fn pr_checks(&self, pr: &str) -> Option<String> {
        // `gh pr checks` exits non-zero while a check fails or is pending but
        // still prints the JSON. Only the required ones count, as in `ci-wait`.
        self.stdout(&[
            "pr",
            "checks",
            pr,
            "--required",
            "--json",
            "name,bucket,startedAt,workflow",
        ])
        .filter(|t| serde_json::from_str::<Value>(t).is_ok())
    }

    fn pr_merge(&self, pr: &str, args: &[&str]) -> (i32, String) {
        let mut all = vec!["pr", "merge", pr];
        all.extend_from_slice(args);
        match self.gh(&all) {
            Some(o) => {
                let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
                t.push_str(&String::from_utf8_lossy(&o.stderr));
                (o.status.code().unwrap_or(-1), t.trim().to_string())
            }
            None => (-1, "gh did not finish (missing, or timed out)".into()),
        }
    }

    fn repo_merge_methods(&self) -> Option<Methods> {
        let t = self.stdout(&[
            "repo",
            "view",
            "--json",
            "squashMergeAllowed,mergeCommitAllowed,rebaseMergeAllowed",
        ])?;
        let v: Value = serde_json::from_str(&t).ok()?;
        let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        Some(Methods {
            squash: b("squashMergeAllowed"),
            merge: b("mergeCommitAllowed"),
            rebase: b("rebaseMergeAllowed"),
        })
    }

    fn open_heads(&self) -> Vec<String> {
        let Some(t) = self.stdout(&[
            "pr",
            "list",
            "--state",
            "open",
            "--json",
            "headRefName,isCrossRepository",
            "--limit",
            "100",
        ]) else {
            return Vec::new();
        };
        let v: Value = serde_json::from_str(&t).unwrap_or_default();
        // A fork's branch can share the base's name; it is not a base PR.
        v.as_array()
            .into_iter()
            .flatten()
            .filter(|e| e.get("isCrossRepository").and_then(Value::as_bool) != Some(true))
            .filter_map(|e| e.get("headRefName")?.as_str().map(String::from))
            .collect()
    }
}

/// Runs the land flow against the real `gh` and the wall clock.
pub fn land_real(dir: Option<std::path::PathBuf>, pr: &str, opts: &Opts) -> Report {
    let start = std::time::Instant::now();
    land(
        &RealGh { dir },
        pr,
        opts,
        &mut || start.elapsed(),
        &mut std::thread::sleep,
    )
}

/// The output of a call: `LAND=<verdict>` and then the report as JSON.
pub fn render(report: &Report) -> String {
    format!(
        "LAND={}\n{}",
        report.verdict,
        serde_json::to_string_pretty(report).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    /// A scripted `gh`: views and checks come from queues, the last entry repeats.
    struct Fake {
        views: RefCell<Vec<Option<Value>>>,
        checks: RefCell<Vec<Option<String>>>,
        merges: RefCell<Vec<(i32, String)>>,
        merge_log: RefCell<Vec<String>>,
        methods: Option<Methods>,
        heads: Vec<String>,
    }

    fn view(state: &str, head: &str) -> Value {
        json!({"state": state, "isDraft": false, "headRefOid": head, "baseRefName": "main",
               "mergeStateStatus": "CLEAN", "reviewDecision": "", "autoMergeRequest": null})
    }

    fn pass() -> String {
        r#"[{"name":"t","workflow":"ci","bucket":"pass","startedAt":"2026-01-01T00:00:01Z"}]"#
            .into()
    }

    impl Fake {
        fn new(views: Vec<Value>) -> Fake {
            Fake {
                views: RefCell::new(views.into_iter().map(Some).collect()),
                checks: RefCell::new(vec![Some(pass())]),
                merges: RefCell::new(vec![(0, "ok".into())]),
                merge_log: RefCell::new(Vec::new()),
                methods: None,
                heads: Vec::new(),
            }
        }
    }

    fn next<T: Clone>(q: &RefCell<Vec<T>>) -> T {
        let mut q = q.borrow_mut();
        if q.len() > 1 {
            q.remove(0)
        } else {
            q[0].clone()
        }
    }

    impl LandGh for Fake {
        fn pr_view(&self, _: &str) -> Option<Value> {
            next(&self.views)
        }
        fn pr_checks(&self, _: &str) -> Option<String> {
            next(&self.checks)
        }
        fn pr_merge(&self, _: &str, args: &[&str]) -> (i32, String) {
            self.merge_log.borrow_mut().push(args.join(" "));
            next(&self.merges)
        }
        fn repo_merge_methods(&self) -> Option<Methods> {
            self.methods
        }
        fn open_heads(&self) -> Vec<String> {
            self.heads.clone()
        }
    }

    fn go(fake: &Fake, opts: Opts) -> Report {
        let clock = RefCell::new(Duration::ZERO);
        let report = land(fake, "7", &opts, &mut || *clock.borrow(), &mut |d| {
            *clock.borrow_mut() += d.max(Duration::from_secs(1))
        });
        // Every case: never admin, never delete the branch.
        for call in fake.merge_log.borrow().iter() {
            assert!(!call.contains("--admin") && !call.contains("--delete-branch"));
        }
        report
    }

    fn quick() -> Opts {
        Opts {
            timeout: Duration::from_secs(60),
            interval: Duration::from_secs(5),
            settle: Duration::ZERO,
            max_polls: None,
        }
    }

    #[test]
    fn checks_pass_arm_and_merged() {
        let f = Fake::new(vec![
            view("OPEN", "a"),
            view("OPEN", "a"),
            view("MERGED", "a"),
        ]);
        let r = go(&f, quick());
        assert_eq!((r.verdict.as_str(), r.exit_code), ("MERGED", 0));
        assert!(r.armed);
        assert_eq!(r.method, "auto");
        assert_eq!(f.merge_log.borrow()[0], "--auto --match-head-commit a");
    }

    #[test]
    fn armed_but_open_past_the_timeout_is_still_waiting_with_next() {
        let f = Fake::new(vec![view("OPEN", "a")]);
        let r = go(&f, quick());
        assert_eq!((r.verdict.as_str(), r.exit_code), ("STILL_WAITING", 7));
        assert!(r.next.contains("pr land 7"));
        assert!(r.next.contains("--settle 0"));
    }

    #[test]
    fn a_failing_check_is_fail_and_names_it() {
        let f = Fake::new(vec![view("OPEN", "a")]);
        *f.checks.borrow_mut() = vec![Some(r#"[{"name":"lint","workflow":"ci","bucket":"fail","startedAt":"2026-01-01T00:00:01Z"}]"#.into())];
        let r = go(&f, quick());
        assert_eq!((r.verdict.as_str(), r.exit_code), ("FAIL", 6));
        assert_eq!(r.checks.failing, vec!["lint"]);
        assert!(f.merge_log.borrow().is_empty());
    }

    #[test]
    fn a_superseded_cancel_is_green() {
        let f = Fake::new(vec![
            view("OPEN", "a"),
            view("OPEN", "a"),
            view("MERGED", "a"),
        ]);
        *f.checks.borrow_mut() = vec![Some(r#"[{"name":"t","workflow":"ci","bucket":"cancel","startedAt":"2026-01-01T00:00:01Z"},{"name":"t","workflow":"ci","bucket":"pass","startedAt":"2026-01-01T00:00:09Z"}]"#.into())];
        let r = go(&f, quick());
        assert_eq!(r.verdict, "MERGED");
        assert_eq!(r.checks.superseded_cancels, 1);
    }

    #[test]
    fn a_closed_pr_is_closed() {
        let f = Fake::new(vec![view("CLOSED", "a")]);
        let r = go(&f, quick());
        assert_eq!((r.verdict.as_str(), r.exit_code), ("CLOSED", 6));
    }

    #[test]
    fn review_gates_and_restate_stop_before_arming() {
        for (field, value, want) in [
            ("reviewDecision", "REVIEW_REQUIRED", "REVIEW_GATE"),
            ("reviewDecision", "CHANGES_REQUESTED", "CHANGES_REQUESTED"),
            ("mergeStateStatus", "DIRTY", "RESTATE"),
        ] {
            let mut v = view("OPEN", "a");
            v[field] = json!(value);
            let f = Fake::new(vec![v]);
            let r = go(&f, quick());
            assert_eq!((r.verdict.as_str(), r.exit_code), (want, 3), "{want}");
            assert!(f.merge_log.borrow().is_empty());
        }
    }

    #[test]
    fn a_head_that_moves_after_the_checks_is_head_moved_with_no_merge() {
        let f = Fake::new(vec![view("OPEN", "a"), view("OPEN", "b")]);
        let r = go(&f, quick());
        assert_eq!((r.verdict.as_str(), r.exit_code), ("HEAD_MOVED", 3));
        assert!(f.merge_log.borrow().is_empty());
    }

    #[test]
    fn a_base_that_is_another_open_pr_head_waits_on_base() {
        let mut f = Fake::new(vec![view("OPEN", "a")]);
        f.heads = vec!["main".into()];
        let r = go(&f, quick());
        assert_eq!((r.verdict.as_str(), r.exit_code), ("WAITING_ON_BASE", 3));
        assert!(f.merge_log.borrow().is_empty());
    }

    #[test]
    fn a_draft_is_refused() {
        let mut v = view("OPEN", "a");
        v["isDraft"] = json!(true);
        let r = go(&Fake::new(vec![v]), quick());
        assert_eq!((r.verdict.as_str(), r.exit_code), ("DRAFT", 3));
    }

    #[test]
    fn a_refusal_that_is_not_a_method_error_is_not_queued() {
        let f = Fake::new(vec![view("OPEN", "a")]);
        *f.merges.borrow_mut() = vec![(1, "auto-merge is not allowed".into())];
        let r = go(&f, quick());
        assert_eq!((r.verdict.as_str(), r.exit_code), ("NOT_QUEUED", 8));
    }

    #[test]
    fn a_method_error_retries_with_the_one_allowed_flag() {
        for (methods, flag) in [
            (
                Methods {
                    merge: true,
                    ..Methods::default()
                },
                "--merge",
            ),
            (
                Methods {
                    squash: true,
                    merge: true,
                    rebase: false,
                },
                "--squash",
            ),
        ] {
            let mut f = Fake::new(vec![
                view("OPEN", "a"),
                view("OPEN", "a"),
                view("MERGED", "a"),
            ]);
            f.methods = Some(methods);
            *f.merges.borrow_mut() = vec![
                (
                    1,
                    "Merge method is not allowed; merge method required".into(),
                ),
                (0, "ok".into()),
            ];
            let r = go(&f, quick());
            assert_eq!(r.verdict, "MERGED");
            assert!(f.merge_log.borrow()[1].contains(flag), "{flag}");
            assert!(f.merge_log.borrow()[1].contains("--match-head-commit a"));
        }
    }

    #[test]
    fn no_checks_proceeds_and_reports_none() {
        let f = Fake::new(vec![
            view("OPEN", "a"),
            view("OPEN", "a"),
            view("MERGED", "a"),
        ]);
        *f.checks.borrow_mut() = vec![Some("[]".into())];
        let r = go(&f, quick());
        assert_eq!(r.verdict, "MERGED");
        assert_eq!(r.checks.seen, "none");
    }

    #[test]
    fn the_first_empty_read_waits_once_for_checks_to_appear() {
        let f = Fake::new(vec![
            view("OPEN", "a"),
            view("OPEN", "a"),
            view("MERGED", "a"),
        ]);
        *f.checks.borrow_mut() = vec![Some("[]".into()), Some(pass())];
        let slept = RefCell::new(Vec::new());
        let clock = RefCell::new(Duration::ZERO);
        let opts = Opts {
            settle: Duration::from_secs(120),
            ..quick()
        };
        let r = land(&f, "7", &opts, &mut || *clock.borrow(), &mut |d| {
            slept.borrow_mut().push(d);
            *clock.borrow_mut() += d.max(Duration::from_secs(1));
        });
        assert_eq!(r.verdict, "MERGED");
        assert_eq!(r.checks.seen, "read");
        assert_eq!(slept.borrow()[0], Duration::from_secs(120));
    }

    #[test]
    fn a_failed_checks_read_is_retried_not_treated_as_no_checks() {
        let f = Fake::new(vec![
            view("OPEN", "a"),
            view("OPEN", "a"),
            view("MERGED", "a"),
        ]);
        *f.checks.borrow_mut() = vec![None, Some(pass())];
        let r = go(&f, quick());
        assert_eq!(r.verdict, "MERGED");
        assert_eq!(r.checks.total, 1);
    }

    #[test]
    fn a_failed_view_while_armed_is_read_again() {
        let f = Fake::new(vec![view("OPEN", "a"), view("OPEN", "a")]);
        f.views
            .borrow_mut()
            .extend([None, Some(view("MERGED", "a"))]);
        let r = go(&f, quick());
        assert_eq!(r.verdict, "MERGED");
    }

    #[test]
    fn a_first_view_that_fails_is_fail() {
        let f = Fake::new(vec![view("OPEN", "a")]);
        *f.views.borrow_mut() = vec![None];
        assert_eq!(go(&f, quick()).verdict, "FAIL");
    }

    #[test]
    fn a_second_call_pins_the_same_head() {
        let f = Fake::new(vec![view("OPEN", "a")]);
        let first = go(&f, quick());
        let second = go(&f, quick());
        assert_eq!(first.verdict, "STILL_WAITING");
        assert_eq!(second.verdict, "STILL_WAITING");
        let log = f.merge_log.borrow();
        assert_eq!(log[0], log[1]);
    }

    #[test]
    fn parity_never_admin_the_exit_codes_follow_the_verdicts() {
        assert_eq!(exit_code("MERGED"), 0);
        assert_eq!(exit_code("DRAFT"), 3);
        assert_eq!(exit_code("FAIL"), 6);
        assert_eq!(exit_code("STILL_WAITING"), 7);
        assert_eq!(exit_code("NOT_QUEUED"), 8);
    }

    #[test]
    fn max_polls_ends_the_wait_without_a_clock() {
        let f = Fake::new(vec![view("OPEN", "a")]);
        let r = go(
            &f,
            Opts {
                max_polls: Some(2),
                timeout: Duration::from_secs(10_000),
                ..quick()
            },
        );
        assert_eq!(r.verdict, "STILL_WAITING");
    }
}
