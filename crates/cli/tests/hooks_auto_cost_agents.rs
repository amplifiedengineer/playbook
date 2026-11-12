// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `auto-cost` budgets per agent, as seen from the real binary. The main
//! session is held to `auto.budget.mainUsd` against its own transcript, each
//! subagent to `auto.budget.agentUsd` against its own, and the whole tree to
//! `auto.budget.ceilingUsd` for new `Agent` spawns. The caller is named by the
//! `agent_id` field of the hook input, and the closing steps stay allowed at
//! every cap.

#[path = "support/auto_config.rs"]
mod auto_config;
#[path = "support/auto_env.rs"]
mod auto_env;
#[path = "support/auto_feature_repo.rs"]
mod auto_feature_repo;
#[path = "support/auto_run.rs"]
mod auto_run;

use auto_env::{scratch, Scratch};
use auto_run::run_hook;
use playbook::usage::pricing::{cost_usd, Tokens};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static LINE_ID: AtomicU64 = AtomicU64::new(0);
const MODEL: &str = "claude-sonnet-5";
const PRE_TOOL_USE: &str = include_str!("fixtures/hooks/pre-agent.json");

#[derive(Debug, PartialEq)]
enum Outcome {
    Silent,
    Note(String),
    Deny(String),
}

impl Outcome {
    fn is_denied(&self) -> bool {
        matches!(self, Outcome::Deny(_))
    }

    fn reason(&self) -> &str {
        match self {
            Outcome::Deny(reason) => reason,
            other => panic!("expected a deny, got {other:?}"),
        }
    }
}

struct Tree {
    s: Scratch,
    sid: String,
}

impl Tree {
    /// An auto session with the given caps in dollars (`0` ceiling means the
    /// default), whose baseline call has already run on an empty main
    /// transcript.
    fn new(tag: &str, budget: f64, agent_budget: f64, ceiling: f64) -> Self {
        Self::with_config(
            tag,
            json!({"mode": "auto", "auto": {
                "budgetUsd": budget, "agentBudgetUsd": agent_budget,
                "sessionCeilingUsd": ceiling, "warnPct": 70}}),
        )
    }

    /// An auto session that stores no `auto.*` key, so every cap is the
    /// built-in default.
    fn with_defaults(tag: &str) -> Self {
        Self::with_config(tag, json!({"mode": "auto"}))
    }

    fn with_config(tag: &str, config: Value) -> Self {
        let s = scratch(tag);
        auto_config::seed_raw_global_config(&s.home, &config.to_string());
        let tree = Tree {
            s,
            sid: format!("sid-{tag}"),
        };
        fs::write(tree.main(), "").expect("main transcript is writable");
        assert_eq!(
            tree.call(None, "Bash", json!({"command": "git status"})),
            Outcome::Silent
        );
        tree
    }

    fn main(&self) -> PathBuf {
        self.s.cwd.join("main.jsonl")
    }

    fn agent_file(&self, id: &str) -> PathBuf {
        let id = id.strip_prefix("agent-").unwrap_or(id);
        self.s
            .cwd
            .join("main")
            .join("subagents")
            .join(format!("agent-{id}.jsonl"))
    }

    fn dir(&self) -> PathBuf {
        self.s.home.join(".config/playbook/runtime").join(&self.sid)
    }

    fn park_note(&self, agent: Option<&str>) -> String {
        let name = match agent {
            None => "park-note.md".to_string(),
            Some(id) => format!("park-note-{}.md", id.strip_prefix("agent-").unwrap_or(id)),
        };
        self.dir().join(name).to_string_lossy().into_owned()
    }

    fn spend_main(&self, usd: f64) {
        append_cost(&self.main(), usd);
    }

    fn spend_agent(&self, id: &str, usd: f64) {
        append_cost(&self.agent_file(id), usd);
    }

    fn payload(&self, agent: Option<&str>, tool: &str, input: Value) -> String {
        let mut value: Value = serde_json::from_str(PRE_TOOL_USE).expect("fixture is JSON");
        value["session_id"] = json!(self.sid);
        value["transcript_path"] = json!(self.main().to_string_lossy());
        value["tool_name"] = json!(tool);
        value["tool_input"] = input;
        value["cwd"] = json!(auto_feature_repo::feature_repo(&self.s).to_string_lossy());
        if let Some(id) = agent {
            value["agent_id"] = json!(id);
            value["agent_type"] = json!("general-purpose");
        }
        value.to_string()
    }

