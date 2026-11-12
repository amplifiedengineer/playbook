// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `pbsync` command line.

use std::io::IsTerminal;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use pbsync::cli::{run, Context};

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or_default()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = |name: &str| std::env::var(name).ok();
    let mut ctx = Context {
        data_dir: playbook_data::data_dir(),
        env: &env,
        now: &unix_now,
        is_terminal: &|| std::io::stderr().is_terminal(),
        input: &mut std::io::stdin().lock(),
        out: &mut std::io::stdout(),
        err: &mut std::io::stderr(),
    };
    ExitCode::from(run(&args, &mut ctx))
}
