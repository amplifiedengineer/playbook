// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook tools map`: the intent-to-command table the SessionStart hook
//! hands to a session, so skills and commands can state an intent ("resolve
//! the plans folder") without naming a `playbook` subcommand. The table is the
//! single source: a unit test parses every entry through clap, so a renamed or
//! removed subcommand or flag fails the build instead of drifting. A binary
//! prints only the commands it has, so an older or absent binary never claims
//! one it lacks.

use std::fmt::Write as _;

/// The most tokens the printed map may cost, about 1.2 KB. One token is
/// counted as four bytes, the usual rough rule for English and shell text.
pub const TOKEN_BUDGET: usize = 300;
/// Bytes the estimator counts as one token.
const BYTES_PER_TOKEN: usize = 4;

/// The last line of the map: where the full list of commands is.
pub const FULL_LIST: &str = "Run `playbook commands` for the full list.";

/// One intent and the command that serves it.
pub struct Tool {
    /// Stable id for the JSON output and the CI test, for example `pr-reply`.
    /// Skills name an intent in prose and mark it with this id for the test;
    /// the text map does not print ids.
    pub intent: &'static str,
    /// The command words before the verb, for example `pr` or `worktree review`.
    pub group: &'static str,
    /// The subcommand or argument that picks the tool, for example `reply`.
    pub verb: &'static str,
    /// Flags and placeholders shown after the merged verbs of a line. Not the
    /// full required set: `playbook <line> --help` and `sample` give that.
    pub args: &'static str,
    /// A real invocation, parsed by clap in a test. Starts with `group verb`.
    pub sample: &'static str,
}

const fn t(
    intent: &'static str,
    group: &'static str,
    verb: &'static str,
    args: &'static str,
    sample: &'static str,
) -> Tool {
    Tool {
        intent,
        group,
        verb,
        args,
        sample,
    }
}

