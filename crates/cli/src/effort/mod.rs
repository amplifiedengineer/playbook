// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook effort`: playbook's own ceiling on effort.
//!
//! The config key `effort.max` (`auto`, `low`, `medium`, `high`, `xhigh`,
//! `max`) is playbook's setting. `auto` is
//! the default: playbook sets no ceiling and Claude Code's applies. `max` sets
//! none either, since nothing is above it. Playbook only ever reads Claude
//! Code's key and never writes to its settings files.
//!
//! The effective ceiling is the lower of the two. Claude Code enforces its own
//! `maxEffortLevel` on everything. Playbook's ceiling is applied by the binary
//! itself: to the effort the orchestrator passes when it spawns an agent
//! (`playbook effort resolve`), to the launcher's default `--effort`, and to
//! what `playbook effort` reports. The
//! launcher passes no `--settings`, and playbook never writes a Claude Code
//! settings file, so the ceiling does not cap a command or skill that ships
//! its own `effort` (`LIMIT`).

pub mod auto;
pub mod component;
pub mod floor;
pub mod model_cap;
pub mod patch;
pub mod suggest;

use crate::config::{self, write};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// What the playbook ceiling does not cap. Shown by `playbook effort` and `playbook doctor`.
pub const LIMIT: &str = "playbook's ceiling lowers the effort an orchestrator passes when it spawns an agent, the launcher's default effort and what the binary reports. It does not cap a command or skill that ships its own effort, a spawn that skips the resolve step, or a level you state for the session (--effort, effortLevel, CLAUDE_CODE_EFFORT_LEVEL, /effort), because only Claude Code's maxEffortLevel setting can, and playbook never writes it";

/// The config key.
pub const KEY: &str = "effort.max";

/// Every accepted value: `auto`, then Claude Code's set, lowest first.
pub const LEVELS: [&str; 6] = ["auto", "low", "medium", "high", "xhigh", "max"];

/// Position of `level` from lowest to highest, `None` for `auto` or unknown.
pub(crate) fn rank(level: &str) -> Option<usize> {
    LEVELS.iter().skip(1).position(|l| *l == level)
}

/// Playbook's `auto` and `max` set no ceiling, so they count as unset.
fn playbook_ceiling(level: &str) -> Option<&str> {
    (level != "max" && level != "auto").then_some(level)
}

/// The lower of two levels. An unknown or missing side loses.
pub(crate) fn lower<'a>(a: Option<&'a str>, b: Option<&'a str>) -> Option<&'a str> {
    let ra = a.and_then(|l| rank(l).map(|r| (l, r)));
    let rb = b.and_then(|l| rank(l).map(|r| (l, r)));
    match (ra, rb) {
        (Some((la, ra)), Some((lb, rb))) => Some(if ra <= rb { la } else { lb }),
        (Some((l, _)), None) | (None, Some((l, _))) => Some(l),
        (None, None) => None,
    }
}

/// Who sets the effective ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Winner {
    Playbook,
    ClaudeCode,
    /// Neither sets one, so each command, skill and agent keeps its own effort.
    Neither,
}

/// Both ceilings and the one that applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ceiling {
    pub playbook: String,
    pub claude: Option<String>,
    pub effective: Option<String>,
    pub winner: Winner,
}

impl Ceiling {
    pub fn new(playbook: &str, claude: Option<&str>) -> Self {
        let own = playbook_ceiling(playbook);
        let effective = lower(own, claude).map(str::to_string);
        let winner = match (own.and_then(rank), claude.and_then(rank)) {
            (None, None) => Winner::Neither,
            (Some(_), None) => Winner::Playbook,
            (None, Some(_)) => Winner::ClaudeCode,
            (Some(p), Some(c)) if p < c => Winner::Playbook,
            _ => Winner::ClaudeCode,
        };
        Ceiling {
            playbook: playbook.to_string(),
            claude: claude.map(str::to_string),
            effective,
            winner,
        }
    }
}

