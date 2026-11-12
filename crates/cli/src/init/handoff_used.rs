// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Moves the handoffs that older versions parked in a `used/` folder next to
//! the live ones into the gc tombstone folder, where they get the same grace
//! period and the same restore as any other tombstone.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::migrate::{Ctx, Outcome};
use super::run::StepReport;
use crate::common::paths::playbook_root_from;
use crate::gc::guard::{FsOps, RealFs};
use crate::gc::lock::{with_run_lock, NoHook, RunOutcome};
use crate::gc::tombstone::{move_locked, Env, MoveOutcome, MoveRequest};
use crate::gc::{Clock, Registry};

const STEP: &str = "handoff used folder";
const OLD_FOLDER: &str = "used";
const LOCK_WAIT: Duration = Duration::from_millis(500);

pub fn run(ctx: &Ctx) -> Outcome {
    let registry = crate::gc::registry(&ctx.home);
    let fs = RealFs::new(&registry);
    let clock = crate::gc::default_clock();
    migrate(&ctx.home, &registry, &fs, &*clock)
}

/// The old folder of every repo's handoffs and of the global handoff folder.
fn old_folders(root: &Path) -> Vec<PathBuf> {
    let subdirs = |dir: &Path| -> Vec<PathBuf> {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect()
    };
    let mut found: Vec<PathBuf> = subdirs(&root.join("repos"))
        .iter()
        .flat_map(|owner| subdirs(owner))
        .map(|repo| repo.join("handoff").join(OLD_FOLDER))
        .chain([root.join("runtime").join("handoff").join(OLD_FOLDER)])
        .filter(|dir| dir.is_dir())
        .collect();
    found.sort();
    found
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .collect();
    files.sort();
    files
}

/// Tombstones every file in the old folders, stopping at the first one that
/// cannot be moved. Files already moved stay moved, so a rerun finishes.
fn move_all(env: &Env, folders: &[PathBuf], grace_days: u32) -> Result<usize, String> {
    let mut moved = 0;
    for folder in folders {
        for file in files_in(folder) {
            env.guard.verify().map_err(|e| e.to_string())?;
            let req = MoveRequest {
                paths: vec![file.clone()],
                reason: "migrated from the old used folder".into(),
                retain_days: grace_days,
            };
            match move_locked(env, &req, None) {
                MoveOutcome::Moved { .. } => moved += 1,
                other => return Err(format!("{}: {other:?}", file.display())),
            }
        }
        if fs::read_dir(folder).is_ok_and(|mut entries| entries.next().is_none()) {
            let _ = env.fs.remove(env.guard, folder);
        }
    }
    Ok(moved)
}

