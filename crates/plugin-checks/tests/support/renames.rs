// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The registry and the pure checks behind the plugin name guard.
//!
//! A command file or a visible skill folder gets one name: one lowercase word,
//! or a group and a word joined by one hyphen. A rename adds a row here and
//! leaves the old name as a deprecated stub until 1.0.0; a removal adds a name
//! to `REMOVED` and leaves nothing. The checks fail on the old name anywhere
//! outside the places that keep history and the stub itself. Every function takes its inputs as arguments (names,
//! file texts, file lists), so a test runs it on literals before any real
//! rename exists.

use crate::rename_table::{Kind, Rename};
use std::collections::BTreeSet;

/// Names removed with no replacement. Each removal PR adds its entry.
pub(crate) const REMOVED: &[&str] = &[
    "deep-review",
    "quick-review",
    "handoff-save",
    "handoff-show",
];

/// The binary command groups a visible name may start with.
pub(crate) const GROUPS: &[&str] = &["pr", "handoff"];

/// Current file names that break the naming rule until their rename lands,
/// and the two aliases that keep their names.
/// The list empties as the renames land. It is an allowlist for the rule, not
/// a docs list: the docs name an old slash name only in `docs/renamed.md`.
pub(crate) const LEGACY_NAMES: &[&str] = &["playbook-usage", "session-handoff", "session-start"];

/// The release that removes every deprecated stub.
pub(crate) const STUB_REMOVED_IN: &str = "1.0.0";

/// The most lines a deprecated stub may have.
pub(crate) const STUB_MAX_LINES: usize = 12;

/// The longest a skill name may be (`skills_lint.rs` holds the same limit).
pub(crate) const SKILL_NAME_MAX: usize = 64;

/// How much of an old name a file may keep.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Keeps {
    /// Any form: the file keeps history.
    Any,
    /// Only the effort config key forms, `effort.commands.<old>` and
    /// `effort.skills.<old>`.
    EffortKeys,
    /// Only `playbook:<old>` for a renamed name, never for a removed one.
    RenamedSkillId,
}

/// A path (a folder when it ends with `/`) where an old name may stay.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Allowed<'a> {
    pub(crate) path: &'a str,
    pub(crate) keeps: Keeps,
}

/// Where an old or removed name may stay.
pub(crate) const OLD_NAME_ALLOWED: &[Allowed<'static>] = &[
    Allowed {
        path: "crates/plugin-checks/tests/support/renames.rs",
        keeps: Keeps::Any,
    },
    Allowed {
        path: "crates/cli/src/hooks/auto_cost/closing.rs",
        keeps: Keeps::RenamedSkillId,
    },
    Allowed {
        path: "crates/cli/src/effort/component.rs",
        keeps: Keeps::EffortKeys,
    },
    Allowed {
        path: "crates/cli/src/config/keys.rs",
        keeps: Keeps::EffortKeys,
    },
    Allowed {
        path: "CHANGELOG.md",
        keeps: Keeps::Any,
    },
    Allowed {
        path: "docs/adr/",
        keeps: Keeps::Any,
    },
    Allowed {
        path: "docs/renamed.md",
        keeps: Keeps::Any,
    },
    Allowed {
        path: "crates/cli/tests/fixtures/",
        keeps: Keeps::Any,
    },
];

/// A file the scan reads: its path from the repository root and its text.
pub(crate) struct ScannedFile<'a> {
    pub(crate) path: &'a str,
    pub(crate) text: &'a str,
}

/// An old name found where it may not stay.
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct Leftover {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) form: String,
}

/// Why a visible name breaks the rule, `None` when it follows it.
pub(crate) fn check_name(name: &str) -> Option<String> {
    let rule = "name rule: one lowercase word, or a group and a word joined by one hyphen";
    let word_ok = |w: &str| {
        !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    };
    let parts: Vec<&str> = name.split('-').collect();
    let follows = match parts.as_slice() {
        [word] => word_ok(word),
        [group, word] => GROUPS.contains(group) && word_ok(word),
        _ => false,
    };
    (!follows).then(|| format!("`{name}` breaks the {rule}"))
}

