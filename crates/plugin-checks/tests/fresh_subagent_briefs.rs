// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Pins the fresh-subagent rule: no dispatch relies on an omitted
//! `subagent_type` (which forks the whole parent history), and the brief
//! template stays present in the delegating-subagents skill.

use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    fs::read_to_string(PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT")).join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn skill_has_the_fresh_section_and_brief_template() {
    let s = read("skills/delegating-subagents/SKILL.md");
    assert!(s.contains("## Fresh, not forked"));
    for key in ["Goal:", "Inputs:", "Output:", "Constraints:"] {
        assert!(s.contains(key), "brief template lacks {key}");
    }
    assert!(s.contains("references/briefs.md"));
}

#[test]
fn briefs_reference_exists_and_has_toc() {
    let s = read("skills/delegating-subagents/references/briefs.md");
    assert!(s.contains("## Contents"));
    assert!(s.contains("## Classification of spawn sites"));
}

#[test]
fn plan_and_implement_name_the_agent_type() {
    let plan = read("commands/plan.md");
    assert!(plan.contains("`subagent_type: Explore`"));
    assert!(plan.contains("`subagent_type: general-purpose`"));
    let imp = read("commands/implement.md");
    assert!(imp.contains("fresh `playbook:implementer` subagent"));
}

#[test]
fn review_brief_builder_keeps_its_four_parts() {
    let s = read("skills/delegating-subagents/references/briefs.md");
    assert!(s.contains("## Review brief builder"));
    assert!(s.contains("## Implementer brief builder"));
    for part in [
        "**Diff.**",
        "**Excerpts.**",
        "**Test command.**",
        "**Limit.**",
    ] {
        assert!(s.contains(part), "review brief builder lacks {part}");
    }
    assert!(s.contains("Read or search beyond it only when a claim cannot be settled"));
}

#[test]
fn dispatching_commands_name_the_brief_builder() {
    for rel in [
        "skills/grounding-review/references/deep-pass.md",
        "skills/grounding-review/references/quick-pass.md",
        "commands/implement.md",
    ] {
        let s = read(rel);
        assert!(
            s.contains("Review brief builder"),
            "{rel} does not point at the review brief builder"
        );
    }
    let imp = read("commands/implement.md");
    assert!(imp.contains("Implementer brief builder"));
}

#[test]
fn reviewer_and_implementer_agents_allow_reading_beyond_the_brief_only_when_needed() {
    let r = read("agents/reviewer.md");
    assert!(r.contains("do not Read a file the excerpts already cover"));
    // The lens scope guardrail must survive the change.
    assert!(r.contains("Stay within the assigned focus"));
    let i = read("agents/implementer.md");
    assert!(i.contains("do not explore the repository"));
    assert!(i.contains("Read every file before you modify it"));
}

#[test]
fn every_returning_agent_carries_the_return_cap() {
    for agent in [
        "reviewer",
        "critic",
        "fact-checker",
        "test-reviewer",
        "analyst",
        "cheap-checker",
        "implementer",
        "collector",
    ] {
        let s = read(&format!("agents/{agent}.md"));
        assert!(
            s.contains("about 300 words"),
            "agents/{agent}.md has no return cap"
        );
    }
}

#[test]
fn the_cap_never_applies_to_the_output_file_or_drops_findings() {
    let r = read("agents/reviewer.md");
    assert!(r.contains("never drop a finding to fit"));
    assert!(r.contains("does not apply to a report file you write"));
    let skill = read("skills/delegating-subagents/SKILL.md");
    assert!(skill
        .contains("Return: status, findings as file:line, output path; at most about 300 words"));
    assert!(skill.contains("Write your full report to <absolute path>."));
}

