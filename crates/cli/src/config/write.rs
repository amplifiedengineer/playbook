// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Validated writes to playbook's own tiered config store: the
//! write-side counterpart to `resolve` in `config::mod`.

use super::{keys, ConfigError};
use serde_json::Value;
use std::path::Path;

/// Which tier `set` writes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Global,
    Org,
    Repo,
}

/// Validate `key` and `value` against `keys::KNOWN_KEYS`, then store them in
/// `tier` of the config database under `home`'s playbook root, creating the
/// database if absent. `repo_slug` (as `<owner>/<repo>`) is required for the
/// org and repo tiers; the global tier ignores it. Every validation runs
/// before any write, so a rejected call never touches the database. SQLite's
/// own locking serializes concurrent writers.
pub fn set(
    tier: Tier,
    key: &str,
    value: &Value,
    home: &Path,
    repo_slug: Option<&str>,
) -> Result<(), ConfigError> {
    validate_key_and_value(key, value)?;
    require_global_for_user_key(key, tier == Tier::Global)?;
    check_cap_order(tier, key, value, home, repo_slug)?;
    let root = crate::common::paths::playbook_root_from(home);
    super::store::put(&root, tier, repo_slug, key, value)
}

/// Reject a per-user key bound for the org or repo tier.
pub(crate) fn require_global_for_user_key(key: &str, global: bool) -> Result<(), ConfigError> {
    if keys::is_user_key(key) && !global {
        return Err(ConfigError::GlobalOnly(key.to_string()));
    }
    Ok(())
}

/// Reject an unknown key, a value whose JSON type does not match
/// `keys::default_value(key)`, or an out-of-enum string, before any file is
/// touched.
pub(crate) fn validate_key_and_value(key: &str, value: &Value) -> Result<(), ConfigError> {
    let default =
        keys::default_value(key).ok_or_else(|| ConfigError::UnknownKey(key.to_string()))?;
    let expected = match default {
        Value::Bool(_) => "boolean",
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Array(_) => "array of paths",
        _ => "value",
    };
    let type_matches = matches!(
        (&default, value),
        (Value::Bool(_), Value::Bool(_))
            | (Value::String(_), Value::String(_))
            | (Value::Number(_), Value::Number(_))
            | (Value::Array(_), Value::Array(_))
    );
    if !type_matches {
        return Err(ConfigError::WrongType {
            key: key.to_string(),
            expected,
        });
    }
    if let (Some(allowed), Value::String(s)) = (keys::allowed_enum_values(key), value) {
        if !allowed.contains(&s.as_str()) {
            return Err(ConfigError::InvalidEnumValue {
                key: key.to_string(),
                value: s.clone(),
                allowed,
            });
        }
    }
    if let Value::String(s) = value {
        if keys::model_override_error(key, s).is_some() {
            return Err(ConfigError::InvalidEnumValue {
                key: key.to_string(),
                value: s.clone(),
                allowed: &["empty", "claude-<tier>-<major>[-<minor>]"],
            });
        }
    }
    if matches!(default, Value::Number(_)) {
        validate_number(key, value)?;
    }
    if let (true, Value::String(text)) = (keys::is_watch_rule_key(key), value) {
        if !text.is_empty() {
            crate::watch::rule::Stored::parse(text).map_err(|e| ConfigError::InvalidRule {
                key: key.to_string(),
                reason: e.to_string(),
            })?;
        }
    }
    if key == "pr.ticketPatterns" {
        let ok = value.as_array().is_some_and(|items| {
            items.iter().all(|i| {
                i.as_str()
                    .is_some_and(|p| crate::pr::body_rules::compile_pattern(p).is_ok())
            })
        });
        if !ok {
            return Err(ConfigError::OutOfRange {
                key: key.to_string(),
                value: value.to_string(),
                constraint: "a list of valid regular expressions",
            });
        }
    }
    if let ("usage.ui.layout", Value::String(text)) = (key, value) {
        if !text.is_empty() && playbook_telemetry::tui::layout::Layout::parse(text).is_err() {
            return Err(ConfigError::OutOfRange {
                key: key.to_string(),
                value: value.to_string(),
                constraint: "tabs like Charts=spend,tokens;Numbers=models (1 to 12 tabs; panels: spend, tokens, models, breakdown, live, sessions, burnup, hourly; views pinned with dotted words: spend.total, spend.model, spend.daily, breakdown.projects, breakdown.agents, breakdown.skills, breakdown.tools, breakdown.health; | starts a row of up to 2 panels, with at most 4 rows, and 8 panels in a tab; a name is 1 to 16 characters)",
            });
        }
    }
    if let ("usage.ui.state", Value::String(text)) = (key, value) {
        if text.len() > playbook_telemetry::config::MAX_STATE_BYTES {
            return Err(ConfigError::OutOfRange {
                key: key.to_string(),
                value: "(too long)".to_string(),
                constraint: "text of at most 4096 bytes, written by pbtop",
            });
        }
    }
    Ok(())
}