/// The configured level, `auto` when no tier sets one.
pub fn configured(home: &Path) -> String {
    match config::resolve(KEY, home, None) {
        Ok((Value::String(level), _)) => level,
        _ => "auto".to_string(),
    }
}

fn cap_in(path: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    v.get("maxEffortLevel")?.as_str().map(str::to_string)
}

/// The project root Claude Code would use for `cwd`: the nearest ancestor
/// holding `.git`, else `cwd` itself.
fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// Claude Code's own `maxEffortLevel`, read from the user, project and local
/// settings files. The lowest one wins, as it does inside Claude Code.
/// Managed settings are not read here: Claude Code applies those itself.
pub fn claude_cap(claude_home: &Path, cwd: &Path) -> Option<String> {
    let root = project_root(cwd);
    let found = [
        claude_home.join("settings.json"),
        root.join(".claude").join("settings.json"),
        root.join(".claude").join("settings.local.json"),
    ]
    .iter()
    .filter_map(|p| cap_in(p))
    .collect::<Vec<_>>();
    found
        .iter()
        .filter_map(|l| rank(l).map(|r| (l, r)))
        .min_by_key(|(_, r)| *r)
        .map(|(l, _)| l.clone())
}

/// Both ceilings for a session started in `cwd`.
pub fn ceiling(home: &Path, claude_home: &Path, cwd: &Path) -> Ceiling {
    Ceiling::new(&configured(home), claude_cap(claude_home, cwd).as_deref())
}

/// `playbook effort <level>`: store playbook's level. Nothing else is written.
pub fn run_set(home: &Path, level: &str) -> Result<String, String> {
    if !LEVELS.contains(&level) {
        return Err(format!(
            "unknown effort level '{level}', use one of: {}",
            LEVELS.join(", ")
        ));
    }
    write::set(
        write::Tier::Global,
        KEY,
        &Value::String(level.to_string()),
        home,
        None,
    )
    .map_err(|err| err.to_string())?;
    Ok(if level == "auto" || level == "max" {
        format!("effort.max set to {level}, playbook sets no ceiling and Claude Code's applies")
    } else {
        format!(
            "playbook effort.max set to {level}. The effort passed to a spawned agent and the launcher's default effort stay at or below it, \
             and never above your Claude Code maxEffortLevel. Claude Code's settings are not changed. {LIMIT}."
        )
    })
}

