// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Every agent-facing command with a `--json` flag prints the shared
//! envelope and exits 0, even when it fails. Each row runs one command on a
//! failing fixture: an empty directory that is not a git repo, no stdin, a
//! scratch `HOME`, and a `gh` that always fails.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(20);

/// A command line and the envelope `code` its failing fixture must give.
struct Row {
    args: &'static [&'static str],
    code: i64,
}

/// Add a row when a command gains the envelope.
const ROWS: &[Row] = &[
    Row {
        args: &["pr", "prepare", "--json"],
        code: 1,
    },
    Row {
        args: &["commit", "all", "--json"],
        code: 1,
    },
    Row {
        args: &["commit", "run", "-A", "--json"],
        code: 1,
    },
    Row {
        args: &["pr", "status", "1", "--json"],
        code: 5,
    },
];

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "playbook-json-envelope-{tag}-{}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// A `gh` on `PATH` that fails every call.
fn fake_gh(bin_dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let gh = bin_dir.join("gh");
    fs::write(&gh, "#!/bin/sh\necho 'gh: not logged in' >&2\nexit 1\n").expect("write fake gh");
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).expect("chmod fake gh");
}

fn run(row: &Row) -> (Option<i32>, String) {
    let root = scratch(&row.args.join("-"));
    let (work, home, bin) = (root.join("work"), root.join("home"), root.join("bin"));
    for d in [&work, &home, &bin] {
        fs::create_dir_all(d).expect("create fixture dir");
    }
    fake_gh(&bin);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(row.args)
        .current_dir(&work)
        .env("HOME", &home)
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn playbook");
    let start = Instant::now();
    while child.try_wait().expect("poll playbook").is_none() {
        if start.elapsed() > TIMEOUT {
            let _ = child.kill();
            panic!("{:?} ran past {TIMEOUT:?}", row.args);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().expect("collect output");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// Why a run breaks the envelope contract, or `None` when it holds.
fn failure(row: &Row, status: Option<i32>, stdout: &str) -> Option<String> {
    let parsed: Option<Value> = serde_json::from_str(stdout).ok();
    let Some(obj) = parsed.as_ref().and_then(Value::as_object) else {
        return Some(format!(
            "{:?}: stdout is not a JSON object: {stdout:?}",
            row.args
        ));
    };
    let mut problems = Vec::new();
    for key in ["ok", "code", "meaning", "error"] {
        if !obj.contains_key(key) {
            problems.push(format!("no `{key}` in {stdout:?}"));
        }
    }
    if status != Some(0) {
        problems.push(format!("exit {status:?}, want 0"));
    }
    if obj.get("ok") != Some(&Value::Bool(false)) {
        problems.push("`ok` is not false".to_string());
    }
    if obj.get("code").and_then(Value::as_i64) != Some(row.code) {
        problems.push(format!("code {:?}, want {}", obj.get("code"), row.code));
    }
    (!problems.is_empty()).then(|| format!("{:?}: {}", row.args, problems.join("; ")))
}

#[test]
fn a_failing_command_with_json_prints_the_envelope_and_exits_zero() {
    let problems: Vec<String> = ROWS
        .iter()
        .filter_map(|row| {
            // Arrange and Act
            let (status, stdout) = run(row);

            // Assert
            failure(row, status, &stdout)
        })
        .collect();
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn the_row_check_fails_a_non_zero_exit_with_empty_stdout() {
    // Arrange
    let row = Row {
        args: &["any", "--json"],
        code: 1,
    };

    // Act
    let reason = failure(&row, Some(2), "");

    // Assert
    assert!(reason.is_some_and(|r| r.contains("not a JSON object")));
}