const CEILING_KEY: &str = "auto.sessionCeilingUsd";
const BUDGET_KEYS: [(&str, u32); 2] = [
    ("auto.budgetUsd", keys::AUTO_BUDGET_USD),
    ("auto.agentBudgetUsd", keys::AUTO_AGENT_BUDGET_USD),
];

/// Keep a set `auto.sessionCeilingUsd` at least as large as `auto.budgetUsd`
/// and `auto.agentBudgetUsd`, since a lower ceiling parks every agent before
/// its own cap is reached. Setting the ceiling checks it against both
/// budgets; setting a budget checks only that budget against the ceiling. A
/// ceiling of 0 means ten times `auto.budgetUsd` and is not checked. The
/// other keys are read the way the spend hook reads them, as they resolve
/// for `repo_slug`, and for a global write also as they resolve outside any
/// repo, since a global value reaches every repo.
fn check_cap_order(
    tier: Tier,
    key: &str,
    value: &Value,
    home: &Path,
    repo_slug: Option<&str>,
) -> Result<(), ConfigError> {
    let Some(budget_default) = BUDGET_KEYS
        .iter()
        .find(|(budget_key, _)| *budget_key == key)
        .map(|(_, default)| *default)
        .or((key == CEILING_KEY).then_some(0))
    else {
        return Ok(());
    };
    let mut slugs = vec![repo_slug];
    if tier == Tier::Global && repo_slug.is_some() {
        slugs.push(None);
    }
    for slug in slugs {
        let cents = |other: &str| {
            super::resolve(other, home, slug)
                .ok()
                .and_then(|(value, _)| super::auto_cap_cents(&value))
        };
        let reason = if key == CEILING_KEY {
            let Some(ceiling) = super::auto_cap_cents(value) else {
                return Ok(());
            };
            BUDGET_KEYS.iter().find_map(|(budget_key, default)| {
                let budget = cents(budget_key).unwrap_or(i64::from(*default) * 100);
                (budget > ceiling).then(|| {
                    format!(
                        "it is below {budget_key} ({}). Set it to 0 (ten times auto.budgetUsd) or to at least {}",
                        usd(budget),
                        usd(budget)
                    )
                })
            })
        } else {
            let budget = super::auto_cap_cents(value).unwrap_or(i64::from(budget_default) * 100);
            cents(CEILING_KEY).filter(|ceiling| budget > *ceiling).map(|ceiling| {
                format!(
                    "it is above auto.sessionCeilingUsd ({}). Raise the ceiling first with `playbook config set auto.sessionCeilingUsd <usd>`, or set it to 0 (ten times auto.budgetUsd)",
                    usd(ceiling)
                )
            })
        };
        if let Some(reason) = reason {
            return Err(ConfigError::CapOrder {
                key: key.to_string(),
                value: value.to_string(),
                reason,
            });
        }
    }
    Ok(())
}

