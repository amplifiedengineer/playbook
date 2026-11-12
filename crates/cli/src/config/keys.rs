// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Known playbook config keys, their default values, and their allowed enum
//! values where one exists. Used by the resolution and write modules to
//! reject an unknown key or an out-of-range value.

use serde_json::{json, Value};
use std::sync::LazyLock;

/// The default `auto.budgetUsd`, the main session's spend cap in US dollars.
/// Measured on 30 days of real sessions (ADR 0028, 2026-10-17 amendment): the
/// main transcript of a session that spent $1 or more cost $199 at the 95th
/// percentile.
pub const AUTO_BUDGET_USD: u32 = 200;
/// The default `auto.agentBudgetUsd`, each subagent's spend cap in US dollars.
/// 3 of 3,083 measured subagents cost more than this.
pub const AUTO_AGENT_BUDGET_USD: u32 = 50;
/// With `auto.sessionCeilingUsd` at 0, the whole tree's ceiling is this many
/// times `auto.budgetUsd`.
pub const AUTO_CEILING_FACTOR: u32 = 10;

// The default ceiling stays above both default budgets, so neither budget
// can be reached only after the whole tree is parked.
const _: () = assert!(
    AUTO_BUDGET_USD * AUTO_CEILING_FACTOR > AUTO_BUDGET_USD
        && AUTO_BUDGET_USD * AUTO_CEILING_FACTOR > AUTO_AGENT_BUDGET_USD
);

/// The default `gate.maxRounds`, the most rounds one quality-gate phase runs.
pub const GATE_MAX_ROUNDS_DEFAULT: i64 = 5;
/// The most `gate.maxRounds` may be.
pub const GATE_MAX_ROUNDS_CEILING: i64 = 20;

/// Whether `n` is a legal `gate.maxRounds`, 1 to the ceiling. Writes and reads
/// both use it, so a hand-edited value cannot widen the cap.
pub fn gate_max_rounds_in_range(n: i64) -> bool {
    (1..=GATE_MAX_ROUNDS_CEILING).contains(&n)
}

/// The largest input a document converter opens, whatever `docs.maxBytes`
/// says. The bundled `pbdoc` keeps its own copy because it must not depend on
/// this crate; a test in `doc::converter` keeps the two equal.
pub const HARD_MAX_INPUT_BYTES: u64 = 104_857_600;
/// The most `docs.maxMemoryMb` may be. The bundled `pbdoc` clamps to it too.
pub const DOCS_MAX_MEMORY_MB_CEILING: i64 = 4096;

/// Every config key playbook resolves, in v1.
pub const KNOWN_KEYS: &[&str] = &[
    "autoReview.enabled",
    "autoReview.type",
    "autoReview.fix",
    "autoMerge.enabled",
    "commit.signOff",
    "handoff.onClear",
    "pr.draft",
    "pr.rules.template",
    "pr.rules.length",
    "pr.rules.issue",
    "pr.rules.ticketLine",
    "pr.rules.scope",
    "pr.bodyMaxChars",
    "pr.ticketPatterns",
    "worktreeCleanup.enabled",
    "worktreeCleanup.staleAfterDays",
    "worktreeCleanup.conflictGracePeriodDays",
    "mode",
    "maxEffortLevel",
    "effort.auto",
    "memory.source",
    "models.haiku",
    "models.sonnet",
    "models.opus",
    "routing.escalate",
    "auto.budgetUsd",
    "auto.agentBudgetUsd",
    "auto.sessionCeilingUsd",
    "auto.warnPct",
    "gate.maxRounds",
    "fix.maxFiles",
    "fix.maxLines",
    "review.stackMaxPrs",
    "review.stackMaxLines",
    "review.suggest.deepLines",
    "review.suggest.quickLines",
    "review.suggest.deepFiles",
    "review.suggest.packageSpan",
    "review.suggest.untestedLines",
    "security.defaults",
    "usage.theme",
    "usage.maskAccount",
    "usage.weekStart",
    "usage.timeZone",
    "usage.refreshSeconds",
    "usage.telemetry",
    "usage.telemetryRetentionDays",
    "usage.telemetryMaxRows",
    "usage.autoIngest",
    "usage.autoIngestMinutes",
    "usage.budget.monthlyUsd",
    "usage.budget.weeklyUsd",
    "usage.ui.layout",
    "usage.ui.state",
    "sessions.staleAfterDays",
    "sessions.recentDays",
    "hygiene.enabled",
    "hygiene.thresholdTokens",
    "docs.convert",
    "docs.maxBytes",
    "docs.timeoutSeconds",
    "docs.maxOutputChars",
    "docs.maxMemoryMb",
    "docs.displayChars",
    "docs.anydocPath",
    "code.briefFactsTokens",
    "launcher.shortName",
    "systemPrompt.enabled",
    "attribution.enabled",
    "hooks.proseLint.mode",
    "hooks.policyGuard.enabled",
    "update.mode",
    "update.checkIntervalHours",
    "update.plugin",
    "watch.enabled",
    "watch.repos",
    "watch.autonomy",
    "watch.pollSeconds",
    "watch.maxCatchupHours",
    "watch.allowedAuthors",
    "watch.tokenEnv",
    "watch.limits.actionsPerDay",
    "watch.limits.usdPerDay",
    "gc.enabled",
];

