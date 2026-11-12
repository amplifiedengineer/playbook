// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The gc engine: a registry of stores, a safety guard that every removal goes
//! through, and one run lock. Stores arrive in later units; this module holds
//! the types they share.

pub mod apply;
pub mod build_output;
pub mod gate;
pub mod guard;
pub mod index;
pub mod lock;
pub mod logs;
pub mod report;
pub mod space;
pub mod stores;
pub mod sweep;
pub mod tombstone;
pub mod worktrees;

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The folder under the playbook root that holds gc state.
pub const GC_DIR: &str = "gc";
/// The folder under [`GC_DIR`] that holds tombstoned items.
pub const TOMBSTONES_DIR: &str = "tombstones";
/// A lock folder, or a pending record, older than this belongs to a dead run.
pub const RUN_LOCK_STALE: Duration = Duration::from_secs(3600);
/// Name of the environment variable that sets the clock in debug builds.
const TEST_NOW_VAR: &str = "PLAYBOOK_TEST_NOW";

/// Days a tombstoned item can be restored before it is deleted.
pub const GRACE_DAYS: u32 = 7;
/// Days a loaded handoff stays restorable.
pub const HANDOFF_GRACE_DAYS: u32 = 14;
/// Days a session folder must be idle before it is tombstoned.
pub const SESSION_DIR_IDLE_DAYS: u32 = 14;
/// Age in days at which a collector log is rotated.
pub const LOG_MAX_DAYS: u32 = 30;
/// Size in MiB at which a collector log is rotated.
pub const LOG_MAX_MIB: u32 = 5;
/// Age in days of a migration leftover before it is tombstoned.
pub const LEFTOVER_DAYS: u32 = 90;
/// Days a gate database of a removed worktree must be idle.
pub const GATE_DB_IDLE_DAYS: u32 = 30;
/// Days a build folder in a playbook worktree must be idle.
pub const BUILD_OUTPUT_IDLE_DAYS: u32 = 7;
/// The shorter build folder idle limit used while free space is low.
pub const BUILD_OUTPUT_PRESSURE_IDLE_DAYS: u32 = 1;
/// Minutes a recent change keeps a build folder in use.
pub const BUILD_OUTPUT_ACTIVE_MINUTES: u32 = 15;
/// Free GiB under which space counts as low.
pub const FREE_SPACE_WARN_GIB: u32 = 10;
/// Free percent of the volume under which space counts as low.
pub const FREE_SPACE_WARN_PERCENT: u32 = 5;

// Pressure may only shorten the idle limit.
const _: () = assert!(BUILD_OUTPUT_PRESSURE_IDLE_DAYS <= BUILD_OUTPUT_IDLE_DAYS);

/// A source of the current time as epoch seconds, so tests control it.
pub trait Clock {
    fn now(&self) -> i64;
}

/// The wall clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        crate::common::time::now_secs()
    }
}

/// A fixed time, used by tests and by the debug-only environment override.
pub struct FixedClock(pub i64);

impl Clock for FixedClock {
    fn now(&self) -> i64 {
        self.0
    }
}

/// The clock the binary uses. A debug build reads `PLAYBOOK_TEST_NOW` (epoch
/// seconds) so integration tests that run the real binary can set the time.
/// A release build always uses the wall clock.
pub fn default_clock() -> Box<dyn Clock> {
    #[cfg(debug_assertions)]
    if let Some(fixed) = std::env::var(TEST_NOW_VAR)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
    {
        return Box::new(FixedClock(fixed));
    }
    Box::new(SystemClock)
}

/// What a store does with an item that has outlived its rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Delete,
    Tombstone,
    Rotate,
    ReportOnly,
}

impl Action {
    /// The word the report prints.
    pub fn label(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Tombstone => "tombstone",
            Self::Rotate => "rotate",
            Self::ReportOnly => "report only",
        }
    }
}

/// One thing a store found: a file or folder, its size and its age.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub path: PathBuf,
    pub bytes: u64,
    /// Newest modification time found, as epoch seconds.
    pub newest_mtime: i64,
    /// Files that move with `path` as one group, such as a database's
    /// `-wal` and `-shm` files.
    pub also: Vec<PathBuf>,
    /// Set when the store's rule says this item should go now.
    pub due: Option<Due>,
}

/// Why an item is due and how long its tombstone is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub reason: String,
    pub retain_days: u32,
}

