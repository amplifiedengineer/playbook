// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook review lenses pick`: which conditional reviewers a deep review
//! adds for a diff, each with the evidence that tripped it. The trigger table
//! lives here so it is tested code, not a sentence the model re-reads each run.

use super::size::parse_numstat;
use super::suggest::{is_doc, is_manifest, is_migration, local_base};
use crate::common::re::static_regex;
use crate::json::envelope;
use crate::pr::shared::git;
use regex::Regex;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::LazyLock;

/// Changed lines past which a diff is big enough for a duplication pass.
const LARGE_DIFF_LINES: u64 = 300;
/// Changed files of one kind in one directory that count as many similar files.
const SIMILAR_FILES: usize = 6;
/// Indent levels of an added line that count as deep nesting.
const DEEP_NESTING_LEVELS: usize = 5;
/// Text in an added line that flags a breaking change.
const BREAKING_MARKER: &str = "BREAKING";
/// The envelope code of a failed run.
const FAILED_CODE: i32 = 1;
/// Printed when no conditional lens is triggered.
const NO_LENSES: &str = "NONE";

struct Pats {
    deploy: Regex,
    adr: Regex,
    readme: Regex,
    collections: Regex,
    integration: Regex,
    public_api: Regex,
    deep: Regex,
}

static PATS: LazyLock<Pats> = LazyLock::new(|| Pats {
    deploy: static_regex(
        r"(?i)^\.github/(workflows|actions)/|(^|/)(Dockerfile[^/]*|docker-compose[^/]*|\.env[^/]*)$|(^|/)(config|deploy|deployment|k8s|helm|terraform)/|\.tf$",
    ),
    adr: static_regex(r"(?i)(^|/)(adrs?|decisions)/[^/]+\.md$|(^|/)adr-[^/]*\.md$"),
    readme: static_regex(r"(?i)(^|/)readme[^/]*$"),
    collections: static_regex(
        r"(?i)\.sort\w*\(|\bsorted\(|binary_search|\bbisect\w*|\b(dfs|bfs|dijkstra|topological_?sort)\b",
    ),
    integration: static_regex(
        r"(?i)feature[_ -]?flags?|\bflags?\.|\.emit\(|\bpublish\(|\bsubscribe\(|https?://|\bfetch\(|reqwest",
    ),
    public_api: static_regex(
        r"^\s*(pub(\([a-z]+\))?\s+(fn|struct|enum|trait|type|const)\b|export\s|func [A-Z])",
    ),
    deep: static_regex(&format!(
        r"^( {{{},}}|\t{{{DEEP_NESTING_LEVELS},}})\S",
        DEEP_NESTING_LEVELS * 4
    )),
});

/// One changed file and whether the branch added it.
struct Changed {
    path: String,
    added: bool,
}

/// One added line and the file it landed in.
struct AddedLine {
    path: String,
    text: String,
}

/// What the triggers read from the diff.
struct Facts {
    files: Vec<Changed>,
    /// Top-most directories of added files that the base did not have.
    new_dirs: BTreeSet<String>,
    added_lines: Vec<AddedLine>,
    changed_lines: u64,
}

/// One triggered lens and why.
#[derive(Debug, PartialEq, Eq)]
pub struct Pick {
    pub lens: &'static str,
    pub trigger: String,
}

fn code_lines(facts: &Facts) -> impl Iterator<Item = &AddedLine> {
    facts.added_lines.iter().filter(|l| !is_doc(&l.path))
}

fn first_path(facts: &Facts, matches: impl Fn(&str) -> bool) -> Option<&str> {
    facts
        .files
        .iter()
        .map(|f| f.path.as_str())
        .find(|p| matches(p))
}

fn architecture(f: &Facts) -> Option<String> {
    if let Some(dir) = f.new_dirs.iter().next() {
        return Some(format!("new directory {dir}"));
    }
    first_path(f, is_manifest).map(|p| format!("dependency manifest {p} changed"))
}

fn big_o(f: &Facts) -> Option<String> {
    code_lines(f)
        .find(|l| PATS.collections.is_match(&l.text))
        .map(|l| format!("sorting, searching or graph traversal added in {}", l.path))
}

fn complexity(f: &Facts) -> Option<String> {
    code_lines(f)
        .find(|l| PATS.deep.is_match(&l.text))
        .map(|l| {
            format!(
                "added line nested {DEEP_NESTING_LEVELS} or more levels deep in {}",
                l.path
            )
        })
}