    fn call(&self, agent: Option<&str>, tool: &str, input: Value) -> Outcome {
        self.run(&self.payload(agent, tool, input))
    }

    fn run(&self, payload: &str) -> Outcome {
        let (out, code) = run_hook(
            &self.s,
            "auto-cost",
            payload,
            &[("PLAYBOOK_AUTO_COST_INTERVAL_MS", "0")],
        );
        assert_eq!(code, 0, "the hook must exit 0 whatever it decides");
        outcome(&out)
    }

    fn bash(&self, agent: Option<&str>, command: &str) -> Outcome {
        self.call(agent, "Bash", json!({"command": command}))
    }
}

fn outcome(out: &str) -> Outcome {
    if out.trim().is_empty() {
        return Outcome::Silent;
    }
    let value: Value = serde_json::from_str(out.trim()).expect("hook prints JSON");
    let inner = &value["hookSpecificOutput"];
    let text = |key: &str| inner[key].as_str().unwrap_or_default().to_string();
    if inner["permissionDecision"] == "deny" {
        Outcome::Deny(text("permissionDecisionReason"))
    } else {
        Outcome::Note(text("additionalContext"))
    }
}

fn append_cost(path: &Path, usd: f64) {
    let unit = cost_usd(
        MODEL,
        &Tokens {
            input: 1_000_000,
            ..Tokens::default()
        },
    )
    .expect("model is priced");
    let input = (usd / unit * 1_000_000.0).round() as u64;
    let line = json!({"type": "assistant", "message": {
        "id": format!("msg_{}", LINE_ID.fetch_add(1, Ordering::Relaxed)),
        "model": MODEL,
        "usage": {"input_tokens": input, "output_tokens": 0,
                  "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}}});
    fs::create_dir_all(path.parent().expect("parent")).expect("dir is creatable");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("transcript opens");
    writeln!(file, "{line}").expect("line is writable");
}

const CLOSING_COMMANDS: [&str; 13] = [
    "git add -A",
    "git commit -S -s -m \"feat: x\"",
    "git push -u origin feat/x",
    "git status",
    "git diff --stat",
    "git log --oneline -3",
    "git worktree list",
    "gh pr view 7",
    "gh pr list",
    "gh pr checks 7",
    "gh run view 9",
    "gh api repos/o/r/pulls/7",
    "playbook pr create --title \"feat: x\" --body-file b.md",
];

const NEW_WORK: [(&str, &str, &str); 7] = [
    ("Agent", "", "spawn"),
    ("Edit", "", "edit"),
    ("Bash", "cargo build", "build"),
    ("Bash", "npm test", "test"),
    ("Bash", "gh pr merge 7 --auto", "merge"),
    ("Bash", "gh release create v1", "release"),
    ("Bash", "git push --force origin main", "force push"),
];

fn new_work_call(tree: &Tree, agent: Option<&str>, tool: &str, command: &str) -> Outcome {
    let input = match tool {
        "Bash" => json!({"command": command}),
        "Edit" => json!({"file_path": "/tmp/x.rs", "old_string": "a", "new_string": "b"}),
        _ => json!({"description": "d", "prompt": "p", "subagent_type": "Explore"}),
    };
    tree.call(agent, tool, input)
}

#[test]
fn an_agent_over_its_cap_does_not_deny_another_agent_or_the_main_session() {
    // Arrange
    let tree = Tree::new("isolation", 5.0, 5.0, 100.0);
    tree.spend_main(1.0);
    tree.spend_agent("a1", 5.0);
    tree.spend_agent("b2", 1.0);

    // Act
    let a = tree.bash(Some("a1"), "npm test");
    let b = tree.bash(Some("b2"), "npm test");
    let main = tree.bash(None, "npm test");

    // Assert
    let reason = a.reason();
    assert!(reason.contains("this agent"), "reason: {reason}");
    assert!(
        reason.contains(&tree.park_note(Some("a1"))),
        "reason: {reason}"
    );
    assert!(!b.is_denied(), "agent B is under its own cap: {b:?}");
    assert!(
        !main.is_denied(),
        "the main session is under its own cap: {main:?}"
    );
}

#[test]
fn the_main_cap_counts_the_main_transcript_only() {
    // Arrange: the main session spent 5.00 of 5.00, agent B 1.00 of 5.00.
    let tree = Tree::new("main-only", 5.0, 5.0, 100.0);
    tree.spend_main(5.0);
    tree.spend_agent("b2", 1.0);

    // Act
    let main = tree.bash(None, "npm test");
    let b = tree.bash(Some("b2"), "npm test");

    // Assert
    assert!(main.reason().contains("the main session"), "{main:?}");
    assert!(!b.is_denied(), "the main cap does not park agent B: {b:?}");
}

#[test]
fn agent_spend_does_not_count_against_the_main_cap() {
    // Arrange: 4 agents at 4.00 each is 16.00 against a main cap of 5.00.
    let tree = Tree::new("agents-not-main", 5.0, 5.0, 100.0);
    for id in ["a1", "b2", "c3", "d4"] {
        tree.spend_agent(id, 4.0);
    }
    tree.spend_main(0.5);

    // Act
    let main = tree.bash(None, "npm test");

    // Assert
    assert!(!main.is_denied(), "{main:?}");
}

#[test]
fn the_agent_id_may_carry_the_agent_prefix() {
    // Arrange
    let tree = Tree::new("prefix", 5.0, 5.0, 100.0);
    tree.spend_agent("a1", 5.0);

    // Act
    let plain = tree.bash(Some("a1"), "npm test");
    let prefixed = tree.bash(Some("agent-a1"), "npm test");

    // Assert
    assert!(plain.is_denied() && prefixed.is_denied());
}

#[test]
fn a_caller_with_no_agent_id_counts_as_the_main_session() {
    // Arrange: the agent over its cap is not named, so its call is the main's.
    let tree = Tree::new("unknown-caller", 5.0, 5.0, 100.0);
    tree.spend_agent("a1", 9.0);
    tree.spend_main(1.0);

    // Act
    let outcome = tree.bash(None, "npm test");

    // Assert
    assert!(!outcome.is_denied(), "{outcome:?}");
}

#[test]
fn a_transcript_path_that_is_a_subagents_own_names_the_caller() {
    // Arrange
    let tree = Tree::new("own-path", 5.0, 5.0, 100.0);
    tree.spend_agent("a1", 5.0);
    let mut value: Value =
        serde_json::from_str(&tree.payload(None, "Bash", json!({"command": "npm test"})))
            .expect("payload is JSON");
    value["transcript_path"] = json!(tree.agent_file("a1").to_string_lossy());

    // Act
    let outcome = tree.run(&value.to_string());

    // Assert
    assert!(outcome.reason().contains("this agent"), "{outcome:?}");
}

#[test]
fn every_closing_step_stays_allowed_at_every_cap() {
    // Arrange: the main session and one agent are both parked.
    let tree = Tree::new("closing", 5.0, 5.0, 100.0);
    tree.spend_main(5.0);
    tree.spend_agent("a1", 5.0);

    for agent in [None, Some("a1")] {
        for command in CLOSING_COMMANDS {
            // Act
            let outcome = tree.bash(agent, command);

            // Assert
            assert!(!outcome.is_denied(), "{agent:?} {command:?}: {outcome:?}");
        }
        for skill in [
            "playbook:pr-create",
            "playbook:commit",
            "playbook:writing-style",
            "playbook:engineering-standards",
        ] {
            let outcome = tree.call(agent, "Skill", json!({"skill": skill}));
            assert!(!outcome.is_denied(), "{agent:?} {skill}: {outcome:?}");
        }
        let note = tree.call(
            agent,
            "Write",
            json!({"file_path": tree.park_note(agent), "content": "stopped"}),
        );
        assert!(!note.is_denied(), "{agent:?} park note: {note:?}");
        let status = tree.bash(agent, "playbook mode status");
        assert!(!status.is_denied(), "{agent:?} mode status: {status:?}");
    }
}

#[test]
fn an_agent_may_write_its_own_park_note_and_not_another_callers() {
    // Arrange
    let tree = Tree::new("park-notes", 5.0, 5.0, 100.0);
    tree.spend_agent("a1", 5.0);

    // Act
    let own = tree.call(
        Some("a1"),
        "Write",
        json!({"file_path": tree.park_note(Some("a1")), "content": "x"}),
    );
    let main_note = tree.call(
        Some("a1"),
        "Write",
        json!({"file_path": tree.park_note(None), "content": "x"}),
    );

    // Assert
    assert!(!own.is_denied(), "{own:?}");
    assert!(main_note.is_denied());
}

#[test]
fn new_work_is_denied_at_a_cap_and_the_reason_names_the_unblocking_commands() {
    // Arrange
    let tree = Tree::new("new-work", 5.0, 5.0, 100.0);
    tree.spend_main(5.0);
    tree.spend_agent("a1", 5.0);

    for (agent, key) in [
        (None, "auto.budget.mainUsd"),
        (Some("a1"), "auto.budget.agentUsd"),
    ] {
        for (tool, command, label) in NEW_WORK {
            // Act
            let outcome = new_work_call(&tree, agent, tool, command);

            // Assert
            let reason = outcome.reason();
            assert!(
                reason.contains(&format!("`playbook config set {key} 10`")),
                "{label}: {reason}"
            );
            assert!(reason.contains("`playbook mode ask`"), "{label}: {reason}");
        }
    }
}

#[test]
fn at_the_ceiling_every_caller_is_parked_and_the_closing_steps_continue() {
    // Arrange: 3.00 + 4.00 + 4.00 is 11.00 against a ceiling of 10.00, and
    // every caller is under its own cap of 5.00.
    let tree = Tree::new("ceiling", 5.0, 5.0, 10.0);
    tree.spend_main(3.0);
    tree.spend_agent("a1", 4.0);
    tree.spend_agent("b2", 4.0);

    for agent in [None, Some("a1"), Some("c3")] {
        // Act
        let spawn = new_work_call(&tree, agent, "Agent", "");
        let edit = new_work_call(&tree, agent, "Edit", "");
        let build = tree.bash(agent, "cargo build");
        let push = tree.bash(agent, "git push origin feat/x");

        // Assert
        for denied in [&spawn, &edit, &build] {
            let reason = denied.reason();
            assert!(
                reason.contains("`playbook config set auto.budget.ceilingUsd 20`"),
                "{reason}"
            );
            assert!(reason.contains("`playbook mode ask`"), "{reason}");
        }
        assert!(!push.is_denied(), "{push:?}");
    }
}

#[test]
fn a_main_cap_and_an_agent_cap_that_differ_each_apply_to_their_own_caller() {
    // Arrange: main may spend 8.00 and an agent 2.00.
    let tree = Tree::new("caps-differ", 8.0, 2.0, 100.0);
    tree.spend_main(3.0);
    tree.spend_agent("a1", 2.5);

    // Act
    let main = tree.bash(None, "npm test");
    let agent = tree.bash(Some("a1"), "npm test");

    // Assert
    assert!(!main.is_denied(), "3.00 is under 8.00: {main:?}");
    let reason = agent.reason();
    assert!(reason.contains("$2.00"), "{reason}");
    assert!(reason.contains("auto.budget.agentUsd"), "{reason}");
}

#[test]
fn the_guard_still_denies_settings_for_a_parked_caller_and_an_unreadable_one() {
    // Arrange: an agent at its cap, and another whose transcript is unpriced.
    let tree = Tree::new("guard-first", 5.0, 1.0, 100.0);
    tree.spend_agent("a1", 2.0);
    let line = json!({"type": "assistant", "message": {"id": "m1", "model": "unknown-model-x",
        "usage": {"input_tokens": 10, "output_tokens": 1}}});
    let path = tree.agent_file("u1");
    fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    fs::write(&path, format!("{line}\n")).expect("agent transcript");

    for agent in ["a1", "u1"] {
        // Act
        let set = tree.bash(Some(agent), "playbook config set auto.budget.agentUsd 99");
        let ask = tree.bash(Some(agent), "playbook mode ask");

        // Assert
        assert!(set.is_denied(), "{agent}: {set:?}");
        if agent == "u1" {
            assert_eq!(ask, Outcome::Silent, "the mode escape stays open");
        }
    }
}

#[test]
fn an_agent_whose_main_transcript_cannot_be_read_fails_closed() {
    // Arrange: the payload names a main transcript that does not exist.
    let tree = Tree::new("no-main", 5.0, 5.0, 100.0);
    let mut value: Value =
        serde_json::from_str(&tree.payload(Some("a1"), "Bash", json!({"command": "npm test"})))
            .expect("payload is JSON");
    value["transcript_path"] = json!(tree.s.cwd.join("gone.jsonl").to_string_lossy());

    // Act
    let outcome = tree.run(&value.to_string());

    // Assert
    assert!(outcome.reason().contains("cost unreadable"), "{outcome:?}");
}

#[test]
fn a_parked_caller_cannot_force_push_push_to_main_or_commit_without_auto() {
    // Arrange
    let tree = Tree::new("parked-git", 5.0, 1.0, 100.0);
    tree.spend_agent("a1", 2.0);

    // Act and assert
    for command in [
        "playbook commit run",
        "git push origin main",
        "git push origin HEAD:master",
        "git push --receive-pack=x origin feat/x",
        "git log --output=x",
        "gh api repos/o/r/issues -fkey=v",
        "gh api repos/o/r/issues -XPOST",
        "git status>out.txt",
    ] {
        assert!(tree.bash(Some("a1"), command).is_denied(), "{command}");
    }
    for command in [
        "playbook commit run --auto <<'MSG'\nfeat: x\nMSG",
        "playbook pr prepare --help >/dev/null 2>&1 || { echo \"upgrade\" >&2; exit 1; }",
        "cat > \"/tmp/s/pr-body.md\" <<'PRBODY_EOF'\nbody\nPRBODY_EOF\nplaybook pr create --title \"fix: x\" --body-file /tmp/s/pr-body.md",
    ] {
        let outcome = tree.bash(Some("a1"), command);
        assert!(!outcome.is_denied(), "{command}: {outcome:?}");
    }
}

#[test]
fn the_ceiling_defaults_to_ten_times_the_main_cap() {
    // Arrange: the default ceiling for a main cap of 5.00 is 50.00.
    let tree = Tree::new("ceiling-default", 5.0, 5.0, 0.0);
    for id in ["a1", "b2", "c3", "d4", "e5", "f6", "g7", "h8", "i9"] {
        tree.spend_agent(id, 4.0);
    }
    let under = new_work_call(&tree, None, "Agent", "");
    tree.spend_agent("j10", 4.0);
    tree.spend_agent("k11", 4.0);
    tree.spend_agent("l12", 4.0);
    tree.spend_agent("m13", 4.0);

    // Act
    let over = new_work_call(&tree, None, "Agent", "");

    // Assert
    assert!(!under.is_denied(), "36.00 is under 50.00: {under:?}");
    assert!(over.reason().contains("$50.00"), "{over:?}");
}

#[test]
fn the_default_caps_allow_the_spend_the_old_five_dollar_cap_denied() {
    // Arrange: no auto key is stored, so the main cap is 200.00, each agent
    // cap 50.00 and the ceiling 2000.00.
    let tree = Tree::with_defaults("default-caps");
    tree.spend_main(20.0);
    tree.spend_agent("a1", 20.0);

    // Act
    let main = new_work_call(&tree, None, "Edit", "");
    let agent = new_work_call(&tree, Some("a1"), "Edit", "");

    // Assert
    assert!(!main.is_denied(), "20.00 is under the main cap: {main:?}");
    assert!(
        !agent.is_denied(),
        "20.00 is under the agent cap: {agent:?}"
    );
}

#[test]
fn the_default_caps_still_park_a_caller_past_its_new_cap() {
    // Arrange: rows are (caller, spend, the command the reason names)
    let rows = [
        (None, 200.0, "`playbook config set auto.budget.mainUsd 400`"),
        (
            Some("a1"),
            50.0,
            "`playbook config set auto.budget.agentUsd 100`",
        ),
    ];
    for (n, (agent, usd, command)) in rows.into_iter().enumerate() {
        let tree = Tree::with_defaults(&format!("default-caps-hit-{n}"));
        match agent {
            None => tree.spend_main(usd),
            Some(id) => tree.spend_agent(id, usd),
        }

        // Act
        let outcome = new_work_call(&tree, agent, "Edit", "");

        // Assert
        let reason = outcome.reason();
        assert!(reason.contains("budget reached"), "{agent:?}: {reason}");
        assert!(reason.contains(command), "{agent:?}: {reason}");
    }
}

#[test]
fn an_invalid_agent_budget_falls_back_to_fifty_with_a_one_time_note() {
    // Arrange: the config turns invalid after the baseline call
    let tree = Tree::with_defaults("bad-agent-budget");
    let config = json!({"mode": "auto", "auto": {"agentBudgetUsd": "banana"}});
    auto_config::seed_raw_global_config(&tree.s.home, &config.to_string());
    tree.spend_agent("a1", 49.99);

    // Act
    let first = new_work_call(&tree, Some("a1"), "Edit", "");
    let second = new_work_call(&tree, Some("a1"), "Edit", "");
    tree.spend_agent("a1", 0.01);
    let at_cap = new_work_call(&tree, Some("a1"), "Edit", "");

    // Assert
    let Outcome::Note(note) = first else {
        panic!("the first call must note the invalid budget, got {first:?}");
    };
    assert!(note.contains("auto.budget.agentUsd"), "{note}");
    assert!(note.contains("$50.00"), "{note}");
    assert!(!second.is_denied(), "{second:?}");
    assert!(at_cap.reason().contains("budget reached"), "{at_cap:?}");
}

#[test]
fn the_default_ceiling_is_ten_times_the_default_main_cap() {
    // Arrange: 45 agents at 45.00 each is 2025.00, each under its own cap.
    let tree = Tree::with_defaults("default-ceiling");
    for n in 0..44 {
        tree.spend_agent(&format!("x{n}"), 45.0);
    }
    let under = new_work_call(&tree, None, "Agent", "");
    tree.spend_agent("x44", 45.0);

    // Act
    let over = new_work_call(&tree, None, "Agent", "");

    // Assert
    assert!(!under.is_denied(), "1980.00 is under 2000.00: {under:?}");
    let reason = over.reason();
    assert!(reason.contains("$2000.00"), "{reason}");
    assert!(
        reason.contains("`playbook config set auto.budget.ceilingUsd 4000`"),
        "{reason}"
    );
}

#[test]
fn the_warn_tier_is_computed_per_scope() {
    // Arrange: 70 percent of 5.00 is 3.50.
    let tree = Tree::new("warn-scope", 5.0, 5.0, 100.0);
    tree.spend_agent("a1", 3.6);
    tree.spend_main(1.0);

    // Act
    let agent = tree.bash(Some("a1"), "npm test");
    let agent_again = tree.bash(Some("a1"), "npm test");
    let main = tree.bash(None, "npm test");
    tree.spend_main(2.7);
    let main_warned = tree.bash(None, "npm test");

    // Assert
    let Outcome::Note(note) = agent else {
        panic!("the agent hears its own warning: {agent:?}")
    };
    assert!(note.contains("this agent"), "{note}");
    assert_eq!(
        agent_again,
        Outcome::Silent,
        "the warning is once per agent"
    );
    assert_eq!(main, Outcome::Silent, "the main session is at 1.00");
    let Outcome::Note(note) = main_warned else {
        panic!("the main session warns at 3.70: {main_warned:?}")
    };
    assert!(note.contains("the main session"), "{note}");
}

#[test]
fn the_new_keys_are_locked_like_the_other_auto_keys() {
    // Arrange
    let tree = Tree::new("self-protection", 5.0, 5.0, 100.0);
    let commands = [
        "playbook config set auto.budget.agentUsd 100",
        "playbook config set --global auto.budget.ceilingUsd 1000",
        "playbook config set auto.budget.mainUsd 100",
    ];

    for agent in [None, Some("a1")] {
        for command in commands {
            // Act
            let outcome = tree.bash(agent, command);

            // Assert
            assert!(outcome.is_denied(), "{agent:?} {command}: {outcome:?}");
        }
    }
}

#[test]
fn an_unreadable_agent_cost_fails_closed_except_for_closing_steps_and_the_park_note() {
    // Arrange: an unpriced model in the agent's transcript, and no telemetry.
    let tree = Tree::new("unreadable-agent", 5.0, 5.0, 100.0);
    let line = json!({"type": "assistant", "message": {"id": "m1", "model": "unknown-model-x",
        "usage": {"input_tokens": 10, "output_tokens": 1}}});
    let path = tree.agent_file("a1");
    fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    fs::write(&path, format!("{line}\n")).expect("agent transcript");

    // Act
    let work = tree.bash(Some("a1"), "npm test");
    let push = tree.bash(Some("a1"), "git push origin feat/x");
    let note = tree.call(
        Some("a1"),
        "Write",
        json!({"file_path": tree.park_note(Some("a1")), "content": "x"}),
    );
    let main = tree.bash(None, "npm test");

    // Assert
    assert!(work.reason().contains("cost unreadable"), "{work:?}");
    assert!(!push.is_denied(), "{push:?}");
    assert!(!note.is_denied(), "{note:?}");
    assert!(
        !main.is_denied(),
        "the main transcript is readable: {main:?}"
    );
}

#[test]
fn an_agent_whose_transcript_does_not_exist_yet_has_spent_nothing() {
    // Arrange
    let tree = Tree::new("fresh-agent", 5.0, 5.0, 100.0);

    // Act
    let outcome = tree.bash(Some("fresh"), "npm test");

    // Assert
    assert_eq!(outcome, Outcome::Silent);
}
