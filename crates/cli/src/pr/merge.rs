// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr merge`: the verdict, reason and next step of a merge call,
//! for both the waiting flow and the direct `--no-wait` and `--admin` calls.

use crate::json::envelope;
use crate::pr::land::{self, verdict as land_verdict};
use serde_json::{json, Map, Value};

/// The words of the `MERGE=` line.
pub mod verdict {
    pub const MERGED: &str = "MERGED";
    pub const ARMED: &str = "ARMED";
    pub const GATE: &str = "GATE";
    pub const FAILED: &str = "FAILED";
    pub const CLOSED: &str = "CLOSED";
    pub const TIMEOUT: &str = "TIMEOUT";
    pub const NOT_QUEUED: &str = "NOT_QUEUED";
}

/// What the caller does next.
pub mod next {
    pub const DONE: &str = "done";
    pub const WAIT: &str = "wait";
    pub const REBASE: &str = "rebase";
    pub const FIX_CI: &str = "fix-ci";
    pub const ASK: &str = "ask";
}

pub mod reason {
    pub const MERGED: &str = "merged";
    pub const ARMED: &str = "auto-merge is armed";
    pub const DRAFT: &str = "the PR is a draft";
    pub const WAITING_ON_BASE: &str = "the base branch is another open PR";
    pub const HEAD_MOVED: &str = "the head moved after the checks";
    pub const REVIEW_REQUIRED: &str = "review required";
    pub const CHANGES_REQUESTED: &str = "changes requested";
    pub const NEEDS_REBASE: &str = "the branch needs a rebase";
    pub const UNREADABLE: &str = "could not read the PR";
    pub const CHECK_FAILED: &str = "a required check failed";
    pub const CLOSED: &str = "the PR is closed";
    pub const TIMEOUT: &str = "still waiting after the timeout";
    pub const NOT_QUEUED: &str = "auto-merge did not queue";
    pub const ADMIN_FAILED: &str = "the admin merge failed";
}

/// Exit codes of the shared table that a merge call uses.
pub mod code {
    pub const OK: i32 = 0;
    pub const GATE: i32 = 3;
    pub const GH_FAILED: i32 = 5;
    pub const CLOSED_OR_CHECK: i32 = 6;
    pub const TIMEOUT: i32 = 7;
    pub const NOT_QUEUED: i32 = 8;
}

/// How a merge call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub verdict: &'static str,
    pub reason: &'static str,
    pub next: &'static str,
    /// The exit code plain mode uses.
    pub code: i32,
}

/// An outcome with the facts behind it.
#[derive(Debug)]
pub struct Merged {
    pub outcome: Outcome,
    pub detail: Map<String, Value>,
}

const fn outcome(
    verdict: &'static str,
    reason: &'static str,
    next: &'static str,
    code: i32,
) -> Outcome {
    Outcome {
        verdict,
        reason,
        next,
        code,
    }
}

fn from_land_report(r: &land::Report) -> Outcome {
    use verdict as v;
    match r.verdict.as_str() {
        land_verdict::MERGED => outcome(v::MERGED, reason::MERGED, next::DONE, code::OK),
        land_verdict::FAIL if r.checks.fail > 0 => outcome(
            v::FAILED,
            reason::CHECK_FAILED,
            next::FIX_CI,
            code::CLOSED_OR_CHECK,
        ),
        land_verdict::FAIL => outcome(v::FAILED, reason::UNREADABLE, next::ASK, code::GH_FAILED),
        land_verdict::CLOSED => {
            outcome(v::CLOSED, reason::CLOSED, next::ASK, code::CLOSED_OR_CHECK)
        }
        land_verdict::STILL_WAITING => {
            outcome(v::TIMEOUT, reason::TIMEOUT, next::WAIT, code::TIMEOUT)
        }
        land_verdict::NOT_QUEUED => outcome(
            v::NOT_QUEUED,
            reason::NOT_QUEUED,
            next::ASK,
            code::NOT_QUEUED,
        ),
        land_verdict::DRAFT => outcome(v::GATE, reason::DRAFT, next::ASK, code::GATE),
        land_verdict::WAITING_ON_BASE => {
            outcome(v::GATE, reason::WAITING_ON_BASE, next::WAIT, code::GATE)
        }
        land_verdict::HEAD_MOVED => outcome(v::GATE, reason::HEAD_MOVED, next::ASK, code::GATE),
        land_verdict::REVIEW_GATE => {
            outcome(v::GATE, reason::REVIEW_REQUIRED, next::ASK, code::GATE)
        }
        land_verdict::CHANGES_REQUESTED => {
            outcome(v::GATE, reason::CHANGES_REQUESTED, next::ASK, code::GATE)
        }
        _ => outcome(v::GATE, reason::NEEDS_REBASE, next::REBASE, code::GATE),
    }
}

/// The result of the waiting flow.
pub fn from_land(r: &land::Report) -> Merged {
    let mut detail = match serde_json::to_value(r) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    for key in ["verdict", "next", "exit_code"] {
        detail.remove(key);
    }
    Merged {
        outcome: from_land_report(r),
        detail,
    }
}

/// The result of a direct `gh pr merge` call: `--admin` merges, `--no-wait`
/// arms and returns.
pub fn from_direct(pr: &str, admin: bool, rc: i32, output: &str) -> Merged {
    let outcome = match (admin, rc == 0) {
        (true, true) => outcome(verdict::MERGED, reason::MERGED, next::DONE, code::OK),
        (true, false) => outcome(
            verdict::FAILED,
            reason::ADMIN_FAILED,
            next::ASK,
            code::GH_FAILED,
        ),
        (false, true) => outcome(verdict::ARMED, reason::ARMED, next::WAIT, code::OK),
        (false, false) => outcome(
            verdict::NOT_QUEUED,
            reason::NOT_QUEUED,
            next::ASK,
            code::NOT_QUEUED,
        ),
    };
    let detail = json!({"pr": pr, "rc": rc, "output": output});
    Merged {
        outcome,
        detail: detail.as_object().cloned().unwrap_or_default(),
    }
}

impl Merged {
    fn fields(&self) -> Value {
        let mut fields = self.detail.clone();
        fields.insert("verdict".into(), json!(self.outcome.verdict));
        fields.insert("reason".into(), json!(self.outcome.reason));
        fields.insert("next".into(), json!(self.outcome.next));
        Value::Object(fields)
    }

    /// The `MERGE=` line, then the report.
    pub fn render(&self) -> String {
        format!(
            "MERGE={} reason={}\n{}",
            self.outcome.verdict,
            self.outcome.reason,
            serde_json::to_string_pretty(&Value::Object(self.detail.clone())).unwrap_or_default()
        )
    }

    /// The shared envelope: `ok` and `code` follow the plain exit code.
    pub fn envelope(&self) -> Value {
        let error = if self.outcome.code == code::OK {
            ""
        } else {
            self.outcome.reason
        };
        envelope::wrap(self.outcome.code, error, self.fields())
    }
}
