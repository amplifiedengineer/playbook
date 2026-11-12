// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Reads the settings.json shapes `doctor check` and the migrations need:
//! the `.statusLine.command` string, every hook `.command` nested under
//! `.hooks`, and an exact per-guard command count scoped to one event.
//!
//! Deliberately not a general JSON query: each function names the one shape it
//! reads. A call site needing a different field gets its own function here, not
//! a flag on a generic getter.

use serde_json::Value;
use std::path::Path;

/// Reads `.statusLine.command` from a `settings.json`-shaped file. Empty on
/// any failure: unreadable file, invalid JSON, a missing field or a non-string
/// value (where `jq -r` would print a number raw).
pub fn statusline_command(path: &Path) -> String {
    string_field(path, &["statusLine", "command"])
}

/// Every hook `.command` string nested under `settings.json`'s `.hooks`,
/// across every event and matcher group: `.hooks | to_entries[]? |
/// .value[]? | .hooks[]?.command // empty`, the same traversal
/// `commands/doctor.md`'s Layer 7 used to run through `jq -r`. Empty on any
/// failure (unreadable file, invalid JSON, `.hooks` missing or not an
/// object), matching `jq`'s own `?` operators, which swallow a shape
/// mismatch at each step rather than erroring. Order matches file order:
/// `serde_json::Value`'s object map preserves insertion order (the `preserve_order`
/// feature this crate already depends on), the same order `jq` walks a
/// `to_entries` result in.
///
/// Diverges from `jq -r` on two edge cases, neither reachable from a real
/// `settings.json`: a `.command` value that exists but is not a string (a
/// number or bool) is skipped here, where `jq -r` would print its raw text
/// form, the same documented tradeoff [`plugin_version`] and
/// [`statusline_command`] already accept; and a `.hooks` that is a JSON
/// array rather than an object is treated as absent here (`and_then`'s
/// `as_object` returns `None`), where jq's `to_entries` is defined over
/// array indices too and would still walk it. Real `settings.json` always
/// shapes `.hooks` as an object keyed by event name, so this never fires on
/// a real install either.
pub fn hook_commands(path: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    let Some(hooks) = value.get("hooks").and_then(Value::as_object) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for event_value in hooks.values() {
        let Some(matcher_groups) = event_value.as_array() else {
            continue;
        };
        out.extend(commands_in_matcher_groups(matcher_groups));
    }
    out
}

/// Counts, for each guard name in `guards`, how many hook `.command` entries
/// wired under one `settings.json` event equal `"playbook hook <guard>"`
/// exactly. Mirrors the per-guard `jq` loop `commands/doctor.md`'s Layer 2
/// block runs once per guard: `--arg event ... --arg cmd ...
/// '[.hooks[$event][]?.hooks[]?.command // ""] | map(select(. == $cmd)) |
/// length'`. Guard names are bare (e.g. `rm-workspace-guard`); the `"playbook
/// hook "` prefix is added internally so callers pass the same names
/// `settings.json`'s wired commands use. Same empty/zero-on-any-shape-mismatch
/// behavior as [`hook_commands`]. Returned pairs keep the input `guards`
/// order.
///
/// Diverges from `jq -r` on one edge case: when `.hooks` is a non-null
/// scalar (a string, say), bracket-indexing it hard-errors in real `jq`
/// (exit 5, empty stdout) rather than degrading gracefully, where this
/// returns zero counts. Same unrealistic-shape tradeoff [`hook_commands`]
/// already documents.
pub fn hook_commands_for_event(path: &Path, event: &str, guards: &[&str]) -> Vec<(String, usize)> {
    let commands = event_hook_commands(path, event);
    guards
        .iter()
        .map(|guard| {
            let wanted = format!("playbook hook {guard}");
            let count = commands
                .iter()
                .filter(|command| **command == wanted)
                .count();
            (guard.to_string(), count)
        })
        .collect()
}

/// Every hook `.command` string wired under one event in a
/// `settings.json`-shaped file's `.hooks`, the traversal
/// [`hook_commands_for_event`] uses:
/// `.hooks[$event][]?.hooks[]?.command // ""`. Same empty-on-any-shape-mismatch
/// behavior as [`hook_commands`].
fn event_hook_commands(path: &Path, event: &str) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    let Some(matcher_groups) = value
        .get("hooks")
        .and_then(Value::as_object)
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    commands_in_matcher_groups(matcher_groups)
}

