// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook sync` against a fake `pbsync` script that records what it was
//! given. Every test uses a scratch home and copies the built `playbook` into
//! its own folder, so only the helper placed there or on the scratch PATH can
//! be found. Nothing reaches the network.

#[path = "support/isolated.rs"]
mod isolated;

use clap::CommandFactory;
use playbook::Cli;
use playbook_common::sync_commands::{FORWARDED, IN_PROCESS};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const OWN_VERSION: &str = env!("CARGO_PKG_VERSION");
const OLDER_VERSION: &str = "0.0.1";
const NOT_FOUND: &str = "pbsync helper not found; run playbook update\n";
const FAKE_OUT: &str = "fake out";
const FAKE_ERR: &str = "fake err";
const SIGTERM_EXIT: i32 = 143;
const FLAG_TAIL: [&str; 3] = ["--force", "--account", "me@example.com"];

/// What a fake `pbsync` prints and returns.
struct Fake {
    version: String,
    code: i32,
    body: String,
}

impl Fake {
    fn new(code: i32) -> Self {
        Self {
            version: OWN_VERSION.to_string(),
            code,
            body: String::new(),
        }
    }

    fn version(mut self, version: &str) -> Self {
        self.version = version.to_string();
        self
    }

    fn body(mut self, body: &str) -> Self {
        self.body = body.to_string();
        self
    }
}

struct Rig {
    root: PathBuf,
    home: PathBuf,
    beside: PathBuf,
    on_path: PathBuf,
}

