// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Helpers for `/playbook:plan` and the review commands: the plan checkpoint,
//! and the glossary append.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::common::atomic::{atomic_append, with_dir_lock, write_atomic};

const LOCK_RETRIES: u32 = 20;
const LOCK_DELAY: Duration = Duration::from_millis(50);

/// A slug is one kebab-case file stem: no separators and no leading dot.
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && !slug.starts_with('.')
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// `<plans_dir>/<slug>.checkpoint.md`.
pub fn checkpoint_path(plans_dir: &Path, slug: &str) -> Result<PathBuf, String> {
    if !valid_slug(slug) {
        return Err(format!(
            "invalid topic slug '{slug}': use letters, digits, '-' and '_' only"
        ));
    }
    Ok(plans_dir.join(format!("{slug}.checkpoint.md")))
}

/// Runs `f` holding the directory lock `<path>.lock`. The lock is advisory:
/// when it cannot be taken `f` still runs.
pub fn with_file_lock<T>(path: &Path, f: impl FnOnce() -> T) -> T {
    let lock = PathBuf::from(format!("{}.lock", path.display()));
    let (acquired, result) = with_dir_lock(&lock, LOCK_RETRIES, LOCK_DELAY, f);
    if acquired {
        let _ = fs::remove_dir(&lock);
    }
    result
}

/// Replaces the checkpoint under the advisory lock of [`with_file_lock`].
pub fn write_checkpoint(path: &Path, content: &str) -> io::Result<()> {
    with_file_lock(path, || write_atomic(path, content))
}

/// Appends one entry to `<root>/GLOSSARY.md` under a directory lock.
pub fn glossary_add(root: &Path, entry: &str) -> PathBuf {
    let file = root.join("GLOSSARY.md");
    atomic_append(&file.display().to_string(), entry);
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("pb-planning-{tag}-{}", crate::testing::run_id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn slugs_with_separators_or_a_leading_dot_are_rejected() {
        for bad in ["", "../x", "a/b", ".hidden", "a b"] {
            assert!(checkpoint_path(Path::new("/p"), bad).is_err(), "{bad}");
        }
        assert_eq!(
            checkpoint_path(Path::new("/p"), "proj-123").unwrap(),
            PathBuf::from("/p/proj-123.checkpoint.md")
        );
    }

    #[test]
    fn a_checkpoint_is_replaced_whole_and_leaves_no_lock_behind() {
        let dir = scratch("cp");
        let path = dir.join("t.checkpoint.md");
        write_checkpoint(&path, "one").unwrap();
        write_checkpoint(&path, "two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        assert!(!dir.join("t.checkpoint.md.lock").exists());
    }

    #[test]
    fn glossary_entries_accumulate_in_order() {
        let dir = scratch("gl");
        glossary_add(&dir, "- **Term**: a thing");
        let file = glossary_add(&dir, "- **Other**: another");
        assert_eq!(
            fs::read_to_string(file).unwrap(),
            "- **Term**: a thing\n- **Other**: another\n"
        );
    }
}
