// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The daily sweep that a session start runs. It takes the run lock without
//! waiting, goes through the stores from a saved cursor until a time budget is
//! spent, and stamps the day only once a lap has reached the last store.

use std::path::Path;
use std::time::Duration;

use super::apply::{run_apply_skipping, ApplyOutcome};
use super::guard::RealFs;
use super::lock::{with_run_lock, NoHook};
use super::tombstone::Env;
use super::{Clock, Registry};

/// The sweep runs at most once in this many seconds.
pub const SWEEP_INTERVAL_SECS: i64 = 86_400;
/// The sweep stops between stores once this many seconds have passed. One
/// store can still run past it, bounded only by its own limits.
pub const SWEEP_BUDGET_SECS: i64 = 20;
const STAMP_KEY: &str = "gc_last_sweep";
const CURSOR_KEY: &str = "gc_cursor";
const FAILED_KEY: &str = "gc_failed_store";
const SECS_PER_DAY: i64 = 86_400;

fn read_number(root: &Path, key: &str) -> Option<i64> {
    crate::state::get(root, key)
        .ok()
        .flatten()
        .and_then(|value| value.trim().parse().ok())
}

fn is_due(root: &Path, now: i64) -> bool {
    read_number(root, STAMP_KEY).is_none_or(|stamp| now - stamp >= SWEEP_INTERVAL_SECS)
}

/// The store the next start begins with. A cursor past the end, left by a
/// registry that shrank, starts a new lap.
fn saved_cursor(root: &Path, store_count: usize) -> usize {
    read_number(root, CURSOR_KEY)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|&n| n < store_count)
        .unwrap_or(0)
}

/// Stores recorded as failed on the day of `now`, as `<day>:<index>,<index>`.
fn failed_today(root: &Path, now: i64) -> Vec<usize> {
    let Some(value) = crate::state::get(root, FAILED_KEY).ok().flatten() else {
        return Vec::new();
    };
    let Some((day, indexes)) = value.split_once(':') else {
        return Vec::new();
    };
    if day.parse::<i64>().ok() != Some(now / SECS_PER_DAY) {
        return Vec::new();
    }
    indexes.split(',').filter_map(|n| n.parse().ok()).collect()
}

fn record_failures(root: &Path, now: i64, failed: &[usize]) {
    let mut all = failed_today(root, now);
    all.extend(failed);
    all.sort_unstable();
    all.dedup();
    let indexes: Vec<String> = all.iter().map(ToString::to_string).collect();
    let value = format!("{}:{}", now / SECS_PER_DAY, indexes.join(","));
    let _ = crate::state::set(root, FAILED_KEY, &value);
}

/// Saves where the next start continues. The day is stamped only when the lap
/// reached the last store, so a cut-short sweep runs again at the next start.
fn record_progress(root: &Path, now: i64, store_count: usize, out: &ApplyOutcome) {
    if !out.failed_stores.is_empty() {
        record_failures(root, now, &out.failed_stores);
    }
    let finished = out.next_store >= store_count;
    let cursor = if finished { 0 } else { out.next_store };
    let _ = crate::state::set(root, CURSOR_KEY, &cursor.to_string());
    if finished {
        let _ = crate::state::set(root, STAMP_KEY, &now.to_string());
    }
}

