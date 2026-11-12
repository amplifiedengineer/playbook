// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Build output in worktrees: the ecosystem table and the detection of folders
//! that are safe to call build output. Detection only reads; nothing here
//! removes anything.

use std::fs;
use std::path::{Path, PathBuf};

use std::time::Duration;

use super::guard::checked_build_output_remove;
use super::lock::{with_run_lock, NoHook, RunGuard, RunOutcome};
use super::space::{under_pressure, RealProbe, SpaceProbe};
use super::stores::mtime_secs;
use super::tombstone::Env;
use super::{Action, Clock, Item, Limits, Registry, Store};
use crate::common::git;

/// How deep below a worktree root the scan looks for a build folder.
pub const SCAN_MAX_DEPTH: usize = 4;

/// One language ecosystem: files that show a project uses it, and the folders
/// it generates. A marker starting with `*` matches a file name suffix.
pub struct Ecosystem {
    pub name: &'static str,
    pub markers: &'static [&'static str],
    pub folders: &'static [&'static str],
}

/// The v1 table. `dist/`, `build/` and `bin/` are left out because they often
/// hold release artifacts or local tools that cannot be regenerated. Adding a
/// row and a fixture test is all a new ecosystem takes.
pub const TABLE: &[Ecosystem] = &[
    Ecosystem {
        name: "Rust",
        markers: &["Cargo.toml"],
        folders: &["target"],
    },
    Ecosystem {
        name: "Node",
        markers: &["package.json"],
        folders: &["node_modules", ".next", ".turbo"],
    },
    Ecosystem {
        name: "Python",
        markers: &["pyproject.toml", "setup.py", "requirements.txt"],
        folders: &[
            "__pycache__",
            ".pytest_cache",
            ".mypy_cache",
            ".ruff_cache",
            ".tox",
        ],
    },
    Ecosystem {
        name: "Java and Kotlin",
        markers: &["pom.xml", "build.gradle", "build.gradle.kts"],
        folders: &["target", ".gradle"],
    },
    Ecosystem {
        name: ".NET",
        markers: &["*.csproj", "*.sln"],
        folders: &["obj"],
    },
    Ecosystem {
        name: "Swift and Zig",
        markers: &["Package.swift", "build.zig"],
        folders: &[".build", ".zig-cache"],
    },
    Ecosystem {
        name: "Go",
        markers: &["go.mod"],
        folders: &[],
    },
];

/// Virtual environment folders. They hold installed packages and interpreters
/// that are costly to rebuild, so the scan never lists one and never looks
/// inside one, even when it sits in an ignored path next to a Python marker.
const VENV_NAMES: &[&str] = &[".venv", "venv"];

/// A build folder that passed every detection check.
#[derive(Debug, PartialEq, Eq)]
pub struct Found {
    pub folder: PathBuf,
    pub ecosystem: &'static str,
}

/// Whether `name` is `entry` byte for byte, so a `Target` folder on a case
/// insensitive volume is never mistaken for `target`.
pub fn matcher_is_exact(entry: &str, name: &str) -> bool {
    entry == name
}

fn marker_present(dir: &Path, marker: &str) -> bool {
    match marker.strip_prefix('*') {
        Some(suffix) => fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| {
                entry.file_name().to_string_lossy().ends_with(suffix)
                    && entry.file_type().is_ok_and(|kind| kind.is_file())
            }),
        None => fs::symlink_metadata(dir.join(marker)).is_ok_and(|meta| meta.is_file()),
    }
}

/// The ecosystem whose marker sits in `folder`'s parent or an ancestor inside
/// `worktree` and which lists the folder's name.
fn ecosystem_of(worktree: &Path, folder: &Path) -> Option<&'static Ecosystem> {
    let name = folder.file_name()?.to_str()?;
    TABLE
        .iter()
        .filter(|eco| {
            eco.folders
                .iter()
                .any(|entry| matcher_is_exact(entry, name))
        })
        .find(|eco| {
            folder
                .ancestors()
                .skip(1)
                .take_while(|dir| dir.starts_with(worktree))
                .any(|dir| eco.markers.iter().any(|marker| marker_present(dir, marker)))
        })
}

fn is_ignored(worktree: &Path, folder: &Path) -> bool {
    let rel = folder.strip_prefix(worktree).unwrap_or(folder);
    let Some(rel) = rel.to_str() else {
        return false;
    };
    git::run(
        Some(worktree),
        &["check-ignore", "-q", "--", rel],
        git::SLOW_TIMEOUT,
    )
    .is_some_and(|out| out.status.success())
}

