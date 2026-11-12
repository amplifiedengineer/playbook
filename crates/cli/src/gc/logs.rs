// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The log store: a log that is too big or too old is copied to `<log>.1` and
//! then truncated in place. One rotated copy is kept, so rotating again drops
//! the older `.1`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::stores::mtime_secs;
use super::{Due, Item, Store};

const SECS_PER_DAY: i64 = 86_400;
const LOG_FILES: [&str; 2] = ["collector.log", "collector-service.log"];
const STAGING_SUFFIX: &str = ".gc-rotate.tmp";
const SERVICE_LOG: &str = "collector-service.log";
/// Whether the service manager opens `collector-service.log` in append mode.
/// systemd's `append:` does; launchd's behaviour is unconfirmed, so until it is
/// the log is only reported, because truncating a file a writer holds without
/// `O_APPEND` leaves a hole of zeros at the writer's old offset.
pub const SERVICE_LOG_APPEND_CONFIRMED: bool = false;

/// The file operations rotation needs, so a test can act between the copy and
/// the truncate.
pub trait LogFs {
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn len(&self, path: &Path) -> io::Result<u64>;
    fn truncate(&self, path: &Path) -> io::Result<()>;
    /// Runs after the copy is in place and before the length check.
    fn after_copy(&self, _log: &Path) {}
}

pub struct RealLogFs;

