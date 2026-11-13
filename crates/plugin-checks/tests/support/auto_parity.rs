// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Structural parity checks for the command files under `commands/`.
//!
//! A command must read the run mode (`playbook mode status`) in a `Step 0`
//! section before it can ask the user anything, so unattended runs never hit
//! a question first. These tests parse each file into frontmatter and
//! heading-delimited sections instead of grepping for markers, because a
//! marker can sit in prose, in a fenced example or in the wrong section and
//! still match.
//!
//! The file table is `COMMANDS`. A row is covered by the Step 0 and ordering
//! checks, and by the per-row checks for its `Auto` kind where one exists. The
//! checks for the `Dedicated`, `Stages` and
//! kinds name their files, so a new command of those kinds
//! needs its own test. Model behaviour is not testable here: these checks
//! cover prose structure only.
//!
//! The parts only one test file uses are in `auto_parity_auto.rs` and
//! `auto_parity_fix.rs`, so each test file includes only what it uses.

use regex::Regex;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

/// What a command does differently when the run mode is auto. Each variant
/// selects the structural checks that apply to the row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Auto {
    /// Checked by its own dedicated tests further down.
    Dedicated,
    /// Stops at Step 0 with a one-line refusal naming the command and why.
    Refuses,
    /// Behaves as `--stage`: collects candidates and skips the write prompt.
    Stages,
    /// Asks nothing and behaves the same in both modes, so it has no Step 0.
    Unchanged,
    /// Asks only inside an explicit `if mode is ask` branch, so an
    /// unattended run never reaches a question.
    AsksOnlyInAskMode,
    /// Picks between two other commands and hands the run to one of them.
    /// In auto mode it takes its own recommendation and passes `--auto` on.
    PicksAnEngine,
}

/// One command file and which structural rules apply to it.
pub(super) struct CommandSpec {
    pub(super) name: &'static str,
    /// The command asks the user something outside Step 0. The positive
    /// control asserts this really is detected, so a detector that matches
    /// nothing cannot let the ordering checks pass vacuously. Rows that never
    /// ask leave it false: their prose only mentions asking in order to
    /// forbid it, so ordering against a detected ask would be meaningless.
    // Only the ordering checks read it; the `fix` checks do not.
    #[allow(dead_code)]
    pub(super) asks: bool,
    pub(super) auto: Auto,
}

pub(super) const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "plan",
        asks: true,
        auto: Auto::Dedicated,
    },
    CommandSpec {
        name: "implement",
        asks: true,
        auto: Auto::Dedicated,
    },
    CommandSpec {
        name: "commit",
        asks: false,
        // The binary decides the mode and never forces; see the
        // parity_no_force tests in crates/cli/tests/commit_cli.rs.
        auto: Auto::Unchanged,
    },
    CommandSpec {
        name: "pr-create",
        asks: false,
        // The binary reports the mode; the finish skill skips the review.
        auto: Auto::Unchanged,
    },
    CommandSpec {
        name: "setup",
        asks: true,
        auto: Auto::Refuses,
    },
    CommandSpec {
        name: "adr",
        asks: true,
        auto: Auto::Refuses,
    },
    CommandSpec {
        name: "pr-comments",
        asks: true,
        auto: Auto::Refuses,
    },
    CommandSpec {
        name: "learn",
        asks: true,
        auto: Auto::Stages,
    },
    CommandSpec {
        name: "audit",
        asks: false,
        auto: Auto::Unchanged,
    },
    CommandSpec {
        name: "doctor",
        asks: false,
        auto: Auto::Unchanged,
    },
    CommandSpec {
        name: "handoff",
        asks: false,
        auto: Auto::Unchanged,
    },
    CommandSpec {
        name: "session-handoff",
        asks: false,
        auto: Auto::Unchanged,
    },
    CommandSpec {
        name: "session-start",
        asks: false,
        auto: Auto::Unchanged,
    },
    CommandSpec {
        name: "review",
        asks: true,
        auto: Auto::PicksAnEngine,
    },
    CommandSpec {
        name: "fix",
        asks: true,
        auto: Auto::AsksOnlyInAskMode,
    },
];

pub(super) fn rows(auto: Auto) -> impl Iterator<Item = &'static CommandSpec> {
    COMMANDS.iter().filter(move |c| c.auto == auto)
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

pub(super) struct CommandFile {
    pub(super) name: String,
    pub(super) lines: Vec<String>,
    /// Index of the first line after the closing frontmatter fence.
    pub(super) body_start: usize,
}