/// Whole cents as dollars, without trailing zeros: 20000 is `200`, 250 `2.5`.
fn usd(cents: i64) -> String {
    let text = format!("{:.2}", cents as f64 / 100.0);
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The most `docs.maxBytes` may be: the converter's own input ceiling.
const DOCS_MAX_BYTES_CEILING: i64 = keys::HARD_MAX_INPUT_BYTES as i64;

const DOCS_DISPLAY_CHARS_MIN: i64 = 1_000;
const DOCS_DISPLAY_CHARS_MAX: i64 = 120_000;

/// `auto.budgetUsd` and `auto.agentBudgetUsd` take any number of at least one cent, since the spend cap
/// is held in whole cents, so a fractional budget works.
/// `usage.budget.*Usd` takes any number from 0 (no budget).
/// `auto.warnPct` takes a whole number within 1 to 100, `usage.refreshSeconds` one within 1 to
/// 3600, `hygiene.thresholdTokens` one of at least 10000, the `fix.` and `review.stack` limits a whole number of at least 1, and every other
/// numeric key 0 or more.
fn validate_number(key: &str, value: &Value) -> Result<(), ConfigError> {
    let whole = value.as_i64();
    let (valid, constraint) = match key {
        "auto.budgetUsd" | "auto.agentBudgetUsd" => (
            value.as_f64().is_some_and(|n| n.is_finite() && n >= 0.01),
            "a number of at least 0.01",
        ),
        "auto.sessionCeilingUsd" => (
            value
                .as_f64()
                .is_some_and(|n| n.is_finite() && (n == 0.0 || n >= 0.01)),
            "0 (ten times auto.budgetUsd) or a number of at least 0.01 that is not below auto.budgetUsd or auto.agentBudgetUsd",
        ),
        "usage.budget.monthlyUsd" | "usage.budget.weeklyUsd" => (
            value
                .as_f64()
                .is_some_and(|n| n.is_finite() && (0.0..=1_000_000_000.0).contains(&n)),
            "a number of dollars, 0 (no budget) to 1000000000",
        ),
        "watch.limits.usdPerDay" => (
            value
                .as_f64()
                .is_some_and(|n| n.is_finite() && (0.0..=1_000_000_000.0).contains(&n)),
            "a number of dollars, 0 to 1000000000",
        ),
        "auto.warnPct" => (
            whole.is_some_and(|n| (1..=100).contains(&n)),
            "a whole number between 1 and 100",
        ),
        "usage.refreshSeconds" => (
            whole.is_some_and(|n| (1..=3600).contains(&n)),
            "a whole number between 1 and 3600",
        ),
        "watch.pollSeconds" => (
            whole.is_some_and(|n| (1..=86_400).contains(&n)),
            "a whole number between 1 and 86400",
        ),
        "watch.maxCatchupHours" => (
            whole.is_some_and(|n| (1..=720).contains(&n)),
            "a whole number between 1 and 720",
        ),
        "gate.maxRounds" => (
            whole.is_some_and(keys::gate_max_rounds_in_range),
            "a whole number from 1 to 20",
        ),
        "sessions.staleAfterDays" | "sessions.recentDays" => {
            (whole.is_some_and(|n| n >= 1), "a positive integer")
        }
        "hygiene.thresholdTokens" => (
            whole.is_some_and(|n| n >= 10_000),
            "a whole number of at least 10000",
        ),
        "fix.maxFiles"
        | "fix.maxLines"
        | "review.stackMaxPrs"
        | "review.stackMaxLines"
        | "review.suggest.deepLines"
        | "review.suggest.quickLines"
        | "review.suggest.deepFiles"
        | "review.suggest.packageSpan"
        | "review.suggest.untestedLines"
        | "update.checkIntervalHours" => (whole.is_some_and(|n| n >= 1), "a positive integer"),
        "docs.maxBytes" => (
            whole.is_some_and(|n| (1..=DOCS_MAX_BYTES_CEILING).contains(&n)),
            "a whole number from 1 to 104857600 (the 100 MiB ceiling)",
        ),
        "docs.displayChars" => (
            whole.is_some_and(|n| (DOCS_DISPLAY_CHARS_MIN..=DOCS_DISPLAY_CHARS_MAX).contains(&n)),
            "a whole number from 1000 to 120000",
        ),
        "docs.maxMemoryMb" => (
            whole.is_some_and(|n| (1..=keys::DOCS_MAX_MEMORY_MB_CEILING).contains(&n)),
            "a whole number from 1 to 4096",
        ),
        "pr.bodyMaxChars" | "docs.timeoutSeconds" | "docs.maxOutputChars" => {
            (whole.is_some_and(|n| n >= 1), "a positive integer")
        }
        _ => {
            return match whole {
                Some(n) if n >= 0 => Ok(()),
                _ => Err(ConfigError::InvalidNumber {
                    key: key.to_string(),
                    value: value.to_string(),
                }),
            }
        }
    };
    if valid {
        return Ok(());
    }
    Err(ConfigError::OutOfRange {
        key: key.to_string(),
        value: value.to_string(),
        constraint,
    })
}
