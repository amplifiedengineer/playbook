// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr review submit`: turn a pending review into a verdict.

use super::{post_review, repo, Done, Failure};
use crate::pr::reply::digits;
use clap::ValueEnum;

/// The review body an approve carries when none is given.
const DEFAULT_APPROVE_BODY: &str = "LGTM";

/// How the review is submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Event {
    Comment,
    Approve,
    RequestChanges,
}

impl Event {
    /// The value GitHub's events endpoint takes.
    fn api_value(self) -> &'static str {
        match self {
            Event::Comment => "COMMENT",
            Event::Approve => "APPROVE",
            Event::RequestChanges => "REQUEST_CHANGES",
        }
    }
}

/// The review body to send: the given text, `LGTM` for a bare approve, or none.
fn body_for(event: Event, body: Option<&str>) -> Option<String> {
    match body.map(str::trim).filter(|b| !b.is_empty()) {
        Some(text) => Some(text.to_string()),
        None => (event == Event::Approve).then(|| DEFAULT_APPROVE_BODY.to_string()),
    }
}

/// Submit review `id` on `pr` and check it left `PENDING`.
pub fn run(pr: &str, id: &str, event: Event, body: Option<&str>) -> Result<Done, Failure> {
    digits("the PR number", pr).map_err(Failure::refused)?;
    digits("the review id", id).map_err(Failure::refused)?;
    let repo = repo()?;
    let event_arg = format!("event={}", event.api_value());
    let mut extra = vec!["-f", event_arg.as_str()];
    let body_arg = body_for(event, body).map(|b| format!("body={b}"));
    if let Some(arg) = &body_arg {
        extra.extend_from_slice(&["-f", arg]);
    }
    let review = post_review(
        &format!("/repos/{repo}/pulls/{pr}/reviews/{id}/events"),
        &extra,
        "submitting the review",
    )?;
    if review.is_pending() {
        return Err(Failure::gh_failed(format!(
            "review {} is still PENDING after the submit",
            review.id
        )));
    }
    Ok(Done {
        review,
        comments: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_approve_says_lgtm_and_other_events_send_no_body() {
        assert_eq!(body_for(Event::Approve, None).as_deref(), Some("LGTM"));
        assert_eq!(
            body_for(Event::Approve, Some("  ")).as_deref(),
            Some("LGTM")
        );
        assert_eq!(body_for(Event::Comment, None), None);
        assert_eq!(
            body_for(Event::RequestChanges, Some("fix")).as_deref(),
            Some("fix")
        );
    }

    #[test]
    fn events_map_to_the_api_words() {
        assert_eq!(Event::Comment.api_value(), "COMMENT");
        assert_eq!(Event::Approve.api_value(), "APPROVE");
        assert_eq!(Event::RequestChanges.api_value(), "REQUEST_CHANGES");
    }
}