/// Why a skill folder name breaks the id rule, `None` when it follows it.
pub(crate) fn check_skill_name(name: &str) -> Option<String> {
    let valid = !name.is_empty()
        && name.len() <= SKILL_NAME_MAX
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    (!valid).then(|| {
        format!("skill name `{name}` must be lowercase letters, digits and hyphens, at most {SKILL_NAME_MAX} characters")
    })
}

/// The names that break the rule and are on neither the legacy list nor the
/// list of deprecated stubs.
pub(crate) fn check_visible_names(
    names: &[String],
    legacy: &[&str],
    stubs: &[String],
) -> Vec<String> {
    names
        .iter()
        .filter(|n| !legacy.contains(&n.as_str()) && !stubs.contains(n))
        .filter_map(|n| check_name(n))
        .collect()
}

/// The legacy names that no command file or skill folder carries.
pub(crate) fn check_legacy_exist(
    legacy: &[&str],
    commands: &[String],
    skills: &[String],
) -> Vec<String> {
    legacy
        .iter()
        .filter(|n| !commands.iter().chain(skills).any(|e| e == **n))
        .map(|n| format!("legacy name `{n}` has no command file or skill folder: remove it from LEGACY_NAMES"))
        .collect()
}

/// The renames whose new name is missing or whose old name has no stub left.
pub(crate) fn check_renames(
    renames: &[Rename],
    commands: &[String],
    skills: &[String],
) -> Vec<String> {
    let mut problems = Vec::new();
    for r in renames {
        let existing = if r.kind == Kind::Command {
            commands
        } else {
            skills
        };
        if !existing.iter().any(|e| e == r.new) {
            problems.push(format!(
                "rename `{}` to `{}`: the new name does not exist",
                r.old, r.new
            ));
        }
        if !existing.iter().any(|e| e == r.old) {
            problems.push(format!(
                "rename `{}` to `{}`: the old name has no deprecated stub",
                r.old, r.new
            ));
        }
    }
    problems
}

/// The removed names that still have a command file or a skill folder.
pub(crate) fn check_removed_files(
    removed: &[&str],
    commands: &[String],
    skills: &[String],
) -> Vec<String> {
    removed
        .iter()
        .filter(|n| commands.iter().chain(skills).any(|e| e == **n))
        .map(|n| format!("removed name `{n}` still has a command file or skill folder"))
        .collect()
}

/// The entries of `commands/` that are not plain files: a subfolder adds a
/// segment to the name the user types.
pub(crate) fn check_commands_flat(entries: &[(String, bool)]) -> Vec<String> {
    entries
        .iter()
        .filter(|(_, is_dir)| *is_dir)
        .map(|(name, _)| format!("commands/{name} is a folder: command files stay flat"))
        .collect()
}

/// The forms an old name takes in text.
fn forms(name: &str) -> Vec<String> {
    vec![
        format!("playbook:{name}"),
        format!("commands/{name}.md"),
        format!("skills/{name}/"),
        format!("effort.commands.{name}"),
        format!("effort.skills.{name}"),
    ]
}

/// Whether `text` holds `form` as a whole name: the next character does not
/// continue a name, so `playbook:commit` does not match `playbook:commit-and-push`.
fn holds_form(text: &str, form: &str) -> bool {
    let continues = |c: char| c.is_ascii_alphanumeric() || c == '-';
    let ends_with_delimiter = form.ends_with('/') || form.ends_with(".md");
    text.match_indices(form).any(|(at, _)| {
        ends_with_delimiter
            || !text[at + form.len()..]
                .chars()
                .next()
                .is_some_and(continues)
    })
}

/// Whether `allowed` lets `path` keep `form` of a renamed or removed name.
fn is_allowed(allowed: &[Allowed], path: &str, form: &str, removed: bool) -> bool {
    allowed
        .iter()
        .filter(|a| path == a.path || (a.path.ends_with('/') && path.starts_with(a.path)))
        .any(|a| match a.keeps {
            Keeps::Any => true,
            Keeps::EffortKeys => form.starts_with("effort."),
            Keeps::RenamedSkillId => !removed && form.starts_with("playbook:"),
        })
}

