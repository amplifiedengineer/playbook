// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! One home for the text rules of a commit message, a PR title and a PR body.
//! Every problem carries a rule id, the place it was found and a sentence, so
//! a caller can list them all at once and fix them in one retry. `deliver::check`
//! formats these findings for its callers.
//!
//! The AI attribution rule is the one the commit-message sanitizer hook uses
//! (`common::attribution`), so every surface refuses the same lines.

use crate::common::{attribution, prose};

/// A title is refused above this many characters, prefix included.
pub const TITLE_LIMIT: usize = 72;

const TYPES: &[&str] = &[
    "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore", "revert",
];

/// One text problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// A stable rule id such as `title-length`, `dash`, `attribution`, `prose`.
    pub rule: &'static str,
    /// Where it was found: `title`, `commit message`, `body:4`.
    pub where_: String,
    pub message: String,
}

impl Problem {
    fn new(rule: &'static str, where_: impl Into<String>, message: impl Into<String>) -> Self {
        Problem {
            rule,
            where_: where_.into(),
            message: message.into(),
        }
    }
}

/// The messages of `problems`, in order.
pub fn messages(problems: Vec<Problem>) -> Vec<String> {
    problems.into_iter().map(|p| p.message).collect()
}

/// Whether `title` is `type(scope)!: description` with a known type.
pub fn is_conventional(title: &str) -> bool {
    let Some((head, rest)) = title.split_once(": ") else {
        return false;
    };
    if rest.trim().is_empty() {
        return false;
    }
    let head = head.strip_suffix('!').unwrap_or(head);
    let kind = match head.split_once('(') {
        Some((kind, scope)) => {
            if !scope.ends_with(')') || scope.len() < 2 {
                return false;
            }
            kind
        }
        None => head,
    };
    TYPES.contains(&kind)
}

/// Em and en dashes outside code, as `(1-based line, dash)`. Lines inside a
/// fenced block and text inside inline backticks are ignored.
fn dash_hits(text: &str) -> Vec<(usize, char)> {
    let mut out = Vec::new();
    let mut fenced = false;
    for (i, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let mut in_code = false;
        for c in line.chars() {
            if c == '`' {
                in_code = !in_code;
            } else if !in_code && (c == '\u{2014}' || c == '\u{2013}') {
                out.push((i + 1, c));
            }
        }
    }
    out
}

/// The word for a dash found by [`dash_hits`].
pub fn dash_name(c: char) -> &'static str {
    if c == '\u{2014}' {
        "em"
    } else {
        "en"
    }
}

fn dash_problems(text: &str, label: &str, place: &str) -> Vec<Problem> {
    dash_hits(text)
        .into_iter()
        .map(|(line, c)| {
            Problem::new(
                "dash",
                format!("{place}:{line}"),
                format!("{label} line {line} has an {} dash", dash_name(c)),
            )
        })
        .collect()
}

fn attribution_problems(text: &str, label: &str, place: &str, is_prose: bool) -> Vec<Problem> {
    let found = if is_prose {
        attribution::prose_problems(text)
    } else {
        attribution::problems(text)
    };
    found
        .into_iter()
        .map(|(line, shape)| {
            Problem::new(
                "attribution",
                format!("{place}:{line}"),
                format!(
                    "{label} line {line} carries AI attribution ({})",
                    shape.name()
                ),
            )
        })
        .collect()
}

/// The title rules: length and conventional form.
pub fn title_problems(title: &str, label: &str, place: &str) -> Vec<Problem> {
    let mut out = Vec::new();
    let length = title.chars().count();
    if length > TITLE_LIMIT {
        out.push(Problem::new(
            "title-length",
            place,
            format!("{label} is {length} characters, the limit is {TITLE_LIMIT}"),
        ));
    }
    if !is_conventional(title) {
        out.push(Problem::new(
            "title-format",
            place,
            format!("{label} is not a conventional commit title (type(scope): description)"),
        ));
    }
    out
}

