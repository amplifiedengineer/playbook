// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The stores that clean up small leftovers under the playbook root: session
//! folders and the markers of old storage migrations.

use std::fs;
use std::path::{Path, PathBuf};

use super::{
    Action, Due, Item, Limits, Store, BUILD_OUTPUT_ACTIVE_MINUTES, BUILD_OUTPUT_IDLE_DAYS,
    BUILD_OUTPUT_PRESSURE_IDLE_DAYS, FREE_SPACE_WARN_GIB, FREE_SPACE_WARN_PERCENT,
    GATE_DB_IDLE_DAYS, GRACE_DAYS, LEFTOVER_DAYS, LOG_MAX_DAYS, LOG_MAX_MIB, SESSION_DIR_IDLE_DAYS,
};
use crate::common::data::data_dir_from;
use crate::common::paths::playbook_root_from;

/// A folder with more entries than this counts as active, so a huge tree never
/// makes one scan slow.
pub const AGE_WALK_MAX_ENTRIES: usize = 10_000;
/// How deep the leftover search goes below its root.
pub const SCAN_MAX_DEPTH: usize = 6;
const SECS_PER_DAY: i64 = 86_400;
/// Markers that a storage migration leaves behind.
const LEFTOVER_NAMES: [&str; 2] = [".copied", "MOVED.txt"];

/// Size and newest modification time of a file or folder tree.
struct Walked {
    bytes: u64,
    newest_mtime: i64,
    /// The tree held more than [`AGE_WALK_MAX_ENTRIES`] entries.
    capped: bool,
}

pub(super) fn mtime_secs(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Walks `path` without following symlinks, so a link planted in a store never
/// pulls the walk (or an age) from outside it.
fn walk(path: &Path) -> Option<Walked> {
    let top = fs::symlink_metadata(path).ok()?;
    let mut out = Walked {
        bytes: top.len(),
        newest_mtime: mtime_secs(&top),
        capped: false,
    };
    // A symlink at the top is measured as the link itself and not followed.
    let mut pending = if top.is_dir() {
        vec![path.to_path_buf()]
    } else {
        Vec::new()
    };
    let mut seen = 0;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > AGE_WALK_MAX_ENTRIES {
                out.capped = true;
                return Some(out);
            }
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            out.bytes += meta.len();
            out.newest_mtime = out.newest_mtime.max(mtime_secs(&meta));
            if meta.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Some(out)
}

/// An item for `path` that is due once it has been idle for the store's
/// `idle_days`. A capped walk counts as active.
fn aged_item(store: &Store, path: PathBuf, now: i64) -> Option<Item> {
    let walked = walk(&path)?;
    let newest_mtime = if walked.capped {
        now
    } else {
        walked.newest_mtime
    };
    let idle_secs = i64::from(store.limits.idle_days) * SECS_PER_DAY;
    let due = (now - newest_mtime >= idle_secs).then(|| Due {
        reason: format!("idle for {} days or more", store.limits.idle_days),
        retain_days: store.limits.retain_days,
    });
    Some(Item {
        path,
        bytes: walked.bytes,
        newest_mtime,
        also: Vec::new(),
        due,
    })
}

/// The runtime folder also holds the handoff and learn queue folders, which
/// are not sessions and must never be aged out with them.
const NOT_SESSIONS: [&str; 2] = ["handoff", "to-learn"];

/// The session folders directly under a root of `store`: real directories
/// only, so neither a symlink nor a log file is ever an item.
fn scan_sessions(store: &Store, now: i64) -> Vec<Item> {
    let mut items = Vec::new();
    for root in &store.roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let is_session = entry.file_type().is_ok_and(|kind| kind.is_dir())
                && !NOT_SESSIONS.iter().any(|name| entry.file_name() == *name);
            if is_session {
                items.extend(aged_item(store, entry.path(), now));
            }
        }
    }
    items.sort_by(|a, b| a.path.cmp(&b.path));
    items
}

/// Live checkouts sit under this folder name. A tracked `MOVED.txt` in one is
/// the user's file, so the leftover scan does not look inside.
const WORKTREES_DIR: &str = "worktrees";