/// The old and removed names found outside the allowed places, sorted.
pub(crate) fn find_old_names(
    renamed: &[&str],
    removed: &[&str],
    files: &[ScannedFile],
    allowed: &[Allowed],
) -> Vec<Leftover> {
    let mut found = BTreeSet::new();
    let names = renamed
        .iter()
        .map(|n| (*n, false))
        .chain(removed.iter().map(|n| (*n, true)));
    for (name, is_removed) in names {
        for file in files {
            for form in forms(name) {
                if holds_form(file.text, &form)
                    && !is_allowed(allowed, file.path, &form, is_removed)
                {
                    found.insert(Leftover {
                        path: file.path.to_owned(),
                        name: name.to_owned(),
                        form,
                    });
                }
            }
        }
    }
    found.into_iter().collect()
}

/// The old names in the first column of the renamed page's table: every row
/// whose first cell holds a `/playbook:<name>` code span.
pub(crate) fn renamed_page_old_names(page: &str) -> Vec<String> {
    const PREFIX: &str = "/playbook:";
    page.lines()
        .filter_map(|line| line.trim().strip_prefix('|'))
        .filter_map(|row| row.split('|').next())
        .filter_map(|cell| cell.trim().strip_prefix('`')?.strip_suffix('`'))
        .filter_map(|span| span.strip_prefix(PREFIX))
        .map(str::to_owned)
        .collect()
}

/// The rows of the renamed page table: (old name, third cell).
pub(crate) fn renamed_page_rows(page: &str) -> Vec<(String, String)> {
    const PREFIX: &str = "/playbook:";
    page.lines()
        .filter_map(|line| line.trim().strip_prefix('|'))
        .filter_map(|row| {
            let cells: Vec<&str> = row.split('|').map(str::trim).collect();
            let old = cells
                .first()?
                .strip_prefix('`')?
                .strip_suffix('`')?
                .strip_prefix(PREFIX)?;
            Some((
                old.to_owned(),
                cells.get(2).copied().unwrap_or_default().to_owned(),
            ))
        })
        .collect()
}

/// The problems with `docs/renamed.md`: a row for every rename and removal, a
/// removal release of 1.0.0 on every renamed row, a deprecation note, and no
/// row for a name that is live or was never renamed. `live` holds the real
/// commands and skills, so a stub is not in it. `page` is `None` when
/// the file does not exist, which only passes while there is nothing to list.
pub(crate) fn check_renamed_page(
    renames: &[(&str, &str)],
    removed: &[&str],
    page: Option<&str>,
    live: &[String],
) -> Vec<String> {
    let expected: Vec<&str> = renames
        .iter()
        .map(|(old, _)| *old)
        .chain(removed.iter().copied())
        .collect();
    let Some(page) = page else {
        return if expected.is_empty() {
            Vec::new()
        } else {
            vec!["docs/renamed.md does not exist but a name was renamed or removed".to_owned()]
        };
    };
    let listed = renamed_page_old_names(page);
    let mut problems = Vec::new();
    if !renames.is_empty() && !page.to_lowercase().contains("deprecated") {
        problems.push("docs/renamed.md has no deprecation note".to_owned());
    }
    for (old, _) in renames {
        let cell = renamed_page_rows(page)
            .into_iter()
            .find(|(name, _)| name == old)
            .map(|(_, c)| c);
        if cell.is_some_and(|c| c != STUB_REMOVED_IN) {
            problems.push(format!(
                "docs/renamed.md: the row for /playbook:{old} must say it is removed in {STUB_REMOVED_IN}"
            ));
        }
    }
    for old in &expected {
        if !listed.iter().any(|l| l == old) {
            problems.push(format!("docs/renamed.md has no row for /playbook:{old}"));
        }
    }
    for name in &listed {
        if live.iter().any(|l| l == name) {
            problems.push(format!(
                "/playbook:{name} is live but listed as an old name"
            ));
        } else if !expected.contains(&name.as_str()) {
            problems.push(format!(
                "/playbook:{name} is listed but was never renamed or removed"
            ));
        }
    }
    problems
}

