// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook commit`: the git work of `/playbook:commit`. `prepare`
//! stages, formats and prints what the message drafter needs. `run` commits
//! with a sign-off and signing decision, rebases when behind, and pushes
//! without ever escalating a rejected push to a force. `run -A` does the three
//! in one call with a message drafted from the files (`all` is its legacy
//! name).

pub mod all;
pub mod prepare;
pub mod run;
pub mod typecheck;

use crate::common::proc::run_with_timeout;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

/// Longest a local read or index write (`add -A` on a big tree) may take.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest a commit (with its hooks and signing), fetch, rebase or push may
/// take. Bounded so a stuck network or credential prompt cannot hang forever.
const GIT_LONG_TIMEOUT: Duration = Duration::from_secs(600);

fn git_with(dir: &Path, args: &[&str], timeout: Duration) -> Option<Output> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0");
    run_with_timeout(&mut command, timeout)
}

/// Runs `git` in `dir`, giving up after [`GIT_TIMEOUT`].
pub(crate) fn git_in(dir: &Path, args: &[&str]) -> Option<Output> {
    git_with(dir, args, GIT_TIMEOUT)
}

/// Like [`git_in`] for commit, fetch, rebase and push, which can run hooks or
/// wait on the network.
pub(crate) fn git_in_long(dir: &Path, args: &[&str]) -> Option<Output> {
    git_with(dir, args, GIT_LONG_TIMEOUT)
}

/// Trimmed stdout of a git call that succeeded, else `None`.
pub(crate) fn git_text(dir: &Path, args: &[&str]) -> Option<String> {
    let out = git_in(dir, args)?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub(crate) fn git_success(dir: &Path, args: &[&str]) -> bool {
    git_in(dir, args).is_some_and(|o| o.status.success())
}

/// The current branch name, or `HEAD` when detached.
pub(crate) fn current_branch(dir: &Path) -> String {
    if let Some(name) = crate::common::gitfacts::abbrev_head(dir) {
        return name;
    }
    git_text(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default()
}
