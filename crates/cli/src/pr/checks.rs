// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr checks`: the checks of a PR, optionally waiting for them,
//! with the run id, failed step and `transient` flag of each failed job.

use crate::json::envelope;
use crate::pr::merge::code;
use crate::pr::wait::{self, verdict as ci};
use serde_json::{json, Value};
use std::time::Duration;

/// A check that has not finished when the call returns without `--watch`.
const PENDING: &str = "PENDING";
/// `gh` could not read the PR's checks.
const UNREADABLE: &str = "UNREADABLE";
/// The first step of every job: a failure here happens before any repo code.
const SETUP_STEP: &str = "Set up job";
const BUCKET_FAIL: &str = "fail";
const BUCKET_CANCEL: &str = "cancel";
const RUNS_SEGMENT: &str = "/runs/";
const JOB_SEGMENT: &str = "/job/";
const STEP_FAILED: &str = "failure";

/// What the caller asked for.
#[derive(Debug, Clone, Copy)]
pub struct Opts {
    pub watch: bool,
    pub fail_fast: bool,
    pub required: bool,
    pub timeout: Duration,
    pub interval: Duration,
}

/// The checks of a PR and what failed.
#[derive(Debug)]
pub struct Report {
    pub pr: String,
    pub verdict: &'static str,
    pub checks: Vec<Value>,
    pub failed: Vec<Value>,
}

impl Report {
    /// The plain exit code of the verdict.
    pub fn code(&self) -> i32 {
        match self.verdict {
            ci::FAIL | ci::CANCELLED => code::CLOSED_OR_CHECK,
            PENDING | ci::TIMEOUT => code::TIMEOUT,
            UNREADABLE => code::GH_FAILED,
            _ => code::OK,
        }
    }

    fn error(&self) -> &'static str {
        match self.code() {
            code::OK => "",
            code::TIMEOUT => "checks are still pending",
            code::GH_FAILED => "could not read the checks",
            _ => "a check failed",
        }
    }

    /// The `CHECKS=` line, a line per check, then a line per failed job.
    pub fn render(&self) -> String {
        let mut lines = vec![format!("CHECKS={}", self.verdict)];
        for c in &self.checks {
            let required = if c["required"] == true {
                "\trequired"
            } else {
                ""
            };
            lines.push(format!(
                "{}\t{}{required}",
                c["bucket"].as_str().unwrap_or(""),
                c["name"].as_str().unwrap_or("")
            ));
        }
        for f in &self.failed {
            let or_dash = |v: &Value| match v.as_str() {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => "-".to_string(),
            };
            let run = f["run_id"]
                .as_u64()
                .map_or("-".to_string(), |r| r.to_string());
            lines.push(format!(
                "FAILED run={run} step={} transient={} name={}",
                or_dash(&f["step"]),
                f["transient"],
                f["name"].as_str().unwrap_or("")
            ));
        }
        lines.join("\n")
    }

    /// The shared envelope: `ok` and `code` follow the plain exit code.
    pub fn envelope(&self) -> Value {
        envelope::wrap(
            self.code(),
            self.error(),
            json!({
                "pr": self.pr,
                "verdict": self.verdict,
                "checks": self.checks,
                "failed": self.failed,
            }),
        )
    }
}

