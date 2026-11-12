// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook effort` end to end: the ceiling reaches `maxEffortLevel` in
//! `settings.json`, `playbook setup` keeps it applied, and `auto` clears only
//! a cap playbook wrote.

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "playbook-effort-cli-{}-{tag}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Home(dir)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CLAUDE_PLUGIN_ROOT")
            .output()
            .expect("playbook should spawn")
    }

    fn cap(&self) -> Option<String> {
        let text = fs::read_to_string(self.0.join(".claude/settings.json")).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        v.get("maxEffortLevel")?.as_str().map(str::to_string)
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn setting_a_level_stores_it_and_leaves_claude_code_settings_alone() {
    let h = Home::new("roundtrip");
    fs::create_dir_all(h.0.join(".claude")).unwrap();
    let settings = h.0.join(".claude/settings.json");
    fs::write(&settings, r#"{"maxEffortLevel":"max"}"#).unwrap();

    let out = h.run(&["effort", "medium"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(h.cap().as_deref(), Some("max"));
    assert_eq!(
        fs::read_to_string(&settings).unwrap(),
        r#"{"maxEffortLevel":"max"}"#
    );

    let out = h.run(&["effort", "max"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(h.cap().as_deref(), Some("max"));
}

#[test]
fn an_unknown_level_fails_and_writes_nothing() {
    let h = Home::new("unknown");
    let out = h.run(&["effort", "turbo"]);
    assert!(!out.status.success());
    assert!(
        text(&out).contains("unknown effort level"),
        "{}",
        text(&out)
    );
    assert!(!h.0.join(".claude/settings.json").exists());
}

#[test]
fn init_never_writes_a_cap() {
    let h = Home::new("init");
    assert!(h.run(&["effort", "low"]).status.success());
    let out = h.run(&["setup", "--yes", "--no-plugin"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(h.cap(), None);
}

#[test]
fn xhigh_under_a_claude_code_max_is_playbooks_to_apply() {
    let h = Home::new("xhigh-max");
    fs::create_dir_all(h.0.join(".claude")).unwrap();
    fs::write(
        h.0.join(".claude/settings.json"),
        r#"{"maxEffortLevel":"max"}"#,
    )
    .unwrap();
    assert!(h.run(&["effort", "xhigh"]).status.success());
    let body = text(&h.run(&["effort"]));
    assert!(body.contains("playbook effort.max: xhigh"), "{body}");
    assert!(body.contains("Claude Code maxEffortLevel: max"), "{body}");
    assert!(
        body.contains("effective ceiling: xhigh (playbook)"),
        "{body}"
    );
}

#[test]
fn a_lower_claude_code_ceiling_wins_in_the_status() {
    let h = Home::new("lower-claude");
    fs::create_dir_all(h.0.join(".claude")).unwrap();
    fs::write(
        h.0.join(".claude/settings.json"),
        r#"{"maxEffortLevel":"medium"}"#,
    )
    .unwrap();
    assert!(h.run(&["effort", "xhigh"]).status.success());
    let out = h.run(&["effort", "--json"]);
    let v: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(v["effective"], "medium");
    assert_eq!(v["winner"], "claude-code");
    assert_eq!(v["playbook"], "xhigh");
    assert_eq!(v["claudeCode"], "medium");
}

#[test]
fn config_set_accepts_the_key_and_rejects_a_bad_value() {
    let h = Home::new("config");
    assert!(h
        .run(&["config", "set", "--global", "effort.max", "xhigh"])
        .status
        .success());
    assert!(!h
        .run(&["config", "set", "--global", "effort.max", "huge"])
        .status
        .success());
}

#[test]
fn auto_is_accepted_and_follows_claude_code() {
    let h = Home::new("auto");
    fs::create_dir_all(h.0.join(".claude")).unwrap();
    fs::write(
        h.0.join(".claude/settings.json"),
        r#"{"maxEffortLevel":"medium"}"#,
    )
    .unwrap();
    assert!(h.run(&["effort", "auto"]).status.success());
    let out = h.run(&["effort", "--json"]);
    let j: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(j["playbook"], "auto");
    assert_eq!(j["effective"], "medium");
    assert_eq!(j["winner"], "claude-code");
}

fn plugin_root(h: &Home) -> std::path::PathBuf {
    let root = h.0.join("plugin");
    for d in ["agents", "commands", "skills/writing"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    let file =
        |effort: &str| format!("---\nname: x\ndescription: d\neffort: {effort}\n---\nBody\n");
    fs::write(root.join("agents/reviewer.md"), file("high")).unwrap();
    fs::write(root.join("commands/deep-review.md"), file("high")).unwrap();
    fs::write(root.join("skills/writing/SKILL.md"), "---\nname: w\n---\n").unwrap();
    root
}

fn run_with_root(h: &Home, root: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(args)
        .env("HOME", &h.0)
        .env("CLAUDE_PLUGIN_ROOT", root)
        .current_dir(&h.0)
        .output()
        .unwrap()
}

#[test]
fn a_component_ceiling_is_set_with_config_and_resolved_to_the_base_agent_and_an_effort() {
    let h = Home::new("component-resolve");
    let root = plugin_root(&h);
    let set = h.run(&["config", "set", "--global", "effort.agents.reviewer", "low"]);
    assert!(set.status.success(), "{}", text(&set));

    let out = run_with_root(
        &h,
        &root,
        &["effort", "resolve", "agents", "reviewer", "--json"],
    );
    assert!(out.status.success(), "{}", text(&out));
    let j: serde_json::Value = serde_json::from_str(&text(&out)).unwrap();
    assert_eq!(j["effective"], "low");
    assert_eq!(j["effort"], "low");
    assert_eq!(j["subagentType"], "playbook:reviewer");
    assert!(j.get("file").is_none() && j.get("allowed").is_none(), "{j}");
    assert_eq!(j["claudeCode"], serde_json::Value::Null);
}

#[test]
fn a_bad_component_key_or_level_is_rejected() {
    let h = Home::new("component-bad");
    assert!(!h
        .run(&["config", "set", "--global", "effort.agents.Bad Name", "low"])
        .status
        .success());
    assert!(!h
        .run(&[
            "config",
            "set",
            "--global",
            "effort.agents.reviewer",
            "huge"
        ])
        .status
        .success());
    assert!(!h
        .run(&[
            "config",
            "set",
            "--global",
            "effort.widgets.reviewer",
            "low"
        ])
        .status
        .success());
}

#[test]
fn list_shows_every_component_and_flags_an_unknown_key() {
    let h = Home::new("component-list");
    let root = plugin_root(&h);
    assert!(h
        .run(&["config", "set", "--global", "effort.agents.ghost", "low"])
        .status
        .success());
    let out = run_with_root(&h, &root, &["effort", "list"]);
    let t = text(&out);
    assert!(t.contains("agents/reviewer"), "{t}");
    assert!(t.contains("commands/deep-review"), "{t}");
    assert!(t.contains("skills/writing"), "{t}");
    assert!(!t.contains("reviewer-low"), "{t}");
    assert!(
        t.contains("effort.agents.ghost matches no component"),
        "{t}"
    );
}

#[test]
fn claude_code_stays_the_top_ceiling_for_a_component() {
    let h = Home::new("component-claude");
    let root = plugin_root(&h);
    fs::create_dir_all(h.0.join(".claude")).unwrap();
    fs::write(
        h.0.join(".claude/settings.json"),
        r#"{"maxEffortLevel":"medium"}"#,
    )
    .unwrap();
    assert!(h
        .run(&[
            "config",
            "set",
            "--global",
            "effort.commands.deep-review",
            "xhigh"
        ])
        .status
        .success());
    let out = run_with_root(
        &h,
        &root,
        &["effort", "resolve", "commands", "deep-review", "--json"],
    );
    let j: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(j["ceiling"], "medium");
    assert_eq!(j["effective"], "medium");
}

#[test]
fn the_plain_effort_status_still_works_next_to_the_subcommands() {
    let h = Home::new("component-compat");
    let out = h.run(&["effort"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("playbook effort.max"));
    assert!(h.run(&["effort", "high"]).status.success());
}

#[test]
fn resolve_cap_validates_the_level() {
    let h = Home::new("cap-bad");
    fs::create_dir_all(h.0.join(".claude")).unwrap();
    let out = h.run(&["effort", "resolve", "agents", "reviewer", "--cap", "turbo"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("unknown cap"), "{}", text(&out));
    let auto = h.run(&["effort", "resolve", "agents", "reviewer", "--cap", "auto"]);
    assert!(!auto.status.success());
}
