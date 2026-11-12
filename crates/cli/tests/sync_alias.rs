// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `pbsync X` and `playbook sync X` give the same stdout, stderr and exit
//! code. The real `pbsync` comes from `PBSYNC_BIN`, because cargo builds a
//! binary only for its own crate's tests. Each run uses a scratch home with
//! sync off, so nothing reaches the network.

#[path = "support/isolated.rs"]
mod isolated;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const PBSYNC_BIN: &str = "PBSYNC_BIN";
const CI: &str = "CI";

/// The words after `pbsync` or `playbook sync`; each runs offline with sync off.
const CASES: [&[&str]; 5] = [
    &["status"],
    &["enable"],
    &["run", "--help"],
    &["config", "--help"],
    &["purge", "--help"],
];

fn pbsync_binary() -> Option<PathBuf> {
    match std::env::var_os(PBSYNC_BIN) {
        Some(path) => Some(PathBuf::from(path)),
        None => {
            assert!(
                std::env::var_os(CI).is_none(),
                "{PBSYNC_BIN} must point at the built pbsync in CI"
            );
            eprintln!("skipped: {PBSYNC_BIN} is not set");
            None
        }
    }
}

fn scratch() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "playbook-sync-alias-{}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(root.join("home")).unwrap();
    fs::create_dir_all(root.join("bin")).unwrap();
    root
}

fn run(program: &Path, args: &[&str], root: &Path) -> Output {
    let mut cmd = Command::new(program);
    isolated::isolate(&mut cmd, &root.join("home"));
    cmd.args(args)
        .env("PATH", root.join("bin"))
        .env_remove("PBSYNC_TOKEN")
        .output()
        .unwrap()
}

#[test]
fn pbsync_and_playbook_sync_give_the_same_output() {
    // Arrange
    let Some(pbsync) = pbsync_binary() else {
        return;
    };
    let root = scratch();
    let bin = root.join("bin");
    fs::copy(env!("CARGO_BIN_EXE_playbook"), bin.join("playbook")).unwrap();
    fs::copy(pbsync, bin.join("pbsync")).unwrap();

    for case in CASES {
        let mut through_playbook = vec!["sync"];
        through_playbook.extend(case);

        // Act
        let direct = run(&bin.join("pbsync"), case, &root);
        let via = run(&bin.join("playbook"), &through_playbook, &root);

        // Assert
        assert_eq!(direct.status.code(), via.status.code(), "{case:?}");
        assert_eq!(direct.stdout, via.stdout, "{case:?}");
        assert_eq!(direct.stderr, via.stderr, "{case:?}");
    }
}

#[test]
fn enable_without_a_url_is_a_usage_error() {
    // Arrange
    let Some(pbsync) = pbsync_binary() else {
        return;
    };
    let root = scratch();
    let bin = root.join("bin");
    fs::copy(env!("CARGO_BIN_EXE_playbook"), bin.join("playbook")).unwrap();
    fs::copy(pbsync, bin.join("pbsync")).unwrap();

    // Act
    let via = run(&bin.join("playbook"), &["sync", "enable"], &root);

    // Assert
    assert_eq!(via.status.code(), Some(2));
}
