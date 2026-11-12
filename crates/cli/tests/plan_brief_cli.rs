// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook plan brief`: writes the per-Work-Unit brief files that
//! `/playbook:implement` used to have a Haiku agent copy out of the plan.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PLAN: &str = "## Implementation Plan\n\n### Per-Work-Unit Detail\n\n#### WU-0: Types and schema\n- **Requires:** nothing\n- **Files:** `src/types.rs`, `src/types_test.rs`\n- **Changes:** add the types\n- **Test scenarios:** Given a value When parsed Then it round trips\n- **Done When:**\n  - [ ] the types compile\n\n#### WU-1: Wire it up\n- **Files:** src/app.rs\n- **Changes:** call the types\n";

const GRAPH: &str = r#"{"nodes":[
  {"id":"g/types","name":"types-are-plain-data","description":"keep the types free of behaviour","scope":"global"},
  {"id":"a1","file":"src/types.rs"}
],"edges":[{"from":"g/types","to":"a1","relation":"anchors"}]}"#;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pb-plan-brief-{}-{tag}",
        playbook::testing::run_id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_playbook"))
        .arg("plan")
        .arg("brief")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .output()
        .unwrap()
}

fn setup(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = scratch(tag);
    let plan = dir.join("plan.md");
    let graph = dir.join("graph.json");
    fs::write(&plan, PLAN).unwrap();
    fs::write(&graph, GRAPH).unwrap();
    (dir, plan, graph)
}

#[test]
fn it_writes_one_brief_per_work_unit_and_prints_the_paths() {
    let (dir, plan, graph) = setup("write");
    let out_dir = dir.join("briefs");

    let out = run(
        &dir,
        &[
            plan.to_str().unwrap(),
            "WU-0",
            "WU-1",
            "--out",
            out_dir.to_str().unwrap(),
            "--worktree-base",
            "/wt/p",
            "--verify",
            "pytest {tests}",
            "--repo",
            "o/r",
            "--graph",
            graph.to_str().unwrap(),
        ],
    );

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(printed.lines().count(), 2, "{printed}");
    let wu0 = fs::read_to_string(out_dir.join("wu-0.brief.md")).unwrap();
    assert!(wu0.contains("Worktree: /wt/p/wu-0"));
    assert!(wu0.contains("Scoped verify: `pytest src/types_test.rs`"));
    assert!(wu0.contains("Given a value When parsed Then it round trips"));
    assert!(wu0.contains(
        "## Relevant memory\n\n- types-are-plain-data: keep the types free of behaviour"
    ));
    let wu1 = fs::read_to_string(out_dir.join("wu-1.brief.md")).unwrap();
    assert!(wu1.contains("call the types"));
    assert!(
        !wu1.contains("Relevant memory"),
        "WU-1 touches nothing anchored"
    );
    assert!(
        !wu1.contains("add the types"),
        "a brief holds only its own section"
    );
}

#[test]
fn a_missing_work_unit_writes_nothing_and_exits_1() {
    let (dir, plan, graph) = setup("missing");
    let out_dir = dir.join("briefs");

    let out = run(
        &dir,
        &[
            plan.to_str().unwrap(),
            "WU-0",
            "WU-7",
            "--out",
            out_dir.to_str().unwrap(),
            "--graph",
            graph.to_str().unwrap(),
        ],
    );

    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no section for WU-7") && err.contains("WU-0, WU-1"),
        "{err}"
    );
    assert!(!out_dir.exists());
}

#[test]
fn a_missing_graph_and_a_single_worktree_still_write_the_brief() {
    let (dir, plan, _) = setup("nograph");
    let out_dir = dir.join("briefs");

    let out = run(
        &dir,
        &[
            plan.to_str().unwrap(),
            "WU-1",
            "--out",
            out_dir.to_str().unwrap(),
            "--worktree",
            "/main/tree",
            "--graph",
            "/no/such/graph.json",
        ],
    );

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = fs::read_to_string(out_dir.join("wu-1.brief.md")).unwrap();
    assert!(text.contains("Worktree: /main/tree"));
}

#[test]
fn an_unreadable_plan_is_an_error() {
    let dir = scratch("noplan");
    let out = run(&dir, &["/no/such/plan.md", "WU-0", "--out", "briefs"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("/no/such/plan.md"));
}

const FACTS_PLAN: &str = "## Implementation Plan\n\n### Per-Work-Unit Detail\n\n#### WU-0: Total\n- **Files:** `src/lib.rs`\n- **Changes:** rename `compute_total`\n";

#[test]
fn a_facts_budget_with_no_index_adds_no_block_and_still_writes_the_brief() {
    // Arrange
    let (dir, plan, graph) = setup("facts-none");
    let out_dir = dir.join("briefs");

    // Act
    let out = run(
        &dir,
        &[
            plan.to_str().unwrap(),
            "WU-0",
            "--out",
            out_dir.to_str().unwrap(),
            "--graph",
            graph.to_str().unwrap(),
            "--facts-tokens",
            "500",
        ],
    );

    // Assert
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = fs::read_to_string(out_dir.join("wu-0.brief.md")).unwrap();
    assert!(!text.contains("## Callers"), "{text}");
}

#[test]
fn a_facts_budget_adds_the_callers_block_from_the_code_index() {
    // Arrange
    let dir = scratch("facts-index");
    let plan = dir.join("plan.md");
    let graph = dir.join("graph.json");
    fs::write(&plan, FACTS_PLAN).unwrap();
    fs::write(&graph, GRAPH).unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/lib.rs"),
        "pub fn compute_total() -> u32 { 1 }\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.rs"),
        "fn main() { let _ = compute_total(); }\n",
    )
    .unwrap();
    for git_args in [
        &["init", "--quiet"][..],
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/test-owner/test-repo.git",
        ][..],
    ] {
        let status = Command::new("git")
            .args(git_args)
            .current_dir(&dir)
            .env("HOME", &dir)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let indexed = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["code", "index"])
        .current_dir(&dir)
        .env("HOME", &dir)
        .output()
        .unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let out_dir = dir.join("briefs");
    let args = [
        plan.to_str().unwrap(),
        "WU-0",
        "--out",
        out_dir.to_str().unwrap(),
        "--graph",
        graph.to_str().unwrap(),
        "--facts-tokens",
        "200",
    ];

    // Act
    let out = run(&dir, &args);

    // Assert
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = fs::read_to_string(out_dir.join("wu-0.brief.md")).unwrap();
    assert!(text.contains("## Callers"), "{text}");
    assert!(text.contains("compute_total"), "{text}");
}

#[test]
fn without_the_flag_no_callers_block_is_added() {
    // Arrange
    let (dir, plan, graph) = setup("facts-default");
    let out_dir = dir.join("briefs");

    // Act
    let out = run(
        &dir,
        &[
            plan.to_str().unwrap(),
            "WU-0",
            "--out",
            out_dir.to_str().unwrap(),
            "--graph",
            graph.to_str().unwrap(),
        ],
    );

    // Assert
    assert!(out.status.success());
    let text = fs::read_to_string(out_dir.join("wu-0.brief.md")).unwrap();
    assert!(!text.contains("## Callers"), "{text}");
}
