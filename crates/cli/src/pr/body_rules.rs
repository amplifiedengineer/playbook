// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The structure rules for a PR body, a PR title and a commit title: the
//! team template, a length cap, the issue clause, the ticket on the first
//! line and the ticket in the conventional-commit scope.
//!
//! Every rule has a level (`off`, `warn`, `block`) read from the
//! `pr.rules` config key. A ticket rule applies only when a ticket key is
//! known for the branch. A key is never invented.

use crate::common::re::static_regex;
use regex::Regex;
use serde_json::Value;
use std::path::Path;

/// The key shape used when `pr.ticketPatterns` is not set.
pub const DEFAULT_TICKET_PATTERN: &str = r"[A-Z][A-Z0-9]+-[0-9]+";

/// Body length cap in characters.
pub const DEFAULT_MAX_CHARS: usize = 1200;

/// The `pr.rules` value that makes every structure rule warn.
const MODE_WARN: &str = "warn";
/// The `pr.rules` value that turns every structure rule off.
const MODE_OFF: &str = "off";

/// What a failed rule does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Off,
    Warn,
    Block,
}

impl Level {
    fn parse(text: &str, fallback: Level) -> Level {
        match text {
            "off" => Level::Off,
            "warn" => Level::Warn,
            "block" => Level::Block,
            _ => fallback,
        }
    }
}

/// The levels and limits, one field per config key.
#[derive(Debug, Clone)]
pub struct Rules {
    pub template: Level,
    pub length: Level,
    pub issue: Level,
    pub ticket_line: Level,
    pub scope: Level,
    pub prose: Level,
    pub max_chars: usize,
    pub patterns: Vec<String>,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            template: Level::Block,
            length: Level::Warn,
            issue: Level::Warn,
            ticket_line: Level::Block,
            scope: Level::Warn,
            prose: Level::Warn,
            max_chars: DEFAULT_MAX_CHARS,
            patterns: vec![DEFAULT_TICKET_PATTERN.to_string()],
        }
    }
}

impl Rules {
    /// Every rule off: only the plain text checks of `pr create` run.
    pub fn off() -> Rules {
        Rules {
            prose: Level::Off,
            ..Rules::with_mode(MODE_OFF)
        }
    }

    /// The five structure rules at the level the `pr.rules` value names. An
    /// unknown value keeps each rule at its default.
    pub fn with_mode(mode: &str) -> Rules {
        let all = |level: Level| Rules {
            template: level,
            length: level,
            issue: level,
            ticket_line: level,
            scope: level,
            ..Rules::default()
        };
        match mode {
            MODE_WARN => all(Level::Warn),
            MODE_OFF => all(Level::Off),
            _ => Rules::default(),
        }
    }

    /// Reads the `pr.rules` key through the config tiers. An unreadable
    /// or invalid value keeps the default.
    pub fn load(home: &Path, repo_slug: Option<&str>) -> Rules {
        let get = |key: &str| {
            crate::config::resolve_valid(key, home, repo_slug)
                .ok()
                .map(|(value, _, _)| value)
        };
        let mut rules = match get("pr.rules") {
            Some(Value::String(mode)) => Rules::with_mode(&mode),
            _ => Rules::default(),
        };
        if let Some(Value::String(s)) = get("hooks.proseLint.mode") {
            rules.prose = Level::parse(&s, rules.prose);
        }
        if let Some(Value::Array(items)) = get("pr.ticketPatterns") {
            let list: Vec<String> = items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .filter(|p| compile_pattern(p).is_ok())
                .collect();
            if !list.is_empty() {
                rules.patterns = list;
            }
        }
        rules
    }
}

/// What is known about the branch when a body or title is checked.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// The ticket key, when one was found. `None` turns every ticket rule off.
    pub ticket: Option<String>,
    /// The GitHub issue number, when the args or the branch name one.
    pub issue: Option<u64>,
    /// The PR targets a branch other than the default branch.
    pub stacked: bool,
}

/// The result of a check. A block stops the command; a warn is printed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Findings {
    pub blocks: Vec<String>,
    pub warns: Vec<String>,
}

