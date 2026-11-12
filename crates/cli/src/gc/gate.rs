// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The store of gate databases whose worktree is gone. Only the current repo's
//! databases can be due; other repos' databases are listed so the report shows
//! them, and nothing is touched when git cannot name the live worktrees.

use std::fs;
use std::path::{Path, PathBuf};

use super::stores::mtime_secs;
use super::{Due, Item, Store};
use crate::common::data::orphan_dbs;
use crate::common::repo::repo_slug_in;
use crate::worktree::live_worktree_ids;

const SECS_PER_DAY: i64 = 86_400;
const SIDE_FILES: [&str; 2] = ["-wal", "-shm"];

/// Scans from the repo that holds the current directory.
pub fn scan(store: &Store, now: i64) -> Vec<Item> {
    match std::env::current_dir() {
        Ok(repo) => scan_repo(store, &repo, now),
        Err(_) => scan_repo(store, Path::new(""), now),
    }
}

/// Every gate database under the store root, with those that belong to a
/// removed worktree of the repo at `repo` marked due once idle long enough.
fn scan_repo(store: &Store, repo: &Path, now: i64) -> Vec<Item> {
    let Some(gate_root) = store.roots.first() else {
        return Vec::new();
    };
    let orphans = orphans_of(gate_root, repo);
    let idle_secs = i64::from(store.limits.idle_days) * SECS_PER_DAY;
    let mut items = Vec::new();
    for db in all_databases(gate_root) {
        let mut item = database_item(&db);
        if orphans.contains(&db) && now - item.newest_mtime >= idle_secs {
            item.due = Some(Due {
                reason: format!(
                    "its worktree is gone and it was idle for {} days or more",
                    store.limits.idle_days
                ),
                retain_days: store.limits.retain_days,
            });
        }
        items.push(item);
    }
    items
}

/// The gate databases of `repo` that no live worktree owns. Empty whenever git
/// cannot say, so a broken repo never makes every database look orphaned.
fn orphans_of(gate_root: &Path, repo: &Path) -> Vec<PathBuf> {
    let (Some(data), slug) = (gate_root.parent(), repo_slug_in(repo)) else {
        return Vec::new();
    };
    let live = match live_worktree_ids(repo) {
        Some(live) if !live.is_empty() && !slug.is_empty() => live,
        _ => return Vec::new(),
    };
    orphan_dbs(data, &slug, &live)
        .into_iter()
        .filter(|(kind, _)| *kind == "gate")
        .map(|(_, path)| path)
        .collect()
}

/// `<root>/<owner>/<repo>/<worktree-id>.db`, sorted.
fn all_databases(gate_root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for owner in subdirs(gate_root) {
        for repo in subdirs(&owner) {
            let Ok(entries) = fs::read_dir(&repo) else {
                continue;
            };
            found.extend(entries.flatten().map(|e| e.path()).filter(|path| {
                path.extension().is_some_and(|ext| ext == "db")
                    && fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file())
            }));
        }
    }
    found.sort();
    found
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|e| e.path())
        .collect()
}

