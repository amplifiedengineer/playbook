// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr status`: the settings, run mode, PR state and review
//! choice the finish flow needs, read in one call.
//!
//! The review choice follows the rules the finish skill applies today: `skip`
//! only when `pr.review.enabled` reads `false`; a read error on
//! `pr.review.enabled` or `pr.review.type` gives `deep`; `type=auto` runs
//! the triage and maps any triage failure to `deep`.

use crate::config;
use crate::json::envelope;
use crate::pr::triage;
use serde_json::{json, Value};

pub const AUTO_REVIEW_ENABLED: &str = "pr.review.enabled";
pub const AUTO_REVIEW_TYPE: &str = "pr.review.type";
pub const AUTO_REVIEW_FIX: &str = "pr.review.fix";
pub const AUTO_MERGE_ENABLED: &str = "pr.merge.enabled";

/// The review type that asks the triage to choose.
const TYPE_AUTO: &str = "auto";
const TYPE_QUICK: &str = "quick";
const TYPE_DEEP: &str = "deep";

/// The code a failed `gh pr view` reports, as `deliver::exit` uses it.
pub const CODE_PR_VIEW: i32 = 5;
/// The code a bad `--dir` reports, as plain mode's exit 1.
pub const CODE_BAD_INPUT: i32 = 1;

/// The code of a successful read, as plain mode exits.
const CODE_OK: i32 = 0;
/// What `--json` exits with, whatever the outcome.
pub const JSON_EXIT: i32 = 0;

/// One setting as the report shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub value: Value,
    pub source: String,
    pub error: Option<String>,
}

impl Setting {
    fn to_json(&self) -> Value {
        json!({"value": self.value, "source": self.source, "error": self.error})
    }

    fn read_error(error: String) -> Setting {
        Setting {
            value: Value::Null,
            source: String::new(),
            error: Some(error),
        }
    }
}

/// Where the settings come from: the config resolver, or a fake in tests.
pub trait SettingsReader {
    /// The value and the tier name that supplied it, or the read error.
    fn read(&self, key: &str) -> Result<(Value, String), String>;
}

/// The `gh` reads the context needs.
pub trait StatusGh {
    /// `gh pr view <pr> --json number,url,isDraft,headRefOid,baseRefName`.
    fn pr_view(&self, pr: u64) -> Result<String, String>;
    /// The numbers of the open PRs whose head branch is `branch`.
    fn open_pr_numbers_for_head(&self, branch: &str) -> Result<Vec<u64>, String>;
}

/// The triage the `auto` review type runs.
pub trait Triager {
    fn review(&self, pr: u64) -> Result<triage::Review, String>;
}

/// What the review decision settles on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Skip,
    Quick,
    Deep,
}

impl Choice {
    pub fn as_str(self) -> &'static str {
        match self {
            Choice::Skip => "skip",
            Choice::Quick => "quick",
            Choice::Deep => "deep",
        }
    }
}

/// A failed run: the envelope code and the error text.
#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: i32,
    pub error: String,
}

impl Failure {
    pub fn bad_input(error: String) -> Failure {
        Failure {
            code: CODE_BAD_INPUT,
            error,
        }
    }
}

/// Reads a boolean setting. An unreadable or non boolean value becomes
/// `false` with the error text kept.
fn bool_setting(reader: &dyn SettingsReader, key: &str) -> Setting {
    match reader.read(key) {
        Ok((Value::Bool(value), source)) => Setting {
            value: Value::Bool(value),
            source,
            error: None,
        },
        Ok((other, source)) => Setting {
            value: Value::Bool(false),
            source,
            error: Some(format!("{key} is {other}, not a boolean")),
        },
        Err(error) => Setting {
            value: Value::Bool(false),
            source: String::new(),
            error: Some(error),
        },
    }
}

/// Reads a string setting. An unreadable or non string value is an error.
fn string_setting(reader: &dyn SettingsReader, key: &str) -> Setting {
    match reader.read(key) {
        Ok((Value::String(value), source)) => Setting {
            value: Value::String(value),
            source,
            error: None,
        },
        Ok((other, _)) => Setting::read_error(format!("{key} is {other}, not a string")),
        Err(error) => Setting::read_error(error),
    }
}

