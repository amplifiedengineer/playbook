// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The apply run: reconcile, each store's action, expiry of old tombstones,
//! then compaction of the index. Also restore, which puts a tombstoned item
//! back where it came from.

use std::fs;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::time::Duration;

use super::guard::RealFs;
use super::index::{self, Record, State};
use super::lock::{with_run_lock, NoHook, RunOutcome};
use super::logs::{append_confirmed, rotate, staging_of, RealLogFs, Rotation};
use super::tombstone::{
    anomalies, move_locked, reconcile, Env, Findings, MoveOutcome, MoveRequest,
};
use super::{Action, Clock, Registry};

/// A run expires at most this many tombstones, so one run stays short.
pub const EXPIRE_CAP: usize = 500;
/// How long `--restore` waits for a busy run lock.
const RESTORE_WAIT: Duration = Duration::from_millis(500);
const SECS_PER_DAY: i64 = 86_400;

/// What one apply run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Where the next run should start, as a store index. Equal to the store
    /// count when every store has had its turn.
    pub next_store: usize,
    pub findings: Findings,
    pub tombstoned: usize,
    pub rotated: usize,
    pub expired: usize,
    pub compacted: usize,
    /// Indexes of stores whose scan panicked or whose items failed to move.
    pub failed_stores: Vec<usize>,
    /// Items that could not be moved, and a lost lock if there was one.
    pub failures: Vec<String>,
}

fn grace_secs(rec: &Record) -> i64 {
    // At least one day, so an item is never expired in the run that moved it.
    i64::from(rec.retain_days.max(1)) * SECS_PER_DAY
}

/// Tombstones every due item of the stores from `cursor` on. Returns the index
/// of the next store, or `None` when the lock was lost and the run must stop.
fn run_stores(
    env: &Env,
    (cursor, skip): (usize, &[usize]),
    budget: Option<i64>,
    out: &mut ApplyOutcome,
) -> Option<usize> {
    let start = env.now();
    let stores = env.registry.stores();
    for (index, store) in stores.iter().enumerate().skip(cursor) {
        if index > cursor && budget.is_some_and(|limit| env.now() - start >= limit) {
            return Some(index);
        }
        if skip.contains(&index)
            || !matches!(
                store.action,
                Action::Tombstone | Action::Rotate | Action::Delete
            )
        {
            continue;
        }
        if store.action == Action::Delete {
            let removed = super::build_output::sweep(env, &store.limits);
            out.failures.extend(removed.failures);
            continue;
        }
        let failures_before = out.failures.len();
        let scanned = catch_unwind(AssertUnwindSafe(|| store.items(env.now())));
        let Ok(items) = scanned else {
            out.failures
                .push(format!("the {} store failed while scanning", store.name));
            out.failed_stores.push(index);
            continue;
        };
        for item in items {
            let Some(due) = item.due else { continue };
            if env.guard.verify().is_err() {
                out.failures
                    .push("the gc lock was lost, so the run stopped".into());
                return None;
            }
            if store.action == Action::Rotate {
                let staged = staging_of(&item.path);
                if fs::symlink_metadata(&staged).is_ok() {
                    // A copy left by a run that stopped midway.
                    if let Err(err) = env.fs.remove(env.guard, &staged) {
                        out.failures.push(format!(
                            "{}: could not clear {}: {err}",
                            item.path.display(),
                            staged.display()
                        ));
                        continue;
                    }
                }
                match rotate(&item.path, &RealLogFs, append_confirmed(&item.path)) {
                    Ok(Rotation::Rotated) => out.rotated += 1,
                    Ok(_) => {}
                    Err(err) => out
                        .failures
                        .push(format!("{}: could not rotate: {err}", item.path.display())),
                }
                continue;
            }
            let req = MoveRequest {
                paths: std::iter::once(item.path.clone())
                    .chain(item.also.iter().cloned())
                    .collect(),
                reason: due.reason,
                retain_days: due.retain_days,
            };
            match move_locked(env, &req, None) {
                MoveOutcome::Moved { .. } => out.tombstoned += 1,
                MoveOutcome::Busy => {}
                MoveOutcome::Refused(why)
                | MoveOutcome::Unavailable(why)
                | MoveOutcome::Failed(why) => {
                    out.failures.push(format!("{}: {why}", item.path.display()))
                }
            }
            let _ = env.guard.touch();
        }
        let _ = env.guard.touch();
        if out.failures.len() > failures_before {
            out.failed_stores.push(index);
        }
    }
    Some(stores.len())
}