fn find_leftovers(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        if kind.is_symlink() {
            continue;
        }
        if LEFTOVER_NAMES.iter().any(|leftover| name == *leftover) {
            found.push(entry.path());
        } else if kind.is_dir() && name != WORKTREES_DIR && depth < SCAN_MAX_DEPTH {
            find_leftovers(&entry.path(), depth + 1, found);
        }
    }
}

/// Whether the old files that `marker` stands guard over are gone. A `MOVED.txt`
/// is the "already copied" flag of the old folder it sits in, and a `.copied`
/// marker stops a consumed handoff in the old global folder from coming back.
/// Removing either while those files remain would copy them again.
fn guarded_files_gone(store: &Store, marker: &Path) -> bool {
    let has_entries = |dir: &Path| fs::read_dir(dir).is_ok_and(|mut it| it.next().is_some());
    if marker.file_name().is_some_and(|name| name == ".copied") {
        let legacy = store
            .roots
            .first()
            .and_then(|root| root.parent())
            .map(|root| root.join("runtime").join("handoff"));
        return legacy.is_none_or(|dir| !has_entries(&dir));
    }
    let Some(folder) = marker.parent() else {
        return false;
    };
    fs::read_dir(folder).is_ok_and(|entries| entries.flatten().count() <= 1)
}

/// The `.copied` markers and `MOVED.txt` notes below each root. One is due only
/// once the old files it guards are gone.
fn scan_leftovers(store: &Store, now: i64) -> Vec<Item> {
    let mut paths = Vec::new();
    for root in &store.roots {
        find_leftovers(root, 0, &mut paths);
    }
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let mut item = aged_item(store, path, now)?;
            if !guarded_files_gone(store, &item.path) {
                item.due = None;
            }
            Some(item)
        })
        .collect()
}

fn store(name: &'static str, root: PathBuf, scan: super::ScanFn, idle_days: u32) -> Store {
    Store {
        name,
        roots: vec![root],
        scan,
        action: Action::Tombstone,
        limits: Limits {
            idle_days,
            max_bytes: 0,
            retain_days: GRACE_DAYS,
            ..Limits::default()
        },
    }
}

/// The build output store with its rule numbers.
fn build_output_store() -> Store {
    let mut store = super::build_output::store();
    store.limits = Limits {
        idle_days: BUILD_OUTPUT_IDLE_DAYS,
        pressure_idle_days: BUILD_OUTPUT_PRESSURE_IDLE_DAYS,
        active_minutes: BUILD_OUTPUT_ACTIVE_MINUTES,
        warn_gib: FREE_SPACE_WARN_GIB,
        warn_percent: FREE_SPACE_WARN_PERCENT,
        ..Limits::default()
    };
    store
}

/// The stores of this unit, rooted under `home`.
pub fn small_stores(home: &Path) -> Vec<Store> {
    vec![
        store(
            "session folders",
            playbook_root_from(home).join("runtime"),
            scan_sessions,
            SESSION_DIR_IDLE_DAYS,
        ),
        store(
            "migration leftovers",
            playbook_root_from(home).join("repos"),
            scan_leftovers,
            LEFTOVER_DAYS,
        ),
        store(
            "gate databases",
            data_dir_from(home).join("gate"),
            super::gate::scan,
            GATE_DB_IDLE_DAYS,
        ),
        log_store(home),
        build_output_store(),
    ]
}

