// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Follow-ups to ADR 0016: edited files are kept, the gate move runs from
//! init inside a repo, and `playbook doctor migrations` reports edits.

use playbook::init::run::{run, InitPaths, StepStatus};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "pb-followups-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A shipped tree with a prompt and one skill, in a version-named dir.
fn plugin_root(base: &Path, prompt: &str, skill: &str) -> PathBuf {
    let root = base.join("plugin").join("9.9.9");
    fs::create_dir_all(root.join("prompts")).unwrap();
    fs::create_dir_all(root.join("skills/demo")).unwrap();
    fs::write(root.join("prompts/SYSTEM_PROMPT.md"), prompt).unwrap();
    fs::write(root.join("skills/demo/SKILL.md"), skill).unwrap();
    root
}

fn paths(home: &Path, root: &Path, repo: Option<(PathBuf, PathBuf)>) -> InitPaths {
    InitPaths {
        plugin: playbook::init::run::PluginRun::Skip,
        plugin_root: Some(root.to_path_buf()),
        self_root: Some(root.to_path_buf()),
        claude_home: home.join(".claude"),
        home: home.to_path_buf(),
        system_prompt: playbook::init::system_prompt::Answer::Yes,
        hooks: true,
        statusline: true,
        security: false,
        telemetry: false,
        telemetry_replace_off: false,
        only: false,
        short_name: playbook::init::pb_link::Choice::Skip,
        policy_guard: false,
        codex_hooks: false,
        pb_target: None,
        completions: None,
        attribution: playbook::init::attribution::Choice::Skip,
        path_setup: None,
        repo,
    }
}

fn prompt_dest(home: &Path) -> PathBuf {
    home.join(".config/playbook/prompts/SYSTEM_PROMPT.md")
}

#[test]
fn an_edited_prompt_is_left_in_place_and_an_unedited_one_still_updates() {
    let base = scratch("prompt");
    let home = base.join("home");
    let root = plugin_root(&base, "v1", "s");
    run(&paths(&home, &root, None));

    fs::write(root.join("prompts/SYSTEM_PROMPT.md"), "v2").unwrap();
    run(&paths(&home, &root, None));
    assert_eq!(fs::read_to_string(prompt_dest(&home)).unwrap(), "v2");

    fs::write(prompt_dest(&home), "mine").unwrap();
    fs::write(root.join("prompts/SYSTEM_PROMPT.md"), "v3").unwrap();
    let outcome = run(&paths(&home, &root, None));
    assert_eq!(fs::read_to_string(prompt_dest(&home)).unwrap(), "mine");
    let step = outcome
        .steps
        .iter()
        .find(|s| s.name == "system-prompt")
        .unwrap();
    assert_eq!(step.status, StepStatus::Skipped);
    assert!(outcome
        .warnings
        .iter()
        .any(|w| w.contains("system-prompt-edited") && w.contains("delete it")));

    fs::remove_file(prompt_dest(&home)).unwrap();
    run(&paths(&home, &root, None));
    assert_eq!(fs::read_to_string(prompt_dest(&home)).unwrap(), "v3");
}

#[test]
fn a_modified_skill_stays_reported_until_resolved() {
    let base = scratch("skill");
    let home = base.join("home");
    let root = plugin_root(&base, "p", "shipped");
    let first = run(&paths(&home, &root, None));
    assert!(first.warnings.is_empty());

    fs::write(root.join("skills/demo/SKILL.md"), "edited").unwrap();
    let second = run(&paths(&home, &root, None));
    assert!(second.warnings.iter().any(|w| w.contains("demo")));
    let third = run(&paths(&home, &root, None));
    assert!(third.warnings.iter().any(|w| w.contains("demo")));
    assert!(pending(&home, &root).contains("demo"));
}