/// Runs a slice of the daily sweep when one is due and `enabled`. Returns
/// without waiting when another run holds the lock.
pub fn maybe_gc(enabled: bool, registry: &Registry, clock: &dyn Clock) {
    let Some(root) = registry.gc_dir().parent() else {
        return;
    };
    let now = clock.now();
    if !enabled || !is_due(root, now) {
        return;
    }
    let fs = RealFs::new(registry);
    let ran = with_run_lock(registry.gc_dir(), Duration::ZERO, clock, &NoHook, |guard| {
        // Read again under the lock: another start may have finished a lap.
        if !is_due(root, now) {
            return;
        }
        let env = Env {
            guard,
            registry,
            fs: &fs,
        };
        let count = registry.stores().len();
        let skip = failed_today(root, now);
        let cursor = saved_cursor(root, count);
        let out = run_apply_skipping(&env, (cursor, &skip), Some(SWEEP_BUDGET_SECS));
        for failure in &out.failures {
            eprintln!("gc-sweep: {failure}");
        }
        record_progress(root, now, count, &out);
    });
    if let Err(err) = ran {
        eprintln!("gc-sweep: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::lock::with_run_lock;
    use crate::gc::{Action, Due, FixedClock, Item, Limits, Store};
    use std::cell::Cell;
    use std::fs;
    use std::path::PathBuf;

    const T0: i64 = 1_800_000_000;
    /// Seconds that a store's scan adds to the test clock.
    const SLOW_STORE_SECS: i64 = 11;

    thread_local! {
        static ELAPSED: Cell<i64> = const { Cell::new(0) };
    }

    /// A clock that moves forward whenever a slow store scans.
    struct SlowClock(i64);

    impl Clock for SlowClock {
        fn now(&self) -> i64 {
            self.0 + ELAPSED.get()
        }
    }

    fn scan_old(store: &Store, _now: i64) -> Vec<Item> {
        ELAPSED.set(ELAPSED.get() + SLOW_STORE_SECS);
        fs::read_dir(&store.roots[0])
            .unwrap()
            .flatten()
            .map(|entry| Item {
                path: entry.path(),
                bytes: 1,
                newest_mtime: 0,
                also: Vec::new(),
                due: Some(Due {
                    reason: "old".into(),
                    retain_days: 7,
                }),
            })
            .collect()
    }

    fn scan_panics(_store: &Store, _now: i64) -> Vec<Item> {
        panic!("a store failed");
    }

    struct Fx {
        base: PathBuf,
        registry: Registry,
    }

    fn fx(tag: &str, scans: &[crate::gc::ScanFn]) -> Fx {
        let base = crate::common::test_support::scratch_dir(tag);
        let mut registry = Registry::new(base.join("gc"));
        for (n, scan) in scans.iter().enumerate() {
            let root = base.join(format!("store{n}"));
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("item"), "x").unwrap();
            registry = registry.with_store(Store {
                name: "test",
                roots: vec![root],
                scan: *scan,
                action: Action::Tombstone,
                limits: Limits::default(),
            });
        }
        fs::create_dir_all(&base).unwrap();
        ELAPSED.set(0);
        Fx { base, registry }
    }

    impl Fx {
        fn item_left(&self, store: usize) -> bool {
            self.base.join(format!("store{store}/item")).exists()
        }

        fn stamp(&self) -> Option<i64> {
            read_number(&self.base, STAMP_KEY)
        }

        fn cursor(&self) -> Option<i64> {
            read_number(&self.base, CURSOR_KEY)
        }

        fn start(&self, now: i64) {
            ELAPSED.set(0);
            maybe_gc(true, &self.registry, &SlowClock(now));
        }
    }

    #[test]
    fn daily_sweep_boundary() {
        // Arrange
        let fx = fx("sweep-boundary", &[scan_old]);
        fx.start(T0);
        fs::write(fx.base.join("store0/item"), "again").unwrap();

        // Act
        fx.start(T0 + SWEEP_INTERVAL_SECS - 1);
        let kept = fx.item_left(0);
        fx.start(T0 + SWEEP_INTERVAL_SECS);

        // Assert
        assert!(kept);
        assert!(!fx.item_left(0));
        assert_eq!(fx.stamp(), Some(T0 + SWEEP_INTERVAL_SECS));
    }

    #[test]
    fn gc_disabled_skips_sweep() {
        // Arrange
        let fx = fx("sweep-disabled", &[scan_old]);

        // Act
        maybe_gc(false, &fx.registry, &FixedClock(T0));

        // Assert
        assert!(fx.item_left(0));
        assert_eq!(fx.stamp(), None);
    }

    #[test]
    fn sweep_stops_at_budget_between_stores() {
        // Arrange
        let fx = fx("sweep-budget", &[scan_old, scan_old, scan_old]);

        // Act
        fx.start(T0);

        // Assert
        assert!(!fx.item_left(0) && !fx.item_left(1));
        assert!(fx.item_left(2));
    }

    #[test]
    fn cut_short_sweep_does_not_stamp() {
        // Arrange
        let fx = fx("sweep-no-stamp", &[scan_old, scan_old, scan_old]);

        // Act
        fx.start(T0);

        // Assert
        assert_eq!(fx.stamp(), None);
    }

    #[test]
    fn cut_short_sweep_resumes_at_cursor() {
        // Arrange
        let fx = fx("sweep-resume", &[scan_old, scan_old, scan_old]);
        fx.start(T0);
        let cursor = fx.cursor();

        // Act
        fx.start(T0 + 1);

        // Assert
        assert_eq!(cursor, Some(2));
        assert!(!fx.item_left(2));
        assert_eq!(fx.stamp(), Some(T0 + 1));
    }

    #[test]
    fn cursor_wrap_across_starts_writes_stamp() {
        // Arrange
        let fx = fx("sweep-wrap", &[scan_old as crate::gc::ScanFn; 5]);

        // Act
        fx.start(T0);
        let after_first = (fx.cursor(), fx.stamp());
        fx.start(T0 + 1);
        let after_second = (fx.cursor(), fx.stamp());
        fx.start(T0 + 2);

        // Assert
        assert_eq!(after_first, (Some(2), None));
        assert_eq!(after_second, (Some(4), None));
        assert_eq!((fx.cursor(), fx.stamp()), (Some(0), Some(T0 + 2)));
    }

    #[test]
    fn maybe_gc_skips_when_locked() {
        // Arrange
        let fx = fx("sweep-locked", &[scan_old]);
        // The real time keeps the lock folder's own mtime from looking stale.
        let clock = FixedClock(crate::common::time::now_secs());

        // Act
        let held = with_run_lock(
            fx.registry.gc_dir(),
            Duration::ZERO,
            &clock,
            &NoHook,
            |_guard| maybe_gc(true, &fx.registry, &clock),
        );

        // Assert
        assert!(held.is_ok());
        assert!(fx.item_left(0));
        assert_eq!(fx.stamp(), None);
    }

    #[test]
    fn failing_store_is_recorded_and_skipped_until_next_day() {
        // Arrange
        let fx = fx("sweep-failing", &[scan_panics, scan_old]);

        // Act
        fx.start(T0);
        let recorded = failed_today(&fx.base, T0);
        let same_day = failed_today(&fx.base, T0 + 60);
        let next_day = failed_today(&fx.base, T0 + SECS_PER_DAY);

        // Assert
        assert_eq!((recorded, same_day), (vec![0], vec![0]));
        assert!(next_day.is_empty());
        assert!(!fx.item_left(1), "a failing store does not stop the others");
        assert_eq!(fx.stamp(), Some(T0));
    }
}
