// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook eval triage`: a live regression eval for the review
//! triage classifier. For each case it fetches the pull request's diff with
//! `gh pr diff`, sends the classifier prompt plus that diff to
//! `claude -p --model haiku` on stdin, and compares the returned tier per lens with
//! the case's known `found` fact. Costs real API calls, never run by tests
//! against the live services (tests shim `gh` and `claude` on PATH).

pub mod bench;
pub mod wu_bench;

use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DEFAULT_FIXTURES: &str = "crates/cli/tests/fixtures/review-triage-eval-set.json";
/// Relative to the plugin folder, see [`crate::plugin_root::in_checkout`].
const DEFAULT_PROMPT: &str = "agents/review-triage.md";

/// One (case, lens) comparison result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Match,
    NonCritical,
    Critical,
    Errored,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Match => "match",
            Verdict::NonCritical => "non-critical mismatch",
            Verdict::Critical => "critical false-negative",
            Verdict::Errored => "errored",
        }
    }
}

/// The prompt body: everything after the second `---` line of the agent file.
fn prompt_body(agent_file: &str) -> String {
    let mut seen = 0;
    let mut body = Vec::new();
    for line in agent_file.lines() {
        if line.trim_end() == "---" {
            seen += 1;
            continue;
        }
        if seen >= 2 {
            body.push(line);
        }
    }
    body.join("\n").trim().to_string()
}

/// Parses a classifier reply as JSON, retrying on the span from the first
/// `{` to the last `}` because models wrap the map in fences or prose.
fn parse_tier_map(response: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(response.trim()) {
        return Some(v);
    }
    let start = response.find('{')?;
    let end = response.rfind('}')?;
    (start < end)
        .then(|| serde_json::from_str(&response[start..=end]).ok())
        .flatten()
}

fn tier_of(tier_map: &Value, lens: &str) -> String {
    tier_map
        .as_object()
        .and_then(|m| m.get(lens))
        .and_then(|e| e.get("tier"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn raw_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Compares a case's `found` fact with the classifier's tier.
pub fn judge(lens: &str, found: &str, tier: &str) -> (Verdict, String) {
    match (found, tier) {
        ("true", "full-lens") => (
            Verdict::Match,
            "ground truth found=true, tier=full-lens".into(),
        ),
        ("true", "cheap-check" | "skip") => (
            Verdict::Critical,
            format!("ground truth found=true, tier={tier}"),
        ),
        ("false", "skip") => (Verdict::Match, "ground truth found=false, tier=skip".into()),
        ("false", "cheap-check" | "full-lens") => (
            Verdict::NonCritical,
            format!("ground truth found=false, tier={tier}"),
        ),
        _ if found != "true" && found != "false" => (
            Verdict::Errored,
            format!("the case's found value '{found}' for lens '{lens}' is not true/false"),
        ),
        _ => (
            Verdict::Errored,
            format!("unrecognised tier '{tier}' for lens '{lens}'"),
        ),
    }
}

#[derive(Default)]
struct Counts {
    matched: usize,
    noncritical: usize,
    critical: usize,
    errored: usize,
}

impl Counts {
    fn record(&mut self, lens: &str, verdict: Verdict, detail: &str) {
        match verdict {
            Verdict::Match => self.matched += 1,
            Verdict::NonCritical => self.noncritical += 1,
            Verdict::Critical => self.critical += 1,
            Verdict::Errored => self.errored += 1,
        }
        println!("    {lens:<14} {:<26} {detail}", verdict.label());
    }
}

fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(tool).is_file()))
        .unwrap_or(false)
}

/// The enclosing git repo's top level, or the current directory outside one.
pub fn repo_root() -> PathBuf {
    Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or_default()
}