/// Removes the date folder that held `dest` once nothing is left in it.
fn tidy_date_folder(env: &Env, dest: &Path) {
    let Some(date_dir) = dest.parent() else {
        return;
    };
    if fs::read_dir(date_dir).is_ok_and(|mut entries| entries.next().is_none()) {
        let _ = env.fs.remove(env.guard, date_dir);
    }
}

/// Deletes tombstones that reached their own grace period, at most
/// [`EXPIRE_CAP`] per call. A record dated in the future is kept.
pub fn expire(env: &Env) -> usize {
    let now = env.now();
    let mut expired = 0;
    for rec in index::latest(index::read(&env.index_path())) {
        if expired >= EXPIRE_CAP {
            break;
        }
        let age = now - rec.tombstoned_at;
        if rec.state != State::Done || age < 0 || age < grace_secs(&rec) {
            continue;
        }
        if env.append(&rec.with_state(State::Expiring, now)).is_err() {
            break;
        }
        if !rec.dest.starts_with(env.registry.tombstones_dir()) {
            continue;
        }
        // On a failed delete the `expiring` record stays and reconcile retries.
        if env.fs.remove(env.guard, &rec.dest).is_err() {
            continue;
        }
        if env.append(&rec.with_state(State::Expired, now)).is_ok() {
            expired += 1;
        }
        tidy_date_folder(env, &rec.dest);
        let _ = env.guard.touch();
    }
    expired
}

/// Rewrites the index without terminal records older than their grace period.
/// Never drops a record that still describes live data or a half-done step.
pub fn compact(env: &Env) -> usize {
    let now = env.now();
    let latest = index::latest(index::read(&env.index_path()));
    let kept: Vec<Record> = latest
        .iter()
        .filter(|rec| !(rec.state.is_terminal() && now - rec.at >= grace_secs(rec)))
        .cloned()
        .collect();
    let dropped = latest.len() - kept.len();
    if dropped > 0 && index::rewrite(&env.index_path(), &kept).is_err() {
        return 0;
    }
    dropped
}

/// One apply run, from store `cursor`. The run stops between stores once more
/// than `budget` seconds have passed on the injected clock. Expiry and
/// compaction run on every call, including a cut-short one.
pub fn run_apply(env: &Env, cursor: usize, budget: Option<i64>) -> ApplyOutcome {
    run_apply_skipping(env, (cursor, &[]), budget)
}

/// [`run_apply`] that leaves the stores in the second part of `start` alone.
pub fn run_apply_skipping(
    env: &Env,
    start: (usize, &[usize]),
    budget: Option<i64>,
) -> ApplyOutcome {
    let mut out = ApplyOutcome {
        findings: reconcile(env),
        next_store: start.0,
        ..ApplyOutcome::default()
    };
    let Some(next) = run_stores(env, start, budget, &mut out) else {
        return out;
    };
    out.next_store = next;
    out.expired = expire(env);
    out.compacted = compact(env);
    out
}

/// Whether `path` or its nearest existing parent sits inside a registered
/// store root once symlinks are resolved. A path with a `..` part is refused,
/// because the nearest existing parent would not be where the path ends up.
fn restorable_location(roots: &[std::path::PathBuf], path: &Path) -> bool {
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return false;
    }
    let Some(parent) = path.ancestors().skip(1).find(|dir| dir.exists()) else {
        return false;
    };
    let Ok(real) = fs::canonicalize(parent) else {
        return false;
    };
    roots
        .iter()
        .filter_map(|root| fs::canonicalize(root).ok())
        .any(|root| real.starts_with(&root))
}