/// The review choice and the reason for it.
pub fn decide(
    enabled: &Setting,
    kind: &Setting,
    triager: &dyn Triager,
    pr: u64,
) -> (Choice, String) {
    if let Some(error) = &enabled.error {
        return (
            Choice::Deep,
            format!("{AUTO_REVIEW_ENABLED} could not be read ({error}), so the review is deep"),
        );
    }
    if enabled.value == Value::Bool(false) {
        return (Choice::Skip, format!("{AUTO_REVIEW_ENABLED} is false"));
    }
    if let Some(error) = &kind.error {
        return (
            Choice::Deep,
            format!("{AUTO_REVIEW_TYPE} could not be read ({error}), so the review is deep"),
        );
    }
    match kind.value.as_str().unwrap_or_default() {
        TYPE_DEEP => (Choice::Deep, format!("{AUTO_REVIEW_TYPE} is deep")),
        TYPE_QUICK => (Choice::Quick, format!("{AUTO_REVIEW_TYPE} is quick")),
        TYPE_AUTO => match triager.review(pr) {
            Ok(triage::Review::Quick) => (Choice::Quick, "the triage chose quick".to_string()),
            Ok(triage::Review::Deep) => (Choice::Deep, "the triage chose deep".to_string()),
            Err(error) => (
                Choice::Deep,
                format!("the triage failed ({error}), so the review is deep"),
            ),
        },
        other => (
            Choice::Deep,
            format!("{AUTO_REVIEW_TYPE} is '{other}', which is not a review type, so the review is deep"),
        ),
    }
}

/// The `pr` object from the `gh pr view` JSON.
fn pr_object(raw: &str, stacked_on: Option<u64>) -> Result<Value, String> {
    let view: Value =
        serde_json::from_str(raw).map_err(|e| format!("gh pr view returned bad JSON: {e}"))?;
    let text = |key: &str| view[key].as_str().unwrap_or_default().to_string();
    Ok(json!({
        "number": view["number"],
        "url": text("url"),
        "isDraft": view["isDraft"].as_bool().unwrap_or(false),
        "head": text("headRefOid"),
        "base": text("baseRefName"),
        "stacked_on": stacked_on,
    }))
}

/// The `settings` object: each setting under its name and, until 1.0.0,
/// under the names it had in 0.42.0, so a reader of the old names keeps working.
fn settings_json<const N: usize>(settings: [(&str, &Setting); N]) -> Value {
    let mut out = serde_json::Map::new();
    for (key, setting) in settings {
        let old = config::keys::old_names(key).map(str::to_string);
        for name in std::iter::once(key.to_string()).chain(old) {
            out.insert(name, setting.to_json());
        }
    }
    Value::Object(out)
}

/// Reads everything the finish flow needs for PR `pr`. `mode` is the
/// `mode` object, resolved by the caller. A stacked lookup that fails leaves
/// `stacked_on` null: the land step checks the base itself.
pub fn collect(
    pr: u64,
    mode: Value,
    gh: &dyn StatusGh,
    reader: &dyn SettingsReader,
    triager: &dyn Triager,
) -> Result<Value, Failure> {
    let raw = gh.pr_view(pr).map_err(|error| Failure {
        code: CODE_PR_VIEW,
        error,
    })?;
    let base = serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| v["baseRefName"].as_str().map(str::to_string))
        .unwrap_or_default();
    let stacked_on = if base.is_empty() {
        None
    } else {
        gh.open_pr_numbers_for_head(&base)
            .ok()
            .and_then(|numbers| numbers.into_iter().find(|n| *n != pr))
    };
    let pr_json = pr_object(&raw, stacked_on).map_err(|error| Failure {
        code: CODE_PR_VIEW,
        error,
    })?;
    let enabled = bool_setting(reader, AUTO_REVIEW_ENABLED);
    let kind = string_setting(reader, AUTO_REVIEW_TYPE);
    let fix = bool_setting(reader, AUTO_REVIEW_FIX);
    let merge = bool_setting(reader, AUTO_MERGE_ENABLED);
    let (choice, reason) = decide(&enabled, &kind, triager, pr);
    Ok(json!({
        "pr": pr_json,
        "mode": mode,
        "settings": settings_json([
            (AUTO_REVIEW_ENABLED, &enabled),
            (AUTO_REVIEW_TYPE, &kind),
            (AUTO_REVIEW_FIX, &fix),
            (AUTO_MERGE_ENABLED, &merge),
        ]),
        "review": choice.as_str(),
        "review_reason": reason,
    }))
}

/// Prints the `--json` envelope for `result` and returns the exit code (0).
pub fn emit(result: Result<Value, Failure>) -> i32 {
    let value = match result {
        Ok(report) => envelope::wrap(CODE_OK, "", report),
        Err(f) => envelope::wrap(f.code, &f.error, Value::Null),
    };
    println!("{value}");
    JSON_EXIT
}

