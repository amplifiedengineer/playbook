// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The size and age report. It reads the registry and changes nothing, takes no
//! lock and runs no reconcile.

use std::fmt::Write as _;

use super::{Action, Registry};

const KIB: u64 = 1024;
const MIB: u64 = KIB * 1024;
const GIB: u64 = MIB * 1024;

/// What one store holds today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreReport {
    pub name: &'static str,
    pub action: Action,
    pub items: usize,
    pub bytes: u64,
    /// Seconds since the oldest item was last touched, if there is an item.
    pub oldest_age_secs: Option<i64>,
}

/// The report for every registered store, in registry order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub stores: Vec<StoreReport>,
}

impl Report {
    pub fn total_bytes(&self) -> u64 {
        self.stores.iter().map(|store| store.bytes).sum()
    }
}

/// Scans every store in `registry` as of `now` (epoch seconds).
pub fn report(registry: &Registry, now: i64) -> Report {
    let stores = registry
        .stores()
        .iter()
        .map(|store| {
            let items = store.items(now);
            StoreReport {
                name: store.name,
                action: store.action,
                items: items.len(),
                bytes: items.iter().map(|item| item.bytes).sum(),
                oldest_age_secs: items
                    .iter()
                    .map(|item| now.saturating_sub(item.newest_mtime))
                    .max(),
            }
        })
        .collect();
    Report { stores }
}

/// A size in the largest unit that keeps the number above one.
pub fn human_bytes(bytes: u64) -> String {
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

const SECS_PER_DAY: i64 = 86_400;

/// The text the command prints.
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    if report.stores.is_empty() {
        out.push_str("No gc stores are registered.\n");
        return out;
    }
    let width = report
        .stores
        .iter()
        .map(|store| store.name.len())
        .max()
        .unwrap_or(0);
    for store in &report.stores {
        let age = store
            .oldest_age_secs
            .map_or_else(|| "-".to_string(), |s| format!("{}d", s / SECS_PER_DAY));
        let _ = writeln!(
            out,
            "{:<width$}  {:>10}  {:>6} {}  oldest {:>5}  {}",
            store.name,
            human_bytes(store.bytes),
            store.items,
            if store.items == 1 { "item " } else { "items" },
            age,
            store.action.label(),
        );
    }
    let _ = writeln!(out, "Total {}", human_bytes(report.total_bytes()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::{Item, Limits, Store};
    use std::fs;
    use std::path::PathBuf;

    const NOW: i64 = 1_800_000_000;

    fn scan_files(store: &Store, _now: i64) -> Vec<Item> {
        let mut items = Vec::new();
        for root in &store.roots {
            let Ok(entries) = fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                let meta = entry.metadata().unwrap();
                items.push(Item {
                    path: entry.path(),
                    bytes: meta.len(),
                    newest_mtime: NOW - 2 * SECS_PER_DAY,
                    also: Vec::new(),
                    due: None,
                });
            }
        }
        items
    }

    fn store(name: &'static str, root: PathBuf, action: Action) -> Store {
        Store {
            name,
            roots: vec![root],
            scan: scan_files,
            action,
            limits: Limits::default(),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = crate::common::test_support::scratch_dir(tag);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn gc_dry_run_removes_nothing() {
        let base = scratch("report-two-stores");
        let (a, b) = (base.join("a"), base.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("one"), vec![0u8; 100]).unwrap();
        fs::write(a.join("two"), vec![0u8; 50]).unwrap();
        fs::write(b.join("three"), vec![0u8; 2048]).unwrap();
        let registry = Registry::new(base.join("gc"))
            .with_store(store("alpha", a.clone(), Action::Tombstone))
            .with_store(store("beta", b.clone(), Action::ReportOnly));

        let result = report(&registry, NOW);

        assert_eq!(result.stores[0].items, 2);
        assert_eq!(result.stores[0].bytes, 150);
        assert_eq!(result.stores[1].items, 1);
        assert_eq!(result.stores[1].bytes, 2048);
        assert_eq!(result.stores[0].oldest_age_secs, Some(2 * SECS_PER_DAY));
        assert!(a.join("one").exists() && a.join("two").exists() && b.join("three").exists());
        let text = render(&result);
        assert!(text.contains("alpha") && text.contains("2.0 KiB") && text.contains("tombstone"));
        assert!(text.contains("2 items") && text.contains("1 item "));
        assert!(!text.contains("1 items"));
    }

    #[test]
    fn dry_run_takes_no_lock() {
        let base = scratch("report-no-lock");
        let registry = Registry::new(base.join("gc"));

        let _ = report(&registry, NOW);

        assert!(!base.join("gc").exists());
    }

    #[test]
    fn empty_store_reports_zero() {
        let base = scratch("report-empty");
        let root = base.join("empty");
        fs::create_dir_all(&root).unwrap();
        let registry = Registry::new(base.join("gc")).with_store(store("e", root, Action::Delete));

        let result = report(&registry, NOW);

        assert_eq!(result.stores[0].items, 0);
        assert_eq!(result.stores[0].bytes, 0);
        assert_eq!(result.stores[0].oldest_age_secs, None);
        assert!(render(&result).contains("0 items"));
    }

    #[test]
    fn human_bytes_picks_the_unit_at_each_boundary() {
        assert_eq!(human_bytes(KIB - 1), "1023 B");
        assert_eq!(human_bytes(KIB), "1.0 KiB");
        assert_eq!(human_bytes(MIB), "1.0 MiB");
        assert_eq!(human_bytes(GIB), "1.0 GiB");
    }
}
