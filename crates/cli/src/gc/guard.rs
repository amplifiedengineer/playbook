// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The only place in `gc` that removes anything. Every removal names the roots
//! it may act inside and the `RunGuard` that proves the caller holds the run
//! lock, and the guard re-checks the owner token just before it removes.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::lock::{LockError, RunGuard, LOCK_DIR, STALE_CLAIM_PREFIX, TOKEN_FILE};
use super::Registry;

/// Why a guarded call did nothing.
#[derive(Debug)]
pub enum GuardError {
    /// The path is not safe to remove; nothing was touched.
    Refused(String),
    /// The run lock was lost.
    Lock(LockError),
    Io(io::Error),
    /// The worktree removal command reported a failure.
    Worktree(String),
}

impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(why) => write!(f, "gc refused to remove: {why}"),
            Self::Lock(err) => write!(f, "{err}"),
            Self::Io(err) => write!(f, "gc could not remove: {err}"),
            Self::Worktree(why) => write!(f, "gc could not remove the worktree: {why}"),
        }
    }
}

impl std::error::Error for GuardError {}

impl From<LockError> for GuardError {
    fn from(err: LockError) -> Self {
        Self::Lock(err)
    }
}

impl From<io::Error> for GuardError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// Whether `path` is strictly below `root`, compared by path components and
/// byte for byte, with no filesystem lookup. `/root-evil` is not inside
/// `/root`, and `/Root/a` is not inside `/root`.
fn lexically_inside(root: &Path, path: &Path) -> bool {
    path != root && path.starts_with(root)
}

fn refuse(why: impl Into<String>) -> GuardError {
    GuardError::Refused(why.into())
}

/// Checks that `path` may be removed under `roots`. Returns `Ok(false)` when
/// the path does not exist, because there is nothing to remove.
pub(super) fn validate(roots: &[PathBuf], path: &Path) -> Result<bool, GuardError> {
    if path.components().any(|part| part == Component::ParentDir) {
        return Err(refuse(format!("{} has a parent reference", path.display())));
    }
    let Some(root) = roots.iter().find(|root| lexically_inside(root, path)) else {
        return Err(refuse(format!(
            "{} is not inside a registered root",
            path.display()
        )));
    };
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err.into()),
    };
    let real_root = fs::canonicalize(root)?;
    let real = fs::canonicalize(path).map_err(|err| {
        if meta.file_type().is_symlink() {
            refuse(format!("{} is a dangling symlink", path.display()))
        } else {
            GuardError::Io(err)
        }
    })?;
    if !(real != real_root && real.starts_with(&real_root)) {
        return Err(refuse(format!(
            "{} resolves outside its root",
            path.display()
        )));
    }
    Ok(true)
}

/// Removes `path` when it is inside one of `roots`, is not a root, and does not
/// escape through a symlink. A missing path is not an error.
pub fn checked_remove(guard: &RunGuard, roots: &[PathBuf], path: &Path) -> Result<(), GuardError> {
    if !validate(roots, path)? {
        return Ok(());
    }
    guard.verify()?;
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// What `worktree::remove` needs besides the path.
pub struct WorktreeCtx<'a> {
    pub repo_root: &'a Path,
    pub home: &'a Path,
    pub repo_slug: Option<&'a str>,
    pub now: i64,
}

/// Removes the worktree at `path` through `worktree::remove`, after the same
/// checks as [`checked_remove`]. Returns the removal report.
pub fn checked_worktree_remove(
    guard: &RunGuard,
    roots: &[PathBuf],
    ctx: &WorktreeCtx,
    path: &Path,
) -> Result<String, GuardError> {
    if !validate(roots, path)? {
        return Ok(format!("{}: already gone", path.display()));
    }
    guard.verify()?;
    crate::worktree::remove(ctx.repo_root, ctx.home, ctx.repo_slug, path, ctx.now)
        .map_err(GuardError::Worktree)
}

/// Removes the swept worktree at `path` with `git worktree remove`, after the
/// checks every gc removal needs: the run lock is still held, and the path is a
/// worktree that playbook's own conventions created, never the repo itself.
pub fn checked_swept_worktree_remove(
    guard: &RunGuard,
    (home, repo_root): (&Path, &Path),
    path: &Path,
    force_twice: bool,
) -> Result<(), GuardError> {
    guard.verify()?;
    if path == repo_root
        || crate::worktree::classify(path, home) == crate::worktree::Convention::Unmanaged
    {
        return Err(refuse(format!(
            "{} is not a worktree that playbook created",
            path.display()
        )));
    }
    crate::worktree::remove_worktree(repo_root, path, force_twice).map_err(GuardError::Worktree)
}

