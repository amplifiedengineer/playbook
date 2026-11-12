// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The grounding check of a text a cheap model wrote from a fact list: every
//! number, identifier, path and reference in the text must appear in the
//! facts, and the text must fit the length limit of its kind.
//!
//! The facts live in a file next to the checked file, named `<file>.facts`.
//! An optional first line `kind: <kind>` picks the length limit. The
//! limits are the `LEN-*` rules of the writing-style skill.
//!
//! The check finds invented specifics only. A number written as a word
//! ("three") passes, and a fact the writer got wrong in prose passes.

use super::prose_lint::{mask, Class, Finding};
use super::re::static_regex;
use regex::Regex;
use std::sync::LazyLock;

/// Most sentences in a review comment.
pub const REVIEW_MAX_SENTENCES: usize = 2;
/// Most sentences in a reply to a PR comment.
pub const REPLY_MAX_SENTENCES: usize = 2;
/// Most characters in a commit title.
pub const TITLE_MAX_CHARS: usize = 72;
/// Most characters in a PR body (the length rule's cap).
pub const PR_BODY_MAX_CHARS: usize = 1200;
/// Most sentences in the Summary of a PR body.
pub const SUMMARY_MAX_SENTENCES: usize = 3;
/// Most sentences in a ticket description, outside its bullets.
pub const TICKET_MAX_SENTENCES: usize = 3;
/// Most acceptance bullets in a ticket.
pub const TICKET_MAX_BULLETS: usize = 5;

/// The kinds the length check knows.
pub const KINDS: [&str; 5] = [
    "pr-body",
    "review-comment",
    "reply",
    "commit-message",
    "ticket",
];

/// The facts file split into its kind and its text.
pub struct Facts {
    pub kind: Option<String>,
    pub text: String,
}

/// Reads an optional first line `kind: <kind>`. An unknown kind is an error
/// so a typo does not turn the length check off.
pub fn parse_facts(raw: &str) -> Result<Facts, String> {
    let first = raw.lines().next().unwrap_or("");
    let Some(kind) = first.strip_prefix("kind:") else {
        return Ok(Facts {
            kind: None,
            text: raw.to_string(),
        });
    };
    let kind = kind.trim();
    if !KINDS.contains(&kind) {
        return Err(format!(
            "unknown kind '{kind}' in the facts file. Use one of: {}",
            KINDS.join(", ")
        ));
    }
    Ok(Facts {
        kind: Some(kind.to_string()),
        text: raw[first.len()..].to_string(),
    })
}

static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    static_regex(
        r"`[^`\n]+`|[\w.-]+(?:/[\w.-]+)+|\b[A-Z][A-Z0-9]+-\d+\b|#\d+\b|\b[A-Za-z][A-Za-z0-9]*(?:_[A-Za-z0-9]+)+\b|\b\w+\(\)|\b\d+(?:\.\d+)*\b",
    )
});

static SENTENCE_END: LazyLock<Regex> = LazyLock::new(|| static_regex(r"[.!?](?:\s|$)"));

fn is_number(token: &str) -> bool {
    token.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// "and/or", "read/write" and "CI/CD" match the path shape but are words. A
/// path has a dot or at least two slashes. The check runs on the token after
/// its backticks are trimmed, so `and/or` in a code span is skipped too.
fn is_word_pair(token: &str) -> bool {
    token.contains('/')
        && !token.contains('.')
        && token.matches('/').count() < 2
        && !token.starts_with('/')
}

/// The numbers of a text as whole tokens, so "3" does not match "30".
fn contains_number(facts: &str, number: &str) -> bool {
    TOKEN
        .find_iter(facts)
        .any(|m| is_number(m.as_str()) && m.as_str() == number)
}

/// The tokens of `text` that the facts do not hold.
pub fn unknown_tokens(text: &str, facts: &str) -> Vec<Finding> {
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let list_marker = line
            .trim_start()
            .split_once(". ")
            .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        for (i, m) in TOKEN.find_iter(line).enumerate() {
            let raw = m.as_str();
            if list_marker && i == 0 && is_number(raw) {
                continue;
            }
            let token = raw
                .trim_matches('`')
                .trim_end_matches(['.', ',', ':', ';'])
                .to_string();
            if token.is_empty() || is_word_pair(&token) {
                continue;
            }
            let known = if is_number(&token) {
                contains_number(facts, &token)
            } else {
                facts.contains(&token)
            };
            if !known {
                found.push(Finding {
                    line: index + 1,
                    class: Class::UnknownToken,
                    text: token,
                });
            }
        }
    }
    found
}

