// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Reads the session and GitHub PR JSON `statusline.sh` pipes to `jq -r`
//! (`statusline.sh:350-366` and `statusline.sh:550-582`), reproducing that
//! filter's exact `@sh`-quoted output. `@sh` only single-quotes a
//! string-typed interpolated value: a JSON number prints bare, so every
//! field here carries the same type its source filter's `\(...)`
//! interpolation would resolve to before quoting.

use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

/// The text a shell variable holds for `value`: the string itself, or a
/// number's own text.
pub(crate) fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// The eighteen session fields as typed JSON values, in `eval` order.
pub(crate) fn session_values(json: &str, pwd_env: &str) -> [(&'static str, Value); 18] {
    let parsed: Value = serde_json::from_str(json).unwrap_or(Value::Null);

    [
        ("cwd", cwd_value(&parsed, pwd_env)),
        ("session_id", field_or_empty(&parsed, &["session_id"])),
        ("model", field_or_empty(&parsed, &["model", "display_name"])),
        (
            "used",
            field_or_empty(&parsed, &["context_window", "used_percentage"]),
        ),
        (
            "ctx_total_tokens",
            field_or_empty(&parsed, &["context_window", "total_input_tokens"]),
        ),
        (
            "ctx_window_size",
            field_or_empty(&parsed, &["context_window", "context_window_size"]),
        ),
        (
            "cache_create",
            field_or_empty(
                &parsed,
                &[
                    "context_window",
                    "current_usage",
                    "cache_creation_input_tokens",
                ],
            ),
        ),
        (
            "cache_read",
            field_or_empty(
                &parsed,
                &["context_window", "current_usage", "cache_read_input_tokens"],
            ),
        ),
        (
            "rl_5h",
            field_or_empty(&parsed, &["rate_limits", "five_hour", "used_percentage"]),
        ),
        (
            "rl_5h_reset",
            field_or_empty(&parsed, &["rate_limits", "five_hour", "resets_at"]),
        ),
        (
            "rl_7d",
            field_or_empty(&parsed, &["rate_limits", "seven_day", "used_percentage"]),
        ),
        ("json_effort", field_or_empty(&parsed, &["effort", "level"])),
        ("json_thinking", thinking_value(&parsed)),
        (
            "cost_usd",
            field_or_empty(&parsed, &["cost", "total_cost_usd"]),
        ),
        (
            "wall_ms",
            field_or_empty(&parsed, &["cost", "total_duration_ms"]),
        ),
        ("model_id", field_or_empty(&parsed, &["model", "id"])),
        (
            "ctx_input_tokens",
            field_or_empty(
                &parsed,
                &["context_window", "current_usage", "input_tokens"],
            ),
        ),
        (
            "transcript_path",
            field_or_empty(&parsed, &["transcript_path"]),
        ),
    ]
}

/// Case-insensitive match against the failed conclusions and states the real
/// filter's `is_failed` def names (`statusline.sh:322-324`).
fn is_failed(conclusion: &str, state: &str) -> bool {
    matches!(
        conclusion.to_ascii_lowercase().as_str(),
        "failure" | "cancelled" | "timed_out" | "action_required" | "startup_failure" | "stale"
    ) || matches!(state.to_ascii_lowercase().as_str(), "failure" | "error")
}

/// Case-insensitive match against the running statuses and states the real
/// filter's `is_running` def names (`statusline.sh:325-327`).
fn is_running(status: &str, state: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "in_progress" | "queued" | "pending" | "waiting"
    ) || state.eq_ignore_ascii_case("pending")
}

/// Reshapes a raw `gh api graphql` CI status response (the standalone CI
/// fetch at `statusline.sh:677-680`) into `{"statusCheckRollup": [...]}`,
/// ready to feed [`ci_rollup`]. Falls back to an empty array when
/// `.data.repository.ref.target.statusCheckRollup.contexts.nodes` is
/// missing, not an array, or the input itself fails to parse, matching the
/// ported filter's `// []`.
pub(crate) fn graphql_ci_checks(json: &str) -> String {
    let parsed: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    let nodes = get_path(
        &parsed,
        &[
            "data",
            "repository",
            "ref",
            "target",
            "statusCheckRollup",
            "contexts",
            "nodes",
        ],
    )
    .and_then(Value::as_array)
    .cloned()
    .unwrap_or_default();

    serde_json::json!({ "statusCheckRollup": nodes }).to_string()
}