/// Whether git tracks anything at or below `folder`. A submodule entry counts.
/// Unknown counts as tracked, the safe answer.
fn has_tracked_files(worktree: &Path, folder: &Path) -> bool {
    let rel = folder.strip_prefix(worktree).unwrap_or(folder);
    let Some(rel) = rel.to_str() else {
        return true;
    };
    match git::run(Some(worktree), &["ls-files", "--", rel], git::SLOW_TIMEOUT) {
        Some(out) if out.status.success() => !out.stdout.is_empty(),
        _ => true,
    }
}

/// Every build folder under `worktree`, down to [`SCAN_MAX_DEPTH`] levels. It
/// never enters `.git`, a virtual environment or a folder it matched, never
/// follows a symlink, and skips a folder that holds its own `.git`.
pub fn detect(worktree: &Path) -> Vec<Found> {
    let mut found = Vec::new();
    scan_dir(worktree, worktree, 1, &mut found);
    found.sort_by(|a, b| a.folder.cmp(&b.folder));
    found
}

/// A folder named like a virtual environment, or one that holds the
/// `pyvenv.cfg` file every Python virtual environment carries.
fn is_virtualenv(dir: &Path) -> bool {
    let named = dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| VENV_NAMES.contains(&name));
    named || fs::symlink_metadata(dir.join("pyvenv.cfg")).is_ok()
}

fn scan_dir(worktree: &Path, dir: &Path, depth: usize, found: &mut Vec<Found>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if is_virtualenv(&path) {
            continue;
        }
        if let Some(eco) = ecosystem_of(worktree, &path) {
            let holds_repo = fs::symlink_metadata(path.join(".git")).is_ok();
            if !holds_repo && is_ignored(worktree, &path) && !has_tracked_files(worktree, &path) {
                found.push(Found {
                    folder: path,
                    ecosystem: eco.name,
                });
            }
        } else if depth < SCAN_MAX_DEPTH {
            scan_dir(worktree, &path, depth + 1, found);
        }
    }
}

/// Total size of the files below `path`, without following symlinks.
fn tree_bytes(path: &Path) -> (u64, i64) {
    let mut bytes = 0;
    let mut newest = 0;
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            bytes += meta.len();
            newest = newest.max(mtime_secs(&meta));
            if meta.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    (bytes, newest)
}

/// The build folders of the secondary worktrees of the repo that holds the
/// current directory. Report only: a primary checkout is never listed.
pub fn scan(_store: &Store, _now: i64) -> Vec<Item> {
    let Ok(repo) = std::env::current_dir() else {
        return Vec::new();
    };
    let Ok(worktrees) = crate::worktree::list_worktrees(&repo) else {
        return Vec::new();
    };
    worktrees
        .iter()
        .skip(1)
        .flat_map(|entry| detect(&entry.path))
        .map(|found| {
            let (bytes, newest_mtime) = tree_bytes(&found.folder);
            Item {
                path: found.folder,
                bytes,
                newest_mtime,
                also: Vec::new(),
                due: None,
            }
        })
        .collect()
}

/// How many levels below a build folder count when judging whether it is in
/// use. Cargo writes `target/debug/deps/*` at depth 3.
pub const ACTIVITY_WALK_DEPTH: usize = 4;
/// A tree with more files than this outside the build folders counts as active.
const IDLE_WALK_MAX_ENTRIES: usize = 20_000;
const SECS_PER_DAY: i64 = 86_400;

/// What the rules ask the machine, injected so tests set the answers.
pub struct Probes<'a> {
    pub space: &'a dyn SpaceProbe,
    /// Whether a process has the folder open.
    pub in_use: &'a dyn Fn(&Path) -> bool,
}

/// A build folder the rules say can go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub worktree: PathBuf,
    pub folder: PathBuf,
    /// Seconds since the newest file outside the build folders changed.
    pub idle_secs: i64,
    /// Due only because disk space is low, not because of the idle limit.
    pub pressure_only: bool,
}

/// The newest file change in `worktree` outside `.git` and `skip`. `None` when
/// the tree is too big to judge, which counts as active.
fn newest_file_change(worktree: &Path, skip: &[&Path]) -> Option<i64> {
    let mut newest = 0;
    let mut seen = 0;
    let mut pending = vec![worktree.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            if entry.file_name() == ".git" || skip.contains(&path.as_path()) {
                continue;
            }
            let meta = fs::symlink_metadata(&path).ok()?;
            if meta.is_dir() {
                pending.push(path);
                continue;
            }
            seen += 1;
            if seen > IDLE_WALK_MAX_ENTRIES {
                return None;
            }
            newest = newest.max(mtime_secs(&meta));
        }
    }
    Some(newest)
}

