// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook plan graph check` and `playbook plan wave next` against plan
//! fixtures in scratch directories, with a scratch HOME.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const SEGMENTS_HEADER: &str = "| Seg | PR title | WUs | Requires |\n|---|---|---|---|\n";
const WUS_HEADER: &str =
    "| WU | Title | Files | Requires | Segment | Parallel group | Done When |\n|----|---|---|---|---|---|---|\n";
const NO_WAVE: &str = "NONE";

struct Fixture {
    root: PathBuf,
    plan: PathBuf,
}

/// A plan with a Segments table and a Work Units table, one row per slice entry.
fn fixture(tag: &str, segments: &[&str], wus: &[&str]) -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "playbook-plan-graph-{tag}-{}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&root).expect("create scratch dir");
    let plan = root.join("plan.md");
    let text = format!(
        "## Implementation Plan\n\n### Segments\n{SEGMENTS_HEADER}{}\n\n### Deliverables\n{WUS_HEADER}{}\n",
        segments.join("\n"),
        wus.join("\n"),
    );
    fs::write(&plan, text).expect("write plan");
    Fixture { root, plan }
}

impl Fixture {
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["plan"])
            .args(args)
            .arg(&self.plan)
            .env("HOME", &self.root)
            .output()
            .expect("playbook runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }

    fn check(&self) -> (i32, String, String) {
        self.run(&["graph", "check"])
    }

    fn wave(&self, done: &str) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["plan", "wave", "next"])
            .arg(&self.plan)
            .args(["--done", done])
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

fn parallel_plan(tag: &str, b_files: &str) -> Fixture {
    let b_row = format!("| WU-3 | Right | {b_files} | WU-1 | S2 | g1 | ok |");
    fixture(
        tag,
        &[
            "| S1 | first | WU-1 | none |",
            "| S2 | second | WU-2, WU-3 | S1 |",
        ],
        &[
            "| WU-1 | Base | `a/base.rs` | none | S1 | none | ok |",
            "| WU-2 | Left | `b/left.rs` | WU-1 | S2 | g1 | ok |",
            &b_row,
        ],
    )
}

#[test]
fn a_clean_linear_plan_passes_with_a_topology_hint() {
    // Arrange
    let f = fixture(
        "linear",
        &[
            "| S1 | first | WU-1 | none |",
            "| S2 | second | WU-2 | S1 |",
        ],
        &[
            "| WU-1 | One | `a.rs` | none | S1 | none | ok |",
            "| WU-2 | Two | `b.rs` | WU-1 | S2 | none | ok |",
        ],
    );

    // Act
    let (code, out, _) = f.check();

    // Assert
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("PASS"), "{out}");
    assert!(out.contains("topology=stacked"), "{out}");
}

#[test]
fn a_cycle_is_named_and_fails() {
    // Arrange
    let f = fixture(
        "cycle",
        &["| S1 | first | WU-1, WU-2 | none |"],
        &[
            "| WU-1 | One | `a.rs` | WU-2 | S1 | none | ok |",
            "| WU-2 | Two | `b.rs` | WU-1 | S1 | none | ok |",
        ],
    );

    // Act
    let (code, out, _) = f.check();

    // Assert
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("cycle: WU-1, WU-2"), "{out}");
    assert!(out.contains("FAIL"), "{out}");
}

#[test]
fn a_forward_cross_segment_dependency_fails() {
    // Arrange
    let f = fixture(
        "forward",
        &[
            "| S1 | first | WU-1 | none |",
            "| S2 | second | WU-2 | S1 |",
        ],
        &[
            "| WU-1 | One | `a.rs` | WU-2 | S1 | none | ok |",
            "| WU-2 | Two | `b.rs` | none | S2 | none | ok |",
        ],
    );

    // Act
    let (code, out, _) = f.check();

    // Assert
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("order: WU-1 (S1) requires WU-2 (S2)"), "{out}");
}

#[test]
fn a_work_unit_in_no_segment_fails_coverage() {
    // Arrange
    let f = fixture(
        "coverage",
        &["| S1 | first | WU-1 | none |"],
        &[
            "| WU-1 | One | `a.rs` | none | S1 | none | ok |",
            "| WU-2 | Two | `b.rs` | WU-1 | S1 | none | ok |",
        ],
    );

    // Act
    let (code, out, _) = f.check();

    // Assert
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("coverage: WU-2"), "{out}");
}