impl Findings {
    fn add(&mut self, level: Level, message: String) {
        match level {
            Level::Off => {}
            Level::Warn => self.warns.push(message),
            Level::Block => self.blocks.push(message),
        }
    }

    pub fn extend(&mut self, other: Findings) {
        self.blocks.extend(other.blocks);
        self.warns.extend(other.warns);
    }
}

/// Compiles a configured pattern the way detection uses it.
pub fn compile_pattern(pattern: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!(r"\b(?:{pattern})\b"))
}

/// Uppercase tokens with a number that are standards, not tickets.
const NOT_TICKETS: &[&str] = &[
    "UTF", "ISO", "SHA", "HTTP", "HTTPS", "RFC", "MD", "TLS", "SSL",
];

fn pattern_regexes(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|p| compile_pattern(p).ok())
        .collect()
}

/// Whether `ticket` appears in `text` as a whole token, so PROJ-1 does not
/// match PROJ-12.
fn has_ticket(text: &str, ticket: &str) -> bool {
    Regex::new(&format!(
        r"(?:^|[^A-Za-z0-9-]){}(?:$|[^A-Za-z0-9])",
        regex::escape(ticket)
    ))
    .is_ok_and(|re| re.is_match(text))
}

/// The ticket key for a branch. Order: explicit arg, then the branch name,
/// then the scopes of the commit subjects (newest first). `none` as the arg
/// turns the rules off. `None` when nothing matches.
pub fn detect_ticket(
    arg: Option<&str>,
    branch: &str,
    commit_subjects: &[String],
    patterns: &[String],
) -> Option<String> {
    if let Some(arg) = arg.map(str::trim).filter(|a| !a.is_empty()) {
        return (!arg.eq_ignore_ascii_case("none")).then(|| arg.to_string());
    }
    let regexes = pattern_regexes(patterns);
    let find = |text: &str| {
        regexes.iter().find_map(|re| {
            re.find_iter(text)
                .map(|m| m.as_str())
                .find(|k| {
                    !k.is_empty() && !NOT_TICKETS.contains(&k.split('-').next().unwrap_or(""))
                })
                .map(str::to_string)
        })
    };
    if let Some(key) = find(branch) {
        return Some(key);
    }
    let scope = static_regex(r"^\w+\(([^)]+)\)!?:");
    commit_subjects.iter().find_map(|subject| {
        let inner = scope.captures(subject)?.get(1)?.as_str();
        find(inner)
    })
}

/// The issue number from the arg, else from a branch such as `fix/issue-12`
/// or `gh-12`.
pub fn detect_issue(arg: Option<&str>, branch: &str) -> Option<u64> {
    if let Some(arg) = arg.map(|a| a.trim().trim_start_matches('#')) {
        if let Ok(n) = arg.parse::<u64>() {
            return Some(n);
        }
    }
    let re = static_regex(r"(?i)(?:^|[/_-])(?:issue|gh)[-_/]?([0-9]+)(?:$|[/_-])");
    re.captures(branch)?.get(1)?.as_str().parse().ok()
}

const ORDER: &[&str] = &["Summary", "Notes for reviewers", "Related work"];
const LEGACY: &str = "What Changed";

struct Section {
    name: String,
    content: String,
}

fn sections(body: &str) -> (String, Vec<Section>) {
    let mut head = String::new();
    let mut out: Vec<Section> = Vec::new();
    let mut fenced = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let heading = (!fenced)
            .then(|| line.strip_prefix("## "))
            .flatten()
            .map(|h| h.trim().to_string());
        match (heading, out.last_mut()) {
            (Some(name), _) => out.push(Section {
                name,
                content: String::new(),
            }),
            (None, Some(sec)) => {
                sec.content.push_str(line);
                sec.content.push('\n');
            }
            (None, None) => {
                head.push_str(line);
                head.push('\n');
            }
        }
    }
    (head, out)
}

fn is_empty_section(content: &str) -> bool {
    let comments = static_regex(r"(?s)<!--.*?-->");
    let text = comments.replace_all(content, "");
    let text = text.trim();
    text.is_empty()
        || matches!(
            text.to_ascii_lowercase().trim_end_matches('.'),
            "n/a" | "none" | "tbd" | "todo" | "nothing"
        )
}

