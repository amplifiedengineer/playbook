// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The pull request half of `deliver`: find, create, edit, mark ready, queue
//! auto-merge and read checks, all through `gh` with fixed argument vectors,
//! plus the body composed when the caller gives none.

use super::sys::Sys;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

/// A pull request as `gh` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pr {
    pub number: u64,
    pub url: String,
    pub draft: bool,
    pub state: String,
    pub title: String,
    pub base: String,
}

/// One required check and its bucket (`pass`, `fail`, `pending`, `cancel`, `skipping`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: String,
    pub bucket: String,
}

const FIELDS: &str = "number,url,isDraft,state,title,baseRefName,isCrossRepository";

fn parse_pr(v: &Value) -> Option<Pr> {
    Some(Pr {
        number: v.get("number")?.as_u64()?,
        url: v.get("url")?.as_str()?.to_string(),
        draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        state: v
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("OPEN")
            .to_string(),
        title: v
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        base: v
            .get("baseRefName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn json(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("gh returned bad JSON: {e}"))
}

/// The open PR whose head is `branch`, if any.
pub fn find(sys: &Sys, branch: &str) -> Result<Option<Pr>, String> {
    let out = sys.gh(&[
        "pr", "list", "--head", branch, "--state", "open", "--json", FIELDS, "--limit", "20",
    ]);
    if !out.ok {
        return Err(format!("gh pr list failed: {}", out.stderr));
    }
    Ok(json(&out.stdout)?
        .as_array()
        .and_then(|a| {
            a.iter().find(|v| {
                !v.get("isCrossRepository")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
        })
        .and_then(parse_pr))
}

/// Branches of open PRs as `(head, base)`, for stack detection.
pub fn open_heads(sys: &Sys) -> Vec<(String, String)> {
    let out = sys.gh(&[
        "pr",
        "list",
        "--state",
        "open",
        "--json",
        "headRefName,baseRefName",
        "--limit",
        "100",
    ]);
    if !out.ok {
        return Vec::new();
    }
    let Ok(value) = json(&out.stdout) else {
        return Vec::new();
    };
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            Some((
                v.get("headRefName")?.as_str()?.to_string(),
                v.get("baseRefName")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

/// Opens a PR and returns it.
pub fn create(
    sys: &Sys,
    branch: &str,
    base: &str,
    title: &str,
    body_file: &Path,
    draft: bool,
) -> Result<Pr, String> {
    let file = body_file.to_string_lossy();
    let mut args = vec![
        "pr",
        "create",
        "--head",
        branch,
        "--base",
        base,
        "--title",
        title,
        "--body-file",
        &file,
    ];
    if draft {
        args.push("--draft");
    }
    let out = sys.gh(&args);
    if !out.ok {
        return Err(format!("gh pr create failed: {}", out.stderr));
    }
    find(sys, branch)?.ok_or_else(|| {
        format!(
            "gh pr create printed {} but no open PR was found for {branch}",
            out.stdout
        )
    })
}

/// Updates the title, the body or the base of PR `number`.
pub fn edit(
    sys: &Sys,
    number: u64,
    title: Option<&str>,
    body_file: Option<&Path>,
    base: Option<&str>,
) -> Result<(), String> {
    let n = number.to_string();
    let file = body_file.map(|p| p.to_string_lossy().into_owned());
    let mut args = vec!["pr", "edit", n.as_str()];
    if let Some(title) = title {
        args.extend(["--title", title]);
    }
    if let Some(file) = &file {
        args.extend(["--body-file", file]);
    }
    if let Some(base) = base {
        args.extend(["--base", base]);
    }
    let out = sys.gh(&args);
    if out.ok {
        Ok(())
    } else {
        Err(format!("gh pr edit failed: {}", out.stderr))
    }
}

/// Marks PR `number` ready, or back to draft when `ready` is false.
pub fn set_ready(sys: &Sys, number: u64, ready: bool) -> Result<(), String> {
    let n = number.to_string();
    let mut args = vec!["pr", "ready", n.as_str()];
    if !ready {
        args.push("--undo");
    }
    let out = sys.gh(&args);
    if out.ok {
        Ok(())
    } else {
        Err(format!("gh pr ready failed: {}", out.stderr))
    }
}

/// Asks GitHub to merge PR `number` once its requirements pass, only if its
/// head is still `head` (no pin when `head` is empty).
pub fn enable_auto_merge(sys: &Sys, number: u64, head: &str) -> Result<(), String> {
    let n = number.to_string();
    let mut args = crate::pr::wait::merge_args(&n, false, Some(head).filter(|h| !h.is_empty()));
    // Name the method, as before, so a non-interactive `gh` does not refuse.
    args.push("--squash");
    let out = sys.gh(&args[..]);
    if out.ok {
        Ok(())
    } else {
        Err(format!("gh pr merge --auto failed: {}", out.stderr))
    }
}

/// State of PR `number` and whether auto-merge or the merge queue holds it.
pub fn status(sys: &Sys, number: u64) -> Result<(String, bool), String> {
    let n = number.to_string();
    let mut out = sys.gh(&[
        "pr",
        "view",
        &n,
        "--json",
        "state,autoMergeRequest,mergeQueueEntry",
    ]);
    if !out.ok {
        out = sys.gh(&["pr", "view", &n, "--json", "state,autoMergeRequest"]);
    }
    if !out.ok {
        return Err(format!("gh pr view failed: {}", out.stderr));
    }
    let v = json(&out.stdout)?;
    let state = v
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("OPEN")
        .to_string();
    let held = |key: &str| v.get(key).is_some_and(|x| !x.is_null());
    Ok((state, held("autoMergeRequest") || held("mergeQueueEntry")))
}

/// Required checks of PR `number`. `gh` exits non-zero while checks are
/// pending or failing but still prints the JSON, so only the JSON counts.
pub fn checks(sys: &Sys, number: u64) -> Vec<Check> {
    let n = number.to_string();
    let out = sys.gh(&["pr", "checks", &n, "--required", "--json", "name,bucket"]);
    let Ok(value) = json(&out.stdout) else {
        return Vec::new();
    };
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            Some(Check {
                name: v.get("name")?.as_str()?.to_string(),
                bucket: v.get("bucket")?.as_str()?.to_string(),
            })
        })
        .collect()
}

/// The repo PR template, if one exists.
pub fn template(dir: &Path) -> Option<String> {
    [
        ".github/pull_request_template.md",
        ".github/PULL_REQUEST_TEMPLATE.md",
        "docs/pull_request_template.md",
        "pull_request_template.md",
    ]
    .iter()
    .find_map(|p| std::fs::read_to_string(dir.join(p)).ok())
}

/// `Closes #N` and `Ticket:` lines found in `texts`, in order, once each.
pub fn kept_lines<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let re = crate::common::re::static_regex(
        r"(?i)^\s*((close[sd]?|fix(e[sd])?|resolve[sd]?)\s+#\d+|ticket:\s*\S+)",
    );
    let mut out: Vec<String> = Vec::new();
    for text in texts {
        for line in text.lines().filter(|l| re.is_match(l)) {
            let line = line.trim().to_string();
            if !out.contains(&line) {
                out.push(line);
            }
        }
    }
    out
}

/// The body used when the caller gives none: a summary from the commit
/// subjects, the repo template, and the lines worth keeping.
pub fn compose_body(
    subjects: &[String],
    messages: &[String],
    template: Option<&str>,
    ticket: Option<&str>,
    issue: Option<u64>,
) -> String {
    let mut body = String::new();
    if let Some(ticket) = ticket {
        body.push_str(&format!("{ticket}\n\n"));
    }
    body.push_str("## Summary\n\n");
    for subject in subjects {
        body.push_str(&format!("- {subject}\n"));
    }
    if let Some(template) = template {
        body.push('\n');
        body.push_str(template.trim_end());
        body.push('\n');
    }
    let kept: Vec<String> = kept_lines(messages.iter().map(String::as_str))
        .into_iter()
        .filter(|l| ticket.is_none() || !l.to_lowercase().starts_with("ticket:"))
        .collect();
    let mut kept = kept;
    if let Some(n) = issue {
        if !kept.iter().any(|l| l.contains(&format!("#{n}"))) {
            kept.push(format!("Refs #{n}"));
        }
    }
    if !kept.is_empty() {
        body.push('\n');
        body.push_str(&kept.join("\n"));
        body.push('\n');
    }
    body
}

/// `body` with any of `kept` it lacks added at the end.
pub fn with_kept(body: &str, kept: &[String]) -> String {
    let missing: Vec<&String> = kept.iter().filter(|l| !body.contains(l.as_str())).collect();
    if missing.is_empty() {
        return body.to_string();
    }
    let mut out = body.trim_end().to_string();
    out.push_str("\n\n");
    for line in missing {
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_and_ticket_lines_are_kept_once() {
        let kept = kept_lines(["fix: a\n\nCloses #12\nTicket: ABC-1", "Closes #12"]);

        assert_eq!(kept, vec!["Closes #12", "Ticket: ABC-1"]);
    }

    #[test]
    fn a_composed_body_lists_subjects_and_keeps_the_ticket() {
        let body = compose_body(
            &["feat: one".to_string()],
            &["feat: one\n\nTicket: ABC-1".to_string()],
            Some("## Test plan\n"),
            None,
            None,
        );

        assert!(body.contains("- feat: one") && body.contains("## Test plan"));
        assert!(body.ends_with("Ticket: ABC-1\n"), "got {body}");
    }
}
