// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Helpers shared by the playbook crates: the playbook paths,
//! git, sqlite, atomic file writes, process runs. The usage data folder stays in
//! `playbook`'s own `common::paths`, because it needs the usage crate.
//! `playbook` re-exports these under `playbook::common`.

pub mod atomic;
pub mod git;
pub mod gitfacts;
pub mod hash;
pub mod org;
pub mod org_state;
pub mod paths;
pub mod payload;
pub mod proc;
pub mod repo;
pub mod session;
pub mod sqlite;
pub mod sync_commands;
#[doc(hidden)]
pub mod testing;
pub mod time;

/// Scratch space for unit tests of this crate and of crates that depend on it.
/// Not created by this call; callers create what they need inside it. Unique
/// per call so parallel test threads never collide on the same path.
#[doc(hidden)]
pub mod test_support {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Serialises tests that change the process-global cwd.
    pub fn lock_cwd() -> std::sync::MutexGuard<'static, ()> {
        CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn scratch_dir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "playbook-test-{}-{tag}-{n}",
            crate::testing::run_id()
        ))
    }
}