impl LogFs for RealLogFs {
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        // `create_new` fails on any existing name, a symlink included, so the
        // copy can never write through a link. A leftover from a crashed run
        // is cleared by the apply run through the guard, before this call.
        let mut src = fs::File::open(from)?;
        let mut dst = fs::File::options().write(true).create_new(true).open(to)?;
        io::copy(&mut src, &mut dst).map(|_| ())
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
    fn len(&self, path: &Path) -> io::Result<u64> {
        fs::metadata(path).map(|meta| meta.len())
    }
    fn truncate(&self, path: &Path) -> io::Result<()> {
        fs::File::options().write(true).open(path)?.set_len(0)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Rotation {
    Rotated,
    /// The log grew while it was copied, so it was left whole for the next run.
    SkippedGrew,
    SkippedUnconfirmed,
}

/// Whether the writer of `log` is known to append, which makes truncating it
/// in place safe.
pub fn append_confirmed(log: &Path) -> bool {
    log.file_name().is_some_and(|name| name != SERVICE_LOG) || SERVICE_LOG_APPEND_CONFIRMED
}

/// The file `rotate` stages its copy in. The apply run clears a leftover one
/// through the guard before it rotates.
pub fn staging_of(log: &Path) -> PathBuf {
    sibling(log, STAGING_SUFFIX)
}

fn sibling(log: &Path, suffix: &str) -> PathBuf {
    let mut name = log.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Copies `log` to `<log>.1` over any older copy, then truncates `log` only if
/// its length is the same as when the copy began.
pub fn rotate(log: &Path, fs: &dyn LogFs, append_confirmed: bool) -> io::Result<Rotation> {
    if !append_confirmed {
        return Ok(Rotation::SkippedUnconfirmed);
    }
    let len_before = fs.len(log)?;
    let staged = staging_of(log);
    fs.copy(log, &staged)?;
    fs.after_copy(log);
    fs.rename(&staged, &sibling(log, ".1"))?;
    if fs.len(log)? != len_before {
        return Ok(Rotation::SkippedGrew);
    }
    fs.truncate(log)?;
    Ok(Rotation::Rotated)
}

/// The known logs under the store root that are over the size limit or older
/// than the age limit. A log whose writer's append mode is unconfirmed is
/// listed but never due.
pub fn scan(store: &Store, now: i64) -> Vec<Item> {
    let Some(root) = store.roots.first() else {
        return Vec::new();
    };
    let max_age = i64::from(store.limits.idle_days) * SECS_PER_DAY;
    LOG_FILES
        .iter()
        .map(|name| root.join(name))
        .filter_map(|path| {
            let meta = fs::symlink_metadata(&path)
                .ok()
                .filter(fs::Metadata::is_file)?;
            let newest_mtime = mtime_secs(&meta);
            let over_size = meta.len() > store.limits.max_bytes;
            let too_old = now - newest_mtime >= max_age;
            let due =
                (meta.len() > 0 && (over_size || too_old) && append_confirmed(&path)).then(|| {
                    Due {
                        reason: if over_size {
                            "over the size limit".into()
                        } else {
                            format!("older than {} days", store.limits.idle_days)
                        },
                        retain_days: store.limits.retain_days,
                    }
                });
            Some(Item {
                path,
                bytes: meta.len(),
                newest_mtime,
                also: Vec::new(),
                due,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::{Action, Limits};
    use std::cell::Cell;
    use std::time::{Duration, SystemTime};

    const NOW: i64 = 1_800_000_000;
    const MIB: u64 = 1024 * 1024;

    fn store(root: &Path) -> Store {
        Store {
            name: "logs",
            roots: vec![root.to_path_buf()],
            scan,
            action: Action::Rotate,
            limits: Limits {
                idle_days: 30,
                max_bytes: 5 * MIB,
                retain_days: 7,
                ..Limits::default()
            },
        }
    }

    fn log(root: &Path, name: &str, bytes: usize, age_secs: i64) -> PathBuf {
        let path = root.join(name);
        fs::write(&path, vec![b'a'; bytes]).unwrap();
        let when =
            SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(NOW - age_secs).unwrap());
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
        path
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = crate::common::test_support::scratch_dir(tag);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Grows the log between the copy and the length check, once.
    struct GrowingFs {
        grew: Cell<bool>,
    }

    impl LogFs for GrowingFs {
        fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
            RealLogFs.copy(from, to)
        }
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            RealLogFs.rename(from, to)
        }
        fn len(&self, path: &Path) -> io::Result<u64> {
            RealLogFs.len(path)
        }
        fn truncate(&self, path: &Path) -> io::Result<()> {
            RealLogFs.truncate(path)
        }
        fn after_copy(&self, log: &Path) {
            use std::io::Write;
            let mut file = fs::File::options().append(true).open(log).unwrap();
            file.write_all(b"more").unwrap();
            self.grew.set(true);
        }
    }

    fn due_count(root: &Path) -> usize {
        store(root)
            .items(NOW)
            .iter()
            .filter(|item| item.due.is_some())
            .count()
    }

    #[cfg(unix)]
    #[test]
    fn rotation_does_not_write_through_a_planted_symlink() {
        // Arrange
        let root = scratch("logs-symlink-tmp");
        let target = root.join("victim.txt");
        fs::write(&target, "keep me").unwrap();
        let path = log(&root, "collector.log", 10, 0);
        std::os::unix::fs::symlink(&target, staging_of(&path)).unwrap();

        // Act
        let result = rotate(&path, &RealLogFs, true);

        // Assert
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep me");
        assert_eq!(fs::metadata(&path).unwrap().len(), 10);
    }

    #[test]
    fn log_size_boundary_5mib() {
        // Arrange
        let root = scratch("logs-size");
        let at_limit = log(&root, "collector.log", usize::try_from(5 * MIB).unwrap(), 0);

        // Act
        let stays = due_count(&root);
        fs::write(&at_limit, vec![b'a'; usize::try_from(5 * MIB).unwrap() + 1]).unwrap();
        let rotates = due_count(&root);

        // Assert
        assert_eq!((stays, rotates), (0, 1));
    }

    #[test]
    fn log_age_boundary_30d() {
        // Arrange
        let root = scratch("logs-age");
        log(&root, "collector.log", 10, 30 * SECS_PER_DAY - 1);

        // Act
        let stays = due_count(&root);
        log(&root, "collector.log", 10, 30 * SECS_PER_DAY);
        let rotates = due_count(&root);

        // Assert
        assert_eq!((stays, rotates), (0, 1));
    }

    #[test]
    fn log_rotates_by_size_and_age() {
        // Arrange
        let root = scratch("logs-rotate");
        let path = log(&root, "collector.log", 100, 40 * SECS_PER_DAY);
        fs::write(sibling(&path, ".1"), "older copy").unwrap();

        // Act
        let outcome = rotate(&path, &RealLogFs, true).unwrap();

        // Assert
        assert_eq!(outcome, Rotation::Rotated);
        assert_eq!(fs::read(sibling(&path, ".1")).unwrap(), vec![b'a'; 100]);
        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
        assert!(!sibling(&path, ".gc-rotate.tmp").exists());
    }

    #[test]
    fn log_not_truncated_when_it_grew() {
        // Arrange
        let root = scratch("logs-grew");
        let path = log(&root, "collector.log", 100, 40 * SECS_PER_DAY);
        let growing = GrowingFs {
            grew: Cell::new(false),
        };

        // Act
        let outcome = rotate(&path, &growing, true).unwrap();

        // Assert
        assert!(growing.grew.get());
        assert_eq!(outcome, Rotation::SkippedGrew);
        assert_eq!(fs::metadata(&path).unwrap().len(), 104);
    }

    #[test]
    fn service_log_report_only_when_append_unconfirmed() {
        // Arrange
        let root = scratch("logs-service");
        let path = log(&root, SERVICE_LOG, 100, 40 * SECS_PER_DAY);

        // Act
        let items = store(&root).items(NOW);
        let unconfirmed = rotate(&path, &RealLogFs, false).unwrap();
        let confirmed = rotate(&path, &RealLogFs, true).unwrap();

        // Assert
        assert_eq!(items.len(), 1);
        assert!(items[0].due.is_none());
        assert_eq!(unconfirmed, Rotation::SkippedUnconfirmed);
        assert_eq!(confirmed, Rotation::Rotated);
    }
}
