// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook config apply-proposal`: shows the config that `pbsync config
//! pull` proposed and, after a yes, stores it in the global tier.

use super::sync_files::{value_text, write_sync_files};
use super::write::{self, Tier};
use super::{keys, parse, store};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::Path;

/// The proposal file `pbsync config pull` writes in the data folder.
pub const PROPOSAL_FILE: &str = "config-proposal.json";
const NO_PROPOSAL: &str = "No sync proposal to apply.";

#[derive(Deserialize)]
struct Proposal {
    proposed: BTreeMap<String, String>,
    #[serde(default)]
    from: BTreeMap<String, String>,
}

/// A proposed key that passed every check.
struct Change {
    key: String,
    value: Value,
    old: Option<String>,
    from: String,
}

/// The streams `run` talks to, so tests and the binary share one code path.
pub struct Streams<'a> {
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

/// Run the command and return its exit code. Nothing is stored before the
/// answer is yes, and the proposal file goes only after every key is stored.
pub fn run(
    home: &Path,
    repo_slug: Option<&str>,
    yes: bool,
    io: &mut Streams<'_>,
) -> std::io::Result<i32> {
    let path = crate::usage::data::data_dir(home).join(PROPOSAL_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            writeln!(io.out, "{NO_PROPOSAL}")?;
            return Ok(0);
        }
        Err(err) => {
            writeln!(
                io.err,
                "config apply-proposal: cannot read {}: {err}",
                path.display()
            )?;
            return Ok(1);
        }
    };
    let proposal: Proposal = match serde_json::from_str(&raw) {
        Ok(proposal) => proposal,
        Err(err) => {
            writeln!(
                io.err,
                "config apply-proposal: {} is not a valid proposal: {err}",
                path.display()
            )?;
            return Ok(1);
        }
    };
    let root = crate::common::paths::playbook_root_from(home);
    let (changes, rejected) = match classify(&proposal, &root) {
        Ok(split) => split,
        Err(err) => {
            writeln!(io.err, "config apply-proposal: {err}")?;
            return Ok(1);
        }
    };
    for (key, reason) in &rejected {
        writeln!(io.out, "rejected {key}: {reason}")?;
    }
    if changes.is_empty() {
        return Ok(0);
    }
    for change in &changes {
        let old = change.old.as_deref().unwrap_or("(unset)");
        let new = value_text(&change.value);
        writeln!(
            io.out,
            "{}: {old} -> {new} (from {})",
            change.key, change.from
        )?;
    }
    if !yes && !confirm(io, changes.len())? {
        writeln!(io.err, "config apply-proposal: nothing was changed")?;
        return Ok(1);
    }
    for change in &changes {
        if let Err(err) = write::set(Tier::Global, &change.key, &change.value, home, None) {
            writeln!(io.err, "config apply-proposal: {err}")?;
            return Ok(1);
        }
    }
    if let Err(err) = write_sync_files(home) {
        writeln!(
            io.err,
            "config apply-proposal: warning: sync files not refreshed: {err}"
        )?;
    }
    std::fs::remove_file(&path)?;
    writeln!(
        io.out,
        "config apply-proposal: applied {} setting(s)",
        changes.len()
    )?;
    note_shadowed(io.out, &root, repo_slug, &changes)?;
    Ok(0)
}