/// `playbook effort` with no level: both ceilings and the one that wins.
pub fn run_status(home: &Path, claude_home: &Path, cwd: &Path, json: bool) -> String {
    let c = ceiling(home, claude_home, cwd);
    let winner = match c.winner {
        Winner::Playbook => "playbook",
        Winner::ClaudeCode => "claude-code",
        Winner::Neither => "neither",
    };
    if json {
        return serde_json::json!({
            "playbook": c.playbook,
            "claudeCode": c.claude,
            "effective": c.effective,
            "winner": winner,
            "limit": LIMIT,
        })
        .to_string();
    }
    format!(
        "playbook effort.max: {}\nClaude Code maxEffortLevel: {}\neffective ceiling: {} ({winner})",
        c.playbook,
        c.claude.as_deref().unwrap_or("none"),
        c.effective
            .as_deref()
            .unwrap_or("none, shipped defaults apply"),
    ) + &format!("\nlimit: {LIMIT}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;

    fn setup(tag: &str) -> (PathBuf, PathBuf) {
        let home = scratch_dir(tag);
        let claude = home.join(".claude");
        fs::create_dir_all(&claude).unwrap();
        (home, claude)
    }

    #[test]
    fn the_lower_of_two_levels_wins() {
        assert_eq!(lower(Some("xhigh"), Some("max")), Some("xhigh"));
        assert_eq!(lower(Some("xhigh"), Some("medium")), Some("medium"));
        assert_eq!(lower(Some("auto"), Some("high")), Some("high"));
        assert_eq!(lower(Some("low"), None), Some("low"));
        assert_eq!(lower(None, None), None);
    }

    #[test]
    fn xhigh_is_below_max() {
        assert!(rank("xhigh") < rank("max"));
        let c = Ceiling::new("xhigh", Some("max"));
        assert_eq!(c.effective.as_deref(), Some("xhigh"));
        assert_eq!(c.winner, Winner::Playbook);
    }

    #[test]
    fn claude_code_medium_beats_playbook_xhigh() {
        let c = Ceiling::new("xhigh", Some("medium"));
        assert_eq!(c.effective.as_deref(), Some("medium"));
        assert_eq!(c.winner, Winner::ClaudeCode);
    }

    #[test]
    fn equal_levels_leave_it_to_claude_code() {
        assert_eq!(
            Ceiling::new("high", Some("high")).winner,
            Winner::ClaudeCode
        );
    }

    #[test]
    fn auto_defers_to_claude_code_or_to_nobody() {
        assert_eq!(Ceiling::new("auto", Some("low")).winner, Winner::ClaudeCode);
        assert_eq!(
            Ceiling::new("auto", Some("low")).effective.as_deref(),
            Some("low")
        );
        assert_eq!(Ceiling::new("auto", None).winner, Winner::Neither);
        assert_eq!(Ceiling::new("auto", None).effective, None);
    }

    #[test]
    fn max_also_defers_to_claude_code_or_to_nobody() {
        assert_eq!(Ceiling::new("max", Some("low")).winner, Winner::ClaudeCode);
        assert_eq!(Ceiling::new("max", None).winner, Winner::Neither);
        assert_eq!(Ceiling::new("max", None).effective, None);
    }

    #[test]
    fn the_lowest_claude_code_scope_counts() {
        let (home, claude) = setup("effort-scopes");
        let proj = home.join("proj");
        fs::create_dir_all(proj.join(".claude")).unwrap();
        fs::create_dir_all(proj.join(".git")).unwrap();
        fs::write(claude.join("settings.json"), r#"{"maxEffortLevel":"max"}"#).unwrap();
        fs::write(
            proj.join(".claude/settings.local.json"),
            r#"{"maxEffortLevel":"low"}"#,
        )
        .unwrap();
        let sub = proj.join("src");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(claude_cap(&claude, &sub).as_deref(), Some("low"));
    }

    #[test]
    fn setting_a_level_never_touches_claude_code_settings() {
        let (home, claude) = setup("effort-no-write");
        let body = r#"{"maxEffortLevel":"high","theme":"dark"}"#;
        fs::write(claude.join("settings.json"), body).unwrap();
        run_set(&home, "low").unwrap();
        run_set(&home, "max").unwrap();
        assert_eq!(
            fs::read_to_string(claude.join("settings.json")).unwrap(),
            body
        );
    }

    #[test]
    fn status_shows_both_sides_and_the_winner() {
        let (home, claude) = setup("effort-status");
        fs::write(
            claude.join("settings.json"),
            r#"{"maxEffortLevel":"medium"}"#,
        )
        .unwrap();
        run_set(&home, "xhigh").unwrap();
        let s = run_status(&home, &claude, &home, false);
        assert!(s.contains("playbook effort.max: xhigh"), "{s}");
        assert!(s.contains("Claude Code maxEffortLevel: medium"), "{s}");
        assert!(s.contains("effective ceiling: medium (claude-code)"), "{s}");
        let j: Value = serde_json::from_str(&run_status(&home, &claude, &home, true)).unwrap();
        assert_eq!(j["winner"], "claude-code");
        assert_eq!(j["effective"], "medium");
    }

    #[test]
    fn an_unknown_level_is_rejected_before_any_write() {
        let (home, _) = setup("effort-unknown");
        assert!(run_set(&home, "turbo").is_err());
    }
}
