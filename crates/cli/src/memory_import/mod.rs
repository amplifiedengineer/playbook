// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook memory claude import`: copy notes from Claude Code's auto memory
//! into playbook memory, one way. Claude Code's directory is only read (see
//! `claude_read`). Everything written lives on the playbook side: the new fact
//! files and the record of what was already imported.

pub mod claude_read;

use crate::common::atomic::write_atomic;
use claude_read::ClaudeFact;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const FACT_TYPES: [&str; 4] = ["user", "feedback", "project", "reference"];

/// Where to read, where to write, and whether to write at all.
pub struct Params {
    /// Claude Code's auto memory directory (read only).
    pub claude_memory_dir: PathBuf,
    /// The playbook memory directory the facts go into.
    pub dest_dir: PathBuf,
    /// Playbook's own record of imported content hashes.
    pub state_file: PathBuf,
    pub dry_run: bool,
}

/// What a run did, or would do on a dry run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Claude file name and the playbook fact file it became.
    pub copied: Vec<(String, String)>,
    /// Already imported earlier, by content hash.
    pub already_imported: Vec<String>,
    /// A playbook fact with that file name exists, so nothing was overwritten.
    pub name_taken: Vec<String>,
}

fn load_state(path: &Path) -> BTreeSet<String> {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| {
            v.get("imported")
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect::<BTreeSet<_>>())
        })
        .unwrap_or_default()
}

fn save_state(path: &Path, imported: &Map<String, Value>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body =
        serde_json::to_string_pretty(&json!({ "imported": imported })).map_err(io::Error::other)?;
    write_atomic(path, &body)
}