impl Rig {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "playbook-sync-cli-{}-{tag}-{n}",
            playbook::testing::run_id()
        ));
        let (home, beside, on_path) = (root.join("home"), root.join("beside"), root.join("path"));
        for dir in [&home, &beside, &on_path] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::copy(env!("CARGO_BIN_EXE_playbook"), beside.join("playbook")).unwrap();
        Self {
            root: root.canonicalize().unwrap(),
            home,
            beside,
            on_path,
        }
    }

    fn argv_log(&self) -> PathBuf {
        self.root.join("argv.log")
    }

    fn env_log(&self) -> PathBuf {
        self.root.join("env.log")
    }

    fn stdin_log(&self) -> PathBuf {
        self.root.join("stdin.log")
    }

    fn started(&self) -> PathBuf {
        self.root.join("started")
    }

    fn data_dir(&self) -> PathBuf {
        playbook_data::data_dir_from(&self.home)
    }

    fn install(&self, dir: &Path, fake: &Fake) {
        let script = format!(
            "#!/bin/sh\n\
             PATH=/usr/bin:/bin\n\
             if [ \"$1\" = \"--version\" ]; then echo 'pbsync {version}'; exit 0; fi\n\
             touch '{started}'\n\
             printf '%s\\n' \"$@\" > '{argv}'\n\
             env > '{env}'\n\
             cat > '{stdin}'\n\
             {body}\n\
             echo '{FAKE_OUT}'\n\
             echo '{FAKE_ERR}' >&2\n\
             exit {code}\n",
            version = fake.version,
            started = self.started().display(),
            argv = self.argv_log().display(),
            env = self.env_log().display(),
            stdin = self.stdin_log().display(),
            body = fake.body,
            code = fake.code,
        );
        let path = dir.join("pbsync");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn fake_beside(&self, fake: &Fake) {
        self.install(&self.beside, fake);
    }

    fn fake_on_path(&self, fake: &Fake) {
        self.install(&self.on_path, fake);
    }

    fn run(&self, args: &[&str], stdin: &str) -> Output {
        let mut cmd = Command::new(self.beside.join("playbook"));
        isolated::isolate(&mut cmd, &self.home);
        let mut child = cmd
            .args(args)
            .env("PATH", &self.on_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn argv(&self) -> Vec<String> {
        fs::read_to_string(self.argv_log())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn words(name: &str) -> Vec<&str> {
    name.split(' ').collect()
}

#[test]
fn each_forwarded_name_passes_its_exact_words_and_flags() {
    // Arrange
    assert!(!FORWARDED.is_empty(), "the forwarded list is empty");
    for name in FORWARDED {
        let rig = Rig::new("forward");
        rig.fake_beside(&Fake::new(0));
        let mut args = vec!["sync"];
        args.extend(words(name));
        args.extend(FLAG_TAIL);

        // Act
        let output = rig.run(&args, "");

        // Assert
        let mut expected: Vec<&str> = words(name);
        expected.extend(FLAG_TAIL);
        assert_eq!(rig.argv(), expected, "{name}: {}", stderr(&output));
        assert_eq!(output.status.code(), Some(0), "{name}");
    }
}

#[test]
fn exit_codes_and_both_streams_pass_through_unchanged() {
    for code in [0, 1, 2] {
        // Arrange
        let rig = Rig::new("codes");
        rig.fake_beside(&Fake::new(code));

        // Act
        let output = rig.run(&["sync", "status"], "");

        // Assert
        assert_eq!(output.status.code(), Some(code));
        assert_eq!(stdout(&output), format!("{FAKE_OUT}\n"));
        assert_eq!(stderr(&output), format!("{FAKE_ERR}\n"));
    }
}

#[test]
fn stdin_and_the_environment_reach_the_helper() {
    // Arrange
    let rig = Rig::new("stdin");
    rig.fake_beside(&Fake::new(0));

    // Act
    rig.run(&["sync", "enable"], "yes\n");

    // Assert
    assert_eq!(fs::read_to_string(rig.stdin_log()).unwrap(), "yes\n");
    let env = fs::read_to_string(rig.env_log()).unwrap();
    assert!(
        env.contains(&format!("HOME={}", rig.home.display())),
        "{env}"
    );
}

#[test]
fn help_is_handed_to_the_helper() {
    // Arrange
    let rig = Rig::new("help");
    rig.fake_beside(&Fake::new(0));

    // Act
    let output = rig.run(&["sync", "push", "--help"], "");

    // Assert
    assert_eq!(rig.argv(), ["push", "--help"]);
    assert_eq!(stdout(&output), format!("{FAKE_OUT}\n"));
}

#[test]
fn the_copy_beside_the_binary_wins_over_one_on_path() {
    // Arrange
    let rig = Rig::new("order");
    rig.fake_beside(&Fake::new(0));
    rig.fake_on_path(&Fake::new(7));

    // Act
    let output = rig.run(&["sync", "status"], "");

    // Assert
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn a_helper_on_path_runs_when_none_is_beside_the_binary() {
    // Arrange
    let rig = Rig::new("path");
    rig.fake_on_path(&Fake::new(3));

    // Act
    let output = rig.run(&["sync", "status"], "");

    // Assert
    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn a_missing_helper_is_exit_1_with_one_hint_line() {
    // Arrange
    let rig = Rig::new("missing");

    // Act
    let output = rig.run(&["sync", "status"], "");

    // Assert
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr(&output), NOT_FOUND);
    assert_eq!(stdout(&output), "");
}

#[test]
fn a_usage_error_from_an_older_helper_adds_one_line_naming_both_versions() {
    // Arrange
    let rig = Rig::new("skew");
    rig.fake_beside(&Fake::new(2).version(OLDER_VERSION));

    // Act
    let output = rig.run(&["sync", "status"], "");

    // Assert
    let err = stderr(&output);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(lines.len(), 2, "{err}");
    assert_eq!(lines[0], FAKE_ERR);
    assert!(
        lines[1].contains(OLDER_VERSION) && lines[1].contains(OWN_VERSION),
        "{err}"
    );
}

#[test]
fn no_skew_hint_for_the_same_version_or_for_another_exit_code() {
    for (version, code) in [(OWN_VERSION, 2), (OLDER_VERSION, 1)] {
        // Arrange
        let rig = Rig::new("no-skew");
        rig.fake_beside(&Fake::new(code).version(version));

        // Act
        let output = rig.run(&["sync", "status"], "");

        // Assert
        assert_eq!(stderr(&output), format!("{FAKE_ERR}\n"), "{version} {code}");
    }
}

#[test]
fn config_push_writes_the_sync_files_before_the_helper_runs() {
    // Arrange
    let rig = Rig::new("push-files");
    let keys = rig.data_dir().join("sync-keys.json");
    let seen = rig.root.join("seen");
    rig.fake_beside(&Fake::new(0).body(&format!(
        "[ -f '{}' ] && touch '{}'",
        keys.display(),
        seen.display()
    )));
    assert!(!keys.exists());

    // Act
    let output = rig.run(&["sync", "config", "push"], "");

    // Assert
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        seen.exists(),
        "the helper ran before sync-keys.json existed"
    );
}

#[test]
fn config_apply_and_refresh_run_in_process_without_the_helper() {
    for args in [
        ["sync", "config", "apply", "--yes"].as_slice(),
        ["sync", "config", "refresh"].as_slice(),
    ] {
        // Arrange
        let rig = Rig::new("in-process");
        rig.fake_beside(&Fake::new(9));

        // Act
        let output = rig.run(args, "");

        // Assert
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(!rig.started().exists(), "{args:?} started the helper");
    }
}

#[cfg(unix)]
#[test]
fn a_helper_killed_by_sigterm_gives_exit_143() {
    // Arrange
    let rig = Rig::new("sigterm");
    rig.fake_beside(&Fake::new(0).body("kill -TERM $$"));

    // Act
    let output = rig.run(&["sync", "status"], "");

    // Assert
    assert_eq!(output.status.code(), Some(SIGTERM_EXIT));
}

fn leaves(cmd: &clap::Command, prefix: &str, out: &mut BTreeSet<String>) {
    let subs: Vec<&clap::Command> = cmd
        .get_subcommands()
        .filter(|sub| sub.get_name() != "help")
        .collect();
    if subs.is_empty() {
        out.insert(prefix.to_string());
    }
    for sub in subs {
        let path = if prefix.is_empty() {
            sub.get_name().to_string()
        } else {
            format!("{prefix} {}", sub.get_name())
        };
        leaves(sub, &path, out);
    }
}

#[test]
fn the_sync_leaves_are_the_forwarded_and_in_process_names() {
    // Arrange
    let root = Cli::command();
    let sync = root.find_subcommand("sync").expect("`sync` is a command");
    let mut found = BTreeSet::new();

    // Act
    leaves(sync, "", &mut found);

    // Assert
    let expected: BTreeSet<String> = FORWARDED
        .iter()
        .chain(IN_PROCESS)
        .map(|name| (*name).to_string())
        .collect();
    assert_eq!(found, expected);
}