/// Whether any entry within [`ACTIVITY_WALK_DEPTH`] levels of `folder` changed
/// less than `active_secs` ago. The walk has no entry cap, so a large folder
/// is still judged.
fn changed_recently(folder: &Path, now: i64, active_secs: i64) -> bool {
    let mut pending = vec![(folder.to_path_buf(), 1)];
    while let Some((dir, depth)) = pending.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if now - mtime_secs(&meta) < active_secs {
                return true;
            }
            if meta.is_dir() && depth < ACTIVITY_WALK_DEPTH {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    false
}

fn pressure_at(worktree: &Path, limits: &Limits, probes: &Probes) -> bool {
    probes
        .space
        .space(worktree)
        .is_ok_and(|space| under_pressure(&space, limits.warn_gib, limits.warn_percent))
}

/// The build folders of `worktrees` that the rules allow to go now, oldest
/// idle first. A folder is kept while something outside it changed within the
/// idle limit (the pressure floor under low space), while an entry in it
/// changed within the active window, or while a process has it open.
pub fn plan(worktrees: &[PathBuf], limits: &Limits, now: i64, probes: &Probes) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let active_secs = i64::from(limits.active_minutes) * 60;
    for worktree in worktrees {
        let found = detect(worktree);
        let skip: Vec<&Path> = found.iter().map(|f| f.folder.as_path()).collect();
        let Some(newest) = (!found.is_empty())
            .then(|| newest_file_change(worktree, &skip))
            .flatten()
        else {
            continue;
        };
        let idle_secs = now - newest;
        let pressured = pressure_at(worktree, limits, probes);
        let days = if pressured {
            limits.pressure_idle_days
        } else {
            limits.idle_days
        };
        if idle_secs < i64::from(days) * SECS_PER_DAY {
            continue;
        }
        for folder in found.iter().map(|f| &f.folder) {
            if changed_recently(folder, now, active_secs) || (probes.in_use)(folder) {
                continue;
            }
            candidates.push(Candidate {
                worktree: worktree.clone(),
                folder: folder.clone(),
                idle_secs,
                pressure_only: idle_secs < i64::from(limits.idle_days) * SECS_PER_DAY,
            });
        }
    }
    candidates.sort_by(|a, b| b.idle_secs.cmp(&a.idle_secs).then(a.folder.cmp(&b.folder)));
    candidates
}

/// What a run removed and what it could not.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub folders: Vec<PathBuf>,
    pub failures: Vec<String>,
}

/// Removes the candidates through the guard, oldest first. A folder that is
/// only due because space is low is skipped once space is no longer low.
pub fn remove_all(
    guard: &RunGuard,
    candidates: &[Candidate],
    limits: &Limits,
    probes: &Probes,
) -> Removed {
    let mut out = Removed::default();
    for candidate in candidates {
        if candidate.pressure_only && !pressure_at(&candidate.worktree, limits, probes) {
            continue;
        }
        match checked_build_output_remove(guard, &candidate.worktree, &candidate.folder) {
            Ok(()) => out.folders.push(candidate.folder.clone()),
            Err(err) => out
                .failures
                .push(format!("{}: {err}", candidate.folder.display())),
        }
        let _ = guard.touch();
    }
    out
}

/// The secondary worktrees of the repo that holds the current directory, as
/// `(created by playbook, hand made)`. The primary checkout is in neither.
pub fn current_repo_worktrees(home: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let Ok(repo) = std::env::current_dir() else {
        return Default::default();
    };
    let Ok(entries) = crate::worktree::list_worktrees(&repo) else {
        return Default::default();
    };
    split_worktrees(
        entries.into_iter().map(|entry| entry.path).collect(),
        &|path| crate::worktree::classify(path, home) != crate::worktree::Convention::Unmanaged,
    )
}

/// Drops the primary checkout, which `git worktree list` names first, and
/// splits the rest into those `is_managed` accepts and the others.
fn split_worktrees(
    listed: Vec<PathBuf>,
    is_managed: &dyn Fn(&Path) -> bool,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    listed
        .into_iter()
        .skip(1)
        .partition(|path| is_managed(path))
}

