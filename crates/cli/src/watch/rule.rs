// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A watch rule: one JSON object that says which event to match, how to
//! filter it by metadata, and which built in action to take. There is no field
//! for a shell command or an API path, and unknown fields are an error.

use std::fmt;

use serde::Deserialize;
use serde_json::Value;

/// How much freedom a rule has. The order is the order of freedom, so the
/// lowest of two levels is `min`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Autonomy {
    Off,
    Propose,
    Auto,
}

/// The events a rule can match. The probe kinds (`pr.updated`, `pr.reviewed`,
/// `pr.checks_green`) join with the first rule that needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum On {
    #[serde(rename = "issue.opened")]
    IssueOpened,
    #[serde(rename = "issue.commented")]
    IssueCommented,
    #[serde(rename = "pr.opened")]
    PrOpened,
    #[serde(rename = "pr.commented")]
    PrCommented,
}

/// The built in actions. More join one per rule delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Do {
    Label,
    Comment,
    /// A model picks one label from `with.labels`. It earns `auto` by the
    /// trust ladder.
    TriageIssue,
    /// A model with tools edits a branch for a comment, in a worktree. Never
    /// runs on its own.
    AddressComment,
    /// Merges the base branch into a pull request's branch through the API.
    /// It earns `auto` by the trust ladder.
    UpdateBranch,
    /// A model reads the diff and the result is saved as a pending review for
    /// you to submit. Never runs on its own.
    ReviewPr,
    /// Merges a pull request that is green and has a human approval, when
    /// `pr.merge.enabled` is on. Never runs on its own.
    MergeWhenGreen,
}

impl Do {
    pub const ALL: [Do; 7] = [
        Do::Label,
        Do::Comment,
        Do::TriageIssue,
        Do::AddressComment,
        Do::UpdateBranch,
        Do::ReviewPr,
        Do::MergeWhenGreen,
    ];

    /// The name the rule file and the audit log use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Label => "label",
            Self::Comment => "comment",
            Self::TriageIssue => "triage_issue",
            Self::AddressComment => "address_comment",
            Self::UpdateBranch => "update_branch",
            Self::ReviewPr => "review_pr",
            Self::MergeWhenGreen => "merge_when_green",
        }
    }

    /// The most freedom the action can have, whatever the rule asks for.
    /// Code written by a model has no ladder to climb in this release.
    pub fn highest(self) -> Autonomy {
        match self {
            Self::AddressComment | Self::ReviewPr | Self::MergeWhenGreen => Autonomy::Propose,
            _ => Autonomy::Auto,
        }
    }

    /// Whether the action may run on its own with no track record: it is
    /// reversible or fixed text, and carries nothing a model wrote.
    pub fn runs_free(self) -> bool {
        matches!(self, Self::Label | Self::Comment)
    }
}

/// Who decides: only fixed rules for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decide {
    #[default]
    Rules,
    /// A model picks one label from `with.labels`, three calls that must agree.
    Model,
}

/// Filters on event metadata. Text is never matched, only an exact comment
/// prefix.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Where {
    #[serde(default)]
    pub labels_any: Vec<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    pub base: Option<String>,
    pub comment_prefix: Option<String>,
    /// Act on pull requests from forks. Off unless the rule says so.
    #[serde(default)]
    pub allow_forks: bool,
}

/// The arguments of the action.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct With {
    #[serde(default)]
    pub labels: Vec<String>,
    /// Fixed text for the `comment` action.
    #[serde(default)]
    pub text: Option<String>,
}

/// Per day caps for one rule.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "default_actions_per_day")]
    pub actions_per_day: i64,
    /// Model spend. Zero means a rule that calls a model needs a number set.
    #[serde(default)]
    pub usd_per_day: f64,
    /// Most files a change may touch. Lowers `fix.maxFiles`, never raises it.
    #[serde(default)]
    pub files: Option<i64>,
    /// Most lines a change may touch. Lowers `fix.maxLines`, never raises it.
    #[serde(default)]
    pub lines: Option<i64>,
}

fn default_actions_per_day() -> i64 {
    20
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            actions_per_day: default_actions_per_day(),
            usd_per_day: 0.0,
            files: None,
            lines: None,
        }
    }
}