/// Sends the prompt to `claude -p` on stdin, so a large diff cannot hit the
/// per-argument size limit and the diff text stays out of `ps` output.
/// Returns (stdout, stderr or the spawn error).
fn call_claude(prompt: &str) -> (String, String) {
    let mut child = match Command::new("claude")
        .args(["-p", "--model", "haiku"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (String::new(), e.to_string()),
    };
    if let Some(mut stdin) = child.stdin.take() {
        // A write error (claude exited early) shows up in its own output.
        let _ = stdin.write_all(prompt.as_bytes());
    }
    match child.wait_with_output() {
        Ok(out) => (
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ),
        Err(e) => (String::new(), e.to_string()),
    }
}

fn fail(msg: &str) -> i32 {
    eprintln!("eval triage: {msg}");
    1
}

/// Runs the eval and returns the process exit code.
pub fn review_triage(fixtures: Option<PathBuf>, prompt: Option<PathBuf>) -> i32 {
    let root = repo_root();
    let fixtures = fixtures.unwrap_or_else(|| root.join(DEFAULT_FIXTURES));
    let prompt =
        prompt.unwrap_or_else(|| crate::plugin_root::in_checkout(&root).join(DEFAULT_PROMPT));
    if !on_path("gh") {
        return fail("gh is required");
    }
    if !on_path("claude") {
        return fail("claude CLI is required (needs a live API key)");
    }
    let Ok(fixture_text) = std::fs::read_to_string(&fixtures) else {
        return fail(&format!("case file not found: {}", fixtures.display()));
    };
    let Ok(prompt_text) = std::fs::read_to_string(&prompt) else {
        return fail(&format!("prompt file not found: {}", prompt.display()));
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&fixture_text) else {
        return fail(&format!(
            "case file is not valid JSON: {}",
            fixtures.display()
        ));
    };
    let Some(cases) = parsed.as_array() else {
        return fail(&format!(
            "case file is not a JSON array: {}",
            fixtures.display()
        ));
    };
    let system_prompt = prompt_body(&prompt_text);
    if system_prompt.is_empty() {
        return fail(&format!(
            "could not extract a prompt body from {}",
            prompt.display()
        ));
    }
    run_cases(&fixtures, cases, &system_prompt)
}

fn run_cases(fixtures: &Path, cases: &[Value], system_prompt: &str) -> i32 {
    let mut counts = Counts::default();
    println!(
        "eval triage: {} case(s) from {}\n",
        cases.len(),
        fixtures.display()
    );
    for case in cases {
        let id = case.get("id").map(raw_text).unwrap_or_default();
        let pr = case.get("pr").map(raw_text).unwrap_or_default();
        let mut lenses: Vec<(&String, &Value)> = case
            .get("lenses")
            .and_then(Value::as_object)
            .map(|m| m.iter().collect())
            .unwrap_or_default();
        lenses.sort_by(|a, b| a.0.cmp(b.0));
        let names: Vec<&str> = lenses.iter().map(|(k, _)| k.as_str()).collect();
        let lens_list = names.join(", ");
        println!("{id} (PR #{pr}) -- lenses: {lens_list}");

        let error_all = |counts: &mut Counts, detail: &str| {
            for name in &names {
                counts.record(name, Verdict::Errored, detail);
            }
            println!();
        };

        let diff = match Command::new("gh").args(["pr", "diff", &pr]).output() {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
            Ok(out) => {
                println!(
                    "  fetch failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                error_all(&mut counts, &format!("gh pr diff {pr} failed to fetch"));
                continue;
            }
            Err(e) => {
                println!("  fetch failed: {e}");
                error_all(&mut counts, &format!("gh pr diff {pr} failed to fetch"));
                continue;
            }
        };

        let full_prompt = format!(
            "{system_prompt}\n\nCandidate lenses to classify (return a tier for every one of these, and only these): {lens_list}\n\nDiff to classify:\n{diff}"
        );
        let (response, claude_err) = call_claude(&full_prompt);

        let Some(tier_map) = parse_tier_map(&response) else {
            let head: String = response.trim().chars().take(300).collect();
            println!("  unparseable classifier response; every lens errored");
            println!("  claude stdout (first 300 chars): {head}");
            if !claude_err.is_empty() {
                println!("  claude stderr: {claude_err}");
            }
            let suffix = if claude_err.is_empty() {
                String::new()
            } else {
                format!(" (claude stderr: {claude_err})")
            };
            error_all(
                &mut counts,
                &format!("classifier response was not valid JSON{suffix}"),
            );
            continue;
        };

        for (lens, entry) in lenses {
            let found = entry.get("found").map(raw_text).unwrap_or("null".into());
            let tier = tier_of(&tier_map, lens);
            if tier.is_empty() {
                let detail = format!("classifier response missing a tier for lens '{lens}'");
                counts.record(lens, Verdict::Errored, &detail);
                continue;
            }
            let (verdict, detail) = judge(lens, &found, &tier);
            counts.record(lens, verdict, &detail);
        }
        println!();
    }
    summarize(&counts, cases.len())
}

fn summarize(c: &Counts, cases: usize) -> i32 {
    let total = c.matched + c.noncritical + c.critical + c.errored;
    println!("Summary: {total} (case, lens) pair(s) evaluated across {cases} case(s)");
    println!("  match:                    {}", c.matched);
    println!("  non-critical mismatch:    {}", c.noncritical);
    println!("  critical false-negative:  {}", c.critical);
    println!("  errored:                  {}", c.errored);
    if total == 0 {
        println!("\nFAIL: no (case, lens) pairs were evaluated, so nothing was validated.");
        return 1;
    }
    if c.critical > 0 || c.errored > 0 {
        println!(
            "\nFAIL: {} critical false-negative(s) and {} errored pair(s). review-triage is NOT the validated default dispatch path until this is zero and zero.",
            c.critical, c.errored
        );
        return 1;
    }
    println!("\nPASS: no critical false-negatives, no errored pairs.");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_body_drops_the_frontmatter() {
        let got = prompt_body("---\nname: x\n---\nBody line\nTwo\n");
        assert_eq!(got, "Body line\nTwo");
    }

    #[test]
    fn parse_tier_map_accepts_fenced_and_prose_wrapped_json() {
        let fenced = "```json\n{\"a\": {\"tier\": \"skip\"}}\n```";
        let prose = "Here:\n{\"a\": {\"tier\": \"skip\"}}\nThanks.";
        assert!(parse_tier_map(fenced).is_some());
        assert!(parse_tier_map(prose).is_some());
        assert!(parse_tier_map("not json {[garbage").is_none());
        assert!(parse_tier_map("").is_none());
    }

    #[test]
    fn judge_follows_the_verdict_table() {
        assert_eq!(judge("l", "true", "full-lens").0, Verdict::Match);
        assert_eq!(judge("l", "true", "skip").0, Verdict::Critical);
        assert_eq!(judge("l", "true", "cheap-check").0, Verdict::Critical);
        assert_eq!(judge("l", "false", "skip").0, Verdict::Match);
        assert_eq!(judge("l", "false", "full-lens").0, Verdict::NonCritical);
        assert_eq!(judge("l", "null", "skip").0, Verdict::Errored);
        assert_eq!(judge("l", "true", "bogus").0, Verdict::Errored);
    }
}