#[test]
fn a_dev_checkout_is_not_tracked_and_old_versions_are_pruned() {
    let base = scratch("versions");
    let home = base.join("home");
    let root = plugin_root(&base, "p", "s");
    run(&paths(&home, &root, None));
    let root = home.join(".config/playbook");
    let state = || {
        playbook::state::list(&root, playbook::state::MIGRATIONS_SHIPPED)
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(state().contains("skill:9.9.9:demo"));

    let next = base.join("plugin").join("10.0.0");
    fs::create_dir_all(next.join("skills/demo")).unwrap();
    fs::write(next.join("skills/demo/SKILL.md"), "s").unwrap();
    run(&paths(&home, &next, None));
    let body = state();
    assert!(body.contains("skill:10.0.0:demo") && !body.contains("skill:9.9.9:"));

    let dev = base.join("dev");
    fs::create_dir_all(dev.join("skills/demo")).unwrap();
    fs::create_dir_all(dev.join(".git")).unwrap();
    fs::write(dev.join("skills/demo/SKILL.md"), "s").unwrap();
    run(&paths(&home, &dev, None));
    assert!(state().contains("skill:10.0.0:demo"));
}

#[test]
fn an_edited_statusline_is_kept_and_reported() {
    let base = scratch("statusline");
    let home = base.join("home");
    let root = plugin_root(&base, "p", "s");
    run(&paths(&home, &root, None));
    let dest = home.join(".config/playbook/statusline.sh");
    fs::write(&dest, "v1").unwrap();
    playbook::init::migrate::record_shipped(&home, "statusline", &dest);
    fs::write(&dest, "mine").unwrap();
    let outcome = run(&paths(&home, &root, None));
    assert_eq!(fs::read_to_string(&dest).unwrap(), "mine");
    assert!(outcome.warnings.iter().any(|w| w.contains("statusline")));
}

fn legacy_repo(base: &Path) -> (PathBuf, PathBuf) {
    let repo = base.join("repo");
    fs::create_dir_all(repo.join(".claude/plans")).unwrap();
    fs::write(repo.join(".claude/plans/p.md"), "plan").unwrap();
    (repo, base.join("dest"))
}

#[test]
fn the_gate_move_runs_from_init_inside_a_repo_and_not_outside() {
    let base = scratch("gate");
    let home = base.join("home");
    let root = plugin_root(&base, "p", "s");
    let (repo, dest) = legacy_repo(&base);

    run(&paths(&home, &root, None));
    assert!(repo.join(".claude/plans/p.md").exists());
    assert!(!dest.join("plans/p.md").exists());

    run(&paths(&home, &root, Some((repo.clone(), dest.clone()))));
    assert!(dest.join("plans/p.md").exists());
    assert!(!repo.join(".claude/plans/p.md").exists());
}

fn pending(home: &Path, root: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["doctor", "migrations"])
        .env("HOME", home)
        .env("CLAUDE_PLUGIN_ROOT", root)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn doctor_prints_pending_items_only_when_something_is_pending() {
    let base = scratch("doctor");
    let home = base.join("home");
    let root = plugin_root(&base, "p", "shipped");
    run(&paths(&home, &root, None));
    assert_eq!(pending(&home, &root), "");

    fs::write(root.join("skills/demo/SKILL.md"), "edited").unwrap();
    let out = pending(&home, &root);
    assert!(out.contains("skills-edited") && out.contains("demo"));
}

#[test]
fn init_adopts_a_config_json_written_after_the_sqlite_import() {
    let base = scratch("late-config");
    let home = base.join("home");
    let root = plugin_root(&base, "p", "shipped");
    let cfg = home.join(".config/playbook");
    fs::create_dir_all(&cfg).unwrap();
    // First run creates the store; then an older tool writes a JSON file.
    run(&paths(&home, &root, None));
    playbook::config::write::set(
        playbook::config::write::Tier::Global,
        "mode",
        &serde_json::json!("ask"),
        &home,
        None,
    )
    .unwrap();
    fs::write(cfg.join("config.json"), r#"{"pr":{"draft":false}}"#).unwrap();

    let out = run(&paths(&home, &root, None));

    assert!(!cfg.join("config.json").exists());
    assert!(cfg.join("config.json.migrated").exists());
    assert!(
        out.steps.iter().any(|s| s.name == "config"),
        "{:?}",
        out.steps
            .iter()
            .map(playbook::init::run::StepReport::render)
            .collect::<Vec<_>>()
    );
}