/// A complete rule.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub on: On,
    #[serde(default, rename = "where")]
    pub filter: Where,
    #[serde(rename = "do")]
    pub action: Do,
    #[serde(default)]
    pub with: With,
    #[serde(default)]
    pub decide: Decide,
    #[serde(default = "propose")]
    pub autonomy: Autonomy,
    #[serde(default)]
    pub limits: Limits,
}

fn propose() -> Autonomy {
    Autonomy::Propose
}

/// What a stored rule value means at one tier.
#[derive(Debug, Clone, PartialEq)]
pub enum Stored {
    /// `{"autonomy":"off"}` alone: switches an inherited rule off at this tier.
    Tombstone,
    Rule(Box<Rule>),
}

/// Why a rule value was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleError(String);

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RuleError {}

/// The longest rule text read. The config store flattens objects, so every rule
/// is stored as text and checked here on each read.
const MAX_RULE_BYTES: usize = 8 * 1024;
/// The most labels one rule can add, and the longest one (the GitHub limit).
const MAX_LABELS: usize = 20;
const MAX_LABEL_CHARS: usize = 50;
/// The most entries in one filter list, and the longest filter text.
const MAX_FILTER_ENTRIES: usize = 50;
const MAX_FILTER_CHARS: usize = 200;
/// The longest rule id.
const MAX_RULE_ID: usize = 64;

impl Stored {
    /// Parses the JSON text of one rule.
    pub fn parse(text: &str) -> Result<Self, RuleError> {
        if text.len() > MAX_RULE_BYTES {
            return Err(RuleError(format!(
                "the rule is longer than {MAX_RULE_BYTES} bytes"
            )));
        }
        let value: Value = serde_json::from_str(text)
            .map_err(|e| RuleError(format!("the rule is not valid JSON: {e}")))?;
        if is_tombstone(&value) {
            return Ok(Self::Tombstone);
        }
        let rule: Rule =
            serde_json::from_value(value).map_err(|e| RuleError(format!("invalid rule: {e}")))?;
        match rule.action {
            Do::TriageIssue if rule.decide != Decide::Model => {
                return Err(RuleError(
                    "the triage_issue action needs decide set to model".to_string(),
                ));
            }
            Do::UpdateBranch | Do::ReviewPr
                if !matches!(rule.on, On::PrOpened | On::PrCommented) =>
            {
                return Err(RuleError(
                    "the update_branch and review_pr actions need on set to pr.opened or pr.commented"
                        .to_string(),
                ));
            }
            Do::MergeWhenGreen
                if rule.on != On::PrCommented || rule.filter.comment_prefix.is_none() =>
            {
                return Err(RuleError(
                    "the merge_when_green action needs on set to pr.commented and a where.comment_prefix"
                        .to_string(),
                ));
            }
            Do::ReviewPr if rule.limits.usd_per_day <= 0.0 => {
                return Err(RuleError(
                    "the review_pr action calls a model and needs limits.usd_per_day above 0"
                        .to_string(),
                ));
            }
            Do::AddressComment
                if rule.on != On::PrCommented || rule.filter.comment_prefix.is_none() =>
            {
                return Err(RuleError(
                    "the address_comment action needs on set to pr.commented and a where.comment_prefix"
                        .to_string(),
                ));
            }
            Do::AddressComment if rule.limits.usd_per_day <= 0.0 => {
                return Err(RuleError(
                    "the address_comment action calls a model and needs limits.usd_per_day above 0"
                        .to_string(),
                ));
            }
            Do::Label | Do::TriageIssue if rule.with.labels.is_empty() => {
                return Err(RuleError(
                    "the label and triage_issue actions need at least one entry in with.labels"
                        .to_string(),
                ));
            }
            Do::Comment
                if rule
                    .with
                    .text
                    .as_deref()
                    .is_none_or(|t| t.trim().is_empty()) =>
            {
                return Err(RuleError(
                    "the comment action needs the text to post in with.text".to_string(),
                ));
            }
            _ => {}
        }
        check_text(&rule)?;
        Ok(Self::Rule(Box::new(rule)))
    }
}