const ROSTER_SENTENCE: &str = "The read-only rows cannot run `playbook code`: the orchestrator runs it and pastes the output into their brief.";
const EXPLORE_SENTENCE: &str = "Built-in `Explore` agents read no CLAUDE.md and none of playbook's prompt, but they have Bash: every `Explore` or `general-purpose` brief carries the code search rule (run `playbook code status`, then `playbook code index` when it says stale or missing, then `playbook code context` before Grep or Glob), or you run the search yourself and paste the result.";
const CALLERS_SENTENCE: &str = "**Callers, only when the user gave a token budget.** Reviewers have no Bash. With no budget, add no code facts to any brief. With one, when `playbook code status` reports `fresh`, after `playbook code index` when it said stale or missing, add the `playbook code refs <name>` output for each public name the diff changes, under a `Callers` header, ranked by the order of the changed names and cut from the last line to that many tokens (four bytes to a token). `playbook plan brief --facts-tokens <N>` writes the same block into implementer briefs.";
const IMPLEMENTER_EXCERPTS_SENTENCE: &str = "Take the excerpts from `playbook code context \"<Work Unit title>\"`, trimmed to the step's files, when the code search rule finds an index; else from Read.";
const IMPLEMENTER_AGENT_SENTENCE: &str = "When an excerpt cannot settle a question about a name, run `playbook code status` (then `playbook code index` when it says stale or missing) and `playbook code symbol <name>` or `playbook code refs <name>` before Grep.";
const REVIEWER_AGENT_SENTENCE: &str = "When the brief has a `Callers` section from `playbook code refs`, use it before Grep: you have no Bash to run `playbook code` yourself.";
const AUDITOR_LIST: &str = "(`ls`, `find`, `git log/show/diff`, dependency and config listing, and `playbook code status`, `context`, `search`, `symbol`, `refs`, `outline` and `read`)";
const AUDITOR_INDEX_SENTENCE: &str = "`playbook code index` is allowed too: it writes only the index cache in playbook's state folder, never the repository.";
const PLAN_SENTENCE: &str = "Each `Explore` brief carries the code search rule: run `playbook code status`, then `playbook code index` when it says stale or missing, then `playbook code context \"<area>\"` before Grep or Glob, which are the fallback only when the binary or the index is unavailable.";
const IMPLEMENT_SENTENCE: &str = "A narrow lookup of a name runs `playbook code symbol <name>` or `playbook code refs <name>` directly first, after `playbook code status` (and `playbook code index` when it says stale or missing); Grep and Glob replace it only when the binary or the index is unavailable, and every `Explore` brief carries the same rule.";
const REPO_AUDIT_SENTENCE: &str = "Run `playbook code status`, then `playbook code index` when it says stale or missing; map entry points and core modules with `playbook code context` and `playbook code outline` before reading files. Use Grep and Glob only when the binary or the index is unavailable.";

fn position(s: &str, needle: &str) -> usize {
    s.find(needle)
        .unwrap_or_else(|| panic!("missing {needle:?}"))
}

#[test]
fn grounding_research_puts_code_search_first() {
    // Arrange
    let path = "skills/grounding-research/SKILL.md";

    // Act
    let s = read(path);

    // Assert
    let heading = position(&s, "## Code search first");
    assert!(s.contains("Use Grep and Glob only when the binary or the index is unavailable"));
    assert!(
        s.contains("Agents without Bash get the `playbook code context` output in their brief.")
    );
    assert!(position(&s, "## Citation Rules") < heading);
    assert!(heading < position(&s, "## Findings Format"));
}

#[test]
fn delegating_subagents_carries_the_rule_into_explore_briefs() {
    // Arrange
    let path = "skills/delegating-subagents/SKILL.md";

    // Act
    let s = read(path);

    // Assert
    assert!(s.contains(ROSTER_SENTENCE));
    assert!(s.contains(EXPLORE_SENTENCE));
    assert!(!s.contains("delegating_subagents_roster.rs"));
}

#[test]
fn briefs_carry_code_search_output() {
    // Arrange
    let path = "skills/delegating-subagents/references/briefs.md";

    // Act
    let s = read(path);

    // Assert
    assert!(s.contains("- Auditor brief"));
    assert!(s.contains("## Auditor brief"));
    assert!(s.contains(CALLERS_SENTENCE));
    assert!(s.contains(IMPLEMENTER_EXCERPTS_SENTENCE));
}

#[test]
fn agents_name_the_code_commands() {
    // Arrange
    let implementer_path = "agents/implementer.md";
    let reviewer_path = "agents/reviewer.md";
    let auditor_path = "agents/auditor.md";

    // Act
    let implementer = read(implementer_path);
    let reviewer = read(reviewer_path);
    let auditor = read(auditor_path);

    // Assert
    assert!(implementer.contains(IMPLEMENTER_AGENT_SENTENCE));
    assert!(reviewer.contains(REVIEWER_AGENT_SENTENCE));
    assert!(auditor.contains(AUDITOR_LIST));
    assert!(auditor.contains(AUDITOR_INDEX_SENTENCE));
    assert!(implementer.contains("do not explore the repository"));
    assert!(implementer.contains("Read every file before you modify it"));
    assert!(reviewer.contains("do not Read a file the excerpts already cover"));
    assert!(reviewer.contains("Stay within the assigned focus"));
}

#[test]
fn commands_run_code_status_before_searching() {
    // Arrange
    let pins = [
        ("commands/plan.md", PLAN_SENTENCE),
        ("commands/implement.md", IMPLEMENT_SENTENCE),
        ("commands/audit.md", REPO_AUDIT_SENTENCE),
    ];

    for (path, sentence) in pins {
        // Act
        let s = read(path);

        // Assert
        assert!(
            s.contains(sentence),
            "{path} lacks the code search sentence"
        );
    }
}