#[test]
fn overlapping_files_in_one_parallel_group_fail() {
    // Arrange
    let f = fixture(
        "overlap",
        &["| S1 | first | WU-1, WU-2 | none |"],
        &[
            "| WU-1 | One | `a/x.rs`, `shared.rs` | none | S1 | g1 | ok |",
            "| WU-2 | Two | `shared.rs` | none | S1 | g1 | ok |",
        ],
    );

    // Act
    let (code, out, _) = f.check();

    // Assert
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("parallel: g1: WU-1 and WU-2 share shared.rs"),
        "{out}"
    );
}

#[test]
fn two_lockfile_writers_in_one_parallel_group_fail() {
    // Arrange
    let f = fixture(
        "lockfile",
        &["| S1 | first | WU-1, WU-2 | none |"],
        &[
            "| WU-1 | One | `a.rs`, `Cargo.lock` | none | S1 | g1 | ok |",
            "| WU-2 | Two | `b.rs`, `crates/x/Cargo.lock` | none | S1 | g1 | ok |",
        ],
    );

    // Act
    let (code, out, _) = f.check();

    // Assert
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("parallel: g1: WU-1 and WU-2"), "{out}");
}

#[test]
fn the_next_wave_holds_both_members_once_their_shared_dependency_is_done() {
    // Arrange
    let f = parallel_plan("wave-both", "`c/right.rs`");

    // Act
    let (code, out, _) = f.wave("WU-1");

    // Assert
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.lines().collect::<Vec<_>>(), ["WU-2", "WU-3"]);
}

#[test]
fn the_next_wave_holds_one_member_when_files_overlap() {
    // Arrange
    let f = parallel_plan("wave-overlap", "`b/left.rs`");

    // Act
    let (code, out, _) = f.wave("WU-1");

    // Assert
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.lines().collect::<Vec<_>>(), ["WU-2"]);
}

#[test]
fn a_unit_behind_an_open_dependency_is_left_out_of_the_wave() {
    // Arrange
    let f = fixture(
        "wave-open",
        &["| S1 | first | WU-1, WU-2, WU-3 | none |"],
        &[
            "| WU-1 | One | `a.rs` | none | S1 | none | ok |",
            "| WU-2 | Two | `b.rs` | WU-1 | S1 | none | ok |",
            "| WU-3 | Three | `c.rs` | WU-2 | S1 | none | ok |",
        ],
    );

    // Act
    let (code, out, _) = f.wave("WU-1");

    // Assert
    assert_eq!(code, 0, "{out}");
    assert_eq!(out, "WU-2");
}

#[test]
fn the_next_wave_says_none_when_everything_is_done() {
    // Arrange
    let f = parallel_plan("wave-done", "`c/right.rs`");

    // Act
    let (code, out, _) = f.wave("WU-1,WU-2,WU-3");

    // Assert
    assert_eq!(code, 0, "{out}");
    assert_eq!(out, NO_WAVE);
}

#[test]
fn the_next_wave_says_none_when_every_ready_unit_is_blocked() {
    // Arrange
    let f = fixture(
        "wave-none",
        &["| S1 | first | WU-1, WU-2 | none |"],
        &[
            "| WU-1 | One | `a.rs` | WU-2 | S1 | none | ok |",
            "| WU-2 | Two | `b.rs` | WU-1 | S1 | none | ok |",
        ],
    );

    // Act
    let (code, out, _) = f.wave("");

    // Assert
    assert_eq!(code, 0, "{out}");
    assert_eq!(out, NO_WAVE);
}

#[test]
fn the_json_envelope_reports_findings_with_exit_zero() {
    // Arrange
    let f = fixture(
        "json",
        &["| S1 | first | WU-1, WU-2 | none |"],
        &[
            "| WU-1 | One | `a.rs` | WU-2 | S1 | none | ok |",
            "| WU-2 | Two | `b.rs` | WU-1 | S1 | none | ok |",
        ],
    );

    // Act
    let (code, out, _) = f.run(&["graph", "check", "--json"]);

    // Assert
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["ok"], false);
    assert_eq!(v["code"], 1);
    assert_eq!(v["findings"][0]["kind"], "cycle");
}