/// Every intent that has a tool, in print order.
pub const TOOLS: &[Tool] = &[
    t(
        "plans-dir",
        "path",
        "plans",
        "--create",
        "path plans --create",
    ),
    t(
        "implement-dir",
        "path",
        "implement",
        "--create",
        "path implement --create",
    ),
    t(
        "worktrees-dir",
        "path",
        "worktrees",
        "--create",
        "path worktrees --create",
    ),
    t("plugin-root", "path", "plugin-root", "", "path plugin-root"),
    t(
        "run-mode",
        "run-context",
        "",
        "--command NAME --args \"$ARGUMENTS\"",
        "run-context --command fix --args x",
    ),
    t("mode-status", "mode", "status", "", "mode status"),
    t(
        "route-model",
        "route",
        "",
        "KIND --json",
        "route review --json",
    ),
    t(
        "config-get",
        "config",
        "get",
        "KEY",
        "config get pr.review.enabled",
    ),
    t("list-plans", "plans", "", "--json", "plans --json"),
    t(
        "pr-prepare",
        "pr",
        "prepare",
        "",
        "pr prepare --json --args-stdin",
    ),
    t(
        "pr-create",
        "pr",
        "create",
        "",
        "pr create --title t --body-file - --json",
    ),
    t("pr-rules", "pr", "rules", "", "pr rules"),
    t("pr-comments", "pr", "comments", "", "pr comments"),
    t(
        "pr-reply",
        "pr",
        "reply",
        "",
        "pr reply --pr 1 --thread t --body-file f",
    ),
    t("pr-stack", "pr", "stack", "", "pr stack"),
    t("pr-ci-wait", "pr", "ci-wait", "", "pr ci-wait 1"),
    t("pr-land-wait", "pr", "land-wait", "", "pr land-wait 1"),
    t("pr-merge", "pr", "merge", "", "pr merge 1"),
    t("pr-status", "pr", "status", "", "pr status 1 --json"),
    t(
        "pr-review-triage",
        "pr",
        "review-triage",
        "",
        "pr review-triage",
    ),
    t(
        "review-prepare",
        "review",
        "prepare",
        "",
        "review prepare quick",
    ),
    t("review-checks", "review", "checks", "", "review checks ."),
    t(
        "review-size",
        "review",
        "size",
        "",
        "review size --base main",
    ),
    t(
        "review-suggest",
        "review",
        "suggest",
        "",
        "review suggest --json",
    ),
    t(
        "review-triage-merge",
        "review",
        "triage-merge",
        "",
        "review triage-merge --lenses a",
    ),
    t(
        "review-worktree-setup",
        "worktree review",
        "setup",
        "",
        "worktree review setup 1 abc",
    ),
    t(
        "review-worktree-teardown",
        "worktree review",
        "teardown",
        "",
        "worktree review teardown .",
    ),
    t(
        "segment-branch",
        "segment",
        "branch",
        "",
        "segment branch --type feat --plan-slug p --n 1 --title t --topology independent",
    ),
    t(
        "segment-size",
        "segment",
        "size",
        "",
        "segment size --base main",
    ),
    t(
        "gate-record",
        "gate",
        "record",
        "",
        "gate record p plan phase -",
    ),
    t(
        "gate-snapshot",
        "gate",
        "snapshot",
        "",
        "gate snapshot p phase",
    ),
    t("gate-check", "gate", "check", "", "gate check p plan"),
    t(
        "checkpoint",
        "plan",
        "checkpoint",
        "",
        "plan checkpoint slug",
    ),
    t(
        "plan-cleanup",
        "plan",
        "cleanup",
        "",
        "plan cleanup my-topic --dry-run",
    ),
    t(
        "work-unit-briefs",
        "plan",
        "brief",
        "",
        "plan brief --out d plan.md WU-1",
    ),
    t("commit-prepare", "commit", "prepare", "", "commit prepare"),
    t("commit-run", "commit", "run", "", "commit run"),
    t("handoff-save", "handoff", "save", "", "handoff save"),
    t("handoff-show", "handoff", "show", "", "handoff show"),
    t(
        "handoff-template",
        "handoff",
        "template",
        "",
        "handoff template",
    ),
    t("handoff-facts", "handoff", "facts", "", "handoff facts"),
    t("handoff-check", "handoff", "check", "", "handoff check"),
    t("memory-context", "memory", "context", "", "memory context"),
    t("memory-rebuild", "memory", "rebuild", "", "memory rebuild"),
    t("memory-stats", "memory", "stats", "", "memory stats"),
    t("code-status", "code", "status", "", "code status"),
    t("code-index", "code", "index", "", "code index"),
    t("code-context", "code", "context", "", "code context retry"),
    t("code-search", "code", "search", "", "code search retry"),
    t("code-symbol", "code", "symbol", "", "code symbol retry"),
    t("code-refs", "code", "refs", "", "code refs retry"),
    t("code-outline", "code", "outline", "", "code outline a.rs"),
    t("code-read", "code", "read", "", "code read a.rs 1:20"),
    t("next-adr-number", "adr", "next", "", "adr next"),
    t(
        "prose-check",
        "prose",
        "check",
        "FILES",
        "prose check docs/adr/0001-x.md",
    ),
    t(
        "add-glossary-term",
        "glossary",
        "add",
        "ENTRY",
        "glossary add x",
    ),
    t(
        "learn-preflight",
        "learn",
        "preflight",
        "",
        "learn preflight",
    ),
    t("learn-collect", "learn", "collect", "", "learn collect git"),
    t(
        "effort-resolve",
        "effort",
        "resolve",
        "",
        "effort resolve agents reviewer",
    ),
    t(
        "effort-choose",
        "effort",
        "choose",
        "",
        "effort choose reviewer",
    ),
    t(
        "atlassian-check",
        "atlassian",
        "check",
        "",
        "atlassian check",
    ),
];

impl Tool {
    /// The full command for `--json`, for example `playbook pr reply`.
    fn command(&self) -> String {
        let mut parts = vec!["playbook", self.group];
        for word in [self.verb, self.args] {
            if !word.is_empty() {
                parts.push(word);
            }
        }
        parts.join(" ")
    }
}