/// Prose findings of `common::prose`, minus the dash lines that [`dash_hits`]
/// already reports, so a dash is listed once.
fn prose_findings(text: &str, place: &str) -> Vec<Problem> {
    prose::check(text)
        .into_iter()
        .filter(|m| !m.contains("an em or en dash"))
        .map(|m| {
            let line = m
                .strip_prefix("line ")
                .and_then(|r| r.split(':').next())
                .unwrap_or("0");
            Problem::new("prose", format!("{place}:{line}"), format!("{place} {m}"))
        })
        .collect()
}

/// Every problem with a commit message, empty when it is fine. Covers the
/// title, dashes, AI attribution and the writing rules of `common::prose`.
pub fn commit_message(message: &str) -> Vec<Problem> {
    let title = message.lines().next().unwrap_or("").trim();
    if title.is_empty() {
        return vec![Problem::new(
            "title-format",
            "commit message",
            "commit message is empty",
        )];
    }
    let mut out = title_problems(title, "commit title", "commit title");
    out.extend(dash_problems(message, "commit message", "commit message"));
    out.extend(attribution_problems(
        message,
        "commit message",
        "commit message",
        false,
    ));
    out.extend(prose_findings(message, "commit message"));
    out
}

/// Every problem with a PR title, empty when it is fine.
pub fn pr_title(title: &str) -> Vec<Problem> {
    let mut out = title_problems(title, "PR title", "title");
    out.extend(dash_problems(title, "PR title", "title"));
    out.extend(attribution_problems(title, "PR title", "title", true));
    out
}

/// Every problem with a PR body, empty when it is fine.
pub fn pr_body(body: &str) -> Vec<Problem> {
    let mut out = dash_problems(body, "PR body", "body");
    out.extend(attribution_problems(body, "PR body", "body", true));
    out
}

/// The commit type check for a message about to be committed (or already
/// committed) over `files`: `docs` files cannot carry `feat`, and so on.
pub fn commit_type(message: &str, files: &[&str]) -> Option<Problem> {
    crate::commit::typecheck::check(message, files)
        .err()
        .map(|m| Problem::new("commit-type", "commit title", m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(p: &[Problem]) -> Vec<&'static str> {
        p.iter().map(|p| p.rule).collect()
    }

    #[test]
    fn a_title_of_72_characters_passes_and_73_does_not() {
        let ok = format!("feat: {}", "x".repeat(66));
        let long = format!("feat: {}", "x".repeat(67));
        assert_eq!(ok.chars().count(), 72);

        assert!(title_problems(&ok, "t", "title").is_empty());
        assert_eq!(
            rules(&title_problems(&long, "t", "title")),
            vec!["title-length"]
        );
    }

    #[test]
    fn a_title_with_an_en_dash_has_the_dash_rule() {
        let got = pr_title("feat: a \u{2013} b");
        assert_eq!(rules(&got), vec!["dash"]);
        assert_eq!(got[0].where_, "title:1");
    }

    #[test]
    fn a_body_with_a_generated_with_line_has_the_attribution_rule() {
        let got = pr_body("ok\nGenerated with Claude Code");
        assert_eq!(rules(&got), vec!["attribution"]);
        assert_eq!(got[0].where_, "body:2");
    }

    #[test]
    fn a_commit_message_with_a_curly_quote_has_the_prose_rule() {
        let got = commit_message("feat: it\u{2019}s fine");
        assert_eq!(rules(&got), vec!["prose"]);
    }

    #[test]
    fn a_dash_in_a_commit_message_is_listed_once() {
        let got = commit_message("feat: a thing\n\nbecause \u{2014} reasons");
        assert_eq!(rules(&got), vec!["dash"]);
    }

    #[test]
    fn a_docs_only_change_cannot_be_a_feat() {
        let got = commit_type("feat: x", &["README.md"]).expect("refused");
        assert_eq!(got.rule, "commit-type");
        assert!(commit_type("docs: x", &["README.md"]).is_none());
    }

    #[test]
    fn a_dash_inside_code_is_allowed() {
        assert!(pr_body("Use `a \u{2013} b` here\n\n```\nx \u{2014} y\n```\n").is_empty());
    }
}
