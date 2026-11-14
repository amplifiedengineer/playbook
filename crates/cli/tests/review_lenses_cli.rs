// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook review lenses pick` against scratch git repos, and
//! `playbook review checks --list` against scratch project directories. Each
//! diff fixture trips one trigger of the deep review's conditional lenses.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const BASE: &str = "main";
const BRANCH: &str = "feat/change";
const NONE: &str = "NONE";
const NO_TOOLCHAIN: &str = "checks skipped";
const BIG_DIFF_LINES: usize = 301;
const SIMILAR_FILES: usize = 6;
/// Added line nested five levels deep.
const DEEP_LINE: &str = "                    let nested = 1;\n";

/// Files present on the base commit, so only the fixture's own change trips a
/// trigger.
const BASE_FILES: &[(&str, &str)] = &[
    ("src/lib.rs", "fn a() {}\n"),
    ("src/other.rs", "fn b() {}\n"),
    ("db/migrations/000_init.sql", "select 1;\n"),
    ("docs/adr/0001-first.md", "# one\n"),
    (".github/workflows/ci.yml", "name: ci\n"),
    ("README.md", "# readme\n"),
    ("Cargo.toml", "[package]\n"),
];

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "playbook-review-lenses-{tag}-{}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir.canonicalize().expect("scratch dir resolves")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(dir: &Path, path: &str, body: &str) {
    let full = dir.join(path);
    fs::create_dir_all(full.parent().expect("file has a parent")).expect("create dirs");
    fs::write(full, body).expect("write file");
}

/// A repo with the base files on `main` and `changes` committed on a branch.
fn repo_with(tag: &str, changes: &[(String, String)]) -> PathBuf {
    let dir = scratch(tag);
    git(&dir, &["init", "-q", "-b", BASE]);
    for (path, body) in BASE_FILES {
        write(&dir, path, body);
    }
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["switch", "-q", "-c", BRANCH]);
    for (path, body) in changes {
        write(&dir, path, body);
    }
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "change"]);
    dir
}

