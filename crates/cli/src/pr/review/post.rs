// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr review post`: create a pending review from a findings file.

use super::{post_review, repo, Done, Failure};
use crate::pr::reply::digits;
use serde::Deserialize;
use std::path::Path;

/// One inline comment of the findings file, as GitHub's review API takes it.
#[derive(Debug, Deserialize)]
struct Comment {
    path: String,
    line: u64,
    side: String,
    body: String,
    start_line: Option<u64>,
    start_side: Option<String>,
}

/// The findings file: the payload of the create-review call. There is no
/// review `body`, so the author frames the review when it is submitted.
#[derive(Debug, Deserialize)]
struct Findings {
    commit_id: String,
    body: Option<String>,
    comments: Vec<Comment>,
}

impl Findings {
    fn read(path: &Path) -> Result<Findings, Failure> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Failure::refused(format!("could not read {}: {e}", path.display())))?;
        let findings: Findings = serde_json::from_str(&text).map_err(|e| {
            Failure::refused(format!("{} is not a findings file: {e}", path.display()))
        })?;
        findings.check()?;
        Ok(findings)
    }

    fn check(&self) -> Result<(), Failure> {
        if self.commit_id.trim().is_empty() {
            return Err(Failure::refused("the findings file has no commit_id"));
        }
        if self.comments.is_empty() {
            return Err(Failure::refused("the findings file has no comments"));
        }
        for (n, c) in self.comments.iter().enumerate() {
            let at = n + 1;
            if c.path.trim().is_empty() || c.body.trim().is_empty() || c.side.trim().is_empty() {
                return Err(Failure::refused(format!(
                    "comment {at} needs a path, a side and a body"
                )));
            }
            if c.start_line.is_some() != c.start_side.is_some() {
                return Err(Failure::refused(format!(
                    "comment {at} needs start_line and start_side together"
                )));
            }
            if c.start_line.is_some_and(|start| start > c.line) {
                return Err(Failure::refused(format!(
                    "comment {at} starts after its last line"
                )));
            }
        }
        if self.body.as_deref().is_some_and(|b| b.trim().is_empty()) {
            return Err(Failure::refused("the review body is empty; leave it out"));
        }
        Ok(())
    }
}

/// Create the pending review on `pr` and check it is `PENDING`.
pub fn run(pr: &str, findings_file: &Path) -> Result<Done, Failure> {
    digits("the PR number", pr).map_err(Failure::refused)?;
    let findings = Findings::read(findings_file)?;
    let repo = repo()?;
    let input = findings_file.display().to_string();
    let review = post_review(
        &format!("/repos/{repo}/pulls/{pr}/reviews"),
        &["--input", &input],
        "creating the pending review",
    )?;
    if !review.is_pending() {
        return Err(Failure::gh_failed(format!(
            "review {} is {}, not PENDING",
            review.id, review.state
        )));
    }
    Ok(Done {
        review,
        comments: Some(findings.comments.len()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<Findings, Failure> {
        let f: Findings = serde_json::from_str(json).unwrap();
        f.check().map(|()| f)
    }

    #[test]
    fn the_pinned_fixture_is_a_valid_findings_file() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/review_post/findings.json");
        let f = Findings::read(&path).unwrap();
        assert_eq!(f.comments.len(), 2);
        assert_eq!(f.comments[1].start_line, Some(20));
    }

    #[test]
    fn a_range_needs_both_ends_and_must_run_forward() {
        let base = |extra: &str| {
            format!(
                r#"{{"commit_id":"a","comments":[{{"path":"p","line":5,"side":"RIGHT","body":"b"{extra}}}]}}"#
            )
        };
        assert!(parse(&base("")).is_ok());
        assert!(parse(&base(r#","start_line":3,"start_side":"RIGHT""#)).is_ok());
        assert!(parse(&base(r#","start_line":3"#)).is_err());
        assert!(parse(&base(r#","start_line":9,"start_side":"RIGHT""#)).is_err());
    }

    #[test]
    fn an_empty_comment_body_or_review_body_is_refused() {
        assert!(parse(
            r#"{"commit_id":"a","comments":[{"path":"p","line":1,"side":"RIGHT","body":" "}]}"#
        )
        .is_err());
        assert!(parse(r#"{"commit_id":"a","body":"","comments":[{"path":"p","line":1,"side":"RIGHT","body":"b"}]}"#).is_err());
    }
}
