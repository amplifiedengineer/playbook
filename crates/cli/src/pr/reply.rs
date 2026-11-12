// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr reply`: post one reply on a pull request, the step
//! `/playbook:pr-comments` used to hand to a `patch-applier` agent
//! with a paragraph about shell quoting. The body goes to GitHub from a file,
//! so quotes, backticks and newlines in a reply need no escaping.

use super::shared::gh;
use crate::common::attribution::sanitize_prose;
use std::path::Path;

/// Where the reply goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A reply in a review thread, named by the `databaseId` of the thread's
    /// first comment.
    Thread(String),
    /// A comment on the pull request itself.
    Issue,
}

/// The `gh api` arguments that post `body_file` to `target` on `pr`.
pub fn api_args(repo: &str, pr: &str, target: &Target, body_file: &Path) -> Vec<String> {
    let endpoint = match target {
        Target::Thread(id) => format!("/repos/{repo}/pulls/{pr}/comments/{id}/replies"),
        Target::Issue => format!("/repos/{repo}/issues/{pr}/comments"),
    };
    [
        "api",
        "-X",
        "POST",
        endpoint.as_str(),
        "-F",
        &format!("body=@{}", body_file.display()),
        "-q",
        ".html_url",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

pub(crate) fn digits(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("{label} must be a number, got {value:?}"));
    }
    Ok(())
}

/// Refuses a reply that GitHub should never see: empty, with an em or en
/// dash, or carrying AI attribution.
pub fn check_body(body: &str) -> Result<(), String> {
    if body.trim().is_empty() {
        return Err("the reply body is empty".to_string());
    }
    if body.contains(['\u{2014}', '\u{2013}']) {
        return Err(
            "the reply contains an em or en dash; use a comma, colon or new sentence".to_string(),
        );
    }
    if !sanitize_prose(body).removed.is_empty() {
        return Err("the reply carries AI attribution; remove it".to_string());
    }
    Ok(())
}

/// What `run` needs.
pub struct Request<'a> {
    pub pr: &'a str,
    pub target: Target,
    pub body_file: &'a Path,
    /// `owner/name`; looked up with `gh repo view` when absent.
    pub repo: Option<&'a str>,
}

/// Post the reply. `Ok` is the posted URL and the body, for the caller to
/// print; `Err` is one plain line.
pub fn run(req: &Request<'_>) -> Result<String, String> {
    digits("--pr", req.pr)?;
    if let Target::Thread(id) = &req.target {
        digits("--thread", id)?;
    }
    let body = std::fs::read_to_string(req.body_file)
        .map_err(|e| format!("could not read {}: {e}", req.body_file.display()))?;
    check_body(&body)?;
    let repo = match req.repo.filter(|r| !r.is_empty()) {
        Some(r) => r.to_string(),
        None => gh(&[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ])?,
    };
    if repo.split('/').count() != 2 || repo.contains(char::is_whitespace) {
        return Err(format!("unexpected repo name: {repo}"));
    }
    let args = api_args(&repo, req.pr, &req.target, req.body_file);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let url = gh(&refs).map_err(|e| format!("posting the reply failed: {e}"))?;
    Ok(format!("Posted: {url}\n{}", body.trim_end()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_reply_posts_to_the_replies_endpoint_from_a_file() {
        let args = api_args(
            "o/r",
            "12",
            &Target::Thread("345".into()),
            Path::new("/t/b.md"),
        );
        assert_eq!(
            args,
            [
                "api",
                "-X",
                "POST",
                "/repos/o/r/pulls/12/comments/345/replies",
                "-F",
                "body=@/t/b.md",
                "-q",
                ".html_url"
            ]
        );
    }

    #[test]
    fn a_pr_level_reply_posts_to_the_issue_comments_endpoint() {
        let args = api_args("o/r", "12", &Target::Issue, Path::new("/t/b.md"));
        assert_eq!(args[3], "/repos/o/r/issues/12/comments");
    }

    #[test]
    fn ids_must_be_numbers() {
        assert!(digits("--pr", "12").is_ok());
        for bad in ["", "1a", "../1", "12 ", "-1"] {
            assert!(digits("--pr", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_body_check_rejects_empty_dashes_and_attribution() {
        assert!(check_body("Fixed in abc1234, thanks.").is_ok());
        assert!(check_body("a `code` span, \"quotes\" and\nnewlines").is_ok());
        assert!(check_body("   \n").is_err());
        assert!(check_body("fixed \u{2014} done")
            .unwrap_err()
            .contains("dash"));
        assert!(check_body("fixed \u{2013} done").is_err());
        assert!(
            check_body("Fixed.\n\nCo-Authored-By: Claude <noreply@anthropic.com>")
                .unwrap_err()
                .contains("attribution")
        );
    }
}
