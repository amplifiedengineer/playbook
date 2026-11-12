// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook mode status --json --command NAME --args ARGS`: what a command's
//! Step 0 needs in one call. It reads `--auto` and `--ask` from the command's
//! arguments, resolves the run mode, and (when a command name is given) the
//! effort ceiling for that command.

use crate::common::mode::Mode;
use serde_json::{json, Value};

/// Which of `--auto` and `--ask` the argument string carries. `--auto-design`
/// (the planning command's own flag) counts as `--auto`.
pub fn flags(args: &str) -> (bool, bool) {
    let has = |flag: &str| args.split_whitespace().any(|w| w == flag);
    (has("--auto") || has("--auto-design"), has("--ask"))
}

/// The JSON for Step 0: `mode`, `source`, `hook_mode`, `warning`, and
/// `ceiling` (null when there is none or it cannot be read). `ceiling` is
/// left out when no command name was given. `Err` is a conflict between the
/// two flags.
pub fn run_context(
    args: &str,
    ceiling: impl FnOnce() -> Option<String>,
    named: bool,
) -> Result<String, String> {
    let flag = match flags(args) {
        (true, true) => return Err("--auto and --ask conflict; pass one.".to_string()),
        (true, false) => Some(Mode::Auto),
        (false, true) => Some(Mode::Ask),
        (false, false) => None,
    };
    let mut out: Value = serde_json::from_str(&super::run_status(flag, true))
        .map_err(|e| format!("mode status returned bad JSON: {e}"))?;
    if named {
        out["ceiling"] = json!(ceiling());
    }
    Ok(out.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flags_are_read_as_whole_words() {
        assert_eq!(flags(""), (false, false));
        assert_eq!(flags("12 --auto"), (true, false));
        assert_eq!(flags("--ask 12"), (false, true));
        assert_eq!(flags("--auto --ask"), (true, true));
        assert_eq!(flags("--automatic --asked feat/auto"), (false, false));
        assert_eq!(flags("idea --auto-design"), (true, false));
    }

    #[test]
    fn both_flags_conflict() {
        let err = run_context("--auto --ask", || None, true).unwrap_err();
        assert_eq!(err, "--auto and --ask conflict; pass one.");
    }

    #[test]
    fn the_ceiling_is_only_resolved_when_a_command_is_named() {
        let named = run_context("", || Some("medium".into()), true).unwrap();
        let v: Value = serde_json::from_str(&named).unwrap();
        assert_eq!(v["ceiling"], "medium");
        for key in ["mode", "source", "hook_mode", "warning"] {
            assert!(v.get(key).is_some(), "{key}");
        }
        let unnamed = run_context("", || panic!("must not resolve"), false).unwrap();
        let v: Value = serde_json::from_str(&unnamed).unwrap();
        assert!(v.get("ceiling").is_none());
    }

    #[test]
    fn an_unreadable_ceiling_is_null() {
        let v: Value = serde_json::from_str(&run_context("", || None, true).unwrap()).unwrap();
        assert!(v["ceiling"].is_null());
    }
}