/// Refuses entries that could never match or that GitHub would reject, so a
/// typo fails when the rule is read and not when an event arrives.
fn check_text(rule: &Rule) -> Result<(), RuleError> {
    let labels = &rule.with.labels;
    if labels.len() > MAX_LABELS {
        return Err(RuleError(format!(
            "with.labels holds more than {MAX_LABELS} entries"
        )));
    }
    for label in labels {
        if label.trim().is_empty() || label.chars().count() > MAX_LABEL_CHARS {
            return Err(RuleError(format!(
                "a label must have 1 to {MAX_LABEL_CHARS} characters"
            )));
        }
        if label.chars().any(char::is_control) || label.trim() != label {
            return Err(RuleError(
                "a label cannot have control characters or edge spaces".to_string(),
            ));
        }
    }
    let f = &rule.filter;
    for (name, list) in [
        ("labels_any", &f.labels_any),
        ("authors", &f.authors),
        ("paths", &f.paths),
    ] {
        if list.len() > MAX_FILTER_ENTRIES {
            return Err(RuleError(format!(
                "where.{name} holds more than {MAX_FILTER_ENTRIES} entries"
            )));
        }
        if list.iter().any(|e| bad_filter_text(e)) {
            return Err(RuleError(format!(
                "where.{name} has an empty or too long entry"
            )));
        }
    }
    for (name, one) in [("base", &f.base), ("comment_prefix", &f.comment_prefix)] {
        if one.as_deref().is_some_and(bad_filter_text) {
            return Err(RuleError(format!(
                "where.{name} is empty or too long, and an empty prefix would match every comment"
            )));
        }
    }
    Ok(())
}

fn bad_filter_text(text: &str) -> bool {
    text.is_empty() || text.chars().count() > MAX_FILTER_CHARS
}

fn is_tombstone(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.len() == 1 && o.get("autonomy").and_then(Value::as_str) == Some("off"))
}