fn playbook(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .output()
        .expect("playbook runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

/// The picked lenses as (lens, trigger) pairs, from the JSON envelope.
fn picked(dir: &Path) -> Vec<(String, String)> {
    let (code, out) = playbook(dir, &["review", "lenses", "pick", "--base", BASE, "--json"]);
    assert_eq!(code, 0, "{out}");
    let v: Value = serde_json::from_str(&out).expect("envelope is JSON");
    assert_eq!(v["ok"], true, "{out}");
    v["lenses"]
        .as_array()
        .expect("lenses array")
        .iter()
        .map(|l| {
            (
                l["lens"].as_str().unwrap().to_string(),
                l["trigger"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn change(path: &str, body: &str) -> (String, String) {
    (path.to_string(), body.to_string())
}

struct Case {
    name: &'static str,
    changes: Vec<(String, String)>,
    lens: &'static str,
    /// Evidence the trigger text must name.
    needle: &'static str,
}

fn cases() -> Vec<Case> {
    let many_lines = "let x = 1;\n".repeat(BIG_DIFF_LINES);
    let similar: Vec<(String, String)> = (0..SIMILAR_FILES)
        .map(|i| change(&format!("src/handler_{i}.rs"), "fn h() {}\n"))
        .collect();
    vec![
        Case {
            name: "a new directory of code is a new module",
            changes: vec![change("src/billing/mod.rs", "fn pay() {}\n")],
            lens: "architecture",
            needle: "src/billing",
        },
        Case {
            name: "a changed dependency manifest is a dependency change",
            changes: vec![change("Cargo.toml", "[package]\nname = \"x\"\n")],
            lens: "architecture",
            needle: "Cargo.toml",
        },
        Case {
            name: "sorting added code is an algorithm over a collection",
            changes: vec![change(
                "src/lib.rs",
                "fn a(v: &mut Vec<u8>) { v.sort(); }\n",
            )],
            lens: "big-o",
            needle: "sort",
        },
        Case {
            name: "a deeply nested added line is complexity",
            changes: vec![change("src/lib.rs", &format!("fn a() {{\n{DEEP_LINE}}}\n"))],
            lens: "complexity",
            needle: "nested",
        },
        Case {
            name: "a workflow file is a deployment change",
            changes: vec![change(".github/workflows/ci.yml", "name: ci2\n")],
            lens: "integration",
            needle: ".github/workflows/ci.yml",
        },
        Case {
            name: "an added feature flag is an integration",
            changes: vec![change("src/lib.rs", "fn a() { feature_flag(\"new\"); }\n")],
            lens: "integration",
            needle: "feature flag",
        },
        Case {
            name: "a new migration file is a migration",
            changes: vec![change("db/migrations/001_add.sql", "alter table t;\n")],
            lens: "migration",
            needle: "db/migrations/001_add.sql",
        },
        Case {
            name: "a schema file is a migration",
            changes: vec![change("src/schema.prisma", "model A {}\n")],
            lens: "migration",
            needle: "src/schema.prisma",
        },
        Case {
            name: "a changed readme is a docs change",
            changes: vec![change("README.md", "# readme\nmore\n")],
            lens: "docs",
            needle: "README.md",
        },
        Case {
            name: "a new public function is a new public api",
            changes: vec![change("src/lib.rs", "pub fn api() {}\n")],
            lens: "docs",
            needle: "public",
        },
        Case {
            name: "a diff over 300 lines is large",
            changes: vec![change("src/lib.rs", &many_lines)],
            lens: "dedup",
            needle: "300",
        },
        Case {
            name: "many similar files are duplication risk",
            changes: similar,
            lens: "dedup",
            needle: "src",
        },
        Case {
            name: "an added decision record is an adr",
            changes: vec![change("docs/adr/0002-next.md", "# two\n")],
            lens: "adr",
            needle: "docs/adr/0002-next.md",
        },
    ]
}

#[test]
fn each_trigger_picks_its_lens_and_names_the_evidence() {
    for case in cases() {
        // Arrange
        let dir = repo_with("trigger", &case.changes);

        // Act
        let lenses = picked(&dir);

        // Assert
        let found = lenses.iter().find(|(lens, _)| lens == case.lens);
        let (_, trigger) = found.unwrap_or_else(|| panic!("{}: got {lenses:?}", case.name));
        assert!(trigger.contains(case.needle), "{}: {trigger}", case.name);
    }
}

#[test]
fn a_plain_small_edit_picks_no_conditional_lens() {
    // Arrange
    let dir = repo_with("plain", &[change("src/lib.rs", "fn a() { let x = 1; }\n")]);

    // Act
    let lenses = picked(&dir);
    let (code, text) = playbook(&dir, &["review", "lenses", "pick", "--base", BASE]);

    // Assert
    assert!(lenses.is_empty(), "{lenses:?}");
    assert_eq!((code, text.as_str()), (0, NONE));
}

#[test]
fn text_mode_prints_one_line_per_lens_with_its_trigger() {
    // Arrange
    let dir = repo_with(
        "text",
        &[
            change("db/migrations/001_add.sql", "alter table t;\n"),
            change("docs/adr/0002-next.md", "# two\n"),
        ],
    );

    // Act
    let (code, text) = playbook(&dir, &["review", "lenses", "pick", "--base", BASE]);

    // Assert
    assert_eq!(code, 0, "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].starts_with("migration: "), "{text}");
    assert!(lines[1].starts_with("adr: "), "{text}");
}

#[test]
fn an_unknown_base_is_an_error() {
    // Arrange
    let dir = repo_with(
        "badbase",
        &[change("src/lib.rs", "fn a() { let x = 1; }\n")],
    );

    // Act
    let (code, _) = playbook(&dir, &["review", "lenses", "pick", "--base", "nope"]);

    // Assert
    assert_eq!(code, 1);
}

#[test]
fn list_mode_prints_the_rust_commands_without_running_them() {
    // Arrange
    let dir = scratch("list-rust");
    write(&dir, "Cargo.toml", "this is not a manifest\n");

    // Act
    let (code, out) = playbook(&dir, &["review", "checks", dir.to_str().unwrap(), "--list"]);

    // Assert
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        ["cargo check", "cargo test"]
    );
}

#[test]
fn list_mode_prints_setup_then_checks_for_a_js_project() {
    // Arrange
    let dir = scratch("list-js");
    write(&dir, "package.json", "{}\n");

    // Act
    let (code, out) = playbook(&dir, &["review", "checks", dir.to_str().unwrap(), "--list"]);

    // Assert
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        [
            "npm install --prefer-offline",
            "npm run typecheck",
            "npm run lint",
            "npm test"
        ]
    );
}

#[test]
fn list_mode_marks_an_unknown_toolchain() {
    // Arrange
    let dir = scratch("list-none");

    // Act
    let (code, out) = playbook(&dir, &["review", "checks", dir.to_str().unwrap(), "--list"]);

    // Assert
    assert_eq!(code, 0);
    assert!(out.contains(NO_TOOLCHAIN), "{out}");
}