fn sentences(text: &str) -> usize {
    let masked = mask(text)
        .replace("e.g.", "eg")
        .replace("i.e.", "ie")
        .replace("vs.", "vs");
    let ends = SENTENCE_END.find_iter(&masked).count();
    let tail = masked.trim_end();
    let open = !tail.is_empty() && !tail.ends_with(['.', '!', '?']);
    ends + usize::from(open)
}

fn over(line: usize, what: &str, got: usize, max: usize) -> Finding {
    Finding {
        line,
        class: Class::OverLimit,
        text: format!("{what} {got}, limit {max}"),
    }
}

fn prose_lines(text: &str) -> String {
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("- ") && !t.starts_with("* ") && !t.starts_with('#')
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn summary(text: &str) -> String {
    let mut inside = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with("## ") {
            inside = line.trim() == "## Summary";
        } else if inside {
            out.push(line);
        }
    }
    out.join("\n")
}

/// The length findings of `text` for `kind` (one of `KINDS`).
pub fn length_findings(kind: &str, text: &str) -> Vec<Finding> {
    let mut found = Vec::new();
    let chars = text.chars().count();
    match kind {
        "review-comment" => {
            let n = sentences(text);
            if n > REVIEW_MAX_SENTENCES {
                found.push(over(1, "sentences", n, REVIEW_MAX_SENTENCES));
            }
        }
        "reply" => {
            let n = sentences(text);
            if n > REPLY_MAX_SENTENCES {
                found.push(over(1, "sentences", n, REPLY_MAX_SENTENCES));
            }
        }
        "commit-message" => {
            let title = text.lines().next().unwrap_or("").chars().count();
            if title > TITLE_MAX_CHARS {
                found.push(over(1, "title characters", title, TITLE_MAX_CHARS));
            }
        }
        "pr-body" => {
            if chars > PR_BODY_MAX_CHARS {
                found.push(over(1, "characters", chars, PR_BODY_MAX_CHARS));
            }
            let n = sentences(&summary(text));
            if n > SUMMARY_MAX_SENTENCES {
                found.push(over(1, "summary sentences", n, SUMMARY_MAX_SENTENCES));
            }
        }
        "ticket" => {
            let n = sentences(&prose_lines(text));
            if n > TICKET_MAX_SENTENCES {
                found.push(over(1, "sentences", n, TICKET_MAX_SENTENCES));
            }
            let bullets = text
                .lines()
                .filter(|l| l.trim_start().starts_with("- ") || l.trim_start().starts_with("* "))
                .count();
            if bullets > TICKET_MAX_BULLETS {
                found.push(over(1, "bullets", bullets, TICKET_MAX_BULLETS));
            }
        }
        _ => {}
    }
    found
}

