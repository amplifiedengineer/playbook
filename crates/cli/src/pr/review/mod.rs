// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr review`: the triage of a PR and the pending review a
//! self-review posts and submits.

pub mod post;
pub mod submit;
pub mod triage;

use crate::json::envelope;
use crate::pr::shared::gh;
use serde::Deserialize;
use serde_json::{json, Value};

/// Exit codes of the shared table that a review call uses.
pub mod code {
    pub const OK: i32 = 0;
    pub const REFUSED: i32 = 1;
    pub const GH_FAILED: i32 = 5;
}

/// The state of a review that is not yet submitted.
pub const PENDING: &str = "PENDING";

/// The jq filter that trims a review reply to the fields read back.
const REVIEW_FIELDS: &str = "{id, state, html_url}";

/// Why a review call stopped, with the exit code plain mode uses.
#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: i32,
    pub error: String,
}

impl Failure {
    pub fn refused(error: impl Into<String>) -> Failure {
        Failure {
            code: code::REFUSED,
            error: error.into(),
        }
    }

    pub fn gh_failed(error: impl Into<String>) -> Failure {
        Failure {
            code: code::GH_FAILED,
            error: error.into(),
        }
    }
}

/// A review as GitHub reports it.
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct Review {
    pub id: u64,
    pub state: String,
    pub html_url: String,
}

impl Review {
    fn parse(reply: &str) -> Result<Review, Failure> {
        serde_json::from_str(reply)
            .map_err(|e| Failure::gh_failed(format!("could not read the review reply: {e}")))
    }

    pub fn is_pending(&self) -> bool {
        self.state == PENDING
    }
}

/// What a call reports on success.
#[derive(Debug, PartialEq, Eq)]
pub struct Done {
    pub review: Review,
    /// Inline comments in the review, when the call created it.
    pub comments: Option<usize>,
}

impl Done {
    pub fn render(&self) -> String {
        format!(
            "id={}\nstate={}\nurl={}",
            self.review.id, self.review.state, self.review.html_url
        )
    }

    fn fields(&self) -> Value {
        let mut fields = json!({
            "id": self.review.id,
            "state": self.review.state,
            "url": self.review.html_url,
        });
        if let Some(count) = self.comments {
            fields["comments"] = json!(count);
        }
        fields
    }
}

/// The shared envelope for either outcome.
pub fn to_envelope(result: &Result<Done, Failure>) -> Value {
    match result {
        Ok(done) => envelope::wrap(code::OK, "", done.fields()),
        Err(f) => envelope::wrap(f.code, &f.error, Value::Null),
    }
}

/// The `owner/name` of the current repo.
fn repo() -> Result<String, Failure> {
    let name = gh(&[
        "repo",
        "view",
        "--json",
        "nameWithOwner",
        "-q",
        ".nameWithOwner",
    ])
    .map_err(|e| Failure::gh_failed(format!("could not read the repo name: {e}")))?;
    if name.split('/').count() != 2 || name.contains(char::is_whitespace) {
        return Err(Failure::gh_failed(format!("unexpected repo name: {name}")));
    }
    Ok(name)
}

/// Runs `gh api -X POST <endpoint> <extra>` and reads the review it returns.
fn post_review(endpoint: &str, extra: &[&str], what: &str) -> Result<Review, Failure> {
    let mut args = vec!["api", "-X", "POST", endpoint];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--jq", REVIEW_FIELDS]);
    let reply = gh(&args).map_err(|e| Failure::gh_failed(format!("{what} failed: {e}")))?;
    Review::parse(&reply)
}