struct Heading {
    level: usize,
    title: String,
    line: usize,
}

/// A heading and the lines it owns, `start..end` (end exclusive). A section
/// ends at the next heading of the same or a shallower level.
pub(super) struct Section {
    pub(super) title: String,
    pub(super) start: usize,
    pub(super) end: usize,
}

pub(super) fn commands_dir() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT")).join("commands")
}

pub(super) fn load(name: &str) -> CommandFile {
    let path = commands_dir().join(format!("{name}.md"));
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let body_start = frontmatter_end(&lines)
        .unwrap_or_else(|| panic!("commands/{name}.md has no closed frontmatter block"));
    CommandFile {
        name: name.to_owned(),
        lines,
        body_start,
    }
}

/// Index just past the closing `---` of a leading frontmatter block.
fn frontmatter_end(lines: &[String]) -> Option<usize> {
    if lines.first().map(String::as_str) != Some("---") {
        return None;
    }
    lines[1..].iter().position(|l| l == "---").map(|i| i + 2)
}

impl CommandFile {
    fn frontmatter(&self) -> &[String] {
        &self.lines[1..self.body_start - 1]
    }

    /// The comma-separated `allowed-tools` frontmatter value as a list.
    pub(super) fn allowed_tools(&self) -> Vec<String> {
        self.frontmatter()
            .iter()
            .find_map(|l| l.strip_prefix("allowed-tools:"))
            .unwrap_or_else(|| panic!("commands/{}.md has no allowed-tools key", self.name))
            .split(',')
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect()
    }

    /// The raw value of a top-level frontmatter key, quotes included.
    pub(super) fn frontmatter_value(&self, key: &str) -> Option<String> {
        let prefix = format!("{key}:");
        self.frontmatter()
            .iter()
            .find_map(|l| l.strip_prefix(&prefix))
            .map(|v| v.trim().to_owned())
    }

    /// Markdown headings outside fenced code blocks, so a `# comment` inside
    /// a shell example is not mistaken for a section.
    fn headings(&self) -> Vec<Heading> {
        let mut out = Vec::new();
        let mut in_fence = false;
        for (line, text) in self.lines.iter().enumerate().skip(self.body_start) {
            if text.trim_start().starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                continue;
            }
            if let Some(title) = heading_title(text) {
                out.push(Heading {
                    level: text.chars().take_while(|&c| c == '#').count(),
                    title: title.to_owned(),
                    line,
                });
            }
        }
        out
    }

    /// The first section whose heading title satisfies `matches`.
    pub(super) fn section(&self, matches: impl Fn(&str) -> bool) -> Option<Section> {
        let headings = self.headings();
        let idx = headings.iter().position(|h| matches(&h.title))?;
        let end = headings[idx + 1..]
            .iter()
            .find(|h| h.level <= headings[idx].level)
            .map_or(self.lines.len(), |h| h.line);
        Some(Section {
            title: headings[idx].title.clone(),
            start: headings[idx].line,
            end,
        })
    }

    pub(super) fn text_of(&self, s: &Section) -> String {
        self.lines[s.start..s.end].join("\n")
    }
}

/// A line that poses a question to the user. Anchored on question forms and
/// whole words: a bare `asks` substring would also match "tasks", and a bare
/// "ask" would match prose that only forbids asking. The `Q<n>` form is a
/// labelled prompt whose quoted text holds a question mark, the shape the
/// review commands use for their posting questions.
pub(super) fn is_ask(line: &str) -> bool {
    static ASK: OnceLock<Regex> = OnceLock::new();
    ASK.get_or_init(|| {
        Regex::new(
            r#"AskUserQuestion|(?i:ask the user)|(?i:ask once)|\bAsk:|\basks\b|\[Y/n\]|\bDoes this\b|Resume it\?|\bQ\d\b[:*\s]*"[^"\n]*\?[^"\n]*""#,
        )
        .expect("ask detector pattern is valid")
    })
    .is_match(line)
}

pub(super) fn heading_title(line: &str) -> Option<&str> {
    let level = line.chars().take_while(|&c| c == '#').count();
    ((1..=6).contains(&level) && line[level..].starts_with(' ')).then(|| line[level..].trim())
}

pub(super) fn require_section(file: &CommandFile, found: Option<Section>, what: &str) -> Section {
    found.unwrap_or_else(|| panic!("commands/{}.md has no {what} section", file.name))
}
