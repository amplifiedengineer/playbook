// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Stage 2 of ADR-0022: a proposed config patch built from the stage 1
//! suggestions, applied only after approval, and reset in one command.
//!
//! Everything here touches playbook's own files only: the config store, and
//! `effort/` under the playbook root (backups, the ledger, the change log).
//! Claude Code's settings and memory are never read for writing.
//!
//! A line is only ever proposed for an agent whose floor is below its
//! effective effort and which is not costly to miss, and its value is the
//! floor. `validate` checks that again at apply time, so a line can never go
//! below the floor, touch a costly agent, or raise anything above what the
//! ceilings allow today.

use super::floor::{floor_of, Floor};
use super::rank;
use super::suggest::{cost_note, Report, Status};
use crate::common::atomic::{atomic_append, write_atomic};
use crate::common::paths::playbook_root_from;
use crate::common::time::iso_utc;
use crate::config::{keys, store, write};
use serde_json::{json, Map, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// One proposed change to the global tier of the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub agent: String,
    /// The config key, `effort.agents.<agent>`.
    pub key: String,
    /// The level to set, the agent's quality floor.
    pub value: String,
    /// The effort the agent runs at today, after every ceiling.
    pub from: String,
    pub shipped: String,
    /// What the key holds now, `auto` when unset.
    pub current: String,
    /// Why, in plain words: counts, the floor and where it came from.
    pub evidence: String,
}

/// The patch for one report.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Patch {
    pub lines: Vec<Line>,
}

/// The agent's own file is never rewritten, so the patch is a global ceiling
/// for the agent. This says so wherever a patch is shown.
pub const SCOPE_NOTE: &str = "A line sets a global ceiling for that agent, so it applies to every project and every diff. Claude Code's settings are not touched.";

fn is_agent_key(key: &str, agent: &str) -> bool {
    key == format!("effort.agents.{agent}")
}

/// The proposal for a report. Only an agent the report marks `lower`, whose
/// floor row exists, is not costly to miss, is below its effective effort and
/// not below that floor, gets a line.
pub fn build(report: &Report) -> Patch {
    let mut lines = Vec::new();
    for a in &report.agents {
        let (Status::Lower, Some(floor)) = (a.status, a.floor) else {
            continue;
        };
        let line = Line {
            agent: a.name.clone(),
            key: format!("effort.agents.{}", a.name),
            value: floor.floor.to_string(),
            from: a.effective.clone(),
            shipped: a.shipped.clone(),
            current: a.configured.clone(),
            evidence: format!(
                "{} dispatches over {} sessions in range {}; floor {} ({}){}",
                a.dispatches,
                a.sessions,
                report.range,
                floor.floor,
                floor.source,
                cost_note(a)
            ),
        };
        if validate(&line).is_ok() {
            lines.push(line);
        }
    }
    Patch { lines }
}

/// Reject a line that is not allowed, whatever produced it.
pub fn validate(line: &Line) -> Result<(), String> {
    let Some(floor) = floor_of(&line.agent) else {
        return Err(format!("{}: no row in the quality floor table", line.agent));
    };
    check(line, floor)
}

fn check(line: &Line, floor: &Floor) -> Result<(), String> {
    if !is_agent_key(&line.key, &line.agent) {
        return Err(format!(
            "{}: only effort.agents.<name> keys are ever patched",
            line.key
        ));
    }
    if floor.costly_to_miss {
        return Err(format!(
            "{}: a missed finding is costly here, so it is never lowered",
            line.agent
        ));
    }
    let (Some(value), Some(floor_rank), Some(from)) =
        (rank(&line.value), rank(floor.floor), rank(&line.from))
    else {
        return Err(format!("{}: unknown effort level", line.agent));
    };
    if value < floor_rank {
        return Err(format!(
            "{}: {} is below the quality floor {}",
            line.agent, line.value, floor.floor
        ));
    }
    if value >= from {
        return Err(format!(
            "{}: {} is not below the current {}, so it would not lower anything and could pass a ceiling",
            line.agent, line.value, line.from
        ));
    }
    Ok(())
}