fn template_findings(
    body: &str,
    level: Level,
    patterns: &[String],
    ticket: Option<&str>,
    out: &mut Findings,
) {
    let (head, secs) = sections(body);
    let fix = "use the template: optional ticket line, ## Summary, optional ## Notes for reviewers, optional ## Related work";
    if secs.is_empty() || secs[0].name != "Summary" {
        out.add(
            level,
            format!("PR body must start its sections with ## Summary ({fix})"),
        );
    }
    let mut last = 0usize;
    for sec in &secs {
        if sec.name == LEGACY {
            if level != Level::Off {
                out.warns.push(
                    "PR body uses the old ## What Changed section; fold it into ## Summary"
                        .to_string(),
                );
            }
            continue;
        }
        match ORDER.iter().position(|n| *n == sec.name) {
            None => out.add(
                level,
                format!(
                    "PR body has the section ## {} that is not in the template ({fix})",
                    sec.name
                ),
            ),
            Some(pos) if pos < last => out.add(
                level,
                format!("PR body section ## {} is out of order ({fix})", sec.name),
            ),
            Some(pos) => last = pos,
        }
        if is_empty_section(&sec.content) {
            out.add(
                level,
                format!(
                    "PR body section ## {} is empty; remove the heading",
                    sec.name
                ),
            );
        }
    }
    let head_lines: Vec<&str> = head.lines().filter(|l| !l.trim().is_empty()).collect();
    let ticket_like = head_lines.len() == 1
        && (pattern_regexes(patterns)
            .iter()
            .any(|re| re.is_match(head_lines[0]))
            || ticket.is_some_and(|t| has_ticket(head_lines[0], t)));
    if head_lines.len() > 1 || (head_lines.len() == 1 && !ticket_like) {
        out.add(
            level,
            "PR body has text before ## Summary; only the ticket line may come first".to_string(),
        );
    }
}

/// Checks a PR body against the rules.
pub fn check_body(body: &str, rules: &Rules, ctx: &Context) -> Findings {
    let mut out = Findings::default();
    template_findings(
        body,
        rules.template,
        &rules.patterns,
        ctx.ticket.as_deref(),
        &mut out,
    );

    let length = body.chars().count();
    if length > rules.max_chars {
        out.add(
            rules.length,
            format!(
                "PR body is {length} characters, the cap is {}; cut it to what a reviewer needs",
                rules.max_chars
            ),
        );
    }

    if let Some(ticket) = &ctx.ticket {
        let first = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        if !has_ticket(first, ticket) {
            out.add(
                rules.ticket_line,
                format!("the first line of the PR body must carry the ticket {ticket}, for example: Ticket: {ticket}"),
            );
        }
    }

    if let Some(n) = ctx.issue {
        let clause = static_regex(&format!(
            r"(?im)\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?|refs?|relates to):?\s+#{n}\b"
        ));
        if !clause.is_match(body) {
            out.add(
                rules.issue,
                format!("the PR body must name issue #{n}: add `Closes #{n}` if it fully resolves it, `Refs #{n}` if it only advances it"),
            );
        }
    }

    // Hook point: the prose lint for filler and banned patterns lives in
    // `common::prose` and is the only table of those patterns.
    for problem in crate::common::prose::check(body) {
        out.add(rules.prose, format!("PR body {problem}"));
    }
    out
}

/// `title` with `ticket` as its scope, or an example title when `title` is
/// not a conventional one.
fn with_ticket_scope(title: &str, ticket: &str) -> String {
    let head = static_regex(r"^(\w+)(?:\([^)]*\))?(!?):\s*(.+)$");
    match head.captures(title) {
        Some(c) => format!("{}({ticket}){}: {}", &c[1], &c[2], &c[3]),
        None => format!("feat({ticket}): add retry to the sync job"),
    }
}