pub(super) fn migrate(
    home: &Path,
    registry: &Registry,
    fs: &dyn FsOps,
    clock: &dyn Clock,
) -> Outcome {
    let folders = old_folders(&playbook_root_from(home));
    if folders.is_empty() {
        return Outcome::Quiet;
    }
    let grace_days = crate::gc::HANDOFF_GRACE_DAYS;
    let ran = with_run_lock(registry.gc_dir(), LOCK_WAIT, clock, &NoHook, |guard| {
        move_all(
            &Env {
                guard,
                registry,
                fs,
            },
            &folders,
            grace_days,
        )
    });
    match ran {
        Ok(RunOutcome::Ran(Ok(0))) => Outcome::Quiet,
        Ok(RunOutcome::Ran(Ok(moved))) => Outcome::Repeat(StepReport::wired(
            STEP,
            format!("moved {moved} loaded handoffs to the gc tombstone folder"),
        )),
        Ok(RunOutcome::Ran(Err(why))) => Outcome::Failed(StepReport::failed(
            STEP,
            format!("could not move a handoff, so the rest wait for the next run: {why}"),
        )),
        Ok(RunOutcome::AlreadyRunning) => Outcome::Failed(StepReport::failed(
            STEP,
            "gc busy; the handoffs move at the next run",
        )),
        Err(err) => Outcome::Failed(StepReport::failed(STEP, err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::apply::expire;
    use crate::gc::guard::GuardError;
    use crate::gc::index;
    use crate::gc::lock::RunGuard;
    use crate::gc::FixedClock;
    use std::cell::Cell;
    use std::io;
    use std::time::SystemTime;

    const NOW: i64 = 1_800_000_000;

    fn home(tag: &str) -> PathBuf {
        let dir = crate::common::test_support::scratch_dir(tag);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn old_file(home: &Path, rel: &str, name: &str) -> PathBuf {
        let dir = playbook_root_from(home).join(rel).join(OLD_FOLDER);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, "loaded").unwrap();
        path
    }

    fn migrate_now(home: &Path) -> Outcome {
        let registry = crate::gc::registry(home);
        let fs = RealFs::new(&registry);
        migrate(home, &registry, &fs, &FixedClock(NOW))
    }

    /// Fails the Nth rename and passes everything else through.
    struct FailingRename {
        real: RealFs,
        calls: Cell<usize>,
        fail_at: usize,
    }

    impl FsOps for FailingRename {
        fn rename(&self, guard: &RunGuard, from: &Path, to: &Path) -> io::Result<()> {
            self.calls.set(self.calls.get() + 1);
            if self.calls.get() == self.fail_at {
                return Err(io::Error::other("injected fault"));
            }
            self.real.rename(guard, from, to)
        }
        fn copy(&self, guard: &RunGuard, from: &Path, to: &Path) -> io::Result<()> {
            self.real.copy(guard, from, to)
        }
        fn append_index(&self, index: &Path, line: &str) -> io::Result<()> {
            self.real.append_index(index, line)
        }
        fn remove(&self, guard: &RunGuard, path: &Path) -> Result<(), GuardError> {
            self.real.remove(guard, path)
        }
    }

    #[test]
    fn migration_moves_two_repos_and_legacy() {
        // Arrange
        let home = home("used-migrate-all");
        let a = old_file(&home, "repos/acme/app/handoff", "-a-1-1.md");
        let b = old_file(&home, "repos/acme/lib/handoff", "-b-1-1.md");
        let legacy = old_file(&home, "runtime/handoff", "-c-1-1.md");

        // Act
        let first = migrate_now(&home);
        let second = migrate_now(&home);

        // Assert
        assert!(matches!(first, Outcome::Repeat(_)));
        assert!(matches!(second, Outcome::Quiet), "a second run is quiet");
        for gone in [&a, &b, &legacy] {
            assert!(!gone.exists());
            assert!(!gone.parent().unwrap().exists(), "the empty folder goes");
        }
        let records = index::latest(index::read(&index::index_path(
            crate::gc::registry(&home).gc_dir(),
        )));
        assert_eq!(records.len(), 3);
    }

    #[test]
    fn migration_resumes_after_midway_failure() {
        // Arrange
        let home = home("used-migrate-fault");
        let files: Vec<PathBuf> = (1..=5)
            .map(|n| old_file(&home, "repos/acme/app/handoff", &format!("-a-{n}-1.md")))
            .collect();
        let registry = crate::gc::registry(&home);
        let faulty = FailingRename {
            real: RealFs::new(&registry),
            calls: Cell::new(0),
            fail_at: 3,
        };

        // Act
        let first = migrate(&home, &registry, &faulty, &FixedClock(NOW));
        let left_after_fault: Vec<bool> = files.iter().map(|f| f.exists()).collect();
        let second = migrate_now(&home);

        // Assert
        assert!(matches!(first, Outcome::Failed(_)));
        assert_eq!(left_after_fault, [false, false, true, true, true]);
        assert!(matches!(second, Outcome::Repeat(_)));
        assert!(files.iter().all(|f| !f.exists()));
        assert!(!files[0].parent().unwrap().exists());
    }

    #[test]
    fn migration_busy_returns_failed() {
        // Arrange
        let home = home("used-migrate-busy");
        let file = old_file(&home, "repos/acme/app/handoff", "-a-1-1.md");
        let registry = crate::gc::registry(&home);
        let clock = FixedClock(crate::common::time::now_secs());

        // Act
        let outcome = with_run_lock(registry.gc_dir(), Duration::ZERO, &clock, &NoHook, |_| {
            let fs = RealFs::new(&registry);
            migrate(&home, &registry, &fs, &clock)
        });

        // Assert
        let Ok(RunOutcome::Ran(Outcome::Failed(report))) = outcome else {
            panic!("the migration should report failure");
        };
        assert!(report.detail.contains("gc busy"), "{}", report.detail);
        assert!(file.exists(), "nothing moves while gc is busy");
    }

    #[test]
    fn migration_keeps_source_mtime_without_expiring() {
        // Arrange
        let home = home("used-migrate-mtime");
        let file = old_file(&home, "repos/acme/app/handoff", "-a-1-1.md");
        let old =
            SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(NOW - 30 * 86_400).unwrap());
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();

        // Act
        migrate_now(&home);
        let registry = crate::gc::registry(&home);
        let fs = RealFs::new(&registry);
        let clock = FixedClock(NOW + 1);
        let expired = with_run_lock(
            registry.gc_dir(),
            Duration::ZERO,
            &clock,
            &NoHook,
            |guard| {
                expire(&Env {
                    guard,
                    registry: &registry,
                    fs: &fs,
                })
            },
        );

        // Assert
        let records = index::latest(index::read(&index::index_path(registry.gc_dir())));
        assert_eq!(records[0].source_mtime, NOW - 30 * 86_400);
        assert!(matches!(expired, Ok(RunOutcome::Ran(0))));
    }
}