/// Removes one build folder inside one worktree. It takes that worktree as its
/// root for this one call: the folder must be a real directory (not a link)
/// that sits inside the worktree and is named like an entry of the ecosystem
/// table.
pub fn checked_build_output_remove(
    guard: &RunGuard,
    worktree_root: &Path,
    folder: &Path,
) -> Result<(), GuardError> {
    guard.verify()?;
    let named_like_build_output = folder
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            super::build_output::TABLE
                .iter()
                .any(|eco| eco.folders.contains(&name))
        });
    if !named_like_build_output {
        return Err(refuse(format!(
            "{} is not a build folder name",
            folder.display()
        )));
    }
    if !fs::symlink_metadata(folder)?.is_dir() {
        return Err(refuse(format!("{} is not a real folder", folder.display())));
    }
    let root = fs::canonicalize(worktree_root)?;
    let parent = folder.parent().map(fs::canonicalize).transpose()?;
    if !parent.is_some_and(|dir| dir.starts_with(&root)) {
        return Err(refuse(format!(
            "{} is outside the worktree {}",
            folder.display(),
            worktree_root.display()
        )));
    }
    fs::remove_dir_all(folder)?;
    Ok(())
}

/// Removes a lock folder: `gc/.lock` or `gc/.lock.stale-*` directly under
/// `gc_dir`. Deletes the owner token first, then any other plain file in it,
/// then the folder.
pub fn checked_remove_lock(gc_dir: &Path, lock: &Path) -> Result<(), GuardError> {
    let is_lock = lock.parent() == Some(gc_dir)
        && lock
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == LOCK_DIR || name.starts_with(STALE_CLAIM_PREFIX));
    if !is_lock {
        return Err(refuse(format!(
            "{} is not a gc lock folder",
            lock.display()
        )));
    }
    // Take the token first so no run can mistake the folder for a live lock,
    // then any other plain file a crashed run left beside it.
    match fs::remove_file(lock.join(TOKEN_FILE)) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    if let Ok(entries) = fs::read_dir(lock) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                fs::remove_file(entry.path())?;
            }
        }
    }
    match fs::remove_dir(lock) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// The filesystem calls gc makes, behind a trait so a test can make one fail.
pub trait FsOps {
    /// Renames `from` to `to`. The real call re-checks the run lock first.
    fn rename(&self, guard: &RunGuard, from: &Path, to: &Path) -> io::Result<()>;
    /// Copies a file or folder to `to`, touching the run lock as it goes.
    fn copy(&self, guard: &RunGuard, from: &Path, to: &Path) -> io::Result<()>;
    /// Runs after a copy finished and before anything else looks at the source.
    fn after_copy(&self, _source: &Path) {}
    /// Appends one line to the tombstone index and syncs it to disk.
    fn append_index(&self, index: &Path, line: &str) -> io::Result<()>;
    fn remove(&self, guard: &RunGuard, path: &Path) -> Result<(), GuardError>;
}

/// The real filesystem. Its roots come from the registry it was built with.
pub struct RealFs {
    roots: Vec<PathBuf>,
}

impl RealFs {
    pub fn new(registry: &Registry) -> Self {
        Self {
            roots: registry.roots(),
        }
    }
}

impl FsOps for RealFs {
    fn rename(&self, guard: &RunGuard, from: &Path, to: &Path) -> io::Result<()> {
        guard.verify().map_err(io::Error::other)?;
        fs::rename(from, to)
    }

    fn copy(&self, guard: &RunGuard, from: &Path, to: &Path) -> io::Result<()> {
        super::tombstone::copy_tree(guard, from, to)
    }

    fn append_index(&self, index: &Path, line: &str) -> io::Result<()> {
        super::index::append_line(index, line)
    }

    fn remove(&self, guard: &RunGuard, path: &Path) -> Result<(), GuardError> {
        checked_remove(guard, &self.roots, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::lock::{with_run_lock, NoHook};
    use crate::gc::{Action, FixedClock, Limits, Store};
    use std::time::Duration;

    const NOW: i64 = 1_800_000_000;

    struct Fixture {
        base: PathBuf,
        root: PathBuf,
        gc: PathBuf,
    }

    fn fixture(tag: &str) -> Fixture {
        let base = crate::common::test_support::scratch_dir(tag);
        let root = base.join("root");
        let gc = base.join("gc");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&gc).unwrap();
        Fixture { base, root, gc }
    }

    fn held<T>(gc: &Path, f: impl FnOnce(&RunGuard) -> T) -> T {
        let clock = FixedClock(NOW);
        match with_run_lock(gc, Duration::ZERO, &clock, &NoHook, f).unwrap() {
            crate::gc::lock::RunOutcome::Ran(value) => value,
            crate::gc::lock::RunOutcome::AlreadyRunning => panic!("lock busy"),
        }
    }

    fn refused(result: Result<(), GuardError>) -> bool {
        matches!(result, Err(GuardError::Refused(_)))
    }

    #[test]
    fn guard_removes_path_inside_root() {
        let fx = fixture("guard-ok");
        let file = fx.root.join("a/b.txt");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "x").unwrap();

        held(&fx.gc, |guard| {
            checked_remove(guard, std::slice::from_ref(&fx.root), &fx.root.join("a")).unwrap()
        });

        assert!(!fx.root.join("a").exists());
    }