/// Tallies a `.statusCheckRollup` array into `(state, failed, running,
/// total)`, matching `JQ_CI_ROLLUP`'s fail-beats-running-beats-pass
/// precedence (`statusline.sh:321-337`).
pub(crate) fn ci_rollup(checks_json: &str) -> (String, usize, usize, usize) {
    let parsed: Value = serde_json::from_str(checks_json).unwrap_or(Value::Null);
    let checks = get_path(&parsed, &["statusCheckRollup"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let total = checks.len();
    let failed = checks
        .iter()
        .filter(|check| is_failed(str_at(check, "conclusion"), str_at(check, "state")))
        .count();
    let running = checks
        .iter()
        .filter(|check| is_running(str_at(check, "status"), str_at(check, "state")))
        .count();

    let state = if total == 0 {
        "none"
    } else if failed > 0 {
        "fail"
    } else if running > 0 {
        "running"
    } else {
        "pass"
    };
    (state.to_string(), failed, running, total)
}

/// First Jira-shaped ticket key (`[A-Z][A-Z0-9]+-[0-9]+`) in `body`, or
/// empty when none is present, matching `scan(...) | .[0] // ""`.
pub(crate) fn jira_from_body(body: &str) -> String {
    jira_key(body).unwrap_or_default().to_string()
}

/// First Jira-shaped ticket key in `text`. The pattern compiles once per process.
pub(crate) fn jira_key(text: &str) -> Option<&str> {
    static JIRA: LazyLock<Regex> =
        LazyLock::new(|| crate::common::re::static_regex(r"[A-Z][A-Z0-9]+-[0-9]+"));
    JIRA.find(text).map(|m| m.as_str())
}

/// `.number // "" | tostring | if . == "null" then "" else . end`: the
/// number's own text when present, empty when missing or null.
pub(crate) fn pr_number_value(json: &Value) -> String {
    match get_path(json, &["number"]) {
        Some(Value::Null) | None => String::new(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
    }
}

/// `[.reviewRequests[]? | (.login // .name // "team")]`: a requested
/// reviewer's login, falling back to a team's name, then the literal
/// `"team"` when neither is present.
pub(crate) fn pending_logins(json: &Value) -> Vec<String> {
    get_path(json, &["reviewRequests"])
        .and_then(Value::as_array)
        .map(|requests| {
            requests
                .iter()
                .map(|request| {
                    request
                        .get("login")
                        .and_then(Value::as_str)
                        .or_else(|| request.get("name").and_then(Value::as_str))
                        .unwrap_or("team")
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `.latestReviews[]? | select(...)`: every review whose author is not
/// `coderabbitai`, whose state is not `COMMENTED`, and whose author is not
/// currently a pending reviewer (a re-request suppresses their earlier
/// review), rendered as `g|r|d:login:submitted_at`.
pub(crate) fn completed_reviews(json: &Value, pending: &[String]) -> Vec<String> {
    get_path(json, &["latestReviews"])
        .and_then(Value::as_array)
        .map(|reviews| {
            reviews
                .iter()
                .filter_map(|review| {
                    let login = review
                        .get("author")
                        .and_then(|author| author.get("login"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let state = str_at(review, "state");
                    if login == "coderabbitai"
                        || state == "COMMENTED"
                        || pending.iter().any(|p| p == login)
                    {
                        return None;
                    }
                    let prefix = match state {
                        "APPROVED" => "g",
                        "CHANGES_REQUESTED" => "r",
                        _ => "d",
                    };
                    let submitted_at = str_at(review, "submittedAt");
                    Some(format!("{prefix}:{login}:{submitted_at}"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `.cwd // .workspace.current_dir // env.PWD // ""`: the first present,
/// non-null field, falling back to `pwd_env` (an already-empty `pwd_env`
/// yields the same result as the filter's own trailing `// ""`).
fn cwd_value(json: &Value, pwd_env: &str) -> Value {
    for path in [["cwd"].as_slice(), ["workspace", "current_dir"].as_slice()] {
        if let Some(value) = get_path(json, path) {
            if is_jq_truthy(value) {
                return value.clone();
            }
        }
    }
    Value::String(pwd_env.to_string())
}

/// `if .thinking.enabled then "true" else "" end`, using jq's own
/// truthiness: only `null` and `false` are falsy, so any other present value
/// (not only the literal `true`) yields `"true"`.
fn thinking_value(json: &Value) -> Value {
    let truthy = get_path(json, &["thinking", "enabled"])
        .map(is_jq_truthy)
        .unwrap_or(false);
    Value::String(if truthy {
        "true".to_string()
    } else {
        String::new()
    })
}

/// `path // ""`: the field at `path`, or an empty string when any step is
/// missing, or the leaf itself is `null` or `false`.
pub(crate) fn field_or_empty(json: &Value, path: &[&str]) -> Value {
    match get_path(json, path) {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Value::String(String::new()),
        Some(value) => value.clone(),
    }
}

/// A field's string value, or empty when any path step is missing, or the
/// leaf is `null` or `false`. Every call site here reads a field the real
/// filter always treats as string-typed once present, matching
/// `path // ""`.
pub(crate) fn str_field(json: &Value, path: &[&str]) -> String {
    match get_path(json, path) {
        Some(Value::String(s)) => s.clone(),
        None | Some(Value::Null) | Some(Value::Bool(false)) => String::new(),
        Some(other) => other.to_string(),
    }
}

/// A flat object field read as `&str`, empty when missing or not a string.
pub(crate) fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

pub(crate) fn get_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = Some(value);
    for key in path {
        current = current.and_then(|v| v.get(key));
    }
    current
}

/// jq's `//` truthiness: only `null` and `false` are falsy.
fn is_jq_truthy(value: &Value) -> bool {
    !matches!(value, Value::Null | Value::Bool(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    // is_failed / is_running --------------------------------------------------

    #[test]
    fn is_failed_matches_every_named_conclusion_case_insensitively() {
        // Arrange, Act, Assert
        for conclusion in [
            "FAILURE",
            "Cancelled",
            "TIMED_OUT",
            "action_required",
            "Startup_Failure",
            "STALE",
        ] {
            assert!(
                is_failed(conclusion, ""),
                "expected '{conclusion}' to be a failed conclusion"
            );
        }
    }

    #[test]
    fn is_failed_matches_error_and_failure_states_case_insensitively() {
        // Arrange, Act, Assert
        assert!(is_failed("", "Error"));
        assert!(is_failed("", "FAILURE"));
    }

    #[test]
    fn is_failed_does_not_match_a_passing_conclusion() {
        // Arrange, Act, Assert
        assert!(!is_failed("SUCCESS", ""));
    }

    #[test]
    fn is_failed_does_not_match_empty_or_missing_values() {
        // Arrange, Act, Assert
        assert!(!is_failed("", ""));
    }

    #[test]
    fn is_running_matches_every_named_status_case_insensitively() {
        // Arrange, Act, Assert
        for status in ["IN_PROGRESS", "Queued", "PENDING", "Waiting"] {
            assert!(
                is_running(status, ""),
                "expected '{status}' to be a running status"
            );
        }
    }

    #[test]
    fn is_running_matches_a_pending_state_case_insensitively() {
        // Arrange, Act, Assert
        assert!(is_running("", "Pending"));
    }

    #[test]
    fn is_running_does_not_match_empty_or_missing_values() {
        // Arrange, Act, Assert
        assert!(!is_running("", ""));
    }

    // graphql_ci_checks -------------------------------------------------------

    #[test]
    fn graphql_ci_checks_pulls_the_contexts_nodes_array_out_of_the_graphql_envelope() {
        // Arrange
        let json = r#"{"data": {"repository": {"ref": {"target": {
            "statusCheckRollup": {"contexts": {"nodes": [
                {"conclusion": "FAILURE"},
                {"status": "IN_PROGRESS"}
            ]}}
        }}}}}"#;

        // Act
        let got = graphql_ci_checks(json);

        // Assert
        assert_eq!(
            got,
            r#"{"statusCheckRollup":[{"conclusion":"FAILURE"},{"status":"IN_PROGRESS"}]}"#
        );
    }

    #[test]
    fn graphql_ci_checks_defaults_to_an_empty_array_when_the_nodes_path_is_missing() {
        // Arrange
        let json = r#"{"data": {"repository": {"ref": null}}}"#;

        // Act
        let got = graphql_ci_checks(json);

        // Assert
        assert_eq!(got, r#"{"statusCheckRollup":[]}"#);
    }

    #[test]
    fn graphql_ci_checks_defaults_to_an_empty_array_when_the_input_fails_to_parse() {
        // Arrange
        let json = "not json";

        // Act
        let got = graphql_ci_checks(json);

        // Assert
        assert_eq!(got, r#"{"statusCheckRollup":[]}"#);
    }

    // ci_rollup -----------------------------------------------------------------

    #[test]
    fn ci_rollup_reports_none_when_there_are_zero_checks() {
        // Arrange
        let checks_json = r#"{"statusCheckRollup": []}"#;

        // Act
        let (state, failed, running, total) = ci_rollup(checks_json);

        // Assert
        assert_eq!((state.as_str(), failed, running, total), ("none", 0, 0, 0));
    }

    #[test]
    fn ci_rollup_counts_and_prioritizes_fail_over_running_over_pass() {
        // Arrange: one failed, two running (an in-progress status and a
        // pending status, both classified as running), one passed.
        let checks_json = r#"{"statusCheckRollup": [
            {"conclusion": "FAILURE"},
            {"status": "IN_PROGRESS"},
            {"status": "PENDING"},
            {"conclusion": "SUCCESS"}
        ]}"#;

        // Act
        let (state, failed, running, total) = ci_rollup(checks_json);

        // Assert
        assert_eq!((state.as_str(), failed, running, total), ("fail", 1, 2, 4));
    }

    #[test]
    fn ci_rollup_reports_pass_when_every_check_passed() {
        // Arrange
        let checks_json = r#"{"statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"conclusion": "SUCCESS"},
            {"conclusion": "SUCCESS"}
        ]}"#;

        // Act
        let (state, failed, running, total) = ci_rollup(checks_json);

        // Assert
        assert_eq!((state.as_str(), failed, running, total), ("pass", 0, 0, 3));
    }
}