/// The settings reader behind `config get`.
pub struct ConfigReader {
    pub home: std::path::PathBuf,
    pub repo_slug: Option<String>,
}

impl SettingsReader for ConfigReader {
    fn read(&self, key: &str) -> Result<(Value, String), String> {
        let (value, source, _ignored) =
            config::resolve_valid(key, &self.home, self.repo_slug.as_deref())
                .map_err(|e| e.to_string())?;
        Ok((value, source_label(source).to_string()))
    }
}

fn source_label(source: config::Source) -> &'static str {
    match source {
        config::Source::Repo => "repo",
        config::Source::Org => "org",
        config::Source::Global => "global",
        config::Source::Default => "default",
    }
}

/// The real `gh`.
pub struct RealGh;

impl StatusGh for RealGh {
    fn pr_view(&self, pr: u64) -> Result<String, String> {
        crate::pr::shared::gh(&[
            "pr",
            "view",
            &pr.to_string(),
            "--json",
            "number,url,isDraft,headRefOid,baseRefName",
        ])
    }

    fn open_pr_numbers_for_head(&self, branch: &str) -> Result<Vec<u64>, String> {
        let raw = crate::pr::shared::gh(&[
            "pr",
            "list",
            "--state",
            "open",
            "--head",
            branch,
            "--json",
            "number,isCrossRepository",
        ])?;
        let list: Vec<Value> = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        // A fork's branch can share the base's name; it is not a base PR.
        Ok(list
            .iter()
            .filter(|v| v["isCrossRepository"].as_bool() != Some(true))
            .filter_map(|v| v["number"].as_u64())
            .collect())
    }
}

/// The real triage: collects the PR facts and asks `claude`.
pub struct RealTriager;