/// Every grounding finding of `text` against the facts file content.
pub fn check(text: &str, raw_facts: &str) -> Result<Vec<Finding>, String> {
    let facts = parse_facts(raw_facts)?;
    let mut found = unknown_tokens(text, &facts.text);
    if let Some(kind) = &facts.kind {
        found.extend(length_findings(kind, text));
    }
    found.sort_by(|a, b| (a.line, a.text.as_str()).cmp(&(b.line, b.text.as_str())));
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(text: &str, facts: &str) -> Vec<String> {
        unknown_tokens(text, facts)
            .into_iter()
            .map(|f| f.text)
            .collect()
    }

    #[test]
    fn a_number_the_facts_do_not_hold_is_unknown() {
        let got = tokens("The job retries 5 times.", "Retry up to 3 times.");

        assert_eq!(got, vec!["5"]);
    }

    #[test]
    fn a_number_matches_whole_only() {
        let got = tokens("It waits 3 seconds.", "It waits 30 seconds.");

        assert_eq!(got, vec!["3"]);
    }

    #[test]
    fn a_known_number_identifier_path_and_reference_pass() {
        let text =
            "Closes #41. `MAX_QUEUE_RETRIES` is 3 in src/sync/job.rs for PLAT-212 and retry_count.";
        let facts = "MAX_QUEUE_RETRIES = 3\nsrc/sync/job.rs\nPLAT-212\n#41\nretry_count";

        assert!(tokens(text, facts).is_empty(), "{:?}", tokens(text, facts));
    }

    #[test]
    fn an_invented_identifier_path_ticket_and_call_are_unknown() {
        let text = "Rename `foo_bar` in lib/x.rs, see OPS-9, call run().";

        let got = tokens(text, "nothing relevant");

        assert_eq!(got, vec!["foo_bar", "lib/x.rs", "OPS-9", "run()"]);
    }

    #[test]
    fn a_word_pair_with_a_slash_is_not_a_path() {
        let got = tokens("Read and/or write, then check CI/CD.", "nothing");

        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn trailing_punctuation_does_not_make_a_token_unknown() {
        let got = tokens("See src/a.rs.", "src/a.rs");

        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn an_ordered_list_marker_is_not_a_number() {
        let got = tokens(
            "1. Add the field.\n2. Run the test.",
            "Add the field. Run the test.",
        );

        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn plain_words_and_number_words_pass() {
        let got = tokens("The job retries three times.", "Retry up to 3 times.");

        assert!(got.is_empty());
    }

    #[test]
    fn a_finding_names_its_line() {
        let found = unknown_tokens("Fine.\nThe cap is 9.", "nothing");

        assert_eq!(found[0].line, 2);
        assert_eq!(found[0].class, Class::UnknownToken);
    }

    #[test]
    fn the_kind_line_is_read_and_an_unknown_kind_is_an_error() {
        let ok = parse_facts("kind: reply\nfacts").unwrap();
        let none = parse_facts("facts only").unwrap();
        let bad = parse_facts("kind: essay\nfacts");

        assert_eq!(ok.kind.as_deref(), Some("reply"));
        assert_eq!(none.kind, None);
        assert!(bad.err().is_some_and(|e| e.contains("unknown kind")));
    }

    #[test]
    fn a_reply_with_three_sentences_is_over_the_limit() {
        let long = "Done. I fixed it. It works now.";

        assert_eq!(length_findings("reply", long).len(), 1);
        assert!(length_findings("reply", "Done. Added the check.").is_empty());
        assert!(length_findings("reply", "Fixed").is_empty());
    }

    #[test]
    fn a_review_comment_counts_sentences_and_skips_code() {
        let ok = "issue (blocking): this reads `a.b()` twice. It races.";
        let long = "issue: one. Two. Three.";

        assert!(length_findings("review-comment", ok).is_empty());
        assert_eq!(length_findings("review-comment", long).len(), 1);
    }

    #[test]
    fn a_commit_title_over_72_characters_is_over_the_limit() {
        let long = format!("feat: {}", "x".repeat(70));

        assert_eq!(length_findings("commit-message", &long).len(), 1);
        assert!(length_findings(
            "commit-message",
            "fix: x\n\nA long body line. Another. More."
        )
        .is_empty());
    }

    #[test]
    fn a_pr_body_is_checked_for_size_and_summary_sentences() {
        let big = format!("## Summary\n\nOne.\n\n## Notes\n\n{}", "x".repeat(1200));
        let wordy = "## Summary\n\nOne. Two. Three. Four.\n\n## Notes for reviewers\n\nOne. Two. Three. Four.";

        let a = length_findings("pr-body", &big);
        let b = length_findings("pr-body", wordy);

        assert_eq!(a.len(), 1);
        assert!(a[0].text.contains("characters"));
        assert_eq!(b.len(), 1);
        assert!(b[0].text.contains("summary sentences"));
    }

    #[test]
    fn a_ticket_limits_sentences_outside_bullets_and_the_bullet_count() {
        let wordy = "One. Two. Three. Four.\n\n- a\n- b";
        let bullets = "One.\n\n- a\n- b\n- c\n- d\n- e\n- f";

        assert_eq!(length_findings("ticket", wordy).len(), 1);
        assert_eq!(length_findings("ticket", bullets).len(), 1);
        assert!(length_findings("ticket", "One. Two.\n\n- a\n- b").is_empty());
    }

    #[test]
    fn check_joins_tokens_and_length() {
        let got = check("Fixed 7. Two. Three.", "kind: reply\nfacts").unwrap();

        assert_eq!(got.len(), 2);
    }
}