/// The paths, from the repository root, of the stubs of `renames`. An old name
/// may stay in its own stub.
pub(crate) fn stub_paths(renames: &[Rename]) -> Vec<String> {
    renames
        .iter()
        .map(|r| match r.kind {
            Kind::Command => format!("plugins/playbook/commands/{}.md", r.old),
            Kind::Skill => format!("plugins/playbook/skills/{}/", r.old),
        })
        .collect()
}

/// `base` plus an entry that keeps any form for each of `stubs`.
pub(crate) fn with_stubs<'a>(base: &[Allowed<'a>], stubs: &'a [String]) -> Vec<Allowed<'a>> {
    base.iter()
        .copied()
        .chain(stubs.iter().map(|path| Allowed {
            path,
            keeps: Keeps::Any,
        }))
        .collect()
}

/// The value of `key` in the frontmatter of `text`, `None` when absent.
pub(crate) fn frontmatter_value(text: &str, key: &str) -> Option<String> {
    let rest = text.strip_prefix("---\n")?;
    let front = rest.split_once("\n---\n")?.0;
    front
        .lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(": "))
        .map(|v| v.trim().to_owned())
}

/// Whether `text` is a deprecated stub: it hides itself from the model.
pub(crate) fn is_stub(text: &str) -> bool {
    frontmatter_value(text, "disable-model-invocation").is_some_and(|v| v == "true")
}

/// The problems with the stub of a rename, read against the new file: the
/// deprecation wording and removal release, a hidden-from-the-model flag, the
/// new file's tools, argument hint, model and effort, the one-line notice, a
/// pointer to the new file and a small size.
pub(crate) fn check_stub(
    kind: Kind,
    old: &str,
    new: &str,
    stub_text: &str,
    new_text: &str,
) -> Vec<String> {
    let mut problems = Vec::new();
    let want = format!("Deprecated, use /playbook:{new}. Removed in {STUB_REMOVED_IN}.");
    if frontmatter_value(stub_text, "description").as_deref() != Some(want.as_str()) {
        problems.push(format!("stub `{old}`: the description must be `{want}`"));
    }
    if !is_stub(stub_text) {
        problems.push(format!(
            "stub `{old}`: it must set `disable-model-invocation: true`"
        ));
    }
    for key in ["allowed-tools", "argument-hint", "model", "effort"] {
        if frontmatter_value(stub_text, key) != frontmatter_value(new_text, key) {
            problems.push(format!("stub `{old}`: `{key}` must match `{new}`"));
        }
    }
    let notice = format!(
        "/playbook:{old} is deprecated and will be removed in {STUB_REMOVED_IN}. Use /playbook:{new}."
    );
    if !stub_text.contains(&notice) {
        problems.push(format!("stub `{old}`: it must print `{notice}`"));
    }
    let target = match kind {
        Kind::Command => format!("commands/{new}.md"),
        Kind::Skill => format!("skills/{new}/SKILL.md"),
    };
    if !stub_text.contains(&target) {
        problems.push(format!("stub `{old}`: it must point at `{target}`"));
    }
    if stub_text.lines().count() > STUB_MAX_LINES {
        problems.push(format!("stub `{old}`: it is over {STUB_MAX_LINES} lines"));
    }
    problems
}

/// The problems with the set of stubs on disk: it equals the renames of `kind`.
pub(crate) fn check_stub_list(kind: Kind, on_disk: &[String], renames: &[Rename]) -> Vec<String> {
    let mut want: Vec<&str> = renames
        .iter()
        .filter(|r| r.kind == kind)
        .map(|r| r.old)
        .collect();
    let mut have: Vec<&str> = on_disk.iter().map(String::as_str).collect();
    want.sort_unstable();
    have.sort_unstable();
    let mut problems = Vec::new();
    for name in &want {
        if !have.contains(name) {
            problems.push(format!("`{name}` is renamed but is not a deprecated stub"));
        }
    }
    for name in &have {
        if !want.contains(name) {
            problems.push(format!(
                "`{name}` is a deprecated stub but has no RENAMES row"
            ));
        }
    }
    problems
}