/// The logs the playbook writes under its data folder. The size limit is in
/// MiB in config, and the age limit counts days since the last write.
fn log_store(home: &Path) -> Store {
    const MIB: u64 = 1024 * 1024;
    let mut store = store("logs", data_dir_from(home), super::logs::scan, LOG_MAX_DAYS);
    store.action = Action::Rotate;
    store.limits.max_bytes = u64::from(LOG_MAX_MIB) * MIB;
    store
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = SECS_PER_DAY;

    fn scratch(tag: &str) -> PathBuf {
        let dir = crate::common::test_support::scratch_dir(tag);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn backdate(path: &Path, age_secs: i64) {
        let when =
            SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(NOW - age_secs).unwrap());
        fs::File::options()
            .write(true)
            .open(path)
            .or_else(|_| fs::File::open(path))
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn test_store(root: &Path, scan: super::super::ScanFn, idle_days: u32) -> Store {
        Store {
            name: "t",
            roots: vec![root.to_path_buf()],
            scan,
            action: Action::Tombstone,
            limits: Limits {
                idle_days,
                max_bytes: 0,
                retain_days: 7,
                ..Limits::default()
            },
        }
    }

    fn due_paths(items: &[Item]) -> Vec<&Path> {
        items
            .iter()
            .filter(|item| item.due.is_some())
            .map(|item| item.path.as_path())
            .collect()
    }

    #[test]
    fn session_idle_boundary_is_exact_to_the_second() {
        // Arrange
        let root = scratch("stores-session-boundary");
        for (name, age) in [("just-under", 14 * DAY - 1), ("exact", 14 * DAY)] {
            let dir = root.join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("state"), "x").unwrap();
            backdate(&dir.join("state"), age);
            backdate(&dir, age);
        }
        let store = test_store(&root, scan_sessions, 14);

        // Act
        let items = store.items(NOW);

        // Assert
        assert_eq!(due_paths(&items), vec![root.join("exact").as_path()]);
    }

    #[test]
    fn session_idle_uses_newest_nested_mtime() {
        // Arrange
        let root = scratch("stores-session-nested");
        let dir = root.join("sess");
        fs::create_dir_all(dir.join("deep")).unwrap();
        fs::write(dir.join("old"), "x").unwrap();
        fs::write(dir.join("deep/fresh"), "x").unwrap();
        backdate(&dir.join("old"), 30 * DAY);
        backdate(&dir.join("deep/fresh"), DAY);
        backdate(&dir.join("deep"), 30 * DAY);
        backdate(&dir, 30 * DAY);
        let store = test_store(&root, scan_sessions, 14);

        // Act
        let items = store.items(NOW);

        // Assert
        assert!(items[0].due.is_none());
        assert_eq!(items[0].newest_mtime, NOW - DAY);
    }

    #[test]
    fn runtime_folder_keeps_the_handoff_queue_and_log_files() {
        // Arrange
        let root = scratch("stores-not-sessions");
        for name in ["handoff", "to-learn"] {
            fs::create_dir_all(root.join(name)).unwrap();
            fs::write(root.join(name).join("queued"), "x").unwrap();
            backdate(&root.join(name).join("queued"), 400 * DAY);
            backdate(&root.join(name), 400 * DAY);
        }
        fs::write(root.join("compactions.log"), "x").unwrap();
        backdate(&root.join("compactions.log"), 400 * DAY);
        let store = test_store(&root, scan_sessions, 14);

        // Act
        let items = store.items(NOW);

        // Assert
        assert!(items.is_empty());
    }

    #[test]
    fn age_walk_cap_counts_as_active() {
        // Arrange
        let root = scratch("stores-walk-cap");
        let dir = root.join("big");
        fs::create_dir_all(&dir).unwrap();
        for n in 0..=AGE_WALK_MAX_ENTRIES {
            fs::write(dir.join(n.to_string()), "").unwrap();
        }
        let store = test_store(&root, scan_sessions, 1);
        backdate(&dir, 30 * DAY);

        // Act
        let items = store.items(NOW);

        // Assert
        assert!(items[0].due.is_none());
        assert_eq!(items[0].newest_mtime, NOW);
    }

    #[test]
    fn leftovers_follow_the_90_day_boundary() {
        // Arrange
        let base = scratch("stores-leftovers");
        let root = base.join("repos");
        let repo = root.join("acme/app");
        fs::create_dir_all(repo.join("handoff/.copied")).unwrap();
        fs::write(repo.join("handoff/.copied/slug"), "").unwrap();
        fs::create_dir_all(repo.join("old")).unwrap();
        fs::write(repo.join("old/MOVED.txt"), "").unwrap();
        fs::write(repo.join("notes.txt"), "").unwrap();
        backdate(&repo.join("handoff/.copied/slug"), 90 * DAY - 1);
        backdate(&repo.join("handoff/.copied"), 90 * DAY - 1);
        backdate(&repo.join("old/MOVED.txt"), 90 * DAY);
        let store = test_store(&root, scan_leftovers, 90);

        // Act
        let items = store.items(NOW);

        // Assert
        assert_eq!(items.len(), 2);
        assert_eq!(
            due_paths(&items),
            vec![repo.join("old/MOVED.txt").as_path()]
        );
    }

    #[test]
    fn leftover_scan_skips_live_checkouts_and_symlinks() {
        // Arrange
        let base = scratch("stores-leftovers-checkout");
        let root = base.join("repos");
        let checkout = root.join("acme/app/worktrees/wt1/docs");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join("MOVED.txt"), "tracked").unwrap();
        backdate(&checkout.join("MOVED.txt"), 400 * DAY);
        backdate(&checkout, 400 * DAY);
        #[cfg(unix)]
        {
            let target = base.join("elsewhere.txt");
            fs::write(&target, "x").unwrap();
            fs::create_dir_all(root.join("acme/app/old")).unwrap();
            std::os::unix::fs::symlink(&target, root.join("acme/app/old/MOVED.txt")).unwrap();
        }
        let store = test_store(&root, scan_leftovers, 90);

        // Act
        let items = store.items(NOW);

        // Assert
        assert!(items.is_empty(), "{items:?}");
    }

    #[test]
    fn leftovers_stay_while_the_old_files_they_guard_remain() {
        // Arrange
        let base = scratch("stores-leftovers-guard");
        let root = base.join("repos");
        let old = root.join("acme/app/old");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("MOVED.txt"), "").unwrap();
        fs::write(old.join("plan.md"), "still here").unwrap();
        backdate(&old.join("MOVED.txt"), 400 * DAY);
        let copied = root.join("acme/app/handoff/.copied");
        fs::create_dir_all(&copied).unwrap();
        fs::write(copied.join("slug"), "").unwrap();
        backdate(&copied.join("slug"), 400 * DAY);
        backdate(&copied, 400 * DAY);
        let legacy = base.join("runtime/handoff");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("old-handoff.md"), "x").unwrap();
        let store = test_store(&root, scan_leftovers, 90);

        // Act
        let guarded = store.items(NOW);
        // Move the guarded files away: a test may not call a removal itself.
        fs::rename(old.join("plan.md"), base.join("plan.moved")).unwrap();
        fs::rename(legacy.join("old-handoff.md"), base.join("handoff.moved")).unwrap();
        let released = store.items(NOW);

        // Assert
        assert!(due_paths(&guarded).is_empty());
        assert_eq!(due_paths(&released).len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn scan_never_follows_a_symlink_out_of_the_root() {
        // Arrange
        let base = scratch("stores-symlink");
        let root = base.join("root");
        let outside = base.join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("MOVED.txt")).unwrap();
        let sessions = test_store(&root, scan_sessions, 1);
        let leftovers = test_store(&root, scan_leftovers, 1);

        // Act
        let session_items = sessions.items(NOW + 365 * DAY);
        let leftover_items = leftovers.items(NOW + 365 * DAY);

        // Assert
        assert!(session_items.is_empty());
        assert!(leftover_items.is_empty());
        assert!(outside.join("keep").exists());
    }

    #[test]
    fn production_stores_use_the_built_in_numbers() {
        // Arrange
        let home = scratch("stores-fixed");
        let root = playbook_root_from(&home);
        fs::create_dir_all(&root).unwrap();
        for key in [
            "gc.graceDays",
            "gc.sessionDirIdleDays",
            "gc.leftoverDays",
            "gc.gateDbIdleDays",
            "gc.logMaxMiB",
        ] {
            crate::config::store::put(
                &root,
                crate::config::write::Tier::Global,
                None,
                key,
                &serde_json::json!(3),
            )
            .unwrap();
        }

        // Act
        let stores = small_stores(&home);

        // Assert
        assert_eq!(stores[0].limits.idle_days, 14);
        assert_eq!(stores[0].limits.retain_days, 7);
        assert_eq!(stores[1].limits.idle_days, 90);
        assert_eq!(stores[2].limits.idle_days, 30);
        assert_eq!(stores[3].limits.max_bytes, 5 * 1024 * 1024);
    }
}