fn id_after(link: &str, segment: &str) -> Option<u64> {
    let rest = &link[link.find(segment)? + segment.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The name of the first failed step of the job in `jobs` that a check ran as.
fn failed_step(jobs: &Value, job_id: Option<u64>, name: &str) -> String {
    let all = jobs["jobs"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let job = all
        .iter()
        .find(|j| job_id.is_some() && j["id"].as_u64() == job_id)
        .or_else(|| all.iter().find(|j| j["name"] == name));
    job.and_then(|j| j["steps"].as_array())
        .and_then(|steps| steps.iter().find(|s| s["conclusion"] == STEP_FAILED))
        .and_then(|s| s["name"].as_str())
        .unwrap_or("")
        .to_string()
}

/// The jobs of each workflow run, read on first use.
struct JobsCache<'a> {
    runs: Vec<(u64, Option<Value>)>,
    read: &'a mut dyn FnMut(u64) -> Option<Value>,
}

impl JobsCache<'_> {
    fn failed_step(&mut self, run: u64, job_id: Option<u64>, name: &str) -> String {
        let at = match self.runs.iter().position(|(r, _)| *r == run) {
            Some(at) => at,
            None => {
                self.runs.push((run, (self.read)(run)));
                self.runs.len() - 1
            }
        };
        self.runs[at]
            .1
            .as_ref()
            .map(|jobs| failed_step(jobs, job_id, name))
            .unwrap_or_default()
    }
}

/// One entry per failed or cancelled check. `jobs_of` returns the jobs of a
/// workflow run, called once per distinct run that has a failed check.
pub fn triage(checks: &[Value], jobs_of: &mut dyn FnMut(u64) -> Option<Value>) -> Vec<Value> {
    let mut jobs = JobsCache {
        runs: Vec::new(),
        read: jobs_of,
    };
    let mut failed = Vec::new();
    for c in checks {
        let bucket = c["bucket"].as_str().unwrap_or("");
        if bucket != BUCKET_FAIL && bucket != BUCKET_CANCEL {
            continue;
        }
        let name = c["name"].as_str().unwrap_or("");
        let link = c["link"].as_str().unwrap_or("");
        let run_id = id_after(link, RUNS_SEGMENT);
        // A cancelled job has no failed step to name.
        let step = match (bucket, run_id) {
            (BUCKET_FAIL, Some(run)) => jobs.failed_step(run, id_after(link, JOB_SEGMENT), name),
            _ => String::new(),
        };
        failed.push(json!({
            "name": name,
            "run_id": run_id,
            "step": step,
            "transient": bucket == BUCKET_CANCEL || step == SETUP_STEP,
        }));
    }
    failed
}

fn check_entry(check: &Value, required: bool) -> Value {
    let mut entry = serde_json::Map::new();
    for key in ["name", "workflow", "state", "bucket", "link"] {
        if let Some(v) = check.get(key) {
            entry.insert(key.to_string(), v.clone());
        }
    }
    entry.insert("required".into(), json!(required));
    Value::Object(entry)
}

fn identity(check: &Value) -> (String, String) {
    let f = |k: &str| check[k].as_str().unwrap_or("").to_string();
    (f("workflow"), f("name"))
}

/// The report for the checks JSON of `pr`. `required_json` is the read of
/// the required checks only, `None` when every check read is required.
pub fn report(
    pr: &str,
    verdict: &'static str,
    checks_json: &str,
    required_json: Option<&str>,
    jobs_of: &mut dyn FnMut(u64) -> Option<Value>,
) -> Report {
    let (latest, _) = wait::latest_checks(checks_json);
    let required: Option<Vec<_>> =
        required_json.map(|j| wait::latest_checks(j).0.iter().map(identity).collect());
    let checks = latest
        .iter()
        .map(|c| {
            check_entry(
                c,
                required.as_ref().is_none_or(|r| r.contains(&identity(c))),
            )
        })
        .collect();
    Report {
        pr: pr.to_string(),
        verdict,
        failed: triage(&latest, jobs_of),
        checks,
    }
}

fn read_jobs(run: u64) -> Option<Value> {
    let path = format!("repos/{{owner}}/{{repo}}/actions/runs/{run}/jobs");
    wait::gh_stdout(&["api", &path]).and_then(|t| serde_json::from_str(&t).ok())
}

/// Reads the checks of `pr` through `gh`, waiting when `opts.watch` is set.
pub fn run(pr: &str, opts: &Opts) -> Report {
    let mut unreadable = None;
    // Without --watch the read is a snapshot, so a failure is final at once.
    let (verdict, json) = wait::wait_checks(
        if opts.watch {
            opts.timeout
        } else {
            Duration::ZERO
        },
        opts.fail_fast || !opts.watch,
        &mut || match wait::try_read_checks(pr, !opts.required) {
            Ok(json) => json,
            Err(why) => {
                unreadable = Some(why);
                "[]".to_string()
            }
        },
        &mut |_| {},
        &mut || std::thread::sleep(opts.interval),
    );
    let verdict = match verdict {
        _ if unreadable.is_some() => UNREADABLE,
        ci::TIMEOUT if !opts.watch => PENDING,
        other => other,
    };
    let required = (!opts.required).then(|| wait::read_checks(pr, false));
    report(pr, verdict, &json, required.as_deref(), &mut read_jobs)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN: u64 = 5;
    const JOB: u64 = 9;

    fn link(run: u64, job: u64) -> String {
        format!("https://github.com/o/r/actions/runs/{run}/job/{job}")
    }

    fn check(name: &str, bucket: &str, run: u64, job: u64) -> Value {
        json!({"name": name, "bucket": bucket, "link": link(run, job)})
    }

    fn jobs(steps: &[(&str, &str)]) -> Value {
        let steps: Vec<Value> = steps
            .iter()
            .map(|(n, c)| json!({"name": n, "conclusion": c}))
            .collect();
        json!({"jobs": [{"id": JOB, "name": "build", "steps": steps}]})
    }

    #[test]
    fn the_run_and_job_ids_come_from_the_link() {
        let l = link(RUN, JOB);
        assert_eq!(id_after(&l, RUNS_SEGMENT), Some(RUN));
        assert_eq!(id_after(&l, JOB_SEGMENT), Some(JOB));
        assert_eq!(id_after("https://ci.example/build/1", RUNS_SEGMENT), None);
    }

    #[test]
    fn a_job_is_found_by_name_when_the_link_has_no_job_id() {
        let j = jobs(&[("Run tests", "failure")]);
        assert_eq!(failed_step(&j, None, "build"), "Run tests");
        assert_eq!(failed_step(&j, None, "other"), "");
    }

    #[test]
    fn jobs_are_read_once_per_run() {
        let checks = [
            check("a", "fail", RUN, JOB),
            check("b", "fail", RUN, JOB + 1),
        ];
        let mut reads = 0;

        let failed = triage(&checks, &mut |_| {
            reads += 1;
            Some(jobs(&[(SETUP_STEP, "failure")]))
        });

        assert_eq!(reads, 1);
        assert_eq!(failed[0]["transient"], true);
        assert_eq!(failed[1]["step"], "");
    }

    #[test]
    fn unreadable_jobs_leave_the_step_empty_and_the_failure_real() {
        let failed = triage(&[check("a", "fail", RUN, JOB)], &mut |_| None);

        assert_eq!(failed[0]["step"], "");
        assert_eq!(failed[0]["transient"], false);
    }

    #[test]
    fn a_check_without_an_actions_link_has_a_null_run_id() {
        let c = json!({"name": "ext", "bucket": "fail", "link": "https://ci.example/1"});

        let failed = triage(&[c], &mut |_| panic!("no run to read"));

        assert_eq!(failed[0]["run_id"], Value::Null);
    }

    #[test]
    fn verdicts_map_to_the_shared_exit_codes() {
        let code_of = |verdict| {
            Report {
                pr: "1".into(),
                verdict,
                checks: vec![],
                failed: vec![],
            }
            .code()
        };
        assert_eq!(code_of(ci::PASS), 0);
        assert_eq!(code_of(ci::NONE), 0);
        assert_eq!(code_of(ci::FAIL), 6);
        assert_eq!(code_of(ci::CANCELLED), 6);
        assert_eq!(code_of(PENDING), 7);
        assert_eq!(code_of(ci::TIMEOUT), 7);
        assert_eq!(code_of(UNREADABLE), 5);
    }
}