/// Keys that were removed. A stored value stays in the store, is never read,
/// and `config import` skips it (one release of grace). A key that became a
/// built-in constant is listed in [`FIXED_KEYS`] instead.
pub const RETIRED_KEYS: &[&str] = &[
    "hooks.rmGuard.allowedRoots",
    "sessions.autoClose",
    "launcher.claudeOverride",
    "agents.variants",
];

/// Keys that became built-in constants, each with the value the constant now
/// has. They behave as [`RETIRED_KEYS`] do, and the `fixed-config-keys`
/// migration reports a stored value that differs. Cutting another key to a
/// constant adds one row here.
pub static FIXED_KEYS: LazyLock<Vec<(&'static str, Value)>> = LazyLock::new(|| {
    vec![
        ("gc.graceDays", json!(7)),
        ("gc.handoffGraceDays", json!(14)),
        ("gc.sessionDirIdleDays", json!(14)),
        ("gc.logMaxDays", json!(30)),
        ("gc.logMaxMiB", json!(5)),
        ("gc.leftoverDays", json!(90)),
        ("gc.gateDbIdleDays", json!(30)),
        ("gc.buildOutputIdleDays", json!(7)),
        ("gc.buildOutputPressureIdleDays", json!(1)),
        ("gc.buildOutputActiveMinutes", json!(15)),
        ("gc.freeSpaceWarnGiB", json!(10)),
        ("gc.freeSpaceWarnPercent", json!(5)),
    ]
});

/// Whether `key` was removed: a stored value is never read, `config set`
/// refuses it and `config import` skips it.
pub fn is_retired(key: &str) -> bool {
    RETIRED_KEYS.contains(&key) || FIXED_KEYS.iter().any(|(fixed, _)| *fixed == key)
}

/// The per-user settings, which live only in the global tier.
pub const USER_KEYS: &[&str] = &[
    "update.mode",
    "update.checkIntervalHours",
    "update.plugin",
    "systemPrompt.enabled",
    "attribution.enabled",
    "hooks.policyGuard.enabled",
    "watch.enabled",
    "watch.repos",
    "watch.pollSeconds",
    "watch.maxCatchupHours",
    "watch.tokenEnv",
    "watch.limits.actionsPerDay",
    "watch.limits.usdPerDay",
    "usage.telemetry",
];

/// Whether `key` is a per-user setting. `config set` writes it to the global
/// tier without `--global` and refuses `--org`, and `config import` refuses
/// it outside the global section.
pub fn is_user_key(key: &str) -> bool {
    USER_KEYS.contains(&key)
}

/// The only keys that may sync between one user's machines: plain
/// preferences that mean the same on every machine. A key missing from this
/// list stays local, so a new key never leaks by default.
pub const SHARED_KEYS: &[&str] = &[
    "usage.theme",
    "usage.maskAccount",
    "usage.weekStart",
    "usage.timeZone",
    "usage.budget.monthlyUsd",
    "usage.budget.weeklyUsd",
    "usage.ui.layout",
    "maxEffortLevel",
    "effort.auto",
    "routing.escalate",
    "auto.budgetUsd",
    "auto.agentBudgetUsd",
    "auto.sessionCeilingUsd",
    "auto.warnPct",
    "pr.draft",
    "commit.signOff",
];