/// Whether a process has `folder` open, per `lsof`. A machine without `lsof`
/// reports not in use, and the activity rule still protects live builds.
pub fn lsof_in_use(folder: &Path) -> bool {
    std::process::Command::new("lsof")
        .args(["-t", "+d"])
        .arg(folder)
        .output()
        .is_ok_and(|out| out.status.success() && !out.stdout.is_empty())
}

/// The daily sweep's share: build output in the worktrees playbook created.
pub fn sweep(env: &Env, limits: &Limits) -> Removed {
    let (managed, _) = current_repo_worktrees(&crate::common::home_dir());
    let probes = Probes {
        space: &RealProbe,
        in_use: &lsof_in_use,
    };
    let candidates = plan(&managed, limits, env.now(), &probes);
    remove_all(env.guard, &candidates, limits, &probes)
}

/// What `--build-output` is given to talk to the person.
pub struct Terminal<'a> {
    pub is_terminal: bool,
    /// Shows the question and returns whether the answer was yes.
    pub confirm: &'a mut dyn FnMut(&str) -> bool,
}

/// `playbook gc --build-output`: lists the build output of the current repo's
/// hand made worktrees that the idle and activity rules allow, asks, and only
/// then removes. Always asks, whatever else was passed. Returns the text to
/// print and the exit code.
pub fn flag_command(
    (registry, clock): (&Registry, &dyn Clock),
    hand_made: &[PathBuf],
    limits: &Limits,
    (probes, terminal): (&Probes, &mut Terminal),
) -> (String, i32) {
    if !terminal.is_terminal {
        return (
            "gc: --build-output asks before it deletes, so it needs a terminal; nothing was deleted\n"
                .into(),
            1,
        );
    }
    let candidates = plan(hand_made, limits, clock.now(), probes);
    if candidates.is_empty() {
        return ("No build output to delete.\n".into(), 0);
    }
    let mut question = String::from("Build output in hand made worktrees:\n");
    for c in &candidates {
        question.push_str(&format!("  {}\n", c.folder.display()));
    }
    question.push_str("Delete these folders? [y/N] ");
    if !(terminal.confirm)(&question) {
        return ("Nothing was deleted.\n".into(), 0);
    }
    let ran = with_run_lock(registry.gc_dir(), Duration::ZERO, clock, &NoHook, |guard| {
        remove_all(guard, &candidates, limits, probes)
    });
    match ran {
        Ok(RunOutcome::Ran(done)) => {
            let mut text = format!("Deleted {} build folders.\n", done.folders.len());
            for failure in &done.failures {
                text.push_str(&format!("needs attention: {failure}\n"));
            }
            (text, i32::from(!done.failures.is_empty()))
        }
        Ok(RunOutcome::AlreadyRunning) => ("gc already running; nothing was changed\n".into(), 1),
        Err(err) => (format!("gc: {err}\n"), 1),
    }
}