/// Lists the items under a store's roots, judged by its limits. Takes `now` so a rule can be tested
/// without the wall clock.
pub type ScanFn = fn(store: &Store, now: i64) -> Vec<Item>;

/// The numbers a store's rule reads, set when the registry is built so a scan
/// never computes them itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    /// Days without a change before an item is due.
    pub idle_days: u32,
    /// Size above which an item is due, in bytes. Zero means no size rule.
    pub max_bytes: u64,
    /// Days a tombstone of this store's items is kept.
    pub retain_days: u32,
    /// Build output only: the shorter idle limit used while disk space is low.
    pub pressure_idle_days: u32,
    /// Build output only: minutes a recent change keeps a folder in use.
    pub active_minutes: u32,
    /// Build output only: free GiB under which space counts as low.
    pub warn_gib: u32,
    /// Build output only: free percent under which space counts as low.
    pub warn_percent: u32,
}

/// One kind of on-disk state with a retention rule.
#[derive(Debug, Clone)]
pub struct Store {
    pub name: &'static str,
    pub roots: Vec<PathBuf>,
    pub scan: ScanFn,
    pub action: Action,
    pub limits: Limits,
}

impl Store {
    pub fn items(&self, now: i64) -> Vec<Item> {
        (self.scan)(self, now)
    }
}

/// Every store, plus the root that holds tombstones.
#[derive(Debug, Clone)]
pub struct Registry {
    gc_dir: PathBuf,
    stores: Vec<Store>,
}

impl Registry {
    /// An empty registry for the gc folder `gc_dir`.
    pub fn new(gc_dir: PathBuf) -> Self {
        Self {
            gc_dir,
            stores: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_store(mut self, store: Store) -> Self {
        self.stores.push(store);
        self
    }

    pub fn stores(&self) -> &[Store] {
        &self.stores
    }

    pub fn gc_dir(&self) -> &Path {
        &self.gc_dir
    }

    pub fn tombstones_dir(&self) -> PathBuf {
        self.gc_dir.join(TOMBSTONES_DIR)
    }

    /// The roots of the registered stores: where an item may be moved from.
    pub fn store_roots(&self) -> Vec<PathBuf> {
        self.stores
            .iter()
            .flat_map(|store| store.roots.iter().cloned())
            .collect()
    }

    /// The paths gc may remove inside: each store's roots and the tombstones
    /// folder.
    pub fn roots(&self) -> Vec<PathBuf> {
        let mut roots = self.store_roots();
        roots.push(self.tombstones_dir());
        roots
    }
}

/// The production registry for the user whose home is `home`. Each unit that
/// adds a store adds it here.
pub fn registry(home: &Path) -> Registry {
    let root = crate::common::paths::playbook_root_from(home);
    stores::small_stores(home)
        .into_iter()
        .fold(Registry::new(root.join(GC_DIR)), Registry::with_store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tombstone_root_is_registered() {
        let registry = registry(Path::new("/pb"));

        let roots = registry.roots();

        assert!(roots.contains(&PathBuf::from("/pb/.config/playbook/gc/tombstones")));
    }

    #[test]
    fn registry_roots_include_every_store_root() {
        let store = Store {
            name: "t",
            roots: vec![PathBuf::from("/a"), PathBuf::from("/b")],
            scan: |_, _| Vec::new(),
            action: Action::Delete,
            limits: Limits::default(),
        };
        let registry = Registry::new(PathBuf::from("/pb/gc")).with_store(store);

        let roots = registry.roots();

        assert!(roots.contains(&PathBuf::from("/a")) && roots.contains(&PathBuf::from("/b")));
    }

    #[test]
    fn scan_reads_the_limits_of_its_store() {
        // Arrange
        let store = Store {
            name: "t",
            roots: Vec::new(),
            scan: |store, now| {
                vec![Item {
                    path: PathBuf::from("/x"),
                    bytes: store.limits.max_bytes,
                    newest_mtime: now,
                    also: Vec::new(),
                    due: None,
                }]
            },
            action: Action::Delete,
            limits: Limits {
                max_bytes: 9,
                ..Limits::default()
            },
        };

        // Act
        let items = store.items(5);

        // Assert
        assert_eq!((items[0].bytes, items[0].newest_mtime), (9, 5));
    }

    #[test]
    fn fixed_clock_returns_its_time() {
        assert_eq!(FixedClock(42).now(), 42);
    }
}