/// Every other known key. Paths, hosts, token names, machine state, safety
/// and automation switches, and per-repo rules stay on the machine that set
/// them. A test requires each known key in exactly one of the two lists.
pub const LOCAL_ONLY_KEYS: &[&str] = &[
    "autoReview.enabled",
    "autoReview.type",
    "autoReview.fix",
    "autoMerge.enabled",
    "handoff.onClear",
    "pr.rules.template",
    "pr.rules.length",
    "pr.rules.issue",
    "pr.rules.ticketLine",
    "pr.rules.scope",
    "pr.bodyMaxChars",
    "pr.ticketPatterns",
    "worktreeCleanup.enabled",
    "worktreeCleanup.staleAfterDays",
    "worktreeCleanup.conflictGracePeriodDays",
    "mode",
    "memory.source",
    "models.haiku",
    "models.sonnet",
    "models.opus",
    "gate.maxRounds",
    "fix.maxFiles",
    "fix.maxLines",
    "review.stackMaxPrs",
    "review.stackMaxLines",
    "review.suggest.deepLines",
    "review.suggest.quickLines",
    "review.suggest.deepFiles",
    "review.suggest.packageSpan",
    "review.suggest.untestedLines",
    "security.defaults",
    "usage.refreshSeconds",
    "usage.telemetry",
    "usage.telemetryRetentionDays",
    "usage.telemetryMaxRows",
    "usage.autoIngest",
    "usage.autoIngestMinutes",
    "usage.ui.state",
    "sessions.staleAfterDays",
    "sessions.recentDays",
    "hygiene.enabled",
    "hygiene.thresholdTokens",
    "docs.convert",
    "docs.maxBytes",
    "docs.timeoutSeconds",
    "docs.maxOutputChars",
    "docs.maxMemoryMb",
    "docs.displayChars",
    "docs.anydocPath",
    "code.briefFactsTokens",
    "launcher.shortName",
    "systemPrompt.enabled",
    "attribution.enabled",
    "hooks.proseLint.mode",
    "hooks.policyGuard.enabled",
    "update.mode",
    "update.checkIntervalHours",
    "update.plugin",
    "watch.enabled",
    "watch.repos",
    "watch.autonomy",
    "watch.pollSeconds",
    "watch.maxCatchupHours",
    "watch.allowedAuthors",
    "watch.tokenEnv",
    "watch.limits.actionsPerDay",
    "watch.limits.usdPerDay",
    "gc.enabled",
];

/// Keys that start with this prefix never sync, whatever `SHARED_KEYS` holds.
pub const SYNC_EXCLUDED_PREFIX: &str = "sync.";

/// Whether `key` may sync between machines: in [`SHARED_KEYS`] and not a
/// `sync.*` key.
pub fn is_shared(key: &str) -> bool {
    !key.starts_with(SYNC_EXCLUDED_PREFIX) && SHARED_KEYS.contains(&key)
}

/// The component kinds an `effort.<kind>.<name>` key can name.
pub const EFFORT_COMPONENT_KINDS: [&str; 3] = ["agents", "commands", "skills"];