/// Every `.command` string under one array of matcher groups (a single
/// event's value in `settings.json`'s `.hooks`): `.[]?.hooks[]?.command`.
/// The one step [`hook_commands`] and [`event_hook_commands`] both repeat
/// once per event they walk.
fn commands_in_matcher_groups(matcher_groups: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    for group in matcher_groups {
        let Some(group_hooks) = group.get("hooks").and_then(Value::as_array) else {
            continue;
        };
        for hook in group_hooks {
            if let Some(command) = hook.get("command").and_then(Value::as_str) {
                out.push(command.to_string());
            }
        }
    }
    out
}

fn string_field(path: &Path, keys: &[&str]) -> String {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return String::new();
    };
    let mut current = &value;
    for key in keys {
        match current.get(key) {
            Some(next) => current = next,
            None => return String::new(),
        }
    }
    current.as_str().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A scratch file holding `contents`, cleaned up on drop.
    struct Fixture {
        path: std::path::PathBuf,
    }

    impl Fixture {
        fn new(tag: &str, contents: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "playbook-doctor-field-{tag}-{}-{n}.json",
                crate::testing::run_id()
            ));
            fs::write(&path, contents).expect("fixture file should be writable");
            Self { path }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    #[test]
    fn statusline_command_reads_the_nested_field() {
        // Arrange
        let f = Fixture::new(
            "statusline-present",
            r#"{"statusLine": {"command": "~/.claude/statusline.sh"}}"#,
        );

        // Act
        let got = statusline_command(&f.path);

        // Assert
        assert_eq!(got, "~/.claude/statusline.sh");
    }

    #[test]
    fn statusline_command_is_empty_when_statusline_key_is_absent() {
        // Arrange
        let f = Fixture::new("statusline-no-key", r#"{"other": true}"#);

        // Act
        let got = statusline_command(&f.path);

        // Assert
        assert_eq!(got, "");
    }

    #[test]
    fn statusline_command_is_empty_when_statusline_is_not_an_object() {
        // Arrange
        let f = Fixture::new(
            "statusline-wrong-shape",
            r#"{"statusLine": "not an object"}"#,
        );

        // Act
        let got = statusline_command(&f.path);

        // Assert
        assert_eq!(got, "");
    }

    #[test]
    fn hook_commands_collects_across_multiple_events_and_matcher_groups() {
        // Arrange
        let f = Fixture::new(
            "hook-commands-multi-event",
            r#"{
                "hooks": {
                    "PreToolUse": [
                        {"matcher": "Write", "hooks": [{"command": "playbook hook preread-edit-check"}]},
                        {"matcher": "Bash", "hooks": [
                            {"command": "/legacy/guard.py"},
                            {"command": "playbook hook bash-guard"}
                        ]}
                    ],
                    "Stop": [
                        {"hooks": [{"command": "playbook hook session-init"}]}
                    ]
                }
            }"#,
        );

        // Act
        let got = hook_commands(&f.path);

        // Assert
        assert_eq!(
            got,
            vec![
                "playbook hook preread-edit-check",
                "/legacy/guard.py",
                "playbook hook bash-guard",
                "playbook hook session-init",
            ]
        );
    }

    #[test]
    fn hook_commands_is_empty_when_hooks_key_is_absent() {
        // Arrange
        let f = Fixture::new("hook-commands-no-key", r#"{"other": true}"#);

        // Act
        let got = hook_commands(&f.path);

        // Assert
        assert!(got.is_empty());
    }

    #[test]
    fn hook_commands_is_empty_when_hooks_is_not_an_object() {
        // Arrange
        let f = Fixture::new("hook-commands-wrong-shape", r#"{"hooks": "not an object"}"#);

        // Act
        let got = hook_commands(&f.path);

        // Assert
        assert!(got.is_empty());
    }

    #[test]
    fn hook_commands_skips_an_event_whose_value_is_not_an_array() {
        // Arrange: a malformed settings.json shouldn't crash the check, just
        // contribute nothing for that one event.
        let f = Fixture::new(
            "hook-commands-event-not-array",
            r#"{"hooks": {"PreToolUse": "not an array"}}"#,
        );

        // Act
        let got = hook_commands(&f.path);

        // Assert
        assert!(got.is_empty());
    }

    #[test]
    fn hook_commands_skips_a_matcher_group_with_no_hooks_array() {
        // Arrange
        let f = Fixture::new(
            "hook-commands-group-no-hooks",
            r#"{"hooks": {"PreToolUse": [{"matcher": "Write"}]}}"#,
        );

        // Act
        let got = hook_commands(&f.path);

        // Assert
        assert!(got.is_empty());
    }

    #[test]
    fn hook_commands_skips_a_non_string_command_value() {
        // Arrange: divergence from `jq -r`, documented on the function; no
        // real settings.json ever has a non-string command.
        let f = Fixture::new(
            "hook-commands-non-string",
            r#"{"hooks": {"PreToolUse": [{"hooks": [{"command": 5}]}]}}"#,
        );

        // Act
        let got = hook_commands(&f.path);

        // Assert
        assert!(got.is_empty());
    }

    #[test]
    fn hook_commands_is_empty_when_file_is_missing() {
        // Arrange
        let path = std::env::temp_dir().join("playbook-doctor-field-hooks-does-not-exist.json");

        // Act
        let got = hook_commands(&path);

        // Assert
        assert!(got.is_empty());
    }

    #[test]
    fn hook_commands_is_empty_on_invalid_json() {
        // Arrange
        let f = Fixture::new("hook-commands-invalid", "not json");

        // Act
        let got = hook_commands(&f.path);

        // Assert
        assert!(got.is_empty());
    }

    #[test]
    fn hook_commands_for_event_counts_one_for_each_wired_guard() {
        // Arrange
        let f = Fixture::new(
            "for-event-all-wired",
            r#"{
                "hooks": {
                    "PreToolUse": [
                        {"hooks": [
                            {"command": "playbook hook rm-workspace-guard"},
                            {"command": "playbook hook bg-await-guard"},
                            {"command": "playbook hook no-slop-guard"},
                            {"command": "playbook hook precommit-check"}
                        ]}
                    ]
                }
            }"#,
        );
        let guards = [
            "rm-workspace-guard",
            "bg-await-guard",
            "no-slop-guard",
            "precommit-check",
        ];

        // Act
        let got = hook_commands_for_event(&f.path, "PreToolUse", &guards);

        // Assert
        assert_eq!(
            got,
            vec![
                ("rm-workspace-guard".to_string(), 1),
                ("bg-await-guard".to_string(), 1),
                ("no-slop-guard".to_string(), 1),
                ("precommit-check".to_string(), 1),
            ]
        );
    }

    #[test]
    fn hook_commands_for_event_counts_zero_for_a_missing_guard() {
        // Arrange
        let f = Fixture::new(
            "for-event-one-missing",
            r#"{
                "hooks": {
                    "PreToolUse": [
                        {"hooks": [
                            {"command": "playbook hook rm-workspace-guard"},
                            {"command": "playbook hook no-slop-guard"},
                            {"command": "playbook hook precommit-check"}
                        ]}
                    ]
                }
            }"#,
        );
        let guards = [
            "rm-workspace-guard",
            "bg-await-guard",
            "no-slop-guard",
            "precommit-check",
        ];

        // Act
        let got = hook_commands_for_event(&f.path, "PreToolUse", &guards);

        // Assert
        assert_eq!(
            got,
            vec![
                ("rm-workspace-guard".to_string(), 1),
                ("bg-await-guard".to_string(), 0),
                ("no-slop-guard".to_string(), 1),
                ("precommit-check".to_string(), 1),
            ]
        );
    }

    #[test]
    fn hook_commands_for_event_only_counts_commands_wired_in_the_named_event() {
        // Arrange: rm-workspace-guard is wired under Stop, not PreToolUse.
        let f = Fixture::new(
            "for-event-scoping",
            r#"{
                "hooks": {
                    "PreToolUse": [{"hooks": [{"command": "playbook hook bg-await-guard"}]}],
                    "Stop": [{"hooks": [{"command": "playbook hook rm-workspace-guard"}]}]
                }
            }"#,
        );
        let guards = ["rm-workspace-guard", "bg-await-guard"];

        // Act
        let got = hook_commands_for_event(&f.path, "PreToolUse", &guards);

        // Assert
        assert_eq!(
            got,
            vec![
                ("rm-workspace-guard".to_string(), 0),
                ("bg-await-guard".to_string(), 1),
            ]
        );
    }

    #[test]
    fn hook_commands_for_event_is_all_zero_when_hooks_key_is_absent() {
        // Arrange
        let f = Fixture::new("for-event-hooks-absent", r#"{"other": true}"#);
        let guards = ["rm-workspace-guard", "bg-await-guard"];

        // Act
        let got = hook_commands_for_event(&f.path, "PreToolUse", &guards);

        // Assert
        assert_eq!(
            got,
            vec![
                ("rm-workspace-guard".to_string(), 0),
                ("bg-await-guard".to_string(), 0),
            ]
        );
    }

    #[test]
    fn hook_commands_for_event_is_all_zero_when_the_named_event_has_no_hooks_wired() {
        // Arrange: hooks exist, just none wired under the requested event, the
        // common case for an event no guard uses yet.
        let f = Fixture::new(
            "for-event-event-key-absent",
            r#"{"hooks": {"Stop": [{"hooks": [{"command": "playbook hook precommit-check"}]}]}}"#,
        );
        let guards = ["rm-workspace-guard", "bg-await-guard"];

        // Act
        let got = hook_commands_for_event(&f.path, "PreToolUse", &guards);

        // Assert
        assert_eq!(
            got,
            vec![
                ("rm-workspace-guard".to_string(), 0),
                ("bg-await-guard".to_string(), 0),
            ]
        );
    }

    #[test]
    fn hook_commands_for_event_is_all_zero_when_hooks_is_not_an_object() {
        // Arrange
        let f = Fixture::new(
            "for-event-hooks-wrong-shape",
            r#"{"hooks": "not an object"}"#,
        );
        let guards = ["rm-workspace-guard"];

        // Act
        let got = hook_commands_for_event(&f.path, "PreToolUse", &guards);

        // Assert
        assert_eq!(got, vec![("rm-workspace-guard".to_string(), 0)]);
    }

    #[test]
    fn hook_commands_for_event_is_all_zero_when_event_value_is_not_an_array() {
        // Arrange
        let f = Fixture::new(
            "for-event-event-not-array",
            r#"{"hooks": {"PreToolUse": "not an array"}}"#,
        );
        let guards = ["rm-workspace-guard"];

        // Act
        let got = hook_commands_for_event(&f.path, "PreToolUse", &guards);

        // Assert
        assert_eq!(got, vec![("rm-workspace-guard".to_string(), 0)]);
    }

    #[test]
    fn hook_commands_for_event_is_all_zero_when_group_has_no_hooks_array() {
        // Arrange
        let f = Fixture::new(
            "for-event-group-no-hooks",
            r#"{"hooks": {"PreToolUse": [{"matcher": "Write"}]}}"#,
        );
        let guards = ["rm-workspace-guard"];

        // Act
        let got = hook_commands_for_event(&f.path, "PreToolUse", &guards);

        // Assert
        assert_eq!(got, vec![("rm-workspace-guard".to_string(), 0)]);
    }

    #[test]
    fn hook_commands_for_event_is_all_zero_when_command_value_is_not_a_string() {
        // Arrange: divergence from `jq -r`, documented on `hook_commands`; no
        // real settings.json ever has a non-string command.
        let f = Fixture::new(
            "for-event-non-string-command",
            r#"{"hooks": {"PreToolUse": [{"hooks": [{"command": 5}]}]}}"#,
        );
        let guards = ["rm-workspace-guard"];

        // Act
        let got = hook_commands_for_event(&f.path, "PreToolUse", &guards);

        // Assert
        assert_eq!(got, vec![("rm-workspace-guard".to_string(), 0)]);
    }
}