/// Kebab-case file stem for a playbook fact.
fn kebab(stem: &str) -> String {
    let mut out = String::new();
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "claude-note".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Split `---` frontmatter from the body. No frontmatter gives empty fields.
fn split_frontmatter(content: &str) -> (Vec<(String, String)>, &str) {
    let Some(rest) = content.strip_prefix("---\n") else {
        return (Vec::new(), content);
    };
    let Some(end) = rest.find("\n---") else {
        return (Vec::new(), content);
    };
    let fields = rest[..end]
        .lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let body = rest[end + 4..].trim_start_matches('\n');
    (fields, body)
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The playbook fact text for one Claude Code note.
fn to_playbook_fact(fact: &ClaudeFact) -> String {
    let stem = fact.file_name.trim_end_matches(".md");
    let (fields, body) = split_frontmatter(&fact.content);
    let get = |k: &str| {
        fields
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    };
    let name = get("name").unwrap_or_else(|| stem.replace(['-', '_'], " "));
    let description = get("description").unwrap_or_else(|| {
        let first = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        one_line(first.trim_start_matches('#'))
            .chars()
            .take(120)
            .collect()
    });
    let kind = get("type")
        .filter(|t| FACT_TYPES.contains(&t.as_str()))
        .unwrap_or_else(|| "project".to_string());
    format!(
        "---\nname: {}\ndescription: {}\ntype: {kind}\n---\n{}\n\nImported from Claude Code auto memory ({}).\n",
        one_line(&name),
        one_line(&description),
        body.trim_end(),
        fact.file_name
    )
}

/// Import every note not yet imported. Never overwrites a playbook fact and
/// never writes to `claude_memory_dir`. A dry run writes nothing at all.
pub fn run(p: &Params) -> io::Result<Report> {
    let facts = claude_read::read_facts(&p.claude_memory_dir)?;
    let mut state = load_state(&p.state_file);
    let mut imported: Map<String, Value> = state.iter().map(|h| (h.clone(), Value::Null)).collect();
    let mut report = Report::default();
    for fact in facts {
        if state.contains(&fact.hash) {
            report.already_imported.push(fact.file_name);
            continue;
        }
        let dest_name = format!("{}.md", kebab(fact.file_name.trim_end_matches(".md")));
        let dest = p.dest_dir.join(&dest_name);
        if dest.exists() {
            report.name_taken.push(fact.file_name);
            continue;
        }
        if !p.dry_run {
            fs::create_dir_all(&p.dest_dir)?;
            write_atomic(&dest, to_playbook_fact(&fact))?;
            imported.insert(fact.hash.clone(), Value::String(dest_name.clone()));
            state.insert(fact.hash.clone());
        }
        report.copied.push((fact.file_name, dest_name));
    }
    if !p.dry_run && !report.copied.is_empty() {
        save_state(&p.state_file, &imported)?;
    }
    Ok(report)
}

/// The human-readable report.
pub fn render(report: &Report, dry_run: bool) -> String {
    let verb = if dry_run { "would copy" } else { "copied" };
    let mut lines = vec![format!(
        "memory claude import: {verb} {}, already imported {}, name taken {}",
        report.copied.len(),
        report.already_imported.len(),
        report.name_taken.len()
    )];
    for (from, to) in &report.copied {
        lines.push(format!("  {from} -> {to}"));
    }
    for name in &report.name_taken {
        lines.push(format!(
            "  {name}: a playbook fact with that name exists, left alone"
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use std::time::SystemTime;

    fn dirs(tag: &str) -> (PathBuf, Params) {
        let root = scratch_dir(tag);
        let claude = root.join("claude/memory");
        fs::create_dir_all(&claude).unwrap();
        let params = Params {
            claude_memory_dir: claude,
            dest_dir: root.join("pb/memory/acme/app"),
            state_file: root.join("pb/state/claude-import.json"),
            dry_run: false,
        };
        (root, params)
    }

    fn tree(dir: &Path) -> Vec<(String, Vec<u8>, SystemTime)> {
        let mut out: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| {
                let m = e.metadata().unwrap();
                (
                    e.file_name().to_string_lossy().into_owned(),
                    fs::read(e.path()).unwrap(),
                    m.modified().unwrap(),
                )
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn seed(p: &Params) {
        fs::write(p.claude_memory_dir.join("MEMORY.md"), "- [a](a.md)\n").unwrap();
        fs::write(
            p.claude_memory_dir.join("User_Role.md"),
            "---\nname: User role\ndescription: Senior engineer\ntype: user\n---\nPrefers short answers.\n",
        )
        .unwrap();
        fs::write(
            p.claude_memory_dir.join("plain.md"),
            "# Build\nRun just test.\n",
        )
        .unwrap();
    }

    #[test]
    fn copies_notes_into_playbook_facts_and_skips_the_index() {
        let (_root, p) = dirs("mi-copy");
        seed(&p);
        let r = run(&p).unwrap();
        assert_eq!(r.copied.len(), 2, "{r:?}");
        let role = fs::read_to_string(p.dest_dir.join("user-role.md")).unwrap();
        assert!(role
            .starts_with("---\nname: User role\ndescription: Senior engineer\ntype: user\n---\n"));
        assert!(role.contains("Prefers short answers."));
        let plain = fs::read_to_string(p.dest_dir.join("plain.md")).unwrap();
        assert!(
            plain.contains("type: project") && plain.contains("description: Build"),
            "{plain}"
        );
        assert!(!p.dest_dir.join("memory.md").exists());
    }

    #[test]
    fn claude_memory_is_byte_identical_with_unchanged_mtimes_and_no_new_files() {
        let (_root, p) = dirs("mi-readonly");
        seed(&p);
        let before = tree(&p.claude_memory_dir);
        run(&p).unwrap();
        run(&p).unwrap();
        assert_eq!(tree(&p.claude_memory_dir), before);
    }

    #[test]
    fn a_second_run_copies_nothing() {
        let (_root, p) = dirs("mi-idem");
        seed(&p);
        run(&p).unwrap();
        let second = run(&p).unwrap();
        assert!(second.copied.is_empty());
        assert_eq!(second.already_imported.len(), 2);
    }

    #[test]
    fn a_deleted_playbook_fact_is_not_brought_back() {
        let (_root, p) = dirs("mi-deleted");
        seed(&p);
        run(&p).unwrap();
        fs::remove_file(p.dest_dir.join("plain.md")).unwrap();
        assert!(run(&p).unwrap().copied.is_empty());
        assert!(!p.dest_dir.join("plain.md").exists());
    }

    #[test]
    fn dry_run_writes_nothing() {
        let (root, mut p) = dirs("mi-dry");
        seed(&p);
        p.dry_run = true;
        let r = run(&p).unwrap();
        assert_eq!(r.copied.len(), 2);
        assert!(!root.join("pb").exists());
    }

    #[test]
    fn an_existing_playbook_fact_is_never_overwritten() {
        let (_root, p) = dirs("mi-taken");
        seed(&p);
        fs::create_dir_all(&p.dest_dir).unwrap();
        fs::write(p.dest_dir.join("plain.md"), "mine\n").unwrap();
        let r = run(&p).unwrap();
        assert_eq!(r.name_taken, vec!["plain.md".to_string()]);
        assert_eq!(
            fs::read_to_string(p.dest_dir.join("plain.md")).unwrap(),
            "mine\n"
        );
    }

    #[test]
    fn a_missing_claude_directory_is_an_empty_import() {
        let (root, mut p) = dirs("mi-missing");
        p.claude_memory_dir = root.join("nope");
        assert_eq!(run(&p).unwrap(), Report::default());
    }
}