/// Moves `from` back to `to`, copying when the two are on different
/// filesystems. The tombstone copy is removed only after the copy is in place.
fn put_back(env: &Env, from: &Path, to: &Path) -> Result<(), String> {
    match env.fs.rename(env.guard, from, to) {
        Ok(()) => return Ok(()),
        Err(err) if err.kind() != io::ErrorKind::CrossesDevices => {
            return Err(format!("could not move {} back: {err}", to.display()));
        }
        Err(_) => {}
    }
    let mut staged = to.as_os_str().to_owned();
    staged.push(".gc-restore");
    let staged = std::path::PathBuf::from(staged);
    if fs::symlink_metadata(&staged).is_ok() {
        // Left by a restore that a crash cut short; copying over it would
        // merge two trees.
        env.fs
            .remove(env.guard, &staged)
            .map_err(|e| format!("cannot clear {}: {e}", staged.display()))?;
    }
    let copied = env
        .fs
        .copy(env.guard, from, &staged)
        .and_then(|()| env.fs.rename(env.guard, &staged, to));
    if let Err(err) = copied {
        let _ = env.fs.remove(env.guard, &staged);
        return Err(format!("could not copy {} back: {err}", to.display()));
    }
    env.guard.verify().map_err(|e| e.to_string())?;
    env.fs.remove(env.guard, from).map_err(|e| e.to_string())
}

/// Restores a `done` item to its original paths. Refuses when an original
/// path exists, when a path is outside every registered root, or when the
/// item is in any other state.
pub fn restore(env: &Env, id: &str) -> Result<String, String> {
    let Some(rec) = index::latest(index::read(&env.index_path()))
        .into_iter()
        .find(|rec| rec.id == id)
    else {
        return Err(format!("no tombstone with id {id}"));
    };
    if rec.state != State::Done {
        return Err(format!(
            "tombstone {id} is {:?}, and only a done item can be restored",
            rec.state
        )
        .to_lowercase());
    }
    let roots = env.registry.store_roots();
    if !rec.dest.starts_with(env.registry.tombstones_dir()) {
        return Err(format!(
            "tombstone {id} points outside the tombstone folder"
        ));
    }
    let members: Vec<_> = rec
        .paths
        .iter()
        .filter(|path| !rec.missing.contains(path))
        .collect();
    for path in &members {
        if fs::symlink_metadata(path).is_ok() {
            return Err(format!(
                "{} already exists, so nothing was restored",
                path.display()
            ));
        }
        if !restorable_location(&roots, path) {
            return Err(format!(
                "{} is not inside a registered root",
                path.display()
            ));
        }
    }
    env.append(&rec.with_state(State::Restoring, env.now()))
        .map_err(|e| format!("cannot write the tombstone index: {e}"))?;
    for path in &members {
        env.guard.verify().map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        put_back(env, &rec.member_dest(path), path)?;
    }
    env.fs
        .remove(env.guard, &rec.dest)
        .map_err(|e| e.to_string())?;
    tidy_date_folder(env, &rec.dest);
    env.append(&rec.with_state(State::Restored, env.now()))
        .map_err(|e| format!("cannot write the tombstone index: {e}"))?;
    Ok(format!("restored {id} to {} place(s)", members.len()))
}

/// The text `playbook gc` adds under the report: anything a reconcile would
/// fix or that needs a person to look at it. Empty when all is well.
pub fn render_anomalies(registry: &Registry, clock: &dyn Clock) -> String {
    let found = anomalies(registry, clock.now());
    let mut text = String::new();
    for (title, lines) in [
        ("Needs attention:", &found.attention),
        ("Would be repaired by `playbook gc --yes`:", &found.fixes),
    ] {
        if !lines.is_empty() {
            text.push_str(&format!("\n{title}\n"));
            for line in lines {
                text.push_str(&format!("  {line}\n"));
            }
        }
    }
    text
}