/// The map as the hook prints it: one header line, then one line per
/// command group with its verbs joined by `|`, then the `FULL_LIST` line.
/// Entries that share a group and the same trailing arguments merge into one
/// line.
pub fn render() -> String {
    let mut out = String::from(
        "playbook tools: when an intent is covered here, run `playbook <line>`; otherwise use plain git, gh and file steps. Add --help for flags.\n",
    );
    let mut lines: Vec<(&str, &str, Vec<&str>)> = Vec::new();
    for tool in TOOLS {
        match lines
            .iter_mut()
            .find(|(g, a, _)| *g == tool.group && *a == tool.args)
        {
            Some((_, _, verbs)) => verbs.push(tool.verb),
            None => lines.push((tool.group, tool.args, vec![tool.verb])),
        }
    }
    for (group, args, verbs) in lines {
        let verbs = verbs
            .iter()
            .filter(|v| !v.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("|");
        let _ = write!(out, "{group}");
        if !verbs.is_empty() {
            let _ = write!(out, " {verbs}");
        }
        if !args.is_empty() {
            let _ = write!(out, " {args}");
        }
        out.push('\n');
    }
    out.push_str(FULL_LIST);
    out.push('\n');
    out
}

/// The map as JSON, `[{"intent": ..., "command": ...}]`, for the CI test.
pub fn render_json() -> String {
    let rows: Vec<serde_json::Value> = TOOLS
        .iter()
        .map(|tool| {
            serde_json::json!({
                "intent": tool.intent,
                "command": tool.command(),
                "group": tool.group,
                "verb": tool.verb,
                "args": tool.args,
            })
        })
        .collect();
    serde_json::to_string_pretty(&rows).unwrap_or_default()
}

/// Rough token cost of `text`: four bytes to a token, rounded up.
pub fn tokens(text: &str) -> usize {
    text.len().div_ceil(BYTES_PER_TOKEN)
}

/// Whether `text` costs no more than [`TOKEN_BUDGET`] tokens.
pub fn fits_budget(text: &str) -> bool {
    tokens(text) <= TOKEN_BUDGET
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cli;
    use clap::Parser;
    use std::collections::BTreeSet;

    #[test]
    fn every_sample_parses_through_the_real_cli() {
        for tool in TOOLS {
            let argv = std::iter::once("playbook").chain(tool.sample.split_whitespace());
            if let Err(err) = Cli::try_parse_from(argv) {
                panic!(
                    "{}: `playbook {}` no longer parses: {err}",
                    tool.intent, tool.sample
                );
            }
        }
    }

    #[test]
    fn every_sample_starts_with_its_group_and_verb() {
        for tool in TOOLS {
            let head = [tool.group, tool.verb]
                .into_iter()
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                tool.sample.starts_with(&head),
                "{}: sample `{}` does not start with `{head}`",
                tool.intent,
                tool.sample
            );
        }
    }

    #[test]
    fn every_shown_flag_is_in_the_sample() {
        for tool in TOOLS {
            for flag in tool.args.split_whitespace().filter(|w| w.starts_with("--")) {
                assert!(
                    tool.sample.split_whitespace().any(|w| w == flag),
                    "{}: `{flag}` is shown but not in the sample `{}`",
                    tool.intent,
                    tool.sample
                );
            }
        }
    }

    #[test]
    fn intent_ids_are_unique_lowercase_kebab_case() {
        let mut seen = BTreeSet::new();
        for tool in TOOLS {
            assert!(
                tool.intent
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-'),
                "{} is not kebab-case",
                tool.intent
            );
            assert!(seen.insert(tool.intent), "{} is listed twice", tool.intent);
        }
    }

    #[test]
    fn the_printed_map_fits_the_token_budget() {
        let cost = tokens(&render());
        assert!(
            fits_budget(&render()),
            "the tool map costs about {cost} tokens, budget {TOKEN_BUDGET}"
        );
    }

    #[test]
    fn the_budget_passes_at_299_and_300_tokens_and_fails_at_301() {
        let of_tokens = |n: usize| "x".repeat(n * BYTES_PER_TOKEN);
        assert!(fits_budget(&of_tokens(TOKEN_BUDGET - 1)));
        assert!(fits_budget(&of_tokens(TOKEN_BUDGET)));
        assert!(!fits_budget(&of_tokens(TOKEN_BUDGET + 1)));
    }

    #[test]
    fn grouped_entries_share_one_line() {
        let text = render();
        assert!(
            text.contains("\npath plans|implement|worktrees --create\n"),
            "{text}"
        );
        assert!(text.contains("\npr prepare|create|rules"), "{text}");
        assert!(text.contains("\nrun-context --command NAME"), "{text}");
    }

    #[test]
    fn the_code_line_lists_status_and_index_before_the_queries() {
        let text = render();
        let line = "\nmemory context|rebuild|stats\ncode status|index|context|search|symbol|refs|outline|read\nadr next\n";
        assert_eq!(text.matches(line).count(), 1, "{text}");

        let rows: Vec<serde_json::Value> = serde_json::from_str(&render_json()).unwrap();
        let status = rows
            .iter()
            .find(|row| row["intent"] == "code-status")
            .expect("code-status is listed");
        assert_eq!(status["command"], "playbook code status");
    }

    #[test]
    fn the_map_ends_with_the_pointer_to_the_full_list() {
        let text = render();
        assert!(text.ends_with(&format!("\n{FULL_LIST}\n")), "{text}");
        assert!(
            text.len() <= TOKEN_BUDGET * BYTES_PER_TOKEN,
            "the map is {} bytes",
            text.len()
        );
    }

    #[test]
    fn the_full_list_command_exists() {
        assert!(Cli::try_parse_from(["playbook", "commands"]).is_ok());
    }

    #[test]
    fn json_lists_every_intent_with_its_full_command() {
        let rows: Vec<serde_json::Value> = serde_json::from_str(&render_json()).unwrap();
        assert_eq!(rows.len(), TOOLS.len());
        assert_eq!(rows[0]["intent"], "plans-dir");
        assert_eq!(rows[0]["command"], "playbook path plans --create");
        assert_eq!(rows[0]["group"], "path");
        assert_eq!(rows[0]["verb"], "plans");
        assert_eq!(rows[0]["args"], "--create");
    }
}