fn integration(f: &Facts) -> Option<String> {
    if let Some(p) = first_path(f, |p| PATS.deploy.is_match(p)) {
        return Some(format!("deployment or config file {p}"));
    }
    code_lines(f)
        .find(|l| PATS.integration.is_match(&l.text))
        .map(|l| format!("feature flags, events or external APIs added in {}", l.path))
}

fn migration(f: &Facts) -> Option<String> {
    first_path(f, is_migration).map(|p| format!("migration or schema file {p}"))
}

fn docs(f: &Facts) -> Option<String> {
    if let Some(p) = first_path(f, |p| PATS.readme.is_match(p)) {
        return Some(format!("{p} changed"));
    }
    if let Some(l) = code_lines(f).find(|l| PATS.public_api.is_match(&l.text)) {
        return Some(format!("new public API in {}", l.path));
    }
    f.added_lines
        .iter()
        .find(|l| l.text.contains(BREAKING_MARKER))
        .map(|l| format!("breaking change noted in {}", l.path))
}

fn dedup(f: &Facts) -> Option<String> {
    if f.changed_lines > LARGE_DIFF_LINES {
        return Some(format!("more than {LARGE_DIFF_LINES} changed lines"));
    }
    let key = |p: &str| {
        let (dir, name) = p.rsplit_once('/').unwrap_or(("", p));
        (
            dir.to_string(),
            name.rsplit_once('.').map(|(_, e)| e.to_string()),
        )
    };
    let mut groups: std::collections::BTreeMap<_, usize> = Default::default();
    for file in &f.files {
        *groups.entry(key(&file.path)).or_default() += 1;
    }
    groups
        .into_iter()
        .find(|(_, n)| *n >= SIMILAR_FILES)
        .map(|((dir, _), _)| format!("{SIMILAR_FILES} or more similar files in {dir}"))
}

fn adr(f: &Facts) -> Option<String> {
    first_path(f, |p| PATS.adr.is_match(p)).map(|p| format!("ADR file {p}"))
}

type Trigger = fn(&Facts) -> Option<String>;

/// The conditional lenses in the order the deep review lists them.
const TRIGGERS: [(&str, Trigger); 8] = [
    ("architecture", architecture),
    ("big-o", big_o),
    ("complexity", complexity),
    ("integration", integration),
    ("migration", migration),
    ("docs", docs),
    ("dedup", dedup),
    ("adr", adr),
];

fn pick(facts: &Facts) -> Vec<Pick> {
    TRIGGERS
        .iter()
        .filter_map(|(lens, trigger)| {
            Some(Pick {
                lens,
                trigger: trigger(facts)?,
            })
        })
        .collect()
}

fn parse_name_status(text: &str) -> Vec<Changed> {
    text.lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let status = cols.next()?;
            let path = cols.next_back()?;
            Some(Changed {
                path: path.to_string(),
                added: status.starts_with('A'),
            })
        })
        .collect()
}

fn parse_added_lines(diff: &str) -> Vec<AddedLine> {
    let mut path = String::new();
    let mut out = Vec::new();
    for line in diff.lines() {
        if let Some(p) = line.strip_prefix("+++ b/") {
            path = p.to_string();
        } else if let Some(text) = line.strip_prefix('+') {
            if !line.starts_with("+++") {
                out.push(AddedLine {
                    path: path.clone(),
                    text: text.to_string(),
                });
            }
        }
    }
    out
}

/// The top-most ancestor directory of `path` that `base` does not have.
fn missing_dir(base: &str, path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    (1..parts.len()).find_map(|n| {
        let dir = parts[..n].join("/");
        let exists = git(&["rev-parse", "--verify", "--quiet", &format!("{base}:{dir}")]).is_ok();
        (!exists).then_some(dir)
    })
}

fn gather(base: &str) -> Result<Facts, String> {
    let range = format!("{base}...HEAD");
    let diff = |extra: &[&str]| {
        let mut args = vec!["-c", "core.quotePath=false", "diff", "--no-color"];
        args.extend_from_slice(extra);
        args.push(&range);
        git(&args)
    };
    let files = parse_name_status(&diff(&["-M", "--name-status"])?);
    let (changed_lines, _) = parse_numstat(&diff(&["-M", "--numstat"])?);
    let added_lines = parse_added_lines(&diff(&["-U0"])?);
    let fork = git(&["merge-base", base, "HEAD"])?;
    let new_dirs = files
        .iter()
        .filter(|f| f.added && !is_doc(&f.path))
        .filter_map(|f| missing_dir(&fork, &f.path))
        .collect();
    Ok(Facts {
        files,
        new_dirs,
        added_lines,
        changed_lines,
    })
}

