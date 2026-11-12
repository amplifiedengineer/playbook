// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook review suggest --scope stack` against a fake `gh` on PATH. The
//! stack comes from `tests/fixtures/pr_stack/api-bottom-merged.json` (#1
//! merged, #2 and #3 open); the per-PR files come from the test.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

/// One scratch root with a fake `gh`. It logs every call to `calls.log`, and
/// serves `files-<n>.jsonl` for `pulls/<n>/files` whatever the PR's state, so a
/// PR the binary never asks for is the binary's choice, not the fake's.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// `files` are `(pr number, path, added lines)`; `login` is who `api /user`
    /// reports.
    fn new(files: &[(u64, &str, u64)], login: &str) -> Sandbox {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "pb-suggest-stack-{}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join(".config")).unwrap();
        for (pr, path, added) in files {
            let line = format!(
                "{{\"path\":\"{path}\",\"added\":{added},\"removed\":0,\"renamed\":false}}\n"
            );
            fs::write(root.join(format!("files-{pr}.jsonl")), line).unwrap();
        }
        fs::write(root.join("login"), login).unwrap();
        let fixtures =
            PathBuf::from(env!("PLAYBOOK_REPO_ROOT")).join("crates/cli/tests/fixtures/pr_stack");
        let script = format!(
            r#"#!/bin/sh
ROOT='{root}'
FX='{fx}'
echo "$*" >> "$ROOT/calls.log"
case "$1 $2" in
  "repo view") cat "$FX/repo.json" ;;
  "api graphql") cat "$FX/api-bottom-merged.json" ;;
  "api --paginate")
    n="${{3#*pulls/}}"; n="${{n%%/*}}"
    if [ -f "$ROOT/files-$n.jsonl" ]; then cat "$ROOT/files-$n.jsonl"; else echo "no files for $n" >&2; exit 1; fi ;;
  "api /user") cat "$ROOT/login" ;;
  "pr view")
    if [ "$5" = "labels,author" ]; then echo '{{"labels":[],"author":{{"login":"ana"}}}}'; else cat "$FX/pr-$3.json"; fi ;;
  "pr list") echo '[]' ;;
  *) exit 1 ;;
esac
"#,
            root = root.display(),
            fx = fixtures.display()
        );
        let gh = root.join("bin/gh");
        fs::write(&gh, script).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        Sandbox { root }
    }

    fn playbook(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join(".config"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .current_dir(&self.root)
            .output()
            .unwrap()
    }

    fn suggest(&self, pr: &str, scope: &str) -> Output {
        self.playbook(&["review", "suggest", pr, "--scope", scope, "--json"])
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.root.join("calls.log")).unwrap_or_default()
    }

    fn fetched(&self, pr: u64) -> bool {
        self.calls().contains(&format!("pulls/{pr}/files"))
    }
}

fn json(o: &Output) -> serde_json::Value {
    let out = String::from_utf8_lossy(&o.stdout);
    serde_json::from_str(&out).unwrap_or_else(|e| {
        panic!(
            "not json ({e}): {out} {}",
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// #1 is a large risky change that must never count; #2 and #3 are small.
fn small_stack() -> Sandbox {
    Sandbox::new(
        &[
            (1, ".github/workflows/ci.yml", 500),
            (2, "crates/cli/src/two.rs", 20),
            (3, "crates/cli/src/three.rs", 30),
        ],
        "ana",
    )
}

#[test]
fn the_stack_scope_measures_the_open_prs_and_never_fetches_the_merged_one() {
    // Arrange
    let sb = small_stack();
    // Act
    let o = sb.suggest("3", "stack");
    // Assert
    assert!(o.status.success(), "{}", stderr(&o));
    let v = json(&o);
    assert_eq!(v["scope"], "stack");
    assert_eq!(v["stack"]["open"], serde_json::json!([2, 3]));
    assert_eq!(v["stack"]["merged"], serde_json::json!([1]));
    assert_eq!(v["signals"]["lines"], 50);
    assert!(sb.fetched(2) && sb.fetched(3), "{}", sb.calls());
    assert!(!sb.fetched(1), "{}", sb.calls());
}

#[test]
fn the_this_pr_scope_measures_the_one_pr_and_has_no_stack_key() {
    // Arrange
    let sb = small_stack();
    // Act
    let o = sb.suggest("3", "this-pr");
    // Assert
    assert!(o.status.success(), "{}", stderr(&o));
    let v = json(&o);
    assert_eq!(v["scope"], "this-pr");
    assert_eq!(v["signals"]["lines"], 30);
    assert!(v.get("stack").is_none(), "{v}");
}

#[test]
fn two_quick_prs_make_a_deep_stack() {
    // Arrange
    let sb = Sandbox::new(
        &[
            (2, "crates/cli/src/widget.rs", 50),
            (3, "crates/cli/src/widget.rs", 50),
        ],
        "bob",
    );
    // Act
    let this_pr = json(&sb.suggest("3", "this-pr"));
    let stack = json(&sb.suggest("3", "stack"));
    // Assert
    assert_eq!(
        (
            this_pr["recommendation"].as_str(),
            this_pr["confidence"].as_str()
        ),
        (Some("quick"), Some("high"))
    );
    assert_eq!(stack["recommendation"], "deep");
    assert_eq!(stack["signals"]["lines"], 100);
}

#[test]
fn a_merged_given_pr_stops_the_review_and_names_the_pr() {
    // Arrange
    let sb = small_stack();
    // Act
    let o = sb.suggest("1", "stack");
    // Assert
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("PR #1 is merged"), "{}", stderr(&o));
    assert!(!sb.fetched(1), "{}", sb.calls());
}

#[test]
fn one_failed_fetch_fails_the_whole_call() {
    // Arrange
    let sb = Sandbox::new(&[(2, "crates/cli/src/two.rs", 20)], "ana");
    // Act
    let o = sb.suggest("3", "stack");
    // Assert
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("error:"), "{}", stderr(&o));
    assert!(o.stdout.is_empty(), "partial score printed");
}