/// `playbook gc --yes`: the text to print and the exit code.
pub fn apply_command(registry: &Registry, clock: &dyn Clock) -> (String, i32) {
    let fs = RealFs::new(registry);
    let ran = with_run_lock(registry.gc_dir(), Duration::ZERO, clock, &NoHook, |guard| {
        run_apply(
            &Env {
                guard,
                registry,
                fs: &fs,
            },
            0,
            None,
        )
    });
    match ran {
        Ok(RunOutcome::Ran(out)) => {
            let mut text = format!(
                "gc: {} tombstoned, {} rotated, {} expired, {} index records dropped\n",
                out.tombstoned, out.rotated, out.expired, out.compacted
            );
            for line in out.findings.fixes.iter().map(|l| format!("repaired {l}")) {
                text.push_str(&format!("{line}\n"));
            }
            for line in out.findings.attention.iter().chain(&out.failures) {
                text.push_str(&format!("needs attention: {line}\n"));
            }
            let code = i32::from(!out.failures.is_empty());
            (text, code)
        }
        Ok(RunOutcome::AlreadyRunning) => ("gc already running; nothing was changed\n".into(), 1),
        Err(err) => (format!("gc: {err}\n"), 1),
    }
}

/// Tombstones `path` for a caller that has decided on its own that it should
/// go, such as session retention. The path must sit inside a store root. When
/// another gc run holds the lock the answer is `Busy` and nothing changes, so
/// the scheduled sweep picks the item up later.
pub fn tombstone_now(
    registry: &Registry,
    clock: &dyn Clock,
    path: &Path,
    reason: &str,
) -> MoveOutcome {
    let Some(store) = registry
        .stores()
        .iter()
        .find(|store| store.roots.iter().any(|root| path.starts_with(root)))
    else {
        return MoveOutcome::Refused("no gc store covers this path".into());
    };
    let req = MoveRequest {
        paths: vec![path.to_path_buf()],
        reason: reason.into(),
        retain_days: store.limits.retain_days,
    };
    let fs = RealFs::new(registry);
    let ran = with_run_lock(registry.gc_dir(), Duration::ZERO, clock, &NoHook, |guard| {
        let env = Env {
            guard,
            registry,
            fs: &fs,
        };
        move_locked(&env, &req, None)
    });
    match ran {
        Ok(RunOutcome::Ran(outcome)) => outcome,
        Ok(RunOutcome::AlreadyRunning) => MoveOutcome::Busy,
        Err(err) => MoveOutcome::Failed(err.to_string()),
    }
}