/// Whether `key` is `effort.<kind>.<name>`: a known kind and a name of
/// lowercase letters, digits and hyphens. Whether the name exists is checked
/// later against the plugin files, never here.
fn is_effort_component_key(key: &str) -> bool {
    let mut parts = key.split('.');
    let (Some("effort"), Some(kind), Some(name), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    EFFORT_COMPONENT_KINDS.contains(&kind)
        && !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Whether `key` is `watch.rule.<id>`: a rule id of lowercase letters, digits
/// and hyphens. The value is the rule as JSON text, checked when it is set.
pub fn is_watch_rule_key(key: &str) -> bool {
    key.strip_prefix("watch.rule.")
        .is_some_and(crate::watch::rule::is_rule_id)
}

/// The default value for a known key, or `None` if `key` is not in
/// `KNOWN_KEYS`.
pub fn default_value(key: &str) -> Option<Value> {
    if is_effort_component_key(key) {
        return Some(Value::String("auto".to_string()));
    }
    if is_watch_rule_key(key) {
        return Some(Value::String(String::new()));
    }
    match key {
        "autoReview.enabled" => Some(Value::Bool(true)),
        "autoReview.type" => Some(Value::String("auto".to_string())),
        "autoReview.fix" => Some(Value::Bool(false)),
        "autoMerge.enabled" => Some(Value::Bool(false)),
        "commit.signOff" => Some(Value::Bool(true)),
        "handoff.onClear" => Some(Value::Bool(false)),
        "pr.draft" => Some(Value::Bool(true)),
        "pr.rules.template" | "pr.rules.ticketLine" => Some(Value::String("block".to_string())),
        "pr.rules.length" | "pr.rules.issue" | "pr.rules.scope" => {
            Some(Value::String("warn".to_string()))
        }
        "pr.bodyMaxChars" => Some(Value::Number(1200.into())),
        "pr.ticketPatterns" => Some(Value::Array(vec![Value::String(
            "[A-Z][A-Z0-9]+-[0-9]+".to_string(),
        )])),
        "worktreeCleanup.enabled" => Some(Value::Bool(true)),
        "worktreeCleanup.staleAfterDays" => Some(Value::Number(30.into())),
        "worktreeCleanup.conflictGracePeriodDays" => Some(Value::Number(90.into())),
        "mode" => Some(Value::String("ask".to_string())),
        "maxEffortLevel" => Some(Value::String("auto".to_string())),
        "effort.auto" => Some(Value::String("off".to_string())),
        "memory.source" => Some(Value::String("both".to_string())),
        "models.haiku" | "models.sonnet" | "models.opus" => Some(Value::String(String::new())),
        "routing.escalate" => Some(Value::String("auto".to_string())),
        "auto.budgetUsd" => Some(Value::Number(AUTO_BUDGET_USD.into())),
        "auto.agentBudgetUsd" => Some(Value::Number(AUTO_AGENT_BUDGET_USD.into())),
        "auto.sessionCeilingUsd" => Some(Value::Number(0.into())),
        "auto.warnPct" => Some(Value::Number(70.into())),
        "gate.maxRounds" => Some(Value::Number(GATE_MAX_ROUNDS_DEFAULT.into())),
        "fix.maxFiles" => Some(Value::Number(3.into())),
        "fix.maxLines" => Some(Value::Number(500.into())),
        "review.stackMaxPrs" => Some(Value::Number(6.into())),
        "review.stackMaxLines" => Some(Value::Number(3000.into())),
        "review.suggest.deepLines" => Some(Value::Number(400.into())),
        "review.suggest.quickLines" => Some(Value::Number(60.into())),
        "review.suggest.deepFiles" => Some(Value::Number(25.into())),
        "review.suggest.packageSpan" => Some(Value::Number(3.into())),
        "review.suggest.untestedLines" => Some(Value::Number(150.into())),
        "security.defaults" => Some(Value::Bool(false)),
        "usage.theme" => Some(Value::String("default".to_string())),
        "usage.maskAccount" => Some(Value::Bool(false)),
        "usage.weekStart" => Some(Value::String("monday".to_string())),
        "usage.timeZone" => Some(Value::String("local".to_string())),
        "usage.refreshSeconds" => Some(Value::Number(5.into())),
        "usage.telemetry" => Some(Value::String("auto".to_string())),
        "usage.autoIngest" => Some(Value::Bool(true)),
        "usage.autoIngestMinutes" => Some(Value::Number(10.into())),
        "usage.telemetryRetentionDays" => Some(Value::Number(90.into())),
        "usage.telemetryMaxRows" => Some(Value::Number(500_000.into())),
        "usage.budget.monthlyUsd" | "usage.budget.weeklyUsd" => Some(Value::Number(0.into())),
        "usage.ui.layout" | "usage.ui.state" => Some(Value::String(String::new())),
        "sessions.staleAfterDays" => Some(Value::Number(3.into())),
        "sessions.recentDays" => Some(Value::Number(7.into())),
        "hygiene.enabled" => Some(Value::Bool(true)),
        "hygiene.thresholdTokens" => Some(Value::Number(250_000.into())),
        "docs.convert" => Some(Value::String("auto".to_string())),
        "docs.maxBytes" => Some(Value::Number(26_214_400.into())),
        "docs.timeoutSeconds" => Some(Value::Number(30.into())),
        "docs.maxOutputChars" => Some(Value::Number(200_000.into())),
        "docs.maxMemoryMb" => Some(Value::Number(1024.into())),
        "docs.displayChars" => Some(Value::Number(24_000.into())),
        "docs.anydocPath" => Some(Value::String(String::new())),
        "code.briefFactsTokens" => Some(Value::Number(0.into())),
        "launcher.shortName" => Some(Value::Bool(false)),
        "systemPrompt.enabled" => Some(Value::Bool(true)),
        "attribution.enabled" => Some(Value::Bool(false)),
        "hooks.proseLint.mode" => Some(Value::String("warn".to_string())),
        "hooks.policyGuard.enabled" => Some(Value::Bool(false)),
        "update.mode" => Some(Value::String("notify".to_string())),
        "update.checkIntervalHours" => Some(Value::Number(24.into())),
        "update.plugin" => Some(Value::String("auto".to_string())),
        "watch.enabled" => Some(Value::Bool(false)),
        "watch.repos" | "watch.allowedAuthors" => Some(Value::Array(Vec::new())),
        "watch.autonomy" => Some(Value::String("propose".to_string())),
        "watch.pollSeconds" => Some(Value::Number(60.into())),
        "watch.maxCatchupHours" => Some(Value::Number(72.into())),
        "watch.tokenEnv" => Some(Value::String(String::new())),
        "watch.limits.actionsPerDay" => Some(Value::Number(50.into())),
        "watch.limits.usdPerDay" => Some(Value::Number(5.into())),
        "gc.enabled" => Some(Value::Bool(true)),
        _ => None,
    }
}

/// The fixed set of string values `key` accepts, or `None` if `key` has no
/// enum constraint (including an unknown key).
pub fn allowed_enum_values(key: &str) -> Option<&'static [&'static str]> {
    if is_effort_component_key(key) {
        return Some(&["auto", "low", "medium", "high", "xhigh", "max"]);
    }
    match key {
        "autoReview.type" => Some(&["quick", "deep", "auto"]),
        "mode" => Some(&["ask", "auto"]),
        "maxEffortLevel" => Some(&["auto", "low", "medium", "high", "xhigh", "max"]),
        "memory.source" => Some(&["both", "playbook"]),
        "effort.auto" => Some(&["off", "suggest", "on"]),
        "routing.escalate" => Some(&["ask", "auto", "deny"]),
        "usage.theme" => Some(&["default", "spectrum", "dark", "light", "mono"]),
        "usage.weekStart" => Some(&["monday", "sunday", "saturday"]),
        "usage.timeZone" => Some(&["local", "utc"]),
        "usage.telemetry" => Some(&["auto", "off"]),
        "docs.convert" => Some(&["auto", "off"]),
        "hooks.proseLint.mode" => Some(&["warn", "block", "off"]),
        "update.mode" => Some(&["notify", "auto", "off"]),
        "update.plugin" => Some(&["auto", "ask", "off"]),
        "watch.autonomy" => Some(&["off", "propose", "auto"]),
        "pr.rules.template"
        | "pr.rules.length"
        | "pr.rules.issue"
        | "pr.rules.ticketLine"
        | "pr.rules.scope" => Some(&["off", "warn", "block"]),
        _ => None,
    }
}

/// The problem with a `models.<tier>` value, or `None` when it is fine.
/// Fine is empty (no override) or `claude-<tier>-<major>[-<minor>]`, so a
/// `models.sonnet` value can only name a Sonnet model. `None` for other keys.
pub fn model_override_error(key: &str, value: &str) -> Option<String> {
    let tier = key.strip_prefix("models.")?;
    if !["haiku", "sonnet", "opus"].contains(&tier) || value.is_empty() {
        return None;
    }
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    let valid = value
        .strip_prefix(&format!("claude-{tier}-"))
        .is_some_and(|rest| {
            let parts: Vec<&str> = rest.split('-').collect();
            (1..=2).contains(&parts.len()) && parts.iter().all(|p| digits(p))
        });
    if valid {
        return None;
    }
    Some(format!(
        "expected empty or claude-{tier}-<major>[-<minor>], for example claude-{tier}-5-5"
    ))
}

/// Keys that predate the shape rule and keep their name until renamed.
#[cfg(test)]
const LEGACY_KEY_NAMES: &[&str] = &[
    "autoReview.enabled",
    "autoReview.type",
    "autoReview.fix",
    "autoMerge.enabled",
    "pr.bodyMaxChars",
    "worktreeCleanup.enabled",
    "worktreeCleanup.staleAfterDays",
    "worktreeCleanup.conflictGracePeriodDays",
    "maxEffortLevel",
    "auto.agentBudgetUsd",
    "auto.sessionCeilingUsd",
    "review.stackMaxPrs",
    "review.stackMaxLines",
    "usage.telemetryRetentionDays",
    "usage.telemetryMaxRows",
    "usage.autoIngestMinutes",
    "sessions.staleAfterDays",
    "docs.maxOutputChars",
    "docs.maxMemoryMb",
    "code.briefFactsTokens",
    "systemPrompt.enabled",
    "update.checkIntervalHours",
    "watch.maxCatchupHours",
    "watch.limits.actionsPerDay",
    "watch.limits.usdPerDay",
];

/// Non-leaf segments that are allowed to be camelCase.
#[cfg(test)]
const COMPOUND_GROUPS: &[&str] = &["proseLint", "policyGuard"];

/// The exact size of `KNOWN_KEYS`; each Segment that cuts keys lowers it.
#[cfg(test)]
const KEY_CEILING: usize = 84;

#[cfg(test)]
mod tests {

    const MACHINE_BOUND_KEYS: &[&str] = &[
        "docs.anydocPath",
        "launcher.shortName",
        "watch.allowedAuthors",
        "watch.repos",
        "watch.tokenEnv",
        "usage.ui.state",
    ];

    #[test]
    fn every_known_key_is_classified_shared_or_local() {
        // Arrange
        let mut classified: Vec<&str> =
            SHARED_KEYS.iter().chain(LOCAL_ONLY_KEYS).copied().collect();
        let mut known: Vec<&str> = KNOWN_KEYS.to_vec();

        // Act
        classified.sort_unstable();
        known.sort_unstable();

        // Assert
        assert_eq!(classified, known);
    }

    #[test]
    fn shared_and_local_lists_do_not_overlap() {
        // Arrange
        let local: std::collections::HashSet<&str> = LOCAL_ONLY_KEYS.iter().copied().collect();

        // Act
        let both: Vec<&&str> = SHARED_KEYS.iter().filter(|k| local.contains(**k)).collect();

        // Assert
        assert!(both.is_empty(), "classified twice: {both:?}");
    }

    #[test]
    fn no_sync_or_machine_bound_key_is_shared() {
        // Arrange
        let forbidden = |key: &&&str| {
            key.starts_with(SYNC_EXCLUDED_PREFIX) || MACHINE_BOUND_KEYS.contains(*key)
        };

        // Act
        let leaked: Vec<&&str> = SHARED_KEYS.iter().filter(forbidden).collect();

        // Assert
        assert!(leaked.is_empty(), "must stay local: {leaked:?}");
    }

    #[test]
    fn is_shared_rejects_sync_keys_and_unclassified_keys() {
        // Arrange
        let cases = [
            ("usage.theme", true),
            ("sync.url", false),
            ("docs.anydocPath", false),
            ("not.a.key", false),
        ];

        // Act and Assert
        for (key, expected) in cases {
            assert_eq!(is_shared(key), expected, "{key}");
        }
    }
    use super::*;

    fn follows_shape_rule(key: &str) -> bool {
        let segments: Vec<&str> = key.split('.').collect();
        let Some((leaf, groups)) = segments.split_last() else {
            return false;
        };
        let leaf_shape = regex::Regex::new(r"^[a-z]+([A-Z][a-z]+)?$").unwrap();
        let group_shape = regex::Regex::new(r"^[a-z]+$").unwrap();
        segments.len() <= 4
            && leaf_shape.is_match(leaf)
            && groups
                .iter()
                .all(|group| group_shape.is_match(group) || COMPOUND_GROUPS.contains(group))
    }

    #[test]
    fn the_shape_rule_rejects_camel_case_groups_and_long_leaves() {
        for key in [
            "review.stackMaxLines",
            "autoReview.enabled",
            "maxEffortLevel",
        ] {
            // Arrange
            // Act
            let ok = follows_shape_rule(key);
            // Assert
            assert!(!ok, "{key}");
        }
    }

    #[test]
    fn the_shape_rule_accepts_lowercase_groups_and_one_hump_leaves() {
        for key in ["review.stack.maxLines", "hooks.proseLint.mode", "mode"] {
            // Arrange
            // Act
            let ok = follows_shape_rule(key);
            // Assert
            assert!(ok, "{key}");
        }
    }

    #[test]
    fn key_names_follow_the_shape_rule() {
        // Arrange
        let mut legacy: Vec<&str> = LEGACY_KEY_NAMES.to_vec();
        // Act
        let mut violators: Vec<&str> = KNOWN_KEYS
            .iter()
            .copied()
            .filter(|key| !follows_shape_rule(key))
            .collect();
        // Assert
        violators.sort_unstable();
        legacy.sort_unstable();
        assert_eq!(violators, legacy);
    }

    #[test]
    fn known_key_count_matches_the_ceiling() {
        // Arrange
        // Act
        let count = KNOWN_KEYS.len();
        // Assert
        assert_eq!(count, KEY_CEILING);
    }

    #[test]
    fn the_retired_rm_guard_key_is_no_longer_known() {
        for key in RETIRED_KEYS
            .iter()
            .copied()
            .chain(FIXED_KEYS.iter().map(|(key, _)| *key))
        {
            assert!(!KNOWN_KEYS.contains(&key), "{key}");
            assert_eq!(default_value(key), None, "{key}");
            assert!(is_retired(key), "{key}");
        }
    }

    #[test]
    fn import_skips_a_retired_key_and_set_refuses_it() {
        let home = crate::common::test_support::scratch_dir("retired-key");
        let root = crate::common::paths::playbook_root_from(&home);
        std::fs::create_dir_all(&root).unwrap();
        let doc = serde_json::json!({"global": {"hooks.rmGuard.allowedRoots": ["~/a/b"], "usage.theme": "dark"}});
        assert_eq!(super::super::store::import(&root, &doc).unwrap(), 1);
        let set = super::super::write::set(
            super::super::write::Tier::Global,
            "hooks.rmGuard.allowedRoots",
            &Value::Array(Vec::new()),
            &home,
            None,
        );
        assert!(set.is_err());
    }

    #[test]
    fn the_auto_budgets_default_to_the_measured_caps_and_the_ceiling_to_zero() {
        assert_eq!(
            default_value("auto.sessionCeilingUsd"),
            Some(Value::Number(0.into()))
        );
        assert_eq!(
            default_value("auto.budgetUsd"),
            Some(Value::Number(200.into()))
        );
        assert_eq!(
            default_value("auto.agentBudgetUsd"),
            Some(Value::Number(50.into()))
        );
    }

    #[test]
    fn update_plugin_defaults_to_auto_and_takes_three_values() {
        // Arrange
        let key = "update.plugin";

        // Act
        let default = default_value(key);
        let allowed = allowed_enum_values(key);

        // Assert
        assert_eq!(default, Some(Value::String("auto".to_string())));
        assert_eq!(allowed, Some(&["auto", "ask", "off"][..]));
    }

    #[test]
    fn routing_escalate_defaults_to_auto_and_allows_three_values() {
        // Arrange
        let key = "routing.escalate";

        // Act
        let default = default_value(key);
        let allowed = allowed_enum_values(key);

        // Assert
        assert_eq!(default, Some(Value::String("auto".to_string())));
        assert_eq!(allowed, Some(&["ask", "auto", "deny"][..]));
    }

    #[test]
    fn the_time_zone_key_is_known_and_accepts_local_and_utc() {
        // Act
        let known = KNOWN_KEYS.contains(&"usage.timeZone");
        let allowed = allowed_enum_values("usage.timeZone");
        let default = default_value("usage.timeZone");

        // Assert
        assert!(known);
        assert_eq!(allowed, Some(&["local", "utc"][..]));
        assert_eq!(default, Some(Value::String("local".to_string())));
    }

    #[test]
    fn the_theme_key_accepts_only_the_current_names() {
        let allowed = allowed_enum_values("usage.theme").unwrap();
        assert!(allowed.contains(&"default") && !allowed.contains(&"solarized"));
        assert!(allowed.contains(&"spectrum"));
    }

    #[test]
    fn model_overrides_must_name_a_model_of_their_own_tier() {
        assert_eq!(model_override_error("models.sonnet", ""), None);
        assert_eq!(
            model_override_error("models.sonnet", "claude-sonnet-5-5"),
            None
        );
        assert_eq!(model_override_error("models.opus", "claude-opus-5"), None);
        assert!(model_override_error("models.sonnet", "claude-opus-5").is_some());
        assert!(model_override_error("models.sonnet", "claude-sonnet-x").is_some());
        assert!(model_override_error("models.sonnet", "sonnet").is_some());
        assert!(model_override_error("models.sonnet", "claude-sonnet-5-5-1").is_some());
        assert_eq!(model_override_error("mode", "anything"), None);
    }

    #[test]
    fn the_stack_review_limits_are_known_keys_with_defaults() {
        assert!(KNOWN_KEYS.contains(&"review.stackMaxPrs"));
        assert!(KNOWN_KEYS.contains(&"review.stackMaxLines"));
        assert_eq!(default_value("review.stackMaxPrs"), Some(Value::from(6)));
        assert_eq!(
            default_value("review.stackMaxLines"),
            Some(Value::from(3000))
        );
    }

    #[test]
    fn effort_auto_is_off_by_default_and_takes_three_values() {
        assert!(KNOWN_KEYS.contains(&"effort.auto"));
        assert_eq!(default_value("effort.auto"), Some(Value::from("off")));
        assert_eq!(
            allowed_enum_values("effort.auto"),
            Some(&["off", "suggest", "on"][..])
        );
        assert!(!is_effort_component_key("effort.auto"));
    }

    #[test]
    fn security_defaults_is_a_known_key_that_is_off_by_default() {
        assert!(KNOWN_KEYS.contains(&"security.defaults"));
        assert_eq!(default_value("security.defaults"), Some(Value::Bool(false)));
        assert_eq!(allowed_enum_values("security.defaults"), None);
    }

    #[test]
    fn default_value_for_auto_review_enabled_is_true() {
        // Arrange
        let key = "autoReview.enabled";

        // Act
        let result = default_value(key);

        // Assert
        assert_eq!(result, Some(Value::Bool(true)));
    }

    #[test]
    fn default_value_for_auto_review_type_is_auto() {
        // Arrange
        let key = "autoReview.type";

        // Act
        let result = default_value(key);

        // Assert
        assert_eq!(result, Some(Value::String("auto".to_string())));
    }

    #[test]
    fn effort_component_keys_are_a_validated_family() {
        assert!(is_effort_component_key("effort.agents.fact-checker"));
        assert!(is_effort_component_key("effort.commands.review"));
        assert!(is_effort_component_key("effort.skills.writing-style"));
        assert!(!is_effort_component_key("effort.agent.reviewer"));
        assert!(!is_effort_component_key("effort.agents."));
        assert!(!is_effort_component_key("effort.agents.Reviewer"));
        assert!(!is_effort_component_key("effort.agents.a.b"));
        assert!(!is_effort_component_key("effort.agents.-x"));
        assert_eq!(
            default_value("effort.agents.reviewer"),
            Some(Value::String("auto".to_string()))
        );
        assert_eq!(
            allowed_enum_values("effort.skills.x").map(<[&str]>::len),
            Some(6)
        );
    }

    #[test]
    fn default_value_for_unknown_key_is_none() {
        // Arrange
        let key = "notAKnownKey";

        // Act
        let result = default_value(key);

        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn allowed_enum_values_for_auto_review_type_is_quick_deep_or_auto() {
        // Arrange
        let key = "autoReview.type";

        // Act
        let result = allowed_enum_values(key);

        // Assert
        assert_eq!(result, Some(["quick", "deep", "auto"].as_slice()));
    }

    #[test]
    fn allowed_enum_values_for_auto_review_enabled_is_none() {
        // Arrange
        let key = "autoReview.enabled";

        // Act
        let result = allowed_enum_values(key);

        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn allowed_enum_values_for_unknown_key_is_none() {
        // Arrange
        let key = "notAKnownKey";

        // Act
        let result = allowed_enum_values(key);

        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn pr_draft_defaults_to_true_with_no_enum_constraint() {
        // Arrange
        let key = "pr.draft";

        // Act
        let default = default_value(key);
        let allowed = allowed_enum_values(key);

        // Assert
        assert_eq!(default, Some(Value::Bool(true)));
        assert_eq!(allowed, None);
    }

    #[test]
    fn default_values_for_the_pr_and_commit_settings() {
        // Arrange
        let cases = [
            ("autoReview.fix", false),
            ("autoMerge.enabled", false),
            ("commit.signOff", true),
            ("pr.draft", true),
        ];

        for (key, expected) in cases {
            // Act
            let result = default_value(key);

            // Assert
            assert_eq!(result, Some(Value::Bool(expected)), "{key}");
        }
    }

    #[test]
    fn the_pr_and_commit_settings_are_known_keys_with_no_enum_constraint() {
        // Arrange
        let keys = [
            "autoReview.fix",
            "autoMerge.enabled",
            "commit.signOff",
            "pr.draft",
        ];

        for key in keys {
            // Act
            let known = KNOWN_KEYS.contains(&key);
            let allowed = allowed_enum_values(key);

            // Assert
            assert!(known, "{key} must be listed in KNOWN_KEYS");
            assert_eq!(allowed, None, "{key}");
        }
    }

    #[test]
    fn gc_enabled_is_a_boolean_defaulting_to_true() {
        assert!(KNOWN_KEYS.contains(&"gc.enabled"));
        assert_eq!(default_value("gc.enabled"), Some(Value::Bool(true)));
    }
}
