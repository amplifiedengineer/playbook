// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr review triage` through the binary, with a fake `gh` that
//! serves a fixture from `tests/fixtures/review_rules/` and a fake `claude`
//! that counts its calls. Nothing here reaches the network or a real model.

#[path = "support/isolated.rs"]
mod isolated;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

const PR: &str = "7";
const SOURCE_RULES: &str = "source=rules";
const SOURCE_MODEL: &str = "source=model";
const SIGNAL_PREFIX: &str = "signal: ";
const MODEL_CALLS: usize = 3;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT")).join("crates/cli/tests/fixtures/review_rules")
}

fn write_script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A scratch home holding a fake `gh` and a fake `claude` in `bin/`.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// The fake `gh` serves `case`'s diff and files. `files_fail` makes the
    /// files endpoint fail, which the rules cannot work without.
    fn new(case: &Path, files_fail: bool) -> Sandbox {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "pb-review-triage-{}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        let files = if files_fail {
            "echo 'gh: not found' >&2; exit 1".to_string()
        } else {
            format!("cat '{}/files.jsonl'", case.display())
        };
        write_script(
            &root.join("bin/gh"),
            &format!(
                r#"case "$1 $2" in
  "pr diff") cat '{case}/diff.patch' ;;
  "pr view")
    if [ "$5" = "labels,author" ]; then echo '{{"labels":[],"author":{{"login":"ana"}}}}'; else echo '{{"title":"t","body":"b"}}'; fi ;;
  "api --paginate") {files} ;;
  "api /user") echo bob ;;
  *) exit 1 ;;
esac"#,
                case = case.display()
            ),
        );
        let reply = r#"{"is_error":false,"structured_output":{"review":"quick","signals":[]}}"#;
        write_script(
            &root.join("bin/claude"),
            &format!(
                "cat >/dev/null\necho x >> '{}/claude.calls'\necho '{reply}'",
                root.display()
            ),
        );
        Sandbox { root }
    }

    fn triage(&self, extra: &[&str]) -> Output {
        isolated::playbook_in(&self.root)
            .args(["pr", "review", "triage", "--pr", PR])
            .args(extra)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .output()
            .unwrap()
    }

    fn model_calls(&self) -> usize {
        fs::read_to_string(self.root.join("claude.calls")).map_or(0, |t| t.lines().count())
    }
}

fn lines(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn cases() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(fixtures())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty(), "no regression fixtures found");
    dirs
}

#[test]
fn every_regression_fixture_keeps_its_answer_and_source() {
    for case in cases() {
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        // Arrange
        let expect = fs::read_to_string(case.join("expect")).unwrap();
        let sandbox = Sandbox::new(&case, false);

        // Act
        let out = sandbox.triage(&[]);

        // Assert
        let got = lines(&out);
        assert!(out.status.success(), "{name}: exit {:?}", out.status.code());
        assert_eq!(got[..2].join("\n"), expect.trim_end(), "{name}");
        let signals = got.iter().filter(|l| l.starts_with(SIGNAL_PREFIX)).count();
        if got[1] == SOURCE_RULES {
            assert!(signals >= 1, "{name}: a rules answer names its reasons");
            assert_eq!(
                sandbox.model_calls(),
                0,
                "{name}: rules never call the model"
            );
        } else {
            assert_eq!(sandbox.model_calls(), MODEL_CALLS, "{name}");
        }
    }
}

#[test]
fn a_rules_answer_prints_one_signal_line_per_reason() {
    // Arrange
    let sandbox = Sandbox::new(&fixtures().join("docs-only-quick"), false);

    // Act
    let out = sandbox.triage(&[]);

    // Assert
    let got = lines(&out);
    assert_eq!(
        got[..3],
        ["review=quick", SOURCE_RULES, "signal: Only docs changed."]
    );
    assert!(
        got[3..].iter().all(|l| l.starts_with(SIGNAL_PREFIX)),
        "{got:?}"
    );
}

#[test]
fn the_model_flag_skips_the_rules() {
    // Arrange
    let sandbox = Sandbox::new(&fixtures().join("docs-only-quick"), false);

    // Act
    let out = sandbox.triage(&["--model"]);

    // Assert
    assert_eq!(lines(&out), ["review=quick", SOURCE_MODEL]);
    assert_eq!(sandbox.model_calls(), MODEL_CALLS);
}

#[test]
fn a_rules_failure_falls_through_to_the_model_with_a_note() {
    // Arrange
    let sandbox = Sandbox::new(&fixtures().join("docs-only-quick"), true);

    // Act
    let out = sandbox.triage(&[]);

    // Assert
    let got = lines(&out);
    assert!(out.status.success());
    assert_eq!(got[..2], ["review=quick", SOURCE_MODEL]);
    assert!(got[2].starts_with("note: rules unavailable:"), "{got:?}");
    assert_eq!(sandbox.model_calls(), MODEL_CALLS);
}

#[test]
fn a_stack_scope_without_a_stack_still_answers() {
    // Arrange
    let sandbox = Sandbox::new(&fixtures().join("docs-only-quick"), false);

    // Act
    let out = sandbox.triage(&["--scope", "stack"]);

    // Assert
    assert!(out.status.success());
    assert!(lines(&out)[0].starts_with("review="));
}
