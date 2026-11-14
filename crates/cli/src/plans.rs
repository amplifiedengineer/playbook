// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook plan list`: the plan picker list that `commands/implement.md` used to
//! build with a bash loop. One row per plan file under the plans folder and
//! per `docs/adr/*-blueprint.md`.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// One plan or blueprint file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Path relative to the repo root when under it, else as listed.
    pub path: String,
    /// `proposed`, `accepted`, `implemented`, or `?`.
    pub status: String,
    pub title: String,
}

fn skipped(name: &str) -> bool {
    name.ends_with("-quality.md")
        || name.ends_with(".checkpoint.md")
        || (name.contains(".gate-source") && name.ends_with(".md"))
}

fn md_files(dir: &Path, only_suffix: Option<&str>) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.ends_with(".md") && only_suffix.is_none_or(|s| n.ends_with(s))
        })
        .collect();
    v.sort();
    v
}

/// The first Markdown heading, without its `#` marks.
fn title_of(text: &str) -> Option<String> {
    text.lines()
        .find(|l| l.starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim_start().to_string())
}

/// `proposed`, `accepted` or `implemented` from the first line that mentions
/// `status`, as written there.
fn status_of(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.to_lowercase().contains("status"))?;
    let lower = line.to_lowercase();
    ["proposed", "accepted", "implemented"]
        .iter()
        .filter_map(|w| lower.find(w).map(|at| (at, *w)))
        .min_by_key(|(at, _)| *at)
        .map(|(at, w)| line[at..at + w.len()].to_string())
}

/// Every plan under `plans_dir` and `<root>/docs/adr/*-blueprint.md`.
pub fn list(plans_dir: &Path, root: &Path) -> Vec<Plan> {
    let mut files = md_files(plans_dir, None);
    files.extend(md_files(&root.join("docs/adr"), Some("-blueprint.md")));
    files
        .into_iter()
        .filter(|f| !skipped(f.file_name().and_then(|n| n.to_str()).unwrap_or("")))
        .map(|f| {
            let text = fs::read_to_string(&f).unwrap_or_default();
            let path = f
                .strip_prefix(root)
                .unwrap_or(&f)
                .to_string_lossy()
                .into_owned();
            Plan {
                path,
                status: status_of(&text).unwrap_or_else(|| "?".to_string()),
                title: title_of(&text)
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "untitled".to_string()),
            }
        })
        .collect()
}

/// The picker text: `path<TAB>[status]<TAB>title` per plan, or `NO_PLANS`.
pub fn render(plans: &[Plan]) -> String {
    if plans.is_empty() {
        return "NO_PLANS".to_string();
    }
    plans
        .iter()
        .map(|p| format!("{}\t[{}]\t{}", p.path, p.status, p.title))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The same rows as JSON.
pub fn to_json(plans: &[Plan]) -> Value {
    json!(plans
        .iter()
        .map(|p| json!({"path": p.path, "status": p.status, "title": p.title}))
        .collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;

    fn setup(tag: &str) -> (PathBuf, PathBuf) {
        let root = scratch_dir(tag);
        let plans = root.join("plans");
        fs::create_dir_all(&plans).unwrap();
        fs::create_dir_all(root.join("docs/adr")).unwrap();
        (root, plans)
    }

    #[test]
    fn rows_carry_path_status_and_title() {
        let (root, plans) = setup("plans-a");
        fs::write(plans.join("a.md"), "# Add widgets\n\nStatus: Accepted\n").unwrap();
        fs::write(
            root.join("docs/adr/0001-x-blueprint.md"),
            "## Blueprint\nstatus: proposed or later\n",
        )
        .unwrap();
        let rows = list(&plans, &root);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, "Accepted");
        assert_eq!(rows[0].title, "Add widgets");
        assert_eq!(rows[1].path, "docs/adr/0001-x-blueprint.md");
        assert_eq!(rows[1].status, "proposed");
    }

    #[test]
    fn quality_checkpoint_and_gate_source_files_are_skipped() {
        let (root, plans) = setup("plans-b");
        for n in [
            "a-quality.md",
            "a.checkpoint.md",
            "a.gate-source-1.md",
            "a.checkpoint.md.lock",
            "keep.md",
        ] {
            fs::write(plans.join(n), "# T\n").unwrap();
        }
        let rows = list(&plans, &root);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].path.ends_with("keep.md"));
    }

    #[test]
    fn a_file_with_no_heading_or_status_reads_untitled_and_question_mark() {
        let (root, plans) = setup("plans-c");
        fs::write(plans.join("a.md"), "just text\n").unwrap();
        let rows = list(&plans, &root);
        assert_eq!(
            (rows[0].status.as_str(), rows[0].title.as_str()),
            ("?", "untitled")
        );
    }

    #[test]
    fn no_plans_prints_the_marker() {
        let (root, plans) = setup("plans-d");
        assert_eq!(render(&list(&plans, &root)), "NO_PLANS");
    }
}
