// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook plan ledger show` and `set`: the run state of a plan's Work Units
//! and Segments (status, branch, PR, land state, counters, parked reason),
//! kept in `<plan stem>.ledger.json` beside the plan. Writes take the same
//! advisory lock as `plan checkpoint`.

use crate::common::atomic::write_atomic;
use crate::json::envelope;
use crate::plan::graph::{is_segment_id, is_wu_id};
use crate::planning::with_file_lock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The fields a ledger entry may carry.
const KEYS: &[&str] = &[
    "status",
    "base",
    "commit",
    "branch",
    "pr",
    "land",
    "review",
    "ci_fix_attempts",
    "ci_rerun_attempts",
    "merge_attempts",
    "parked_reason",
    "parked_at",
];
/// Printed by `show` when the ledger has no entry.
const NO_ENTRIES: &str = "NONE";
/// The failure code of the shared envelope.
const FAILED_CODE: i32 = 1;
const LEDGER_SUFFIX: &str = "ledger.json";

/// Fields per Work Unit or Segment id.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Ledger {
    entries: BTreeMap<String, BTreeMap<String, String>>,
}

impl Ledger {
    /// The value of `key` on `id`, when set.
    pub fn get(&self, id: &str, key: &str) -> Option<&str> {
        self.entries.get(id)?.get(key).map(String::as_str)
    }

    fn render(&self) -> String {
        if self.entries.is_empty() {
            return NO_ENTRIES.to_string();
        }
        self.entries
            .iter()
            .map(|(id, fields)| {
                let lines = fields.iter().map(|(k, v)| format!("  {k}={v}"));
                std::iter::once(id.clone())
                    .chain(lines)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn ledger_path(plan: &Path) -> PathBuf {
    let stem = plan
        .file_stem()
        .map_or_else(|| "plan".to_string(), |s| s.to_string_lossy().into_owned());
    plan.with_file_name(format!("{stem}.{LEDGER_SUFFIX}"))
}

fn require_plan(plan: &Path) -> Result<(), String> {
    std::fs::metadata(plan)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", plan.display()))
}

fn read(path: &Path) -> Result<Ledger, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("{}: not a ledger: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Reads the ledger of `plan`; an absent ledger is empty.
pub fn load(plan: &Path) -> Result<Ledger, String> {
    require_plan(plan)?;
    read(&ledger_path(plan))
}

fn parse_pair(pair: &str) -> Result<(&str, &str), String> {
    let (key, value) = pair
        .split_once('=')
        .filter(|(key, _)| !key.is_empty())
        .ok_or_else(|| format!("'{pair}' is not key=value"))?;
    if !KEYS.contains(&key) {
        return Err(format!(
            "unknown ledger field '{key}': use one of {}",
            KEYS.join(", ")
        ));
    }
    Ok((key, value))
}

/// Sets `pairs` (`key=value`) on the Work Unit or Segment `id`, keeping every
/// other field. Nothing is written when any pair or the id is invalid.
pub fn set(plan: &Path, id: &str, pairs: &[String]) -> Result<(), String> {
    require_plan(plan)?;
    if !(is_wu_id(id) || is_segment_id(id)) {
        return Err(format!(
            "'{id}' is not a Work Unit (WU-n) or Segment (Sn) id"
        ));
    }
    let parsed = pairs
        .iter()
        .map(|p| parse_pair(p))
        .collect::<Result<Vec<_>, _>>()?;
    let path = ledger_path(plan);
    with_file_lock(&path, || {
        let mut ledger = read(&path)?;
        let fields = ledger.entries.entry(id.to_string()).or_default();
        for (key, value) in parsed {
            fields.insert(key.to_string(), value.to_string());
        }
        let text = serde_json::to_string_pretty(&ledger).map_err(|e| e.to_string())?;
        write_atomic(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    })
}

fn fail(json: bool, error: &str) -> i32 {
    if json {
        println!("{}", envelope::wrap(FAILED_CODE, error, Value::Null));
        0
    } else {
        eprintln!("error: {error}");
        FAILED_CODE
    }
}

/// `plan ledger show`: prints the ledger and returns the exit code.
pub fn run_show(plan: &Path, json: bool) -> i32 {
    match load(plan) {
        Ok(ledger) if json => {
            println!(
                "{}",
                envelope::wrap(0, "", json!({"entries": ledger.entries}))
            );
            0
        }
        Ok(ledger) => {
            println!("{}", ledger.render());
            0
        }
        Err(error) => fail(json, &error),
    }
}

/// `plan ledger set`: records the fields and returns the exit code.
pub fn run_set(plan: &Path, id: &str, pairs: &[String]) -> i32 {
    match set(plan, id, pairs) {
        Ok(()) => 0,
        Err(error) => fail(false, &error),
    }
}