/// Checks that the conventional scope of a PR title or commit title carries
/// the ticket. Stacked work always blocks.
pub fn check_scope(label: &str, title: &str, rules: &Rules, ctx: &Context) -> Findings {
    let mut out = Findings::default();
    let Some(ticket) = &ctx.ticket else {
        return out;
    };
    let scope = static_regex(r"^\w+\(([^)]*)\)!?:");
    let has = scope
        .captures(title)
        .and_then(|c| c.get(1))
        .is_some_and(|m| has_ticket(m.as_str(), ticket));
    if !has {
        let level = if ctx.stacked && rules.scope != Level::Off {
            Level::Block
        } else {
            rules.scope
        };
        out.add(
            level,
            format!(
                "{label} must carry the ticket in its scope, for example `{}`",
                with_ticket_scope(title, ticket)
            ),
        );
    }
    out
}

/// Everything one PR needs checked, for every path that opens one.
pub struct Input<'a> {
    /// `None` skips the title check (an unchanged PR title).
    pub title: Option<&'a str>,
    /// `None` skips the body checks (an update that leaves the body alone).
    pub body: Option<&'a str>,
    pub branch: &'a str,
    /// The subject line of every commit in the PR.
    pub subjects: &'a [String],
    pub stacked: bool,
    pub ticket_arg: Option<&'a str>,
    pub issue_arg: Option<&'a str>,
}