    #[test]
    fn unregistered_root_fails_closed() {
        let fx = fixture("guard-unregistered");
        let elsewhere = fx.base.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("keep"), "x").unwrap();
        let store = Store {
            name: "t",
            roots: vec![fx.root.clone()],
            scan: |_, _| Vec::new(),
            action: Action::Delete,
            limits: Limits::default(),
        };
        let ops = RealFs::new(&Registry::new(fx.gc.clone()).with_store(store));

        let result = held(&fx.gc, |guard| ops.remove(guard, &elsewhere.join("keep")));

        assert!(matches!(result, Err(GuardError::Refused(_))));
        assert!(elsewhere.join("keep").exists());
    }

    #[test]
    fn guard_refuses_dotdot_escape() {
        let fx = fixture("guard-dotdot");
        let outside = fx.base.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let sneaky = fx.root.join("..").join("outside");

        let result = held(&fx.gc, |guard| {
            checked_remove(guard, std::slice::from_ref(&fx.root), &sneaky)
        });

        assert!(refused(result));
        assert!(outside.exists());
    }

    #[test]
    fn guard_refuses_root_itself() {
        let fx = fixture("guard-root");

        let result = held(&fx.gc, |guard| {
            checked_remove(guard, std::slice::from_ref(&fx.root), &fx.root)
        });

        assert!(refused(result));
        assert!(fx.root.exists());
    }

    #[test]
    fn guard_refuses_prefix_sibling() {
        let fx = fixture("guard-prefix");
        let evil = fx.base.join("root-evil");
        fs::create_dir_all(&evil).unwrap();

        let result = held(&fx.gc, |guard| {
            checked_remove(guard, std::slice::from_ref(&fx.root), &evil)
        });

        assert!(refused(result));
        assert!(evil.exists());
    }

    #[test]
    fn guard_case_compare_is_exact_on_strings() {
        assert!(lexically_inside(Path::new("/root"), Path::new("/root/a")));
        assert!(!lexically_inside(Path::new("/root"), Path::new("/Root/a")));
        assert!(!lexically_inside(
            Path::new("/root"),
            Path::new("/root-evil")
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn guard_refuses_case_variant_path_linux() {
        let fx = fixture("guard-case-linux");
        let lower = fx.base.join("target");
        let upper = fx.base.join("Target");
        fs::create_dir_all(&lower).unwrap();
        fs::create_dir_all(&upper).unwrap();
        fs::write(upper.join("a"), "x").unwrap();

        let result = held(&fx.gc, |guard| {
            checked_remove(guard, std::slice::from_ref(&lower), &upper.join("a"))
        });

        assert!(refused(result));
        assert!(upper.join("a").exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn guard_refuses_case_variant_macos() {
        let fx = fixture("guard-case-macos");
        let lower = fx.base.join("target");
        fs::create_dir_all(&lower).unwrap();
        fs::write(lower.join("a"), "x").unwrap();

        let result = held(&fx.gc, |guard| {
            checked_remove(
                guard,
                std::slice::from_ref(&lower),
                &fx.base.join("TARGET").join("a"),
            )
        });

        assert!(refused(result));
        assert!(lower.join("a").exists());
    }

    #[cfg(unix)]
    #[test]
    fn guard_refuses_escaping_symlink() {
        let fx = fixture("guard-symlink");
        let outside = fx.base.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, fx.root.join("link")).unwrap();

        let through = held(&fx.gc, |guard| {
            checked_remove(
                guard,
                std::slice::from_ref(&fx.root),
                &fx.root.join("link/keep"),
            )
        });
        let itself = held(&fx.gc, |guard| {
            checked_remove(guard, std::slice::from_ref(&fx.root), &fx.root.join("link"))
        });

        assert!(refused(through) && refused(itself));
        assert!(outside.join("keep").exists());
    }

    #[test]
    fn guard_refusals_cover_worktree_remove() {
        let fx = fixture("guard-worktree");
        let evil = fx.base.join("root-evil");
        fs::create_dir_all(&evil).unwrap();
        let ctx = WorktreeCtx {
            repo_root: &fx.base,
            home: &fx.base,
            repo_slug: None,
            now: NOW,
        };
        let roots = [fx.root.clone()];
        let escapes = [
            fx.root.join("..").join("root-evil"),
            fx.root.clone(),
            evil.clone(),
        ];

        for path in &escapes {
            let result = held(&fx.gc, |guard| {
                checked_worktree_remove(guard, &roots, &ctx, path)
            });

            assert!(matches!(result, Err(GuardError::Refused(_))), "{path:?}");
        }
        assert!(evil.exists());
    }

    #[test]
    fn lock_removal_accepts_only_gc_lock_folders() {
        let fx = fixture("guard-lock-folders");
        let other = fx.gc.join("tombstones");
        fs::create_dir_all(&other).unwrap();

        let result = checked_remove_lock(&fx.gc, &other);

        assert!(refused(result));
        assert!(other.exists());
    }
}