/// `playbook gc --restore <id>`: the text to print and the exit code.
pub fn restore_command(registry: &Registry, clock: &dyn Clock, id: &str) -> (String, i32) {
    let fs = RealFs::new(registry);
    let ran = with_run_lock(registry.gc_dir(), RESTORE_WAIT, clock, &NoHook, |guard| {
        let env = Env {
            guard,
            registry,
            fs: &fs,
        };
        reconcile(&env);
        restore(&env, id)
    });
    match ran {
        Ok(RunOutcome::Ran(Ok(done))) => (format!("{done}\n"), 0),
        Ok(RunOutcome::Ran(Err(why))) => (format!("gc: {why}\n"), 1),
        Ok(RunOutcome::AlreadyRunning) => ("gc is busy; try again in a moment\n".into(), 1),
        Err(err) => (format!("gc: {err}\n"), 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::index::Mode;
    use crate::gc::lock::NoHook as TestNoHook;
    use crate::gc::tombstone::tests::{fx, Fx, NOW};
    use crate::gc::{Due, FixedClock, Item, Limits, Registry, Store};

    const WEEK: i64 = 7 * SECS_PER_DAY;

    fn scan_old(store: &Store, _now: i64) -> Vec<Item> {
        let mut found: Vec<Item> = fs::read_dir(&store.roots[0])
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("old-"))
            .map(|e| Item {
                path: e.path(),
                bytes: 1,
                newest_mtime: 0,
                also: Vec::new(),
                due: Some(Due {
                    reason: "idle".into(),
                    retain_days: 7,
                }),
            })
            .collect();
        found.sort_by(|a, b| a.path.cmp(&b.path));
        found
    }

    fn with_store(mut fx: Fx) -> Fx {
        let store = Store {
            name: "old",
            roots: vec![fx.root.clone()],
            scan: scan_old,
            action: Action::Tombstone,
            limits: Limits::default(),
        };
        fx.registry = Registry::new(fx.gc.clone()).with_store(store);
        fx
    }

    /// A `done` item whose data sits in the tombstone, tombstoned at `at`.
    fn done_with_data(fx: &Fx, id: &str, at: i64) -> Record {
        let original = fx.root.join(format!("{id}.txt"));
        let rec = fx.plant(id, State::Done, Mode::Rename, &[&original], at);
        fs::create_dir_all(&rec.dest).unwrap();
        fs::write(rec.member_dest(&original), "data").unwrap();
        rec
    }

    fn plant_state(fx: &Fx, id: &str, state: State, at: i64) {
        let original = fx.root.join(format!("{id}.txt"));
        fx.plant(id, state, Mode::Rename, &[&original], at);
    }

    fn moved(outcome: MoveOutcome) -> String {
        match outcome {
            MoveOutcome::Moved { id } => id,
            other => panic!("expected a move, got {other:?}"),
        }
    }

    #[test]
    fn expire_boundary_at_grace_and_one_second_under() {
        let cases = [(WEEK, true), (WEEK - 1, false), (-10, false)];
        for (age, deleted) in cases {
            let fx = fx("apply-expire-boundary");
            let rec = done_with_data(&fx, "a", NOW - age);

            let count = fx.run(NOW, expire);

            assert_eq!(count == 1, deleted, "age {age}");
            assert_eq!(!rec.dest.exists(), deleted, "age {age}");
        }
    }

    #[test]
    fn expire_caps_one_run() {
        let fx = fx("apply-expire-cap");
        for n in 0..(EXPIRE_CAP + 100) {
            done_with_data(&fx, &format!("i{n}"), NOW - WEEK);
        }

        let count = fx.run(NOW, expire);

        let left = fx
            .records()
            .iter()
            .filter(|r| r.state == State::Done)
            .count();
        assert_eq!((count, left), (EXPIRE_CAP, 100));
    }

    #[test]
    fn expire_rewrites_index_under_lock() {
        let fx = fx("apply-expire-lock");
        done_with_data(&fx, "a", NOW - WEEK);
        let real_now = crate::common::time::now_secs();

        let (count, second) = fx.run(real_now, |env| {
            let count = expire(env);
            let clock = FixedClock(real_now);
            let second = with_run_lock(&fx.gc, Duration::ZERO, &clock, &TestNoHook, |_| ());
            (count, second.unwrap())
        });

        assert_eq!((count, second), (0, RunOutcome::AlreadyRunning));
    }

    #[test]
    fn compaction_keeps_non_terminal_records() {
        let fx = fx("apply-compact-live");
        let live = [
            State::Pending,
            State::Copied,
            State::Done,
            State::Expiring,
            State::Restoring,
        ];
        for (n, state) in live.into_iter().enumerate() {
            plant_state(&fx, &format!("s{n}"), state, NOW - 10 * WEEK);
        }

        let dropped = fx.run(NOW, compact);

        assert_eq!((dropped, fx.records().len()), (0, live.len()));
    }

    #[test]
    fn compaction_drops_terminal_only_past_grace() {
        let fx = fx("apply-compact-terminal");
        plant_state(&fx, "young", State::Expired, NOW - WEEK + 1);
        plant_state(&fx, "old", State::Expired, NOW - WEEK);

        let dropped = fx.run(NOW, compact);

        let ids: Vec<String> = fx.records().into_iter().map(|r| r.id).collect();
        assert_eq!((dropped, ids), (1, vec!["young".to_string()]));
    }

    #[test]
    fn restore_round_trips_file_and_folder() {
        let fx = fx("apply-restore-trip");
        let file = fx.file("a.txt", "file body");
        let inner = fx.file("dir/b.txt", "folder body");
        let folder = inner.parent().unwrap().to_path_buf();
        let file_id = moved(fx.mv(&[&file], None));
        let folder_id = moved(fx.mv(&[&folder], None));

        fx.run(NOW, |env| {
            restore(env, &file_id).unwrap();
            restore(env, &folder_id).unwrap();
        });

        assert_eq!(fs::read_to_string(&file).unwrap(), "file body");
        assert_eq!(fs::read_to_string(&inner).unwrap(), "folder body");
        assert!(fx.records().iter().all(|r| r.state == State::Restored));
    }

    #[test]
    fn restore_in_registered_root_round_trips() {
        let fx = with_store(fx("apply-restore-store"));
        let file = fx.file("old-one", "body");
        let id = moved(fx.mv(&[&file], None));

        fx.run(NOW, |env| restore(env, &id)).unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "body");
    }

    #[test]
    fn restore_creates_missing_parent() {
        let fx = fx("apply-restore-parent");
        let file = fx.file("sub/a.txt", "x");
        let id = moved(fx.mv(&[&file], None));
        fs::rename(fx.root.join("sub"), fx.gc.join("parked")).unwrap();

        fx.run(NOW, |env| restore(env, &id)).unwrap();

        assert!(file.exists());
    }

    #[test]
    fn restore_refuses_existing_path() {
        let fx = fx("apply-restore-exists");
        let file = fx.file("a.txt", "old");
        let id = moved(fx.mv(&[&file], None));
        fx.file("a.txt", "new");

        let result = fx.run(NOW, |env| restore(env, &id));

        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        assert_eq!(fx.state_of(&id), State::Done);
    }

    #[test]
    fn restore_refuses_unknown_pending_and_lost() {
        let fx = fx("apply-restore-refuse");
        plant_state(&fx, "pending", State::Pending, NOW);
        plant_state(&fx, "lost", State::Lost, NOW);

        for id in ["nope", "pending", "lost"] {
            let result = fx.run(NOW, |env| restore(env, id));

            assert!(result.is_err(), "{id}");
        }
    }

    #[test]
    fn restore_outside_a_registered_root_is_refused() {
        let fx = fx("apply-restore-root");
        let outside = fx.gc.parent().unwrap().join("outside.txt");
        let rec = fx.plant("out", State::Done, Mode::Rename, &[&outside], NOW);
        fs::create_dir_all(&rec.dest).unwrap();
        fs::write(rec.member_dest(&outside), "x").unwrap();

        let result = fx.run(NOW, |env| restore(env, "out"));

        assert!(result.is_err() && !outside.exists());
    }

    #[test]
    fn restore_refuses_a_path_with_a_parent_part() {
        let fx = fx("apply-restore-dotdot");
        let sneaky = fx
            .root
            .join("nope")
            .join("..")
            .join("..")
            .join("escaped.txt");
        let rec = fx.plant("dd", State::Done, Mode::Rename, &[&sneaky], NOW);
        fs::create_dir_all(&rec.dest).unwrap();
        fs::write(rec.member_dest(&sneaky), "x").unwrap();

        let result = fx.run(NOW, |env| restore(env, "dd"));

        assert!(result.is_err());
        assert!(!fx.root.parent().unwrap().join("escaped.txt").exists());
    }

    #[test]
    fn a_zero_day_retention_still_waits_a_day() {
        let fx = fx("apply-zero-retain");
        let rec = fx.plant(
            "zr",
            State::Done,
            Mode::Rename,
            &[&fx.root.join("z.txt")],
            NOW,
        );
        let zero = Record {
            retain_days: 0,
            ..rec
        };

        assert_eq!(grace_secs(&zero), SECS_PER_DAY);
    }

    #[test]
    fn restore_crash_is_reconciled() {
        let fx = fx("apply-restore-crash");
        let back = fx.file("back.txt", "x");
        fx.plant("back", State::Restoring, Mode::Rename, &[&back], NOW);
        let gone = fx.root.join("gone.txt");
        let rec = fx.plant("gone", State::Restoring, Mode::Rename, &[&gone], NOW);
        fs::create_dir_all(&rec.dest).unwrap();
        fs::write(rec.member_dest(&gone), "x").unwrap();

        fx.reconcile(NOW);

        assert_eq!(fx.state_of("back"), State::Restored);
        assert_eq!(fx.state_of("gone"), State::Done);
    }

    #[test]
    fn restore_both_present_needs_attention() {
        let fx = fx("apply-restore-both");
        let original = fx.file("a.txt", "orig");
        let rec = fx.plant("both", State::Restoring, Mode::Rename, &[&original], NOW);
        fs::create_dir_all(&rec.dest).unwrap();
        fs::write(rec.member_dest(&original), "copy").unwrap();

        let found = fx.reconcile(NOW);

        assert_eq!(found.attention.len(), 1);
        assert!(original.exists() && rec.member_dest(&original).exists());
    }

    #[test]
    fn restored_item_is_not_lost() {
        let fx = fx("apply-restored-not-lost");
        plant_state(&fx, "r", State::Restored, NOW - 10);

        let found = fx.reconcile(NOW);

        assert_eq!(fx.state_of("r"), State::Restored);
        assert!(found.fixes.is_empty() && found.attention.is_empty());
    }

    #[test]
    fn restore_reports_busy() {
        let fx = fx("apply-restore-busy");
        let real_now = crate::common::time::now_secs();

        let (text, code) = fx.run(real_now, |_| {
            restore_command(&fx.registry, &FixedClock(real_now), "any")
        });

        assert_eq!(code, 1);
        assert!(text.contains("busy"), "{text}");
    }

    #[test]
    fn yes_applies_reconcile_stores_expiry_in_order() {
        let fx = with_store(fx("apply-order"));
        let stale = fx.file("stale.txt", "x");
        fx.plant("stale", State::Pending, Mode::Rename, &[&stale], NOW - 3600);
        let due = fx.file("old-session", "x");
        let ripe = done_with_data(&fx, "ripe", NOW - WEEK);

        let out = fx.run(NOW, |env| run_apply(env, 0, None));

        assert_eq!(fx.state_of("stale"), State::Dropped);
        assert!(!due.exists(), "the due item was tombstoned");
        assert!(!ripe.dest.exists(), "the ripe tombstone expired");
        assert_eq!(
            (out.tombstoned, out.expired, out.next_store),
            (1, 1, fx.registry.stores().len())
        );
        assert!(out.failures.is_empty(), "{:?}", out.failures);
    }

    #[test]
    fn a_run_clears_a_leftover_staging_file_and_rotates_the_log() {
        let mut fx = fx("apply-rotate-leftover");
        let store = Store {
            name: "logs",
            roots: vec![fx.root.clone()],
            scan: crate::gc::logs::scan,
            action: Action::Rotate,
            limits: Limits {
                idle_days: 1,
                max_bytes: 0,
                retain_days: 7,
                ..Limits::default()
            },
        };
        fx.registry = Registry::new(fx.gc.clone()).with_store(store);
        let log = fx.file("collector.log", "some log text");
        let stale = staging_of(&log);
        fs::write(&stale, "half a copy").unwrap();

        let out = fx.run(NOW + 40 * 86_400, |env| run_apply(env, 0, None));

        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert_eq!(out.rotated, 1);
        assert_eq!(fs::metadata(&log).unwrap().len(), 0);
        assert!(!stale.exists());
        assert_eq!(
            fs::read_to_string(log.with_file_name("collector.log.1")).unwrap(),
            "some log text"
        );
    }

    #[test]
    fn budget_stops_between_stores_and_resumes() {
        let mut fx = with_store(fx("apply-budget"));
        let second = fx.registry.stores()[0].clone();
        fx.registry = fx.registry.clone().with_store(second);
        fx.file("old-one", "x");

        let first = fx.run(NOW, |env| run_apply(env, 0, Some(0)));
        let rest = fx.run(NOW, |env| run_apply(env, first.next_store, Some(0)));

        assert_eq!((first.next_store, rest.next_store), (1, 2));
    }

    #[test]
    fn dry_run_text_lists_anomalies() {
        let fx = fx("apply-render");
        plant_state(&fx, "lost", State::Done, NOW - 10);

        let text = render_anomalies(&fx.registry, &FixedClock(NOW));

        assert!(text.contains("Would be repaired"), "{text}");
    }
}