impl Patch {
    pub fn to_text(&self) -> String {
        if self.lines.is_empty() {
            return "No change is proposed: every agent is at its quality floor, held back by the evidence, or not enough has been used yet. Nothing was written.".to_string();
        }
        let mut out =
            String::from("Proposed config patch (nothing is written until you approve it):\n");
        for l in &self.lines {
            out.push_str(&format!(
                "\n  playbook config set --global {} {}\n    {} now runs at {} (shipped {}, key {}), the floor is {}\n    evidence: {}\n",
                l.key, l.value, l.agent, l.from, l.shipped, l.current, l.value, l.evidence
            ));
        }
        out.push_str(&format!(
            "\n{SCOPE_NOTE}\nApply with `playbook effort apply`, undo with `playbook effort reset`."
        ));
        out
    }

    pub fn to_json(&self) -> Value {
        json!({
            "scope": "global",
            "note": SCOPE_NOTE,
            "lines": self.lines.iter().map(|l| json!({
                "agent": l.agent,
                "key": l.key,
                "value": l.value,
                "from": l.from,
                "shipped": l.shipped,
                "current": l.current,
                "evidence": l.evidence,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Where backups, the ledger and the change log live.
pub fn dir(home: &Path) -> PathBuf {
    playbook_root_from(home).join("effort")
}

fn ledger_path(home: &Path) -> PathBuf {
    dir(home).join("ledger.json")
}

/// The change log, one JSON object per line, append only.
pub fn changes_log_path(home: &Path) -> PathBuf {
    dir(home).join("changes.log")
}

fn global_value(home: &Path, key: &str) -> Result<Option<Value>, String> {
    store::lookup(&playbook_root_from(home), key, None)
        .map(|v| v.global)
        .map_err(|e| e.to_string())
}

fn set_global(home: &Path, key: &str, value: Value) -> Result<(), String> {
    write::set(write::Tier::Global, key, &value, home, None).map_err(|e| e.to_string())
}

/// A ledger that cannot be read is treated as empty, so a damaged file never
/// blocks a reset. Entries: `{previous, applied, at}` per key.
fn load_ledger(home: &Path) -> Map<String, Value> {
    std::fs::read_to_string(ledger_path(home))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("keys").and_then(Value::as_object).cloned())
        .unwrap_or_default()
}

fn save_ledger(home: &Path, keys: &Map<String, Value>) -> Result<(), String> {
    std::fs::create_dir_all(dir(home)).map_err(|e| e.to_string())?;
    let body = serde_json::to_string_pretty(&json!({ "keys": keys })).map_err(|e| e.to_string())?;
    write_atomic(&ledger_path(home), body).map_err(|e| format!("failed to write the ledger: {e}"))
}

/// Copy every global `effort.*` key to `effort/backups/config-<time>.json`
/// before a change. Returns the file written.
fn backup(home: &Path, now: i64) -> Result<PathBuf, String> {
    let root = playbook_root_from(home);
    let mut snapshot = Map::new();
    for key in store::global_keys(&root).map_err(|e| e.to_string())? {
        if key.starts_with("effort.") {
            if let Some(v) = global_value(home, &key)? {
                snapshot.insert(key, v);
            }
        }
    }
    let dir = dir(home).join("backups");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut path = dir.join(format!("config-{now}.json"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("config-{now}-{n}.json"));
        n += 1;
    }
    let body = serde_json::to_string_pretty(&json!({
        "createdAt": iso_utc(now),
        "tier": "global",
        "keys": snapshot,
    }))
    .map_err(|e| e.to_string())?;
    write_atomic(&path, body).map_err(|e| format!("failed to write the backup: {e}"))?;
    Ok(path)
}

fn log_change(home: &Path, now: i64, action: &str, key: &str, from: &Value, to: &Value, why: &str) {
    let line = json!({
        "ts": iso_utc(now),
        "action": action,
        "key": key,
        "from": from,
        "to": to,
        "why": why,
    });
    atomic_append(&changes_log_path(home).to_string_lossy(), &line.to_string());
}

/// Apply `patch`, but only when `approved`. Every line is validated before
/// any key is written. The previous global `effort.*` keys are saved to a
/// backup first, each change goes in the ledger (so `reset` can undo it) and
/// the change log.
pub fn apply(home: &Path, patch: &Patch, approved: bool, now: i64) -> Result<String, String> {
    if !approved {
        return Err("not approved, nothing was written".to_string());
    }
    if patch.lines.is_empty() {
        return Ok("nothing to apply".to_string());
    }
    for l in &patch.lines {
        validate(l)?;
    }
    let backup_path = backup(home, now)?;
    let mut ledger = load_ledger(home);
    let mut done = Vec::new();
    let mut failure = None;
    for l in &patch.lines {
        let current = match global_value(home, &l.key) {
            Ok(v) => v,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        // Applying again keeps the first previous value, so reset goes back
        // to what the user had before any patch.
        let previous = match ledger.get(&l.key) {
            Some(e) if e.get("applied") == current.as_ref() => e.get("previous").cloned(),
            _ => Some(current.clone().unwrap_or(Value::Null)),
        }
        .unwrap_or(Value::Null);
        let new = Value::String(l.value.clone());
        if let Err(e) = set_global(home, &l.key, new.clone()) {
            failure = Some(e);
            break;
        }
        ledger.insert(
            l.key.clone(),
            json!({ "previous": previous, "applied": new, "at": iso_utc(now) }),
        );
        log_change(
            home,
            now,
            "apply",
            &l.key,
            &current.unwrap_or(Value::Null),
            &new,
            &l.evidence,
        );
        done.push(l.key.clone());
    }
    save_ledger(home, &ledger)?;
    if let Some(e) = failure {
        return Err(format!(
            "stopped after {} of {} changes: {e}. Backup: {}. `playbook effort reset` undoes what was applied",
            done.len(),
            patch.lines.len(),
            backup_path.display()
        ));
    }
    Ok(format!(
        "applied {} change(s): {}. Backup: {}. Undo with `playbook effort reset`.",
        done.len(),
        done.join(", "),
        backup_path.display()
    ))
}

/// Undo what `apply` did, and with `all` set every global `effort.*` key to
/// its default. A ledgered key the user has edited since is left alone. The
/// previous keys are backed up first and every change is logged.
pub fn reset(home: &Path, all: bool, now: i64) -> Result<String, String> {
    let ledger = load_ledger(home);
    let root = playbook_root_from(home);
    let mut targets: Vec<(String, Value, &str)> = Vec::new();
    let mut left = Vec::new();
    for (key, entry) in &ledger {
        let current = global_value(home, key)?;
        if current.as_ref() != entry.get("applied") {
            left.push(key.clone());
            continue;
        }
        let restore = match entry.get("previous") {
            Some(Value::Null) | None => keys::default_value(key).unwrap_or(Value::Null),
            Some(v) => v.clone(),
        };
        targets.push((
            key.clone(),
            restore,
            "restore the value from before the patch",
        ));
    }
    if all {
        for key in store::global_keys(&root).map_err(|e| e.to_string())? {
            if key.starts_with("effort.")
                && key != super::KEY
                && !targets.iter().any(|(k, _, _)| *k == key)
            {
                if let Some(default) = keys::default_value(&key) {
                    if global_value(home, &key)?.as_ref() != Some(&default) {
                        targets.push((key, default, "reset to the default"));
                    }
                }
            }
        }
    }
    let backup_path = if targets.is_empty() && ledger.is_empty() {
        None
    } else {
        Some(backup(home, now)?)
    };
    let mut restored = Vec::new();
    for (key, value, why) in targets {
        let before = global_value(home, &key)?.unwrap_or(Value::Null);
        set_global(home, &key, value.clone())?;
        log_change(home, now, "reset", &key, &before, &value, why);
        restored.push(key);
    }
    if !ledger.is_empty() {
        save_ledger(home, &Map::new())?;
    }
    let mut msg = if restored.is_empty() {
        "nothing to reset".to_string()
    } else {
        format!("reset {} key(s): {}", restored.len(), restored.join(", "))
    };
    if !left.is_empty() {
        msg.push_str(&format!(
            ". Left alone because you changed them since: {}",
            left.join(", ")
        ));
    }
    if let Some(p) = backup_path {
        msg.push_str(&format!(". Backup: {}", p.display()));
    }
    Ok(msg)
}

/// `playbook effort apply`: show the patch, ask, then apply. `yes` skips the
/// question. Without `yes`, input must come from a person at a terminal
/// (`interactive`), so a script or a pipe never approves by accident.
pub fn run_apply(
    home: &Path,
    patch: &Patch,
    yes: bool,
    interactive: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    now: i64,
) -> Result<String, String> {
    if patch.lines.is_empty() {
        return Ok(patch.to_text());
    }
    let _ = writeln!(out, "{}\n", patch.to_text());
    let approved = if yes {
        true
    } else if !interactive {
        return Err("not at a terminal and no --yes, so nothing was written".to_string());
    } else {
        let _ = write!(out, "Apply this patch? [y/N] ");
        let _ = out.flush();
        let mut answer = String::new();
        input.read_line(&mut answer).map_err(|e| e.to_string())?;
        matches!(answer.trim(), "y" | "Y" | "yes" | "Yes")
    };
    apply(home, patch, approved, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use crate::effort::suggest::AgentReport;
    use std::collections::BTreeMap;

    fn agent(name: &str, status: Status) -> AgentReport {
        AgentReport {
            name: name.to_string(),
            model: "sonnet".into(),
            shipped: "high".into(),
            effective: "high".into(),
            configured: "auto".into(),
            floor: floor_of(name),
            dispatches: 40,
            sessions: 9,
            by_level: BTreeMap::new(),
            reruns: 0,
            overrides_down: 0,
            overrides_up: 0,
            buckets: Vec::new(),
            status,
            findings: Vec::new(),
        }
    }

    fn report(agents: Vec<AgentReport>) -> Report {
        Report {
            range: "30d",
            history_days: 30,
            cold_start: false,
            other_dispatches: 0,
            agents,
        }
    }

    fn line(agent: &str, value: &str, from: &str) -> Line {
        Line {
            agent: agent.into(),
            key: format!("effort.agents.{agent}"),
            value: value.into(),
            from: from.into(),
            shipped: from.into(),
            current: "auto".into(),
            evidence: "test".into(),
        }
    }

    fn home(tag: &str) -> PathBuf {
        let h = scratch_dir(tag);
        std::fs::create_dir_all(&h).unwrap();
        h
    }

    fn cfg(h: &Path, key: &str) -> Option<Value> {
        global_value(h, key).unwrap()
    }

    #[test]
    fn a_lowerable_agent_gets_one_line_with_its_evidence() {
        // `implementer` has a floor of medium: from high it may go to medium.
        let p = build(&report(vec![agent("implementer", Status::Lower)]));
        assert_eq!(p.lines.len(), 1);
        let l = &p.lines[0];
        assert_eq!(
            (l.key.as_str(), l.value.as_str()),
            ("effort.agents.implementer", "medium")
        );
        assert!(
            l.evidence.contains("40 dispatches over 9 sessions"),
            "{}",
            l.evidence
        );
        assert!(l.evidence.contains("bench campaign"), "{}", l.evidence);
        let text = p.to_text();
        assert!(text.contains("playbook config set --global effort.agents.implementer medium"));
        assert!(text.contains("every project"));
    }

    #[test]
    fn only_lower_status_agents_are_proposed() {
        let p = build(&report(vec![
            agent("implementer", Status::NoChange),
            agent("critic", Status::Insufficient),
            agent("analyst", Status::Needed),
            agent("collector", Status::Unused),
        ]));
        assert!(p.lines.is_empty());
        assert!(p.to_text().contains("No change is proposed"));
    }

    #[test]
    fn costly_to_miss_agents_are_never_in_a_patch() {
        for name in ["reviewer", "auditor"] {
            let mut a = agent(name, Status::Lower);
            a.shipped = "xhigh".into();
            a.effective = "xhigh".into();
            assert!(build(&report(vec![a])).lines.is_empty(), "{name}");
            assert!(validate(&line(name, "high", "xhigh")).is_err(), "{name}");
        }
    }

    #[test]
    fn an_agent_without_a_floor_row_is_refused() {
        let mut a = agent("mystery", Status::Lower);
        a.floor = None;
        assert!(build(&report(vec![a])).lines.is_empty());
        assert!(validate(&line("mystery", "low", "high")).is_err());
    }

    #[test]
    fn a_value_below_the_floor_or_not_lower_is_refused() {
        // implementer floor is medium
        assert!(validate(&line("implementer", "low", "high"))
            .unwrap_err()
            .contains("below the quality floor"));
        assert!(validate(&line("implementer", "high", "high")).is_err());
        assert!(validate(&line("implementer", "xhigh", "high")).is_err());
        assert!(validate(&line("implementer", "medium", "high")).is_ok());
        assert!(validate(&line("implementer", "bogus", "high")).is_err());
        let mut other = line("implementer", "medium", "high");
        other.key = "effort.max".into();
        assert!(validate(&other).is_err());
    }

    #[test]
    fn apply_needs_approval_and_writes_nothing_without_it() {
        let h = home("effort-patch-approval");
        let p = Patch {
            lines: vec![line("implementer", "medium", "high")],
        };
        assert!(apply(&h, &p, false, 100)
            .unwrap_err()
            .contains("not approved"));
        assert_eq!(cfg(&h, "effort.agents.implementer"), None);
        assert!(!dir(&h).exists());
    }

    #[test]
    fn interactive_answers_decide_and_scripts_cannot_approve() {
        let h = home("effort-patch-prompt");
        let p = Patch {
            lines: vec![line("implementer", "medium", "high")],
        };
        let mut out = Vec::new();
        let r = run_apply(&h, &p, false, true, &mut "n\n".as_bytes(), &mut out, 100);
        assert!(r.unwrap_err().contains("not approved"));
        let r = run_apply(&h, &p, false, true, &mut "".as_bytes(), &mut out, 100);
        assert!(r.is_err(), "empty answer is no");
        let r = run_apply(&h, &p, false, false, &mut "y\n".as_bytes(), &mut out, 100);
        assert!(r.unwrap_err().contains("no --yes"));
        assert_eq!(cfg(&h, "effort.agents.implementer"), None);
        run_apply(&h, &p, false, true, &mut "y\n".as_bytes(), &mut out, 100).unwrap();
        assert_eq!(cfg(&h, "effort.agents.implementer"), Some(json!("medium")));
        assert!(String::from_utf8(out).unwrap().contains("[y/N]"));
    }

    #[test]
    fn yes_applies_with_backup_ledger_and_log() {
        let h = home("effort-patch-apply");
        set_global(&h, "effort.agents.implementer", json!("high")).unwrap();
        let p = Patch {
            lines: vec![line("implementer", "medium", "high")],
        };
        let msg = run_apply(
            &h,
            &p,
            true,
            false,
            &mut "".as_bytes(),
            &mut Vec::new(),
            1_791_676_800,
        )
        .unwrap();
        assert!(msg.contains("applied 1 change"), "{msg}");
        assert_eq!(cfg(&h, "effort.agents.implementer"), Some(json!("medium")));
        let backups: Vec<_> = std::fs::read_dir(dir(&h).join("backups"))
            .unwrap()
            .collect();
        assert_eq!(backups.len(), 1);
        let b: Value = serde_json::from_str(
            &std::fs::read_to_string(backups[0].as_ref().unwrap().path()).unwrap(),
        )
        .unwrap();
        assert_eq!(b["keys"]["effort.agents.implementer"], "high");
        assert_eq!(b["createdAt"], "2026-10-11T00:00:00Z");
        let log = std::fs::read_to_string(changes_log_path(&h)).unwrap();
        let row: Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(
            (
                row["action"].as_str(),
                row["from"].as_str(),
                row["to"].as_str()
            ),
            (Some("apply"), Some("high"), Some("medium"))
        );
        assert_eq!(row["ts"], "2026-10-11T00:00:00Z");
    }

    #[test]
    fn an_invalid_line_stops_everything_before_any_write() {
        let h = home("effort-patch-invalid");
        let p = Patch {
            lines: vec![
                line("implementer", "medium", "high"),
                line("reviewer", "high", "xhigh"),
            ],
        };
        assert!(apply(&h, &p, true, 100).is_err());
        assert_eq!(cfg(&h, "effort.agents.implementer"), None);
        assert!(!dir(&h).exists());
    }

    #[test]
    fn apply_then_reset_round_trips_to_the_previous_value() {
        let h = home("effort-patch-roundtrip");
        set_global(&h, "effort.agents.critic", json!("high")).unwrap();
        let p = Patch {
            lines: vec![
                line("critic", "medium", "high"),
                line("implementer", "medium", "high"),
            ],
        };
        apply(&h, &p, true, 100).unwrap();
        // Applying the same patch twice keeps the original previous value.
        apply(&h, &p, true, 101).unwrap();
        let msg = reset(&h, false, 102).unwrap();
        assert!(msg.contains("reset 2 key(s)"), "{msg}");
        assert_eq!(cfg(&h, "effort.agents.critic"), Some(json!("high")));
        assert_eq!(cfg(&h, "effort.agents.implementer"), Some(json!("auto")));
        assert!(load_ledger(&h).is_empty());
        assert_eq!(reset(&h, false, 103).unwrap(), "nothing to reset");
    }

    #[test]
    fn reset_leaves_a_key_the_user_edited_by_hand() {
        let h = home("effort-patch-edited");
        let p = Patch {
            lines: vec![line("implementer", "medium", "high")],
        };
        apply(&h, &p, true, 100).unwrap();
        set_global(&h, "effort.agents.implementer", json!("low")).unwrap();
        let msg = reset(&h, false, 101).unwrap();
        assert!(msg.contains("Left alone"), "{msg}");
        assert_eq!(cfg(&h, "effort.agents.implementer"), Some(json!("low")));
    }

    #[test]
    fn reset_all_returns_every_effort_key_to_its_default() {
        let h = home("effort-patch-all");
        set_global(&h, "effort.agents.reviewer", json!("low")).unwrap();
        set_global(&h, "effort.commands.fix", json!("high")).unwrap();
        set_global(&h, "effort.max", json!("medium")).unwrap();
        reset(&h, true, 100).unwrap();
        assert_eq!(cfg(&h, "effort.agents.reviewer"), Some(json!("auto")));
        assert_eq!(cfg(&h, "effort.commands.fix"), Some(json!("auto")));
        // The global ceiling is not a component key, so reset-all leaves it.
        assert_eq!(cfg(&h, "effort.max"), Some(json!("medium")));
    }

    #[test]
    fn nothing_outside_the_playbook_root_is_written() {
        let h = home("effort-patch-scope");
        let claude = h.join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(claude.join("settings.json"), "{}").unwrap();
        let p = Patch {
            lines: vec![line("implementer", "medium", "high")],
        };
        apply(&h, &p, true, 100).unwrap();
        reset(&h, true, 101).unwrap();
        assert_eq!(
            std::fs::read_to_string(claude.join("settings.json")).unwrap(),
            "{}"
        );
        assert!(std::fs::read_dir(&claude).unwrap().count() == 1);
    }

    #[test]
    fn a_damaged_ledger_does_not_block_reset() {
        let h = home("effort-patch-badledger");
        std::fs::create_dir_all(dir(&h)).unwrap();
        std::fs::write(ledger_path(&h), "{not json").unwrap();
        assert_eq!(reset(&h, false, 100).unwrap(), "nothing to reset");
    }

    #[test]
    fn the_atomic_write_leaves_no_temp_files() {
        let h = home("effort-patch-atomic");
        let p = Patch {
            lines: vec![line("implementer", "medium", "high")],
        };
        apply(&h, &p, true, 100).unwrap();
        for d in [dir(&h), dir(&h).join("backups")] {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                assert!(!e.file_name().to_string_lossy().ends_with(".tmp"));
            }
        }
        let ledger: Value =
            serde_json::from_str(&std::fs::read_to_string(ledger_path(&h)).unwrap()).unwrap();
        assert_eq!(
            ledger["keys"]["effort.agents.implementer"]["applied"],
            "medium"
        );
    }
}