fn confirm(io: &mut Streams<'_>, count: usize) -> std::io::Result<bool> {
    write!(
        io.out,
        "Apply {count} setting(s) to your global config? [y/N] "
    )?;
    io.out.flush()?;
    let mut answer = String::new();
    io.input.read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// The valid changes, and the rejected keys with the reason for each.
type Classified = (Vec<Change>, Vec<(String, String)>);

/// Split the proposal into valid changes and rejected keys with the reason.
fn classify(proposal: &Proposal, root: &Path) -> Result<Classified, super::ConfigError> {
    let mut changes = Vec::new();
    let mut rejected = Vec::new();
    for (given, text) in &proposal.proposed {
        // A proposal naming both an old and a new key keeps the new one.
        let Some(key) =
            keys::canonical_unless_shadowed(given, |key| proposal.proposed.contains_key(key))
        else {
            continue;
        };
        match check(key, text) {
            Ok(value) => changes.push(Change {
                key: key.to_string(),
                value,
                old: store::lookup(root, key, None)?
                    .global
                    .as_ref()
                    .map(value_text),
                from: proposal.from.get(given).cloned().unwrap_or_default(),
            }),
            Err(reason) => rejected.push((given.clone(), reason)),
        }
    }
    Ok((changes, rejected))
}

/// The value a proposed key would store, or why it must not be stored.
fn check(key: &str, text: &str) -> Result<Value, String> {
    if !keys::is_shared(key) {
        return Err("not a key that syncs between machines".to_string());
    }
    let value = parse::parse_config_value(key, text).map_err(|err| err.to_string())?;
    write::validate_key_and_value(key, &value).map_err(|err| err.to_string())?;
    Ok(value)
}

/// One line when a project or org value still wins over an applied key.
fn note_shadowed(
    out: &mut dyn Write,
    root: &Path,
    repo_slug: Option<&str>,
    changes: &[Change],
) -> std::io::Result<()> {
    let Some(slug) = repo_slug else {
        return Ok(());
    };
    let shadowed: Vec<&str> = changes
        .iter()
        .filter(|change| {
            store::lookup(root, &change.key, Some(slug))
                .is_ok_and(|found| found.repo.is_some() || found.org.is_some())
        })
        .map(|change| change.key.as_str())
        .collect();
    if shadowed.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "note: project settings override the applied value for {}.",
        shadowed.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The previous release's `check`: it knows only the old names.
    fn previous_release_rejects(key: &str, known_then: &[&str]) -> bool {
        !known_then.contains(&key)
    }

    fn propose(home: &Path, proposed: &Value) {
        let path = crate::usage::data::data_dir(home).join(PROPOSAL_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let doc = json!({ "created_at": 1_700_000_000, "proposed": proposed, "from": {} });
        std::fs::write(path, doc.to_string()).unwrap();
    }

    /// Runs `apply-proposal --yes` and returns its exit code and stdout.
    fn apply(home: &Path) -> (i32, String) {
        let mut input = std::io::Cursor::new(Vec::new());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut io = Streams {
            input: &mut input,
            out: &mut out,
            err: &mut err,
        };
        let code = run(home, None, true, &mut io).unwrap();
        (code, String::from_utf8(out).unwrap())
    }

    fn global(home: &Path, key: &str) -> Option<Value> {
        let root = crate::common::paths::playbook_root_from(home);
        store::lookup(&root, key, None).unwrap().global
    }

    #[test]
    fn a_proposal_with_an_old_name_applies_to_the_new_name() {
        for row in keys::renames() {
            // Arrange
            let home = crate::testing::scratch_root("proposal-old-name");
            propose(&home, &json!({ row.old: "true" }));

            // Act
            let (code, shown) = apply(&home);

            // Assert
            assert_eq!(code, 0, "{shown}");
            assert_eq!(global(&home, row.new), Some(json!(true)), "{}", row.new);
            assert_eq!(global(&home, row.old), None, "{}", row.old);
            assert!(!shown.contains("rejected"), "{shown}");
        }
    }

    #[test]
    fn a_proposal_naming_both_names_keeps_the_new_value() {
        for row in keys::renames() {
            // Arrange
            let home = crate::testing::scratch_root("proposal-both-names");
            propose(&home, &json!({ row.old: "false", row.new: "true" }));

            // Act
            let (code, shown) = apply(&home);

            // Assert
            assert_eq!(code, 0, "{shown}");
            assert_eq!(global(&home, row.new), Some(json!(true)), "{}", row.new);
            assert_eq!(shown.matches(row.new).count(), 1, "{shown}");
        }
    }

    #[test]
    fn a_proposal_with_a_retired_key_is_rejected_and_listed() {
        // Arrange
        let home = crate::testing::scratch_root("proposal-retired");
        let (retired, _) = keys::FIXED_KEYS[0].clone();
        propose(&home, &json!({ retired: "9" }));

        // Act
        let (code, shown) = apply(&home);

        // Assert
        assert_eq!(code, 0);
        assert!(shown.contains(&format!("rejected {retired}:")), "{shown}");
        assert_eq!(global(&home, retired), None);
    }

    #[test]
    fn the_previous_release_rejects_the_new_names_the_sync_files_now_carry() {
        // Arrange
        let current: Vec<&str> = keys::SHARED_KEYS.to_vec();
        let renamed_to: Vec<&str> = keys::renames().iter().map(|r| r.new).collect();
        let known_then: Vec<&str> = current
            .iter()
            .filter(|key| !renamed_to.contains(key))
            .copied()
            .chain(keys::renames().iter().map(|r| r.old))
            .collect();

        for row in keys::renames() {
            // Act
            let new_rejected = previous_release_rejects(row.new, &known_then);
            let old_rejected = previous_release_rejects(row.old, &known_then);

            // Assert
            assert!(new_rejected, "{}", row.new);
            assert!(!old_rejected, "{}", row.old);
        }
    }
}
