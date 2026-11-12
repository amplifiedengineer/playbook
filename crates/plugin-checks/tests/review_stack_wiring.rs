// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Pins the stack rules for both review commands: they detect the stack, say
//! which PRs are below and above, and review only the PR's own diff against
//! its real base unless the whole stack is asked for.

use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT")).join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn both_commands_run_the_stack_check() {
    for name in ["quick-pass", "deep-pass"] {
        let text = read(&format!("skills/grounding-review/references/{name}.md"));
        assert!(
            text.contains("## Step 1b: Stack check"),
            "{name}: no stack step"
        );
        assert!(
            text.contains("skills/grounding-review/references/stack-review.md"),
            "{name}: stack reference not loaded"
        );
    }
}

#[test]
fn the_stack_reference_names_neighbours_and_the_real_base() {
    let text = read("skills/grounding-review/references/stack-review.md");
    assert!(text.contains("Intent: pr-stack"));
    assert!(text.contains("Below: "), "no below line");
    assert!(text.contains("Above: "), "no above line");
    assert!(text.contains("Own diff base: <base>"), "no base line");
    assert!(
        text.contains("the parent PR's branch, not the default branch"),
        "base is not the parent branch"
    );
}

#[test]
fn this_pr_only_is_the_default_and_the_auto_choice() {
    let text = read("skills/grounding-review/references/stack-review.md");
    assert!(text.contains("1. This PR only. This is the default."));
    assert!(text.contains("Assumed: this PR only (auto mode)."));
    assert!(text.contains("never the default branch"));
    assert!(!text.contains("Assumed: whole stack"));
}

#[test]
fn review_asks_the_scope_before_the_depth() {
    let text = read("commands/review.md");
    for needle in [
        "## Step 1b: Stack scope",
        "Intent: pr-stack",
        "--scope stack",
        "--this-pr",
        "--whole-stack",
        "Assumed: this PR only (auto mode).",
        "--this-pr and --whole-stack conflict",
        "stack-review.md",
        "), scope: <this PR | whole stack | not a stack>.",
    ] {
        assert!(text.contains(needle), "review.md lacks `{needle}`");
    }
    assert!(
        !text.contains("changed in the stack question"),
        "the child no longer asks when called from review"
    );
    let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("no `{needle}`"));
    let scope_step = at("## Step 1b: Stack scope");
    assert!(scope_step < at("## Step 2"), "scope must come before depth");
    let in_scope_step = &text[scope_step..at("## Step 2")];
    let summary = in_scope_step
        .find("Stack of <n> PRs")
        .expect("summary line");
    let question = in_scope_step
        .find("1. This PR only")
        .expect("scope question");
    assert!(summary < question, "summary must come before the question");
}

#[test]
fn routing_scores_a_whole_stack_on_the_union_of_the_open_files() {
    let text = read("skills/grounding-review/references/review-routing.md");
    assert!(text.contains("union of the open PRs' files"));
    assert!(text.contains("over the limit of <max>"));
    assert!(!text.contains("review.stackMaxPrs"));
}

#[test]
fn this_pr_review_reads_the_digest_and_drops_findings_fixed_above() {
    let text = read("skills/grounding-review/references/stack-review.md");
    assert!(text.contains("\"<PR_NUMBER> --digest\""), "no digest call");
    assert!(
        text.contains("fixed above in #<n>"),
        "no fixed-above report"
    );
    assert!(text.contains("`over_line_budget`"), "no per-PR budget");
}