impl Triager for RealTriager {
    fn review(&self, pr: u64) -> Result<triage::Review, String> {
        let facts = triage::collect(Some(pr), None)?;
        // A triage that cannot run already comes back as `deep`.
        Ok(triage::triage(&triage::ClaudeRunner, &facts).review)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Settings(HashMap<&'static str, Result<Value, String>>);

    impl SettingsReader for Settings {
        fn read(&self, key: &str) -> Result<(Value, String), String> {
            match self.0.get(key) {
                Some(Ok(v)) => Ok((v.clone(), "default".to_string())),
                Some(Err(e)) => Err(e.clone()),
                None => Err(format!("{key} is not set in the fake")),
            }
        }
    }

    struct Fixed(Result<triage::Review, String>);

    impl Triager for Fixed {
        fn review(&self, _pr: u64) -> Result<triage::Review, String> {
            self.0.clone()
        }
    }

    struct Gh {
        view: Result<String, String>,
        stacked: Result<Vec<u64>, String>,
    }

    impl StatusGh for Gh {
        fn pr_view(&self, _pr: u64) -> Result<String, String> {
            self.view.clone()
        }
        fn open_pr_numbers_for_head(&self, _branch: &str) -> Result<Vec<u64>, String> {
            self.stacked.clone()
        }
    }

    const VIEW: &str = r#"{"number":7,"url":"https://x/pull/7","isDraft":true,"headRefOid":"abc123","baseRefName":"main"}"#;

    fn settings(
        enabled: Result<Value, String>,
        kind: Result<Value, String>,
        fix: Result<Value, String>,
        merge: Result<Value, String>,
    ) -> Settings {
        Settings(HashMap::from([
            (AUTO_REVIEW_ENABLED, enabled),
            (AUTO_REVIEW_TYPE, kind),
            (AUTO_REVIEW_FIX, fix),
            (AUTO_MERGE_ENABLED, merge),
        ]))
    }

    fn gh() -> Gh {
        Gh {
            view: Ok(VIEW.to_string()),
            stacked: Ok(Vec::new()),
        }
    }

    fn run(enabled: Result<Value, String>, kind: Result<Value, String>, t: Fixed) -> Value {
        let s = settings(enabled, kind, Ok(json!(true)), Ok(json!(false)));
        collect(7, json!({}), &gh(), &s, &t).expect("context")
    }

    fn quiet() -> Fixed {
        Fixed(Err("the triage must not run".to_string()))
    }

    /// Enabled, type, triage and the review they must give.
    type Case = (
        Result<Value, String>,
        Result<Value, String>,
        Fixed,
        &'static str,
    );

    #[test]
    fn decision_table() {
        let boom = || Err("boom".to_string());
        let cases: Vec<Case> = vec![
            (Ok(json!(false)), Ok(json!("deep")), quiet(), "skip"),
            (Ok(json!(false)), boom(), quiet(), "skip"),
            (boom(), Ok(json!("quick")), quiet(), "deep"),
            (Ok(json!(true)), boom(), quiet(), "deep"),
            (Ok(json!(true)), Ok(json!("deep")), quiet(), "deep"),
            (Ok(json!(true)), Ok(json!("quick")), quiet(), "quick"),
            (
                Ok(json!(true)),
                Ok(json!("auto")),
                Fixed(Ok(triage::Review::Quick)),
                "quick",
            ),
            (
                Ok(json!(true)),
                Ok(json!("auto")),
                Fixed(Ok(triage::Review::Deep)),
                "deep",
            ),
            (
                Ok(json!(true)),
                Ok(json!("auto")),
                Fixed(Err("no claude".to_string())),
                "deep",
            ),
        ];
        for (enabled, kind, triager, want) in cases {
            let report = run(enabled.clone(), kind.clone(), triager);
            assert_eq!(report["review"], want, "enabled {enabled:?} type {kind:?}");
        }
    }

    #[test]
    fn a_failed_triage_names_the_failure_in_the_reason() {
        let report = run(
            Ok(json!(true)),
            Ok(json!("auto")),
            Fixed(Err("no claude".to_string())),
        );
        assert!(report["review_reason"]
            .as_str()
            .unwrap()
            .contains("no claude"));
    }

    #[test]
    fn a_fix_read_error_gives_false_and_keeps_the_text() {
        let s = settings(
            Ok(json!(true)),
            Ok(json!("deep")),
            Err("fix unreadable".to_string()),
            Ok(json!(true)),
        );
        let report = collect(7, json!({}), &gh(), &s, &quiet()).unwrap();
        let fix = &report["settings"][AUTO_REVIEW_FIX];
        assert_eq!(fix["value"], false);
        assert_eq!(fix["error"], "fix unreadable");
    }

    #[test]
    fn a_merge_read_error_gives_false_and_keeps_the_text() {
        let s = settings(
            Ok(json!(true)),
            Ok(json!("deep")),
            Ok(json!(true)),
            Err("merge unreadable".to_string()),
        );
        let report = collect(7, json!({}), &gh(), &s, &quiet()).unwrap();
        let merge = &report["settings"][AUTO_MERGE_ENABLED];
        assert_eq!(merge["value"], false);
        assert_eq!(merge["error"], "merge unreadable");
    }

    #[test]
    fn an_unknown_review_type_is_deep() {
        let report = run(Ok(json!(true)), Ok(json!("sideways")), quiet());
        assert_eq!(report["review"], "deep");
    }

    #[test]
    fn the_pr_object_carries_the_gh_fields() {
        let report = run(Ok(json!(true)), Ok(json!("deep")), quiet());
        assert_eq!(report["pr"]["number"], 7);
        assert_eq!(report["pr"]["isDraft"], true);
        assert_eq!(report["pr"]["head"], "abc123");
        assert_eq!(report["pr"]["base"], "main");
        assert_eq!(report["pr"]["stacked_on"], Value::Null);
    }

    #[test]
    fn a_base_that_is_another_open_pr_head_is_stacked() {
        let mut g = gh();
        g.stacked = Ok(vec![7, 3]);
        let s = settings(
            Ok(json!(true)),
            Ok(json!("deep")),
            Ok(json!(true)),
            Ok(json!(true)),
        );
        let report = collect(7, json!({}), &g, &s, &quiet()).unwrap();
        assert_eq!(report["pr"]["stacked_on"], 3);
    }

    #[test]
    fn a_failed_pr_view_is_code_five() {
        let mut g = gh();
        g.view = Err("no such pr".to_string());
        let s = settings(
            Ok(json!(true)),
            Ok(json!("deep")),
            Ok(json!(true)),
            Ok(json!(true)),
        );
        let failure = collect(7, json!({}), &g, &s, &quiet()).unwrap_err();
        assert_eq!(failure.code, CODE_PR_VIEW);
        assert_eq!(failure.error, "no such pr");
    }

    #[test]
    fn bad_gh_json_is_code_five() {
        let mut g = gh();
        g.view = Ok("not json".to_string());
        let s = settings(
            Ok(json!(true)),
            Ok(json!("deep")),
            Ok(json!(true)),
            Ok(json!(true)),
        );
        let failure = collect(7, json!({}), &g, &s, &quiet()).unwrap_err();
        assert_eq!(failure.code, CODE_PR_VIEW);
    }
}