/// The database and its `-wal` and `-shm` files as one group.
fn database_item(db: &Path) -> Item {
    let mut also = Vec::new();
    for side in SIDE_FILES {
        let mut name = db.as_os_str().to_owned();
        name.push(side);
        let path = PathBuf::from(name);
        if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
            also.push(path);
        }
    }
    let (mut bytes, mut newest_mtime) = (0, 0);
    for path in std::iter::once(db).chain(also.iter().map(PathBuf::as_path)) {
        if let Ok(meta) = fs::symlink_metadata(path) {
            bytes += meta.len();
            newest_mtime = newest_mtime.max(mtime_secs(&meta));
        }
    }
    Item {
        path: db.to_path_buf(),
        bytes,
        newest_mtime,
        also,
        due: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::{Action, Limits};
    use std::process::Command;
    use std::time::{Duration, SystemTime};

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = SECS_PER_DAY;

    struct Fx {
        repo: PathBuf,
        gate: PathBuf,
        store: Store,
    }

    fn git(dir: &Path, args: &[&str]) {
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

    /// A repo with origin `acme/app` and one extra linked worktree.
    fn fx(tag: &str) -> Fx {
        let dir = crate::common::test_support::scratch_dir(tag);
        fs::create_dir_all(&dir).unwrap();
        // Git prints real paths, and the live worktree ids are made from them.
        let base = fs::canonicalize(dir).unwrap();
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &["remote", "add", "origin", "https://github.com/acme/app.git"],
        );
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let gate = base.join("data/gate");
        fs::create_dir_all(gate.join("acme/app")).unwrap();
        let store = Store {
            name: "gate databases",
            roots: vec![gate.clone()],
            scan,
            action: Action::Tombstone,
            limits: Limits {
                idle_days: 30,
                max_bytes: 0,
                retain_days: 7,
                ..Limits::default()
            },
        };
        Fx { repo, gate, store }
    }

    fn id_of(path: &Path) -> String {
        crate::common::paths::worktree_id_for(path)
    }

    fn plant(fx: &Fx, slug: &str, id: &str, age_secs: i64) -> PathBuf {
        let dir = fx.gate.join(slug);
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join(format!("{id}.db"));
        for name in [
            db.clone(),
            db.with_extension("db-wal"),
            db.with_extension("db-shm"),
        ] {
            fs::write(&name, "x").unwrap();
            let when = SystemTime::UNIX_EPOCH
                + Duration::from_secs(u64::try_from(NOW - age_secs).unwrap());
            fs::File::options()
                .write(true)
                .open(&name)
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
        db
    }

    fn due(items: &[Item]) -> Vec<&Path> {
        items
            .iter()
            .filter(|item| item.due.is_some())
            .map(|item| item.path.as_path())
            .collect()
    }

    #[test]
    fn gate_db_idle_boundary() {
        // Arrange
        let fx = fx("gate-boundary");
        let under = plant(&fx, "acme/app", "gone-a", 30 * DAY - 1);
        let exact = plant(&fx, "acme/app", "gone-b", 30 * DAY);

        // Act
        let items = scan_repo(&fx.store, &fx.repo, NOW);

        // Assert
        assert_eq!(items.len(), 2);
        assert_eq!(due(&items), vec![exact.as_path()]);
        assert!(under.exists());
    }

    #[test]
    fn gate_live_worktree_db_stays() {
        // Arrange
        let fx = fx("gate-live");
        plant(&fx, "acme/app", &id_of(&fx.repo), 90 * DAY);

        // Act
        let items = scan_repo(&fx.store, &fx.repo, NOW);

        // Assert
        assert_eq!(items.len(), 1);
        assert!(due(&items).is_empty());
    }

    #[test]
    fn gate_wal_and_shm_move_with_the_database() {
        // Arrange
        let fx = fx("gate-side-files");
        let db = plant(&fx, "acme/app", "gone", 90 * DAY);

        // Act
        let items = scan_repo(&fx.store, &fx.repo, NOW);

        // Assert
        assert_eq!(
            items[0].also,
            vec![db.with_extension("db-wal"), db.with_extension("db-shm")]
        );
        assert_eq!(items[0].bytes, 3);
    }

    #[test]
    fn gate_store_fails_closed_when_git_errors() {
        // Arrange
        let fx = fx("gate-git-error");
        plant(&fx, "acme/app", "gone", 90 * DAY);
        let not_a_repo = fx.repo.parent().unwrap().join("plain");
        fs::create_dir_all(&not_a_repo).unwrap();

        // Act
        let items = scan_repo(&fx.store, &not_a_repo, NOW);

        // Assert
        assert_eq!(items.len(), 1);
        assert!(due(&items).is_empty());
    }

    #[test]
    fn gate_store_fails_closed_on_missing_repo() {
        // Arrange
        let fx = fx("gate-missing-repo");
        plant(&fx, "acme/app", "gone", 90 * DAY);
        let missing = fx.repo.parent().unwrap().join("nowhere");

        // Act
        let items = scan_repo(&fx.store, &missing, NOW);

        // Assert
        assert!(due(&items).is_empty());
    }

    #[test]
    fn gate_store_skips_other_repos() {
        // Arrange
        let fx = fx("gate-other-repo");
        let other = plant(&fx, "acme/other", "gone", 90 * DAY);

        // Act
        let items = scan_repo(&fx.store, &fx.repo, NOW);

        // Assert
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, other);
        assert!(due(&items).is_empty());
    }
}