/// The one rule path for `pr create` and `deliver`: the body, the title and
/// every commit title. `Ok` carries the warnings; `Err` the blocks.
pub fn enforce(input: &Input<'_>, rules: &Rules) -> Result<Vec<String>, Vec<String>> {
    if let Some(raw) = input.issue_arg {
        if detect_issue(Some(raw), "").is_none() {
            return Err(vec![format!(
                "--issue must be a number such as 12 or #12, got `{raw}`"
            )]);
        }
    }
    let ctx = Context {
        ticket: detect_ticket(
            input.ticket_arg,
            input.branch,
            input.subjects,
            &rules.patterns,
        ),
        issue: detect_issue(input.issue_arg, input.branch),
        stacked: input.stacked,
    };
    let mut found = Findings::default();
    if let Some(body) = input.body {
        found.extend(check_body(body, rules, &ctx));
    }
    if let Some(title) = input.title {
        found.extend(check_scope("PR title", title, rules, &ctx));
    }
    for subject in input.subjects {
        found.extend(check_scope(
            &format!("commit title `{subject}`"),
            subject,
            rules,
            &ctx,
        ));
    }
    if found.blocks.is_empty() {
        Ok(found.warns)
    } else {
        Err(found.blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(ticket: Option<&str>, issue: Option<u64>, stacked: bool) -> Context {
        Context {
            ticket: ticket.map(str::to_string),
            issue,
            stacked,
        }
    }

    const GOOD: &str = "Ticket: PROJ-12\n\n## Summary\n\nRetries the sync job when the queue is busy.\n\nCloses #7\n";

    #[test]
    fn a_body_that_follows_the_template_passes() {
        let found = check_body(
            GOOD,
            &Rules::default(),
            &ctx(Some("PROJ-12"), Some(7), false),
        );

        assert_eq!(found, Findings::default());
    }

    #[test]
    #[rustfmt::skip]
    fn template_cases_are_judged_by_the_template_level() {
        let cases: &[(&str, &str, bool)] = &[("no sections", "just text\n", true),
            ("wrong first section", "## Notes for reviewers\n\nx\n", true),
            ("unknown section", "## Summary\n\nx\n\n## Test plan\n\n- run it\n", true),
            ("out of order", "## Summary\n\nx\n\n## Related work\n\ny\n\n## Notes for reviewers\n\nz\n", true),
            ("empty section", "## Summary\n\nx\n\n## Notes for reviewers\n\nN/A\n", true),
            ("comment only", "## Summary\n\n<!-- why -->\n", true),
            ("text before summary", "Hello there\nmore\n\n## Summary\n\nx\n", true),
            ("summary only", "## Summary\n\nx\n", false),
            ("all sections", "## Summary\n\nx\n\n## Notes for reviewers\n\ny\n\n## Related work\n\nz\n", false),];
        for (name, body, blocked) in cases {
            let found = check_body(body, &Rules::default(), &ctx(None, None, false));

            assert_eq!(!found.blocks.is_empty(), *blocked, "{name}: {found:?}");
        }
    }

    #[test]
    fn the_old_what_changed_section_only_warns() {
        let body = "## Summary\n\nx\n\n## What Changed\n\n- y\n";

        let found = check_body(body, &Rules::default(), &ctx(None, None, false));

        assert!(found.blocks.is_empty(), "{found:?}");
        assert!(found.warns.iter().any(|w| w.contains("What Changed")));
    }

    #[test]
    fn a_heading_inside_a_code_fence_is_not_a_section() {
        let body = "## Summary\n\n```\n## Anything\n```\n";

        let found = check_body(body, &Rules::default(), &ctx(None, None, false));

        assert!(found.blocks.is_empty(), "{found:?}");
    }

    #[test]
    fn a_body_over_the_cap_warns_by_default_and_blocks_when_configured() {
        let body = format!("## Summary\n\n{}\n", "a ".repeat(700));
        let mut rules = Rules::default();

        let warned = check_body(&body, &rules, &ctx(None, None, false));
        rules.length = Level::Block;
        let blocked = check_body(&body, &rules, &ctx(None, None, false));
        rules.length = Level::Off;
        let off = check_body(&body, &rules, &ctx(None, None, false));

        assert!(warned.warns.iter().any(|w| w.contains("cap is 1200")));
        assert!(blocked.blocks.iter().any(|w| w.contains("cap is 1200")));
        assert!(off.warns.is_empty() && off.blocks.is_empty());
    }

    #[test]
    #[rustfmt::skip]
    fn ticket_line_cases() {
        let cases: &[(&str, &str, Option<&str>, bool)] = &[("known and present", "Ticket: PROJ-1\n\n## Summary\n\nx\n", Some("PROJ-1"), false),
            ("known and linked", "[PROJ-1](https://x.test/PROJ-1)\n\n## Summary\n\nx\n", Some("PROJ-1"), false),
            ("known and missing", "## Summary\n\nx\n", Some("PROJ-1"), true),
            ("known and in the wrong place", "## Summary\n\nPROJ-1 here\n", Some("PROJ-1"), true),
            ("none known", "## Summary\n\nx\n", None, false),];
        for (name, body, ticket, blocked) in cases {
            let found = check_body(body, &Rules::default(), &ctx(*ticket, None, false));

            assert_eq!(!found.blocks.is_empty(), *blocked, "{name}: {found:?}");
        }
    }

    #[test]
    #[rustfmt::skip]
    fn issue_clause_cases() {
        let cases: &[(&str, &str, Option<u64>, bool)] = &[("closes", "## Summary\n\nx\n\nCloses #4\n", Some(4), false),
            ("refs", "## Summary\n\nx\n\nRefs #4\n", Some(4), false),
            ("missing", "## Summary\n\nx\n", Some(4), true),
            ("colon form", "## Summary\n\nCloses: #4\n", Some(4), false),
            ("other number", "## Summary\n\nCloses #44\n", Some(4), true),
            ("no issue known", "## Summary\n\nx\n", None, false),];
        for (name, body, issue, warned) in cases {
            let found = check_body(body, &Rules::default(), &ctx(None, *issue, false));

            assert_eq!(!found.warns.is_empty(), *warned, "{name}: {found:?}");
            assert!(found.blocks.is_empty(), "{name}: warn by default");
        }
    }

    #[test]
    #[rustfmt::skip]
    #[allow(clippy::type_complexity, reason = "a case table: name, title, ticket, stacked, warn, block")]
    fn scope_cases() {
        let cases: &[(&str, &str, Option<&str>, bool, bool, bool)] = &[("scoped", "feat(PROJ-1): add retry", Some("PROJ-1"), false, false, false),
            ("scoped and stacked", "feat(PROJ-1): add retry", Some("PROJ-1"), true, false, false),
            ("missing, not stacked", "feat(sync): add retry", Some("PROJ-1"), false, true, false),
            ("missing, stacked", "feat(sync): add retry", Some("PROJ-1"), true, false, true),
            ("no scope, stacked", "feat: add retry", Some("PROJ-1"), true, false, true),
            ("no ticket known, stacked", "feat: add retry", None, true, false, false),
            ("no ticket known", "feat: add retry", None, false, false, false),];
        for (name, title, ticket, stacked, warn, block) in cases {
            let found = check_scope(
                "PR title",
                title,
                &Rules::default(),
                &ctx(*ticket, None, *stacked),
            );

            assert_eq!(!found.warns.is_empty(), *warn, "{name}: {found:?}");
            assert_eq!(!found.blocks.is_empty(), *block, "{name}: {found:?}");
        }
    }

    #[test]
    #[rustfmt::skip]
    #[allow(clippy::type_complexity, reason = "a case table")]
    fn ticket_detection_follows_the_documented_order() {
        let patterns = vec![DEFAULT_TICKET_PATTERN.to_string()];
        let subjects = vec!["feat(OPS-9): older".to_string()];
        let cases: &[(&str, Option<&str>, &str, &[String], Option<&str>)] = &[("arg wins", Some("ARG-1"), "feat/BR-2-x", &subjects, Some("ARG-1")),
            ("none turns it off", Some("none"), "feat/BR-2-x", &subjects, None),
            ("branch next", None, "feat/BR-2-x", &subjects, Some("BR-2")),
            ("commit scope last", None, "feat/plain", &subjects, Some("OPS-9")),
            ("nothing found", None, "feat/plain", &[], None),];
        for (name, arg, branch, subs, expected) in cases {
            let got = detect_ticket(*arg, branch, subs, &patterns);

            assert_eq!(got.as_deref(), *expected, "{name}");
        }
    }

    #[test]
    fn detection_skips_standards_and_matches_whole_tokens_only() {
        let patterns = vec![DEFAULT_TICKET_PATTERN.to_string()];

        let got = detect_ticket(None, "fix/UTF-8-decoding", &[], &patterns);

        assert_eq!(got, None);
        assert!(has_ticket("Ticket: PROJ-1", "PROJ-1"));
        assert!(!has_ticket("Ticket: PROJ-12", "PROJ-1"));
        assert!(!has_ticket("feat(XPROJ-1): x", "PROJ-1"));
    }

    #[test]
    fn prose_before_the_summary_is_refused_even_as_one_line() {
        let body = "This PR fixes stuff.\n\n## Summary\n\nx\n";

        let found = check_body(body, &Rules::default(), &ctx(None, None, false));

        assert!(found.blocks.iter().any(|b| b.contains("before ## Summary")));
    }

    #[test]
    fn the_pr_rules_mode_sets_the_level_of_each_rule() {
        use Level::{Block, Off, Warn};
        // Arrange
        // (mode, template, length, issue, ticketLine, scope)
        let table = [
            ("default", [Block, Warn, Warn, Block, Warn]),
            ("warn", [Warn, Warn, Warn, Warn, Warn]),
            ("off", [Off, Off, Off, Off, Off]),
        ];
        for (mode, want) in table {
            // Act
            let rules = Rules::with_mode(mode);
            // Assert
            let got = [
                rules.template,
                rules.length,
                rules.issue,
                rules.ticket_line,
                rules.scope,
            ];
            assert_eq!(got, want, "pr.rules = {mode}");
        }
    }

    #[test]
    fn an_unknown_pr_rules_mode_keeps_the_defaults() {
        // Arrange
        // Act
        let rules = Rules::with_mode("strict");
        // Assert
        assert_eq!(format!("{rules:?}"), format!("{:?}", Rules::default()));
    }

    #[test]
    fn an_empty_config_loads_the_same_defaults_as_the_struct() {
        let home = std::env::temp_dir().join(format!("pb-rules-{}", crate::testing::run_id()));

        let loaded = Rules::load(&home, None);

        let want = Rules::default();
        assert_eq!(
            format!("{loaded:?}"),
            format!("{want:?}"),
            "keys.rs defaults and Rules::default must agree"
        );
    }

    #[test]
    fn issue_detection_reads_the_arg_then_the_branch() {
        assert_eq!(detect_issue(Some("#12"), "main"), Some(12));
        assert_eq!(detect_issue(None, "fix/issue-34-retry"), Some(34));
        assert_eq!(detect_issue(None, "fix/gh-5"), Some(5));
        assert_eq!(detect_issue(None, "fix/retry"), None);
    }
}
