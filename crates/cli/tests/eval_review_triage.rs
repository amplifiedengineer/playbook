// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook eval triage`, ported from the former shell test. `gh`
//! and `claude` are stub executables on PATH, so no test touches the network
//! or spends an API call.

#[path = "support/isolated.rs"]
mod isolated;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Run {
    code: i32,
    out: String,
    dir: PathBuf,
}

fn scratch() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("playbook-eval-{}-{n}", playbook::testing::run_id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn stub(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Stubs `gh` (diff fetch ok or failing) and `claude`, which counts its calls
/// and prints `response_N.json` for call N, else `response.json`.
fn shims(dir: &Path, gh_ok: bool) {
    if gh_ok {
        stub(dir, "gh", "echo 'diff --git a/x b/x'; echo '+placeholder'");
    } else {
        stub(
            dir,
            "gh",
            "echo 'gh: could not resolve pull request' >&2; exit 1",
        );
    }
    let d = dir.display();
    stub(
        dir,
        "claude",
        &format!(
            "touch '{d}/sentinel'\necho \"$@\" > '{d}/argv'\ncat > '{d}/stdin'\nn=0; [ -f '{d}/count' ] && n=$(cat '{d}/count')\nn=$((n+1)); echo $n > '{d}/count'\nif [ -f '{d}/response_'$n'.json' ]; then cat '{d}/response_'$n'.json'; else cat '{d}/response.json'; fi"
        ),
    );
}

/// Runs the eval over `cases` (fixture JSON) with the given default response.
fn run_with(cases: &str, response: &str, gh_ok: bool, extra: &[(&str, &str)]) -> Run {
    let dir = scratch();
    shims(&dir, gh_ok);
    fs::write(dir.join("response.json"), response).unwrap();
    for (name, body) in extra {
        fs::write(dir.join(name), body).unwrap();
    }
    fs::write(dir.join("cases.json"), cases).unwrap();
    fs::write(
        dir.join("triage.md"),
        "---\nname: t\n---\nClassify each lens.\n",
    )
    .unwrap();
    let out = isolated::playbook()
        .args(["eval", "triage"])
        .arg(dir.join("cases.json"))
        .arg("--prompt")
        .arg(dir.join("triage.md"))
        .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
        .output()
        .unwrap();
    Run {
        code: out.status.code().unwrap_or(-1),
        out: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        dir,
    }
}

fn lens_line<'a>(out: &'a str, lens: &str) -> &'a str {
    out.lines()
        .find(|l| l.trim_start().starts_with(lens) && l.starts_with("    "))
        .unwrap_or_else(|| panic!("no report line for lens {lens} in:\n{out}"))
}

const SEC_FULL: &str = r#"{"security": {"tier": "full-lens", "reason": "auth"}}"#;
const SEC_SKIP: &str = r#"{"security": {"tier": "skip", "reason": "none"}}"#;

fn one(found: bool) -> String {
    format!(r#"[{{"id": "c1", "pr": 101, "lenses": {{"security": {{"found": {found}}}}}}}]"#)
}

#[test]
fn a_full_lens_tier_for_a_found_lens_is_a_match_and_passes() {
    let r = run_with(&one(true), SEC_FULL, true, &[]);
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(lens_line(&r.out, "security").contains("match"));
    assert!(r.out.contains("  critical false-negative:  0"));
    assert!(r.out.contains("PASS:"));
}

#[test]
fn skipping_a_found_lens_is_a_critical_false_negative_and_fails() {
    let r = run_with(&one(true), SEC_SKIP, true, &[]);
    assert_eq!(r.code, 1, "{}", r.out);
    assert!(lens_line(&r.out, "security").contains("critical false-negative"));
    assert!(r.out.contains("FAIL:"));
}

#[test]
fn a_non_critical_mismatch_alone_still_exits_zero() {
    let r = run_with(&one(false), SEC_FULL, true, &[]);
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(lens_line(&r.out, "security").contains("non-critical mismatch"));
    assert!(r.out.contains("PASS:"));
}

#[test]
fn a_lens_missing_from_the_tier_map_errors_only_that_lens() {
    let cases = r#"[{"id": "c1", "pr": 104, "lenses": {"security": {"found": true}, "correctness": {"found": false}}}]"#;
    let r = run_with(cases, SEC_FULL, true, &[]);
    assert_eq!(r.code, 1, "{}", r.out);
    assert!(lens_line(&r.out, "correctness").contains("errored"));
    assert!(lens_line(&r.out, "security").contains("match"));
    assert!(r.out.contains("Summary: 2 (case, lens) pair(s)"));
}

#[test]
fn an_unparseable_reply_errors_every_declared_lens() {
    let cases = r#"[{"id": "c1", "pr": 105, "lenses": {"security": {"found": true}, "correctness": {"found": false}}}]"#;
    let r = run_with(cases, "not valid json at all {[garbage\n", true, &[]);
    assert_eq!(r.code, 1, "{}", r.out);
    assert!(lens_line(&r.out, "security").contains("errored"));
    assert!(lens_line(&r.out, "correctness").contains("errored"));
    assert!(r.out.contains("classifier response was not valid JSON"));
}

#[test]
fn a_failed_diff_fetch_errors_the_case_and_never_calls_claude() {
    let r = run_with(&one(true), SEC_FULL, false, &[]);
    assert_eq!(r.code, 1, "{}", r.out);
    assert!(lens_line(&r.out, "security").contains("errored"));
    assert!(r.out.contains("failed to fetch"));
    assert!(
        !r.dir.join("sentinel").exists(),
        "claude ran after a fetch failure"
    );
}

#[test]
fn the_summary_counts_each_verdict_across_cases() {
    let cases = r#"[
      {"id": "m", "pr": 207, "lenses": {"security": {"found": true}}},
      {"id": "n", "pr": 208, "lenses": {"security": {"found": false}}},
      {"id": "c", "pr": 209, "lenses": {"security": {"found": true}}}
    ]"#;
    let r = run_with(cases, SEC_FULL, true, &[("response_3.json", SEC_SKIP)]);
    assert_eq!(r.code, 1, "{}", r.out);
    assert!(r.out.contains("  match:                    1"));
    assert!(r.out.contains("  non-critical mismatch:    1"));
    assert!(r.out.contains("  critical false-negative:  1"));
    assert!(r.out.contains("  errored:                  0"));
}