/// The build output store. It has no root: a removal names the worktree it
/// works in, so no run wide root reaches into one. The scan only lists; the
/// run removes through [`sweep`].
pub fn store() -> Store {
    Store {
        name: "build output",
        roots: Vec::new(),
        scan,
        action: Action::Delete,
        limits: Limits::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A git repo whose ignore file lists `ignore`, with the given files.
    fn repo(tag: &str, ignore: &str, files: &[&str]) -> PathBuf {
        let dir = crate::common::test_support::scratch_dir(tag);
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(dir).unwrap();
        git_in(&dir, &["init", "-q", "-b", "main"]);
        fs::write(dir.join(".gitignore"), ignore).unwrap();
        for file in files {
            put(&dir, file);
        }
        dir
    }

    fn put(dir: &Path, rel: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn folders(dir: &Path) -> Vec<String> {
        detect(dir)
            .iter()
            .map(|f| {
                f.folder
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn matcher_is_case_exact() {
        assert!(!matcher_is_exact("target", "Target"));
        assert!(matcher_is_exact("target", "target"));
    }

    #[test]
    fn fixture_rust() {
        let dir = repo("bo-rust", "target/\n", &["Cargo.toml", "target/debug/x"]);
        assert_eq!(folders(&dir), ["target"]);
    }

    #[test]
    fn fixture_node() {
        let dir = repo(
            "bo-node",
            "node_modules/\n.next/\n",
            &["package.json", "node_modules/a/x", ".next/y"],
        );
        assert_eq!(folders(&dir), [".next", "node_modules"]);
    }

    #[test]
    fn fixture_python() {
        let dir = repo(
            "bo-python",
            "__pycache__/\n",
            &["pyproject.toml", "src/pkg/__pycache__/x.pyc"],
        );
        assert_eq!(folders(&dir), ["src/pkg/__pycache__"]);
    }

    #[test]
    fn fixture_java() {
        let dir = repo(
            "bo-java",
            "target/\n.gradle/\n",
            &["pom.xml", "target/c", ".gradle/g"],
        );
        assert_eq!(folders(&dir), [".gradle", "target"]);
    }

    #[test]
    fn fixture_dotnet() {
        let dir = repo(
            "bo-dotnet",
            "obj/\nbin/\n",
            &["App.csproj", "obj/x", "bin/y"],
        );
        assert_eq!(folders(&dir), ["obj"]);
    }

    #[test]
    fn fixture_swift_zig() {
        let dir = repo(
            "bo-swift",
            ".build/\n.zig-cache/\n",
            &["Package.swift", ".build/x", ".zig-cache/y"],
        );
        assert_eq!(folders(&dir), [".build", ".zig-cache"]);
    }

    #[test]
    fn fixture_go_matches_nothing() {
        let dir = repo("bo-go", "vendor/\nbin/\n", &["go.mod", "vendor/x", "bin/y"]);
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn tracked_folder_is_protected() {
        let dir = repo("bo-tracked", "target/\n", &["Cargo.toml", "target/kept"]);
        git_in(&dir, &["add", "-f", "target/kept"]);
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn folder_without_marker_is_ignored() {
        let dir = repo("bo-nomarker", "target/\n", &["target/x"]);
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn non_ignored_folder_is_ignored() {
        let dir = repo("bo-notignored", "", &["Cargo.toml", "target/x"]);
        assert!(folders(&dir).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_target_is_skipped() {
        let dir = repo("bo-symlink", "target\n", &["Cargo.toml"]);
        let elsewhere = repo("bo-symlink-elsewhere", "", &["x"]);
        std::os::unix::fs::symlink(&elsewhere, dir.join("target")).unwrap();
        assert!(folders(&dir).is_empty());
        assert!(elsewhere.join("x").exists());
    }

    #[test]
    fn nested_git_is_skipped() {
        let dir = repo("bo-nested", "target/\n", &["Cargo.toml", "target/inner/x"]);
        fs::create_dir_all(dir.join("target/.git")).unwrap();
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn submodule_is_skipped() {
        let dir = repo("bo-submodule", "target/\n", &["Cargo.toml", "target/x"]);
        // A tracked gitlink at the folder path makes `ls-files` list it.
        let sha = "1".repeat(40);
        git_in(
            &dir,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{sha},target"),
            ],
        );
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn git_file_worktree_is_handled() {
        let main = repo("bo-wt-main", "target/\n", &["Cargo.toml"]);
        git_in(&main, &["add", "."]);
        git_in(&main, &["commit", "-q", "-m", "init"]);
        let linked = main.with_file_name(format!(
            "{}-linked",
            main.file_name().unwrap().to_string_lossy()
        ));
        git_in(
            &main,
            &[
                "worktree",
                "add",
                "--detach",
                "-q",
                linked.to_str().unwrap(),
            ],
        );
        put(&linked, "target/debug/x");
        assert!(linked.join(".git").is_file());
        assert_eq!(folders(&fs::canonicalize(&linked).unwrap()), ["target"]);
    }

    #[test]
    fn external_cargo_target_untouched() {
        let dir = repo("bo-external", "target/\n", &["Cargo.toml"]);
        let external = repo("bo-external-target", "", &["debug/x"]);
        assert!(folders(&dir).is_empty());
        assert!(external.join("debug/x").exists());
    }

    #[test]
    fn dist_is_not_in_the_table() {
        let dir = repo("bo-dist", "dist/\n", &["pyproject.toml", "dist/pkg.whl"]);
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn venv_is_out_of_v1() {
        let dir = repo(
            "bo-venv",
            ".venv/\nvenv/\nenv/\n",
            &[
                "pyproject.toml",
                ".venv/pyvenv.cfg",
                ".venv/lib/python3.12/__pycache__/x.pyc",
                "venv/lib/python3.12/__pycache__/x.pyc",
                "env/pyvenv.cfg",
                "env/lib/python3.12/__pycache__/x.pyc",
            ],
        );
        assert!(!TABLE
            .iter()
            .flat_map(|eco| eco.folders)
            .any(|name| VENV_NAMES.contains(name)));
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn scan_depth_4_found_5_not() {
        let dir = repo(
            "bo-depth",
            "target/\n",
            &["Cargo.toml", "a/b/c/target/x", "a/b/c/d/target/x"],
        );
        assert_eq!(folders(&dir), ["a/b/c/target"]);
    }

    // The rules.

    use crate::gc::lock::{with_run_lock as locked, NoHook as NoLockHook};
    use crate::gc::space::{Space, SpaceProbe};
    use crate::gc::FixedClock;
    use std::cell::Cell;
    use std::io;
    use std::time::SystemTime;

    const DAY: i64 = 86_400;
    const GIB: u64 = 1024 * 1024 * 1024;

    /// One clock reading for the whole test run, so a file aged against it and
    /// a plan judged against it cannot straddle a second tick.
    fn now() -> i64 {
        static NOW: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
        *NOW.get_or_init(crate::common::time::now_secs)
    }

    fn limits() -> Limits {
        Limits {
            idle_days: 7,
            pressure_idle_days: 1,
            active_minutes: 15,
            warn_gib: 10,
            warn_percent: 5,
            ..Limits::default()
        }
    }

    fn set_age(path: &Path, age_secs: i64) {
        let when = SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(u64::try_from(now() - age_secs).unwrap());
        fs::File::open(path).unwrap().set_modified(when).unwrap();
    }

    /// Sets every file and folder under `dir`, except `.git`, to `age_secs` old.
    fn age_tree(dir: &Path, age_secs: i64) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            if entry.file_name() == ".git" {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                age_tree(&entry.path(), age_secs);
            }
            set_age(&entry.path(), age_secs);
        }
    }

    /// A Rust worktree whose files and build folder all changed `idle_secs` ago.
    fn rust_worktree(tag: &str, idle_secs: i64) -> PathBuf {
        let dir = repo(
            tag,
            "target/\n",
            &["Cargo.toml", "target/debug/deps/x", "notes.txt"],
        );
        age_tree(&dir, idle_secs);
        dir
    }

    /// Answers by path prefix; a path with no entry gets the default.
    struct FakeSpace {
        by_prefix: Vec<(PathBuf, Option<Space>)>,
        default: Option<Space>,
    }

    impl SpaceProbe for FakeSpace {
        fn space(&self, path: &Path) -> io::Result<Space> {
            let answer = self
                .by_prefix
                .iter()
                .find(|(prefix, _)| path.starts_with(prefix))
                .map_or(self.default, |(_, answer)| *answer);
            answer.ok_or_else(|| io::Error::other("no free space reading"))
        }
    }

    fn roomy() -> FakeSpace {
        FakeSpace {
            by_prefix: Vec::new(),
            default: Some(Space {
                free_bytes: 500 * GIB,
                total_bytes: 1000 * GIB,
            }),
        }
    }

    fn tight() -> Space {
        Space {
            free_bytes: GIB,
            total_bytes: 1000 * GIB,
        }
    }

    fn never_in_use(_: &Path) -> bool {
        false
    }

    fn plan_for(dirs: &[&Path], space: &dyn SpaceProbe) -> Vec<Candidate> {
        let probes = Probes {
            space,
            in_use: &never_in_use,
        };
        let worktrees: Vec<PathBuf> = dirs.iter().map(|d| d.to_path_buf()).collect();
        plan(&worktrees, &limits(), now(), &probes)
    }

    #[test]
    fn idle_boundary() {
        let at_limit = rust_worktree("bo-idle-at", 7 * DAY);
        let under = rust_worktree("bo-idle-under", 7 * DAY - 1);

        assert_eq!(plan_for(&[&at_limit], &roomy()).len(), 1);
        assert!(plan_for(&[&under], &roomy()).is_empty());
    }

    #[test]
    fn active_boundary_15m() {
        let busy = rust_worktree("bo-active-busy", 30 * DAY);
        set_age(&busy.join("target/debug/deps/x"), 14 * 60 + 59);
        let quiet = rust_worktree("bo-active-quiet", 30 * DAY);
        set_age(&quiet.join("target/debug/deps/x"), 15 * 60);

        assert!(plan_for(&[&busy], &roomy()).is_empty());
        assert_eq!(plan_for(&[&quiet], &roomy()).len(), 1);
    }

    #[test]
    fn activity_depth_4_counted_5_not() {
        let near = rust_worktree("bo-act-near", 30 * DAY);
        put(&near, "target/a/b/c/fresh");
        age_tree(&near, 30 * DAY);
        set_age(&near.join("target/a/b/c/fresh"), 0);
        let far = rust_worktree("bo-act-far", 30 * DAY);
        put(&far, "target/a/b/c/d/fresh");
        age_tree(&far, 30 * DAY);
        set_age(&far.join("target/a/b/c/d/fresh"), 0);

        assert!(plan_for(&[&near], &roomy()).is_empty());
        assert_eq!(plan_for(&[&far], &roomy()).len(), 1);
    }

    #[test]
    fn pressure_floor_holds() {
        let low = FakeSpace {
            by_prefix: Vec::new(),
            default: Some(tight()),
        };
        let too_young = rust_worktree("bo-floor-young", DAY - 1);
        let old_enough = rust_worktree("bo-floor-old", DAY);

        assert!(plan_for(&[&too_young], &low).is_empty());
        let found = plan_for(&[&old_enough], &low);
        assert_eq!(found.len(), 1);
        assert!(found[0].pressure_only);
    }

    #[test]
    fn oldest_goes_first_under_pressure() {
        let low = FakeSpace {
            by_prefix: Vec::new(),
            default: Some(tight()),
        };
        let three_days = rust_worktree("bo-order-3", 3 * DAY);
        let five_days = rust_worktree("bo-order-5", 5 * DAY);

        let found = plan_for(&[&three_days, &five_days], &low);

        assert_eq!(found[0].worktree, five_days);
        assert_eq!(found[1].worktree, three_days);
    }

    #[test]
    fn lsof_in_use_keeps_folder() {
        let dir = rust_worktree("bo-lsof", 30 * DAY);
        let probes = Probes {
            space: &roomy(),
            in_use: &|_| true,
        };

        let found = plan(&[dir], &limits(), now(), &probes);

        assert!(found.is_empty());
    }

    #[test]
    fn space_probe_error_skips_pressure_rule() {
        let broken = FakeSpace {
            by_prefix: Vec::new(),
            default: None,
        };
        let two_days = rust_worktree("bo-probe-error", 2 * DAY);

        assert!(plan_for(&[&two_days], &broken).is_empty());
    }

    #[test]
    fn pressure_reads_each_worktree_volume() {
        let squeezed = rust_worktree("bo-volume-low", 2 * DAY);
        let roomy_one = rust_worktree("bo-volume-roomy", 2 * DAY);
        let space = FakeSpace {
            by_prefix: vec![(squeezed.clone(), Some(tight()))],
            default: roomy().default,
        };

        let found = plan_for(&[&squeezed, &roomy_one], &space);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].worktree, squeezed);
    }

    #[test]
    fn fixture_go_deletes_nothing() {
        let dir = repo(
            "bo-go-delete",
            "bin/\nvendor/\n",
            &["go.mod", "bin/x", "vendor/y"],
        );
        age_tree(&dir, 30 * DAY);

        assert!(plan_for(&[&dir], &roomy()).is_empty());
    }

    /// Runs `remove_all` for `candidates` while holding a gc lock.
    fn remove_under_lock(candidates: &[Candidate], space: &dyn SpaceProbe) -> Removed {
        let gc = crate::common::test_support::scratch_dir("bo-gc");
        let clock = FixedClock(now());
        let probes = Probes {
            space,
            in_use: &never_in_use,
        };
        match locked(
            &gc,
            std::time::Duration::ZERO,
            &clock,
            &NoLockHook,
            |guard| remove_all(guard, candidates, &limits(), &probes),
        ) {
            Ok(RunOutcome::Ran(done)) => done,
            _ => panic!("the gc lock should be free"),
        }
    }

    #[test]
    fn build_output_deleted_in_playbook_worktree() {
        let dir = rust_worktree("bo-delete", 30 * DAY);
        let space = roomy();

        let done = remove_under_lock(&plan_for(&[&dir], &space), &space);

        assert_eq!(done.folders, [dir.join("target")]);
        assert!(!dir.join("target").exists());
    }

    #[test]
    fn dirty_worktree_loses_only_build_output() {
        let dir = rust_worktree("bo-dirty", 30 * DAY);
        put(&dir, "untracked-work.txt");
        age_tree(&dir, 30 * DAY);
        let space = roomy();

        remove_under_lock(&plan_for(&[&dir], &space), &space);

        assert!(dir.join("untracked-work.txt").exists());
        assert!(dir.join("Cargo.toml").exists());
        assert!(!dir.join("target").exists());
    }

    #[test]
    fn the_guard_refuses_a_folder_outside_the_worktree() {
        let inside = rust_worktree("bo-guard-inside", 30 * DAY);
        let outside = rust_worktree("bo-guard-outside", 30 * DAY);
        let foreign = Candidate {
            worktree: inside,
            folder: outside.join("target"),
            idle_secs: 30 * DAY,
            pressure_only: false,
        };

        let done = remove_under_lock(&[foreign], &roomy());

        assert!(
            done.folders.is_empty() && done.failures.len() == 1,
            "{done:?}"
        );
        assert!(outside.join("target").exists());
    }

    fn registry_for_flag(tag: &str) -> Registry {
        Registry::new(crate::common::test_support::scratch_dir(tag).join("gc"))
    }

    fn run_flag(dirs: &[PathBuf], answer: bool, is_terminal: bool) -> (String, i32, u32) {
        let asked = Cell::new(0);
        let mut confirm = |_: &str| {
            asked.set(asked.get() + 1);
            answer
        };
        let mut terminal = Terminal {
            is_terminal,
            confirm: &mut confirm,
        };
        let probes = Probes {
            space: &roomy(),
            in_use: &never_in_use,
        };
        let registry = registry_for_flag("bo-flag-gc");
        let (text, code) = flag_command(
            (&registry, &FixedClock(now())),
            dirs,
            &limits(),
            (&probes, &mut terminal),
        );
        (text, code, asked.get())
    }

    #[test]
    fn build_output_flag_declined_deletes_nothing() {
        let dir = rust_worktree("bo-declined", 30 * DAY);

        let (text, code, _) = run_flag(std::slice::from_ref(&dir), false, true);

        assert_eq!(code, 0);
        assert!(text.contains("Nothing was deleted"), "{text}");
        assert!(dir.join("target").exists());
    }

    #[test]
    fn build_output_yes_still_asks() {
        let dir = rust_worktree("bo-asks", 30 * DAY);

        let (_, _, asked) = run_flag(std::slice::from_ref(&dir), true, true);

        assert_eq!(asked, 1);
    }

    #[test]
    fn build_output_confirmed_run_respects_roots() {
        let dir = rust_worktree("bo-confirmed", 30 * DAY);

        let (text, code, _) = run_flag(std::slice::from_ref(&dir), true, true);

        assert_eq!(code, 0, "{text}");
        assert!(!dir.join("target").exists());
        assert!(dir.join("Cargo.toml").exists());
    }

    #[test]
    fn flag_without_a_terminal_asks_nothing_and_deletes_nothing() {
        let dir = rust_worktree("bo-no-tty", 30 * DAY);

        let (text, code, asked) = run_flag(std::slice::from_ref(&dir), true, false);

        assert_eq!((code, asked), (1, 0));
        assert!(text.contains("needs a terminal"), "{text}");
        assert!(dir.join("target").exists());
    }

    #[test]
    fn build_output_flag_skips_primary_checkout() {
        let primary = PathBuf::from("/repo");
        let hand_made = PathBuf::from("/elsewhere/wt");
        let managed = PathBuf::from("/home/pb/wt");

        let (made_by_playbook, other) = split_worktrees(
            vec![primary.clone(), managed.clone(), hand_made.clone()],
            &|path| path == managed,
        );

        assert_eq!(made_by_playbook, [managed]);
        assert_eq!(other, [hand_made]);
    }

    #[test]
    fn primary_checkout_build_output_survives() {
        let primary = rust_worktree("bo-primary", 30 * DAY);
        let managed = rust_worktree("bo-primary-managed", 30 * DAY);
        let (to_sweep, _) = split_worktrees(vec![primary.clone(), managed.clone()], &|_| true);
        let space = roomy();

        remove_under_lock(
            &plan_for(
                &to_sweep.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
                &space,
            ),
            &space,
        );

        assert!(primary.join("target").exists());
        assert!(!managed.join("target").exists());
    }

    #[test]
    fn unmanaged_build_output_survives_daily_sweep() {
        let primary = rust_worktree("bo-unmanaged-primary", 30 * DAY);
        let managed = rust_worktree("bo-unmanaged-managed", 30 * DAY);
        let hand_made = rust_worktree("bo-unmanaged-hand", 30 * DAY);
        let (to_sweep, _) =
            split_worktrees(vec![primary, managed.clone(), hand_made.clone()], &|path| {
                path == managed
            });
        let space = roomy();

        remove_under_lock(
            &plan_for(
                &to_sweep.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
                &space,
            ),
            &space,
        );

        assert!(!managed.join("target").exists());
        assert!(hand_made.join("target").exists());
    }
}