/// Whether `id` can name a rule: lowercase letters, digits and hyphens, not
/// empty and not starting with a hyphen.
pub fn is_rule_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_RULE_ID
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const LABEL: &str = r#"{"on":"issue.opened","do":"label","with":{"labels":["triage"]}}"#;

    #[test]
    fn a_valid_rule_parses_with_the_defaults() {
        // Arrange / Act
        let stored = Stored::parse(LABEL).unwrap();
        // Assert
        let Stored::Rule(rule) = stored else {
            panic!("expected a rule");
        };
        assert_eq!(rule.on, On::IssueOpened);
        assert_eq!(rule.autonomy, Autonomy::Propose);
        assert_eq!(rule.decide, Decide::Rules);
        assert_eq!(rule.with.labels, vec!["triage".to_string()]);
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let text = r#"{"on":"issue.opened","do":"label","with":{"labels":["a"]},"run":"rm -rf"}"#;
        assert!(Stored::parse(text).is_err());
    }

    #[test]
    fn an_unknown_field_inside_where_is_refused() {
        let text =
            r#"{"on":"issue.opened","do":"label","with":{"labels":["a"]},"where":{"title":"x"}}"#;
        assert!(Stored::parse(text).is_err());
    }

    #[test]
    fn a_bad_event_or_action_is_refused() {
        assert!(Stored::parse(r#"{"on":"push","do":"label"}"#).is_err());
        assert!(Stored::parse(r#"{"on":"issue.opened","do":"shell"}"#).is_err());
    }

    #[test]
    fn an_autonomy_outside_the_three_values_is_refused() {
        let text =
            r#"{"on":"issue.opened","do":"label","with":{"labels":["a"]},"autonomy":"yolo"}"#;
        assert!(Stored::parse(text).is_err());
    }

    #[test]
    fn auto_is_accepted_and_the_limits_default() {
        // Arrange
        let text =
            r#"{"on":"issue.opened","do":"label","with":{"labels":["a"]},"autonomy":"auto"}"#;

        // Act
        let Stored::Rule(rule) = Stored::parse(text).unwrap() else {
            panic!("expected a rule");
        };

        // Assert
        assert_eq!(rule.autonomy, Autonomy::Auto);
        assert_eq!(rule.limits, Limits::default());
        assert_eq!(rule.limits.actions_per_day, 20);
    }

    #[test]
    fn limits_can_be_set_and_unknown_limit_fields_are_refused() {
        let ok = r#"{"on":"issue.opened","do":"label","with":{"labels":["a"]},"limits":{"actions_per_day":3,"usd_per_day":1.5}}"#;
        let Stored::Rule(rule) = Stored::parse(ok).unwrap() else {
            panic!("expected a rule");
        };
        assert_eq!(
            (rule.limits.actions_per_day, rule.limits.usd_per_day),
            (3, 1.5)
        );
        let bad =
            r#"{"on":"issue.opened","do":"label","with":{"labels":["a"]},"limits":{"words":3}}"#;
        assert!(Stored::parse(bad).is_err());
    }

    #[test]
    fn a_comment_rule_needs_text() {
        let ok = r#"{"on":"issue.opened","do":"comment","with":{"text":"Thanks"}}"#;
        assert!(Stored::parse(ok).is_ok());
        assert!(Stored::parse(r#"{"on":"issue.opened","do":"comment"}"#).is_err());
        assert!(
            Stored::parse(r#"{"on":"issue.opened","do":"comment","with":{"text":" "}}"#).is_err()
        );
    }

    #[test]
    fn the_short_off_form_is_a_tombstone() {
        assert_eq!(
            Stored::parse(r#"{"autonomy":"off"}"#).unwrap(),
            Stored::Tombstone
        );
    }

    #[test]
    fn the_short_form_must_be_alone() {
        assert!(Stored::parse(r#"{"autonomy":"off","do":"label"}"#).is_err());
    }

    #[test]
    fn a_label_rule_without_labels_is_refused() {
        assert!(Stored::parse(r#"{"on":"issue.opened","do":"label"}"#).is_err());
    }

    #[test]
    fn text_that_is_not_json_is_refused() {
        assert!(Stored::parse("not json").is_err());
    }

    fn with_labels(labels: &str) -> String {
        format!(r#"{{"on":"issue.opened","do":"label","with":{{"labels":{labels}}}}}"#)
    }

    #[test]
    fn a_blank_padded_control_or_long_label_is_refused() {
        for bad in [r#"[""]"#, r#"["  "]"#, r#"[" x"]"#, r#"["a\nb"]"#] {
            assert!(Stored::parse(&with_labels(bad)).is_err(), "{bad}");
        }
        let long = format!(r#"["{}"]"#, "x".repeat(51));
        assert!(Stored::parse(&with_labels(&long)).is_err());
        let fifty = format!(r#"["{}"]"#, "x".repeat(50));
        assert!(Stored::parse(&with_labels(&fifty)).is_ok());
    }

    #[test]
    fn too_many_labels_are_refused() {
        let many = format!("[{}]", vec![r#""a""#; 21].join(","));
        assert!(Stored::parse(&with_labels(&many)).is_err());
    }

    #[test]
    fn an_empty_comment_prefix_is_refused_because_it_matches_every_comment() {
        let text = r#"{"on":"issue.commented","do":"label","with":{"labels":["a"]},"where":{"comment_prefix":""}}"#;
        assert!(Stored::parse(text).is_err());
    }

    #[test]
    fn an_empty_filter_entry_is_refused() {
        let text =
            r#"{"on":"pr.opened","do":"label","with":{"labels":["a"]},"where":{"authors":[""]}}"#;
        assert!(Stored::parse(text).is_err());
    }

    #[test]
    fn a_rule_text_over_the_size_limit_is_refused() {
        let text = format!("{LABEL}{}", " ".repeat(MAX_RULE_BYTES));
        assert!(Stored::parse(&text).is_err());
    }

    #[test]
    fn rule_ids_are_lowercase_words() {
        assert!(is_rule_id("triage-issue"));
        assert!(is_rule_id("a1"));
        assert!(!is_rule_id("Triage"));
        assert!(!is_rule_id(""));
        assert!(!is_rule_id("-x"));
        assert!(!is_rule_id("a.b"));
        assert!(!is_rule_id(&"a".repeat(65)));
    }
}