#[test]
fn a_fenced_or_prose_wrapped_reply_still_parses() {
    let cases = r#"[
      {"id": "a", "pr": 109, "lenses": {"security": {"found": true}}},
      {"id": "b", "pr": 110, "lenses": {"security": {"found": true}}}
    ]"#;
    let fenced = format!("```json\n{SEC_FULL}\n```\n");
    let prose = format!("Here is the result:\n```json\n{SEC_FULL}\n```\nLet me know.\n");
    let r = run_with(
        cases,
        SEC_FULL,
        true,
        &[("response_1.json", &fenced), ("response_2.json", &prose)],
    );
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(!r.out.contains("not valid JSON"));
}

#[test]
fn a_missing_case_file_is_a_setup_error() {
    let dir = scratch();
    shims(&dir, true);
    let out = isolated::playbook()
        .args(["eval", "triage"])
        .arg(dir.join("absent.json"))
        .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("case file not found"));
}

#[test]
fn the_shipped_case_file_is_a_non_empty_array_with_a_known_answer_per_lens() {
    let path = Path::new(env!("PLAYBOOK_REPO_ROOT"))
        .join("crates/cli/tests/fixtures/review-triage-eval-set.json");
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let cases = v.as_array().expect("array");
    assert!(!cases.is_empty());
    for c in cases {
        for (_, lens) in c["lenses"].as_object().expect("lenses") {
            assert!(lens["found"].is_boolean());
        }
    }
}

#[test]
fn an_empty_case_file_fails_instead_of_passing_vacuously() {
    let r = run_with("[]", SEC_FULL, true, &[]);
    assert_eq!(r.code, 1, "{}", r.out);
    assert!(r.out.contains("nothing was validated"));
}

#[test]
fn the_prompt_sent_to_claude_carries_the_body_lenses_and_diff() {
    let r = run_with(&one(true), SEC_FULL, true, &[]);
    let argv = fs::read_to_string(r.dir.join("argv")).unwrap();
    assert_eq!(argv.trim(), "-p --model haiku", "prompt must not ride argv");
    let sent = fs::read_to_string(r.dir.join("stdin")).unwrap();
    assert!(sent.contains("Classify each lens."));
    assert!(!sent.contains("name: t"), "frontmatter leaked: {sent}");
    assert!(sent.contains("Candidate lenses to classify"));
    assert!(sent.contains("security"));
    assert!(sent.contains("+placeholder"));
}

#[test]
fn the_default_prompt_is_found_in_this_checkout() {
    let dir = scratch();
    shims(&dir, true);
    fs::write(dir.join("response.json"), SEC_FULL).unwrap();
    fs::write(dir.join("cases.json"), one(true)).unwrap();
    let out = isolated::playbook()
        .args(["eval", "triage"])
        .arg(dir.join("cases.json"))
        .current_dir(env!("PLAYBOOK_REPO_ROOT"))
        .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0), "{text}");
    let sent = fs::read_to_string(dir.join("stdin")).unwrap();
    assert!(
        sent.contains("read-only triage classifier"),
        "the shipped prompt was not sent: {sent}"
    );
}
