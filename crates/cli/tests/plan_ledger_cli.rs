// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook plan ledger show` and `set` against plans in scratch directories,
//! with a scratch HOME.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;

const NO_LEDGER: &str = "NONE";
const ROUNDS: usize = 50;
const FIELDS: [(&str, &str); 8] = [
    ("status", "IN_PROGRESS"),
    ("branch", "feat/x"),
    ("pr", "https://example.test/pr/1"),
    ("land", "CI_RUNNING"),
    ("review", "implement-step9"),
    ("ci_fix_attempts", "1/3"),
    ("parked_reason", "blocked by review"),
    ("parked_at", "2026-11-13T00:00:00Z"),
];

struct Fixture {
    root: PathBuf,
    plan: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "playbook-plan-ledger-{tag}-{}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&root).expect("create scratch dir");
    let plan = root.join("plan.md");
    fs::write(&plan, "## Implementation Plan\n").expect("write plan");
    Fixture { root, plan }
}

impl Fixture {
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["plan", "ledger", args[0]])
            .arg(&self.plan)
            .args(&args[1..])
            .env("HOME", &self.root)
            .output()
            .expect("playbook runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }
}

#[test]
fn show_prints_none_when_nothing_was_set() {
    // Arrange
    let fx = fixture("empty");

    // Act
    let (code, out, _) = fx.run(&["show"]);

    // Assert
    assert_eq!(code, 0);
    assert_eq!(out, NO_LEDGER);
}

#[test]
fn set_then_show_round_trips_fields_and_keeps_other_ids_apart() {
    // Arrange
    let fx = fixture("round-trip");

    // Act
    let (set_code, _, set_err) = fx.run(&[
        "set",
        "WU-1",
        "status=DONE",
        "parked_reason=blocked by review",
    ]);
    let (seg_code, _, _) = fx.run(&["set", "S1", "branch=feat/x", "pr=https://e.test/1"]);
    let (show_code, out, _) = fx.run(&["show"]);

    // Assert
    assert_eq!(set_code, 0, "{set_err}");
    assert_eq!(seg_code, 0);
    assert_eq!(show_code, 0);
    assert!(out.contains("WU-1"), "{out}");
    assert!(out.contains("status=DONE"), "{out}");
    assert!(out.contains("parked_reason=blocked by review"), "{out}");
    assert!(out.contains("branch=feat/x"), "{out}");
    assert!(out.contains("pr=https://e.test/1"), "{out}");
}

#[test]
fn set_overwrites_a_field_and_keeps_the_rest() {
    // Arrange
    let fx = fixture("overwrite");
    fx.run(&["set", "WU-2", "status=IN_PROGRESS", "base=abc123"]);

    // Act
    fx.run(&["set", "WU-2", "status=DONE"]);
    let (_, out, _) = fx.run(&["show"]);

    // Assert
    assert!(out.contains("status=DONE"), "{out}");
    assert!(!out.contains("IN_PROGRESS"), "{out}");
    assert!(out.contains("base=abc123"), "{out}");
}

#[test]
fn the_ledger_sits_next_to_the_plan() {
    // Arrange
    let fx = fixture("location");

    // Act
    fx.run(&["set", "WU-1", "status=DONE"]);

    // Assert
    assert!(fx.root.join("plan.ledger.json").is_file());
}

#[test]
fn set_rejects_a_bad_id_key_or_pair_and_writes_nothing() {
    // Arrange
    let fx = fixture("rejects");
    let cases: [(&str, &str); 4] = [
        ("TASK-1", "status=DONE"),
        ("WU-1", "colour=blue"),
        ("WU-1", "status"),
        ("WU-1", "=DONE"),
    ];

    for (id, pair) in cases {
        // Act
        let (code, _, err) = fx.run(&["set", id, pair]);

        // Assert
        assert_eq!(code, 1, "{id} {pair}");
        assert!(!err.is_empty(), "{id} {pair}");
    }
    assert!(!fx.root.join("plan.ledger.json").exists());
}

#[test]
fn show_json_wraps_the_entries_in_the_shared_envelope() {
    // Arrange
    let fx = fixture("json");
    fx.run(&["set", "WU-1", "status=DONE"]);

    // Act
    let (code, out, _) = fx.run(&["show", "--json"]);

    // Assert
    let value: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(code, 0);
    assert_eq!(value["ok"], true);
    assert_eq!(value["entries"]["WU-1"]["status"], "DONE");
}

#[test]
fn a_missing_plan_fails_with_its_path() {
    // Arrange
    let fx = fixture("missing");
    let _ = fs::remove_file(&fx.plan);

    // Act
    let (code, _, err) = fx.run(&["show"]);

    // Assert
    assert_eq!(code, 1);
    assert!(err.contains("plan.md"), "{err}");
}

#[test]
fn eight_threads_setting_different_fields_lose_none() {
    for round in 0..ROUNDS {
        // Arrange
        let fx = fixture(&format!("race-{round}"));
        let barrier = Arc::new(Barrier::new(FIELDS.len()));

        // Act
        let handles: Vec<_> = FIELDS
            .iter()
            .map(|(key, value)| {
                let barrier = Arc::clone(&barrier);
                let plan = fx.plan.clone();
                let pair = format!("{key}={value}");
                thread::spawn(move || {
                    barrier.wait();
                    playbook::plan::ledger::set(&plan, "S1", &[pair])
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("thread").expect("set");
        }

        // Assert
        let ledger = playbook::plan::ledger::load(&fx.plan).expect("load");
        for (key, value) in FIELDS {
            assert_eq!(ledger.get("S1", key), Some(value), "round {round} {key}");
        }
        let _ = fs::remove_dir_all(&fx.root);
    }
}
