// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook memory claude import` end to end: copies Claude Code's auto
//! memory into playbook memory and leaves Claude Code's directory untouched.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

static N: AtomicU64 = AtomicU64::new(0);

struct Box_ {
    home: PathBuf,
    from: PathBuf,
}

impl Box_ {
    fn new(tag: &str) -> Self {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let home =
            std::env::temp_dir().join(format!("pb-mi-{tag}-{}-{n}", playbook::testing::run_id()));
        let _ = fs::remove_dir_all(&home);
        let from = home.join(".claude/projects/p/memory");
        fs::create_dir_all(&from).unwrap();
        fs::write(from.join("MEMORY.md"), "- [x](x.md)\n").unwrap();
        fs::write(from.join("x.md"), "# X\nUse tabs.\n").unwrap();
        fs::write(
            from.join("y.md"),
            "---\nname: Y\ndescription: why y\ntype: feedback\n---\nBody.\n",
        )
        .unwrap();
        Self { home, from }
    }

    fn run(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["memory", "claude", "import", "--from"])
            .arg(&self.from)
            .args(extra)
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("run playbook")
    }

    fn snapshot(&self) -> Vec<(String, Vec<u8>, SystemTime)> {
        let mut v: Vec<_> = fs::read_dir(&self.from)
            .unwrap()
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    fs::read(e.path()).unwrap(),
                    e.metadata().unwrap().modified().unwrap(),
                )
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    fn facts(&self) -> Vec<String> {
        let mut out = Vec::new();
        fn walk(dir: &Path, out: &mut Vec<String>) {
            for e in fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "md") {
                    out.push(p.file_name().unwrap().to_string_lossy().into_owned());
                }
            }
        }
        walk(&self.home.join(".config/playbook/memory"), &mut out);
        out.sort();
        out
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn an_import_copies_the_notes_and_leaves_claude_memory_untouched() {
    let b = Box_::new("copy");
    let before = b.snapshot();
    let out = b.run(&[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(b.facts(), vec!["x.md".to_string(), "y.md".to_string()]);
    assert_eq!(b.snapshot(), before);
}

#[test]
fn a_second_run_copies_nothing() {
    let b = Box_::new("again");
    b.run(&[]);
    let out = b.run(&[]);
    assert!(text(&out).contains("copied 0"), "{}", text(&out));
    assert_eq!(b.facts().len(), 2);
}

#[test]
fn a_dry_run_writes_nothing() {
    let b = Box_::new("dry");
    let out = b.run(&["--dry-run"]);
    assert!(text(&out).contains("would copy 2"), "{}", text(&out));
    assert!(b.facts().is_empty());
    assert!(!b.home.join(".config/playbook/state").exists());
}
