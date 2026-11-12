// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook sync`: the entry point for usage sync. Most names run the
//! `pbsync` helper with the same words and flags, so `playbook` needs no HTTP
//! client or second SQLite engine. `config apply` and `config refresh` run in
//! process, since they change the config store.

use crate::{SyncCommand, SyncConfigCommand};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const PBSYNC_BINARY: &str = "pbsync";
const VERSION_FLAG: &str = "--version";
const VERSION_PREFIX: &str = "pbsync ";
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_FAILED: i32 = 1;
const EXIT_USAGE: i32 = 2;
/// A process killed by a signal exits with 128 plus its number in a shell.
const SIGNAL_EXIT_BASE: i32 = 128;
const NOT_FOUND: &str = "pbsync helper not found; run playbook update";

/// The `pbsync` helper: beside the running binary first, then the first
/// absolute `PATH` folder that holds one.
pub fn helper_path(path_var: Option<&OsStr>) -> Option<PathBuf> {
    let name = format!("{PBSYNC_BINARY}{}", std::env::consts::EXE_SUFFIX);
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&name)))
        .filter(|path| path.is_file());
    beside.or_else(|| {
        std::env::split_paths(path_var?)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(&name))
            .find(|path| path.is_file())
    })
}

/// Runs one `playbook sync` command and returns its exit code.
pub fn run(sub: SyncCommand) -> i32 {
    match sub {
        SyncCommand::Enable { args } => forward("enable", args),
        SyncCommand::Disable { args } => forward("disable", args),
        SyncCommand::Status { args } => forward("status", args),
        SyncCommand::Push { args } => forward("push", args),
        SyncCommand::Pull { args } => forward("pull", args),
        SyncCommand::Run { args } => forward("run", args),
        SyncCommand::Purge { args } => forward("purge", args),
        SyncCommand::Config { sub, args } => match sub {
            None => forward("config", args),
            Some(SyncConfigCommand::Push { args }) => config_push(args),
            Some(SyncConfigCommand::Pull { args }) => forward("config pull", args),
            Some(SyncConfigCommand::Apply { yes, .. }) => config_apply(yes),
            Some(SyncConfigCommand::Refresh { .. }) => config_refresh(),
        },
    }
}

fn refresh_files() -> Result<(), crate::config::sync_files::SyncFilesError> {
    crate::config::sync_files::write_sync_files(&crate::common::home_dir())
}

/// `pbsync` reads the key list and values files, so they are refreshed first.
fn config_push(args: Vec<String>) -> i32 {
    match refresh_files() {
        Ok(()) => forward("config push", args),
        Err(err) => {
            eprintln!("sync config push: {err}");
            EXIT_FAILED
        }
    }
}

fn config_refresh() -> i32 {
    match refresh_files() {
        Ok(()) => {
            println!("config sync-files: key list and values refreshed");
            0
        }
        Err(err) => {
            eprintln!("config sync-files: {err}");
            EXIT_FAILED
        }
    }
}

fn config_apply(yes: bool) -> i32 {
    let home = crate::common::home_dir();
    for warning in crate::config::adopt_late_config(&home) {
        eprintln!("config: warning: {warning}");
    }
    let slug = crate::common::repo_slug();
    let repo_slug = (!slug.is_empty()).then_some(slug.as_str());
    let stdin = std::io::stdin();
    let (mut input, mut out, mut err) = (stdin.lock(), std::io::stdout(), std::io::stderr());
    let mut streams = crate::config::proposal::Streams {
        input: &mut input,
        out: &mut out,
        err: &mut err,
    };
    match crate::config::proposal::run(&home, repo_slug, yes, &mut streams) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("config apply-proposal: {err}");
            EXIT_FAILED
        }
    }
}

/// Runs the helper with `name` and `args`, inheriting stdin, stdout and
/// stderr, and returns its exit code.
fn forward(name: &str, args: Vec<String>) -> i32 {
    let path_var = std::env::var_os("PATH");
    let Some(helper) = helper_path(path_var.as_deref()) else {
        eprintln!("{NOT_FOUND}");
        return EXIT_FAILED;
    };
    let words = name.split(' ').map(str::to_string).chain(args);
    let status = match Command::new(&helper).args(words).status() {
        Ok(status) => status,
        Err(err) => {
            eprintln!("cannot start {}: {}", helper.display(), err.kind());
            return EXIT_FAILED;
        }
    };
    let code = exit_code(status);
    if code == EXIT_USAGE {
        if let Some(line) = skew_line(&helper) {
            eprintln!("{line}");
        }
    }
    code
}

#[cfg(unix)]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|signal| SIGNAL_EXIT_BASE + signal))
        .unwrap_or(EXIT_FAILED)
}

#[cfg(not(unix))]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(EXIT_FAILED)
}

/// The hint for a helper whose version differs from this binary's, which a
/// failed helper swap during an upgrade can leave behind.
fn skew_line(helper: &Path) -> Option<String> {
    let output =
        crate::common::run_with_timeout(Command::new(helper).arg(VERSION_FLAG), VERSION_TIMEOUT)?;
    let text = String::from_utf8_lossy(&output.stdout);
    let found = text.trim().strip_prefix(VERSION_PREFIX)?;
    (found != env!("CARGO_PKG_VERSION")).then(|| {
        format!(
            "pbsync {found} does not match playbook {}; run playbook update",
            env!("CARGO_PKG_VERSION")
        )
    })
}