fn render(picks: &[Pick]) -> String {
    if picks.is_empty() {
        return NO_LENSES.to_string();
    }
    picks
        .iter()
        .map(|p| format!("{}: {}", p.lens, p.trigger))
        .collect::<Vec<_>>()
        .join("\n")
}

fn picks_for(base: Option<&str>) -> Result<Vec<Pick>, String> {
    let base = local_base(base)?;
    Ok(pick(&gather(&base)?))
}

/// Print the picked lenses for the current branch against `base`; returns the
/// exit code. With `json` the outcome rides the shared envelope and the code is
/// always 0.
pub fn run(base: Option<&str>, json: bool) -> i32 {
    match picks_for(base) {
        Ok(picks) if json => {
            let lenses: Vec<_> = picks
                .iter()
                .map(|p| json!({"lens": p.lens, "trigger": p.trigger}))
                .collect();
            println!("{}", envelope::wrap(0, "", json!({"lenses": lenses})));
            0
        }
        Ok(picks) => {
            println!("{}", render(&picks));
            0
        }
        Err(error) if json => {
            println!(
                "{}",
                envelope::wrap(FAILED_CODE, &error, serde_json::Value::Null)
            );
            0
        }
        Err(error) => {
            eprintln!("error: {error}");
            FAILED_CODE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            files: Vec::new(),
            new_dirs: BTreeSet::new(),
            added_lines: Vec::new(),
            changed_lines: 0,
        }
    }

    fn line(path: &str, text: &str) -> AddedLine {
        AddedLine {
            path: path.into(),
            text: text.into(),
        }
    }

    #[test]
    fn nothing_changed_picks_nothing() {
        assert_eq!(pick(&facts()), Vec::new());
        assert_eq!(render(&[]), NO_LENSES);
    }

    #[test]
    fn name_status_reads_added_and_renamed_files() {
        let files = parse_name_status("A\ta.rs\nM\tb.rs\nR100\told.rs\tnew.rs\n");
        let seen: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.added)).collect();
        assert_eq!(seen, [("a.rs", true), ("b.rs", false), ("new.rs", false)]);
    }

    #[test]
    fn added_lines_carry_their_file_and_skip_headers() {
        let diff = "+++ b/a.rs\n@@ -0,0 +1,2 @@\n+one\n+two\n--- a/b.rs\n+++ b/b.rs\n+three\n";
        let lines = parse_added_lines(diff);
        let seen: Vec<_> = lines
            .iter()
            .map(|l| (l.path.as_str(), l.text.as_str()))
            .collect();
        assert_eq!(seen, [("a.rs", "one"), ("a.rs", "two"), ("b.rs", "three")]);
    }

    #[test]
    fn documentation_lines_do_not_trip_code_triggers() {
        let mut f = facts();
        f.added_lines = vec![line("docs/guide.md", "pub fn x() {} v.sort();")];
        assert_eq!(pick(&f), Vec::new());
    }

    #[test]
    fn a_breaking_note_in_docs_picks_the_docs_lens() {
        let mut f = facts();
        f.added_lines = vec![line("CHANGELOG.md", "BREAKING: flag removed")];
        assert_eq!(pick(&f)[0].lens, "docs");
    }

    #[test]
    fn the_large_diff_cut_is_exclusive() {
        let mut f = facts();
        f.changed_lines = LARGE_DIFF_LINES;
        assert_eq!(pick(&f), Vec::new());
        f.changed_lines = LARGE_DIFF_LINES + 1;
        assert_eq!(pick(&f)[0].lens, "dedup");
    }

    #[test]
    fn tabs_count_as_nesting_levels() {
        let mut f = facts();
        f.added_lines = vec![line("a.go", "\t\t\t\t\tx()")];
        assert_eq!(pick(&f)[0].lens, "complexity");
        f.added_lines = vec![line("a.go", "\t\t\t\tx()")];
        assert_eq!(pick(&f), Vec::new());
    }

    #[test]
    fn a_line_nested_deeper_than_the_cut_still_counts() {
        let mut f = facts();
        f.added_lines = vec![line("a.rs", &format!("{}x();", " ".repeat(24)))];
        assert_eq!(pick(&f)[0].lens, "complexity");
        f.added_lines = vec![line("a.go", "\t\t\t\t\t\tx()")];
        assert_eq!(pick(&f)[0].lens, "complexity");
    }
}
