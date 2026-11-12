// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Crash safe moves into the tombstone folder, and the reconcile that repairs a
//! move a crash cut short.
//!
//! A move appends a `pending` record, moves the data, then appends `done`. If
//! the process dies in between, the next apply run reads the records and the
//! disk and finishes or drops the move. The destination is authoritative: data
//! is never deleted from the source until it is safe at the destination.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::guard::{self, checked_remove_lock, FsOps};
use super::index::{self, Fingerprint, Mode, Record, State};
use super::lock::{with_run_lock, LockHook, RunGuard, RunOutcome, STALE_CLAIM_PREFIX};
use super::{Clock, Registry, RUN_LOCK_STALE};
use crate::common::time::epoch_secs_of;

/// A copy touches the run lock after this many copied entries.
pub const COPY_TOUCH_ENTRIES: usize = 1000;

/// What a move or a repair works with: proof of the lock, the registered
/// roots and the filesystem.
pub struct Env<'a> {
    pub guard: &'a RunGuard<'a>,
    pub registry: &'a Registry,
    pub fs: &'a dyn FsOps,
}

impl Env<'_> {
    pub fn now(&self) -> i64 {
        self.guard.now()
    }

    pub fn index_path(&self) -> PathBuf {
        index::index_path(self.registry.gc_dir())
    }

    pub fn append(&self, record: &Record) -> io::Result<()> {
        self.fs
            .append_index(&self.index_path(), &index::encode(record))
    }
}

/// One item to tombstone: the paths that move together under one id.
pub struct MoveRequest {
    pub paths: Vec<PathBuf>,
    pub reason: String,
    pub retain_days: u32,
}

/// What a move did. Only `Failed` means a rename, copy or removal went wrong.
#[derive(Debug, PartialEq, Eq)]
pub enum MoveOutcome {
    Moved {
        id: String,
    },
    /// Another run holds the lock; nothing changed.
    Busy,
    /// The move was not allowed or could not be recorded; the source is intact.
    Refused(String),
    /// The tombstone folder cannot be created, so nothing can be moved into it;
    /// the source is intact.
    Unavailable(String),
    Failed(String),
}

/// Why a rename step stopped.
enum RenameStop {
    CrossDevice,
    Stopped(MoveOutcome),
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn mtime_of(meta: &fs::Metadata) -> i64 {
    meta.modified().ok().and_then(epoch_secs_of).unwrap_or(0)
}

fn measure_into(path: &Path, total: &mut Fingerprint) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                measure_into(&entry.path(), total);
            }
        }
    } else {
        total.mtime = total.mtime.max(mtime_of(&meta));
        total.bytes += meta.len();
    }
}

/// The summed size and newest file mtime of `paths`, without following
/// symlinks. A folder's own mtime is left out because removing an entry from
/// it changes that time, which would make a partly removed source look new.
pub fn measure<'a>(paths: impl IntoIterator<Item = &'a PathBuf>) -> Fingerprint {
    let mut total = Fingerprint { bytes: 0, mtime: 0 };
    for path in paths {
        measure_into(path, &mut total);
    }
    total
}

/// Copies a file, a symlink or a folder tree, touching the lock as it goes.
pub fn copy_tree(guard: &RunGuard, from: &Path, to: &Path) -> io::Result<()> {
    copy_entry(guard, from, to, &mut 0)
}

fn copy_entry(guard: &RunGuard, from: &Path, to: &Path, copied: &mut usize) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    *copied += 1;
    if copied.is_multiple_of(COPY_TOUCH_ENTRIES) {
        guard.touch().map_err(io::Error::other)?;
    }
    if meta.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_entry(guard, &entry.path(), &to.join(entry.file_name()), copied)?;
        }
        sync_dir(to);
    } else if meta.file_type().is_symlink() {
        copy_link(from, to)?;
    } else {
        // Copy by hand: `fs::copy` carries a read-only mode over, and the
        // mtime and the sync both need a writable handle.
        let mut src = fs::File::open(from)?;
        let mut dst = fs::File::create(to)?;
        io::copy(&mut src, &mut dst)?;
        dst.set_modified(meta.modified()?)?;
        dst.sync_all()?;
        drop(dst);
        fs::set_permissions(to, meta.permissions())?;
    }
    Ok(())
}

/// Flushes a folder's entries to disk. Best effort: not every platform lets a
/// folder be opened for sync.
pub(crate) fn sync_dir(dir: &Path) {
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

#[cfg(unix)]
fn copy_link(from: &Path, to: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(from)?, to)
}

#[cfg(not(unix))]
fn copy_link(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::other(
        "symlinks cannot be copied on this platform",
    ))
}

#[cfg(unix)]
fn same_filesystem(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_filesystem(_a: &Path, _b: &Path) -> bool {
    true
}

fn date_folder(now: i64) -> String {
    chrono::DateTime::from_timestamp(now, 0).map_or_else(
        || "unknown-date".to_string(),
        |when| when.format("%Y-%m-%d").to_string(),
    )
}

fn short_hash(path: &Path) -> String {
    let digest = Sha256::digest(path.to_string_lossy().as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// The first id for `first` at time `now` whose destination is free.
fn fresh_id(date_dir: &Path, first: &Path, now: i64) -> (String, PathBuf) {
    // Second resolution is enough: a clash adds a counter.
    let base = format!("{}-{}", now * 1000, short_hash(first));
    let mut id = base.clone();
    let mut counter = 1;
    loop {
        let dest = date_dir.join(&id);
        let taken = exists(&dest) || exists(&Record::tmp_of(&dest));
        if !taken {
            return (id, dest);
        }
        id = format!("{base}-{counter}");
        counter += 1;
    }
}

fn refused(what: &str, err: impl std::fmt::Display) -> MoveOutcome {
    MoveOutcome::Refused(format!("{what}: {err}"))
}

/// Checks the request and builds its `pending` record.
fn prepare(env: &Env, req: &MoveRequest, mode: Option<Mode>) -> Result<Record, MoveOutcome> {
    let roots = env.registry.store_roots();
    let mut missing = Vec::new();
    for path in &req.paths {
        match guard::validate(&roots, path) {
            Ok(true) => {}
            Ok(false) => missing.push(path.clone()),
            Err(err) => return Err(MoveOutcome::Refused(err.to_string())),
        }
    }
    let present: Vec<&PathBuf> = req.paths.iter().filter(|p| !missing.contains(p)).collect();
    let Some(first) = present.first() else {
        return Err(MoveOutcome::Refused(
            "nothing to move: every path is already gone".into(),
        ));
    };
    let mut names: Vec<_> = req.paths.iter().map(|p| p.file_name()).collect();
    names.sort();
    if names.windows(2).any(|pair| pair[0] == pair[1]) || names.contains(&None) {
        return Err(MoveOutcome::Refused(
            "the paths of one item need distinct names".into(),
        ));
    }
    let now = env.now();
    let date_dir = env.registry.tombstones_dir().join(date_folder(now));
    fs::create_dir_all(&date_dir).map_err(|e| {
        MoveOutcome::Unavailable(format!("cannot create the tombstone folder: {e}"))
    })?;
    let (id, dest) = fresh_id(&date_dir, first, now);
    let fingerprint = measure(present.iter().copied());
    let mode = mode.unwrap_or(if present.iter().all(|p| same_filesystem(p, &date_dir)) {
        Mode::Rename
    } else {
        Mode::Copy
    });
    Ok(Record {
        id,
        state: State::Pending,
        at: now,
        paths: req.paths.clone(),
        dest,
        size: fingerprint.bytes,
        reason: req.reason.clone(),
        tombstoned_at: now,
        retain_days: req.retain_days,
        mode,
        source_mtime: fingerprint.mtime,
        fingerprint: None,
        missing,
    })
}

/// Moves each member that is still at its source and not yet at its
/// destination. Members already moved are left alone.
fn rename_members(env: &Env, rec: &Record) -> Result<(), RenameStop> {
    let stop = |outcome| RenameStop::Stopped(outcome);
    fs::create_dir_all(&rec.dest).map_err(|e| {
        stop(MoveOutcome::Failed(format!(
            "cannot create the tombstone folder: {e}"
        )))
    })?;
    for path in rec.paths.iter().filter(|p| !rec.missing.contains(p)) {
        let to = rec.member_dest(path);
        if exists(&to) || !exists(path) {
            continue;
        }
        env.guard
            .verify()
            .map_err(|e| stop(refused("the run lock was lost", e)))?;
        match env.fs.rename(env.guard, path, &to) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::CrossesDevices => {
                return Err(RenameStop::CrossDevice)
            }
            Err(err) => {
                return Err(stop(MoveOutcome::Failed(format!(
                    "could not move {}: {err}",
                    path.display()
                ))))
            }
        }
    }
    Ok(())
}

fn discard(env: &Env, path: &Path) {
    // Best effort: a leftover staging folder is cleaned by the next reconcile.
    let _ = env.fs.remove(env.guard, path);
}

/// Copy mode: stage a copy, rename it into place, record `copied`, then remove
/// the sources. The source stays until the copy is whole and unchanged.
fn copy_members(env: &Env, rec: &Record) -> Result<(), MoveOutcome> {
    let present: Vec<&PathBuf> = rec.paths.iter().filter(|p| exists(p)).collect();
    let tmp = rec.tmp_dest();
    let before = measure(present.iter().copied());
    let copied = fs::create_dir_all(&tmp).and_then(|()| {
        for path in &present {
            env.fs.copy(
                env.guard,
                path,
                &tmp.join(path.file_name().unwrap_or_default()),
            )?;
            env.fs.after_copy(path);
        }
        Ok(())
    });
    if let Err(err) = copied {
        discard(env, &tmp);
        return Err(MoveOutcome::Failed(format!("could not copy: {err}")));
    }
    if measure(present.iter().copied()).changed_since(&before) {
        discard(env, &tmp);
        let _ = env.append(&rec.with_state(State::Dropped, env.now()));
        return Err(MoveOutcome::Failed(
            "the source changed while it was copied".into(),
        ));
    }
    if let Err(err) = env.guard.verify() {
        discard(env, &tmp);
        return Err(refused("the run lock was lost", err));
    }
    if let Err(err) = env.fs.rename(env.guard, &tmp, &rec.dest) {
        discard(env, &tmp);
        return Err(MoveOutcome::Failed(format!(
            "could not put the copy in place: {err}"
        )));
    }
    if let Some(parent) = rec.dest.parent() {
        sync_dir(parent);
    }
    let copied_record = Record {
        fingerprint: Some(before),
        ..rec.with_state(State::Copied, env.now())
    };
    env.append(&copied_record)
        .map_err(|e| refused("cannot write the tombstone index", e))?;
    env.guard
        .verify()
        .map_err(|e| refused("the run lock was lost", e))?;
    for path in present {
        env.fs.remove(env.guard, path).map_err(|e| {
            MoveOutcome::Failed(format!("could not remove {}: {e}", path.display()))
        })?;
    }
    Ok(())
}

fn try_move(env: &Env, req: &MoveRequest, mode: Option<Mode>) -> Result<String, MoveOutcome> {
    let mut rec = prepare(env, req, mode)?;
    env.append(&rec)
        .map_err(|e| refused("cannot write the tombstone index", e))?;
    if rec.mode == Mode::Rename {
        match rename_members(env, &rec) {
            Ok(()) => {}
            Err(RenameStop::Stopped(outcome)) => return Err(outcome),
            Err(RenameStop::CrossDevice) => {
                rec.mode = Mode::Copy;
                env.append(&rec)
                    .map_err(|e| refused("cannot write the tombstone index", e))?;
            }
        }
    }
    if rec.mode == Mode::Copy {
        copy_members(env, &rec)?;
    }
    // If this append fails the data is already safe; reconcile writes `done`.
    let _ = env.append(&rec.with_state(State::Done, env.now()));
    Ok(rec.id)
}

/// Tombstones the item for a caller that already holds the run lock.
pub fn move_locked(env: &Env, req: &MoveRequest, mode: Option<Mode>) -> MoveOutcome {
    match try_move(env, req, mode) {
        Ok(id) => MoveOutcome::Moved { id },
        Err(outcome) => outcome,
    }
}

/// Tombstones the item for a caller outside a gc run: takes the lock first,
/// waiting up to `wait`.
pub fn move_to_tombstone(
    registry: &Registry,
    fs: &dyn FsOps,
    (clock, hook): (&dyn Clock, &dyn LockHook),
    wait: Duration,
    req: &MoveRequest,
    mode: Option<Mode>,
) -> MoveOutcome {
    let ran = with_run_lock(registry.gc_dir(), wait, clock, hook, |guard| {
        move_locked(
            &Env {
                guard,
                registry,
                fs,
            },
            req,
            mode,
        )
    });
    match ran {
        Ok(RunOutcome::Ran(outcome)) => outcome,
        Ok(RunOutcome::AlreadyRunning) => MoveOutcome::Busy,
        Err(err) => refused("cannot take the gc lock", err),
    }
}

/// What reconcile found: repairs made (or, in a dry run, repairs it would
/// make) and items a person has to look at.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Findings {
    pub fixes: Vec<String>,
    pub attention: Vec<String>,
}

/// The repair for one record.
enum Fix {
    Nothing,
    Mark(State),
    /// Remove these paths, then record `then`.
    Cleanup {
        remove: Vec<PathBuf>,
        then: State,
    },
    FinishRenames,
    Attention(String),
}

fn diagnose(rec: &Record, now: i64) -> Fix {
    let src_any = rec.paths.iter().any(|p| exists(p));
    let dst_any = rec.paths.iter().any(|p| exists(&rec.member_dest(p)));
    let present_dest = || {
        if exists(&rec.dest) {
            vec![rec.dest.clone()]
        } else {
            Vec::new()
        }
    };
    match (rec.state, rec.mode) {
        (State::Pending, _) if now - rec.at < RUN_LOCK_STALE.as_secs().cast_signed() => {
            Fix::Nothing
        }
        (State::Pending, Mode::Rename) if dst_any => Fix::FinishRenames,
        (State::Pending, Mode::Rename) => Fix::Mark(State::Dropped),
        (State::Pending, Mode::Copy) => match (src_any, dst_any) {
            (false, true) => Fix::Mark(State::Done),
            (true, true) => Fix::Cleanup {
                remove: vec![rec.dest.clone(), rec.tmp_dest()],
                then: State::Dropped,
            },
            (_, false) => Fix::Cleanup {
                remove: vec![rec.tmp_dest()],
                then: State::Dropped,
            },
        },
        (State::Copied, _) => match (src_any, dst_any) {
            (false, true) => Fix::Mark(State::Done),
            (true, true) => finish_copied(rec),
            (true, false) => Fix::Attention("copied, but the tombstone copy is gone".into()),
            (false, false) => Fix::Mark(State::Lost),
        },
        (State::Done, _) if !dst_any => Fix::Mark(State::Lost),
        (State::Expiring, _) => Fix::Cleanup {
            remove: present_dest(),
            then: State::Expired,
        },
        (State::Restoring, _) => match (src_any, dst_any) {
            (true, true) => {
                Fix::Attention("restoring, and both the original and the tombstone exist".into())
            }
            (true, false) => Fix::Cleanup {
                remove: present_dest(),
                then: State::Restored,
            },
            (false, true) => Fix::Mark(State::Done),
            (false, false) => Fix::Mark(State::Lost),
        },
        _ => Fix::Nothing,
    }
}

/// A `copied` record with both sides present: remove the source unless it
/// holds data that was not in the copy.
fn finish_copied(rec: &Record) -> Fix {
    let sources: Vec<PathBuf> = rec.paths.iter().filter(|p| exists(p)).cloned().collect();
    let changed = match &rec.fingerprint {
        Some(recorded) => measure(&sources).grew_since(recorded),
        None => true,
    };
    if changed {
        Fix::Attention("the source changed after it was copied, so it was kept".into())
    } else {
        Fix::Cleanup {
            remove: sources,
            then: State::Done,
        }
    }
}

/// Everything reconcile can see, decided before anything is changed.
struct Survey {
    fixes: Vec<(Record, Fix)>,
    orphans: Vec<PathBuf>,
    stale_claims: Vec<PathBuf>,
}

fn entries(dir: &Path) -> Vec<fs::DirEntry> {
    fs::read_dir(dir)
        .map(|rd| rd.flatten().collect())
        .unwrap_or_default()
}

fn survey(registry: &Registry, now: i64) -> Survey {
    let all = index::read(&index::index_path(registry.gc_dir()));
    let known: Vec<String> = all.iter().map(|r| r.id.clone()).collect();
    let fixes = index::latest(all)
        .into_iter()
        .map(|rec| {
            let fix = diagnose(&rec, now);
            (rec, fix)
        })
        .filter(|(_, fix)| !matches!(fix, Fix::Nothing))
        .collect();
    let orphans = entries(&registry.tombstones_dir())
        .into_iter()
        .filter(|date| date.path().is_dir())
        .flat_map(|date| entries(&date.path()))
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".tmp").unwrap_or(&name);
            !known.iter().any(|known| known == id)
        })
        .map(|entry| entry.path())
        .collect();
    let stale_claims = entries(registry.gc_dir())
        .into_iter()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(STALE_CLAIM_PREFIX)
                && entry.metadata().is_ok_and(|meta| {
                    now - mtime_of(&meta) >= RUN_LOCK_STALE.as_secs().cast_signed()
                })
        })
        .map(|entry| entry.path())
        .collect();
    Survey {
        fixes,
        orphans,
        stale_claims,
    }
}

fn describe(rec: &Record, fix: &Fix) -> String {
    match fix {
        Fix::Mark(state) | Fix::Cleanup { then: state, .. } => {
            format!("{}: mark {state:?}", rec.id).to_lowercase()
        }
        Fix::FinishRenames => format!("{}: finish the move", rec.id),
        Fix::Attention(why) => format!("{}: {why}", rec.id),
        Fix::Nothing => String::new(),
    }
}

fn orphan_message(path: &Path) -> String {
    format!(
        "{} is in the tombstone folder but has no index record",
        path.display()
    )
}

/// What a reconcile would do, without taking the lock or changing anything.
pub fn anomalies(registry: &Registry, now: i64) -> Findings {
    let survey = survey(registry, now);
    let mut out = Findings::default();
    for (rec, fix) in &survey.fixes {
        let line = describe(rec, fix);
        if matches!(fix, Fix::Attention(_)) {
            out.attention.push(line);
        } else {
            out.fixes.push(line);
        }
    }
    out.attention
        .extend(survey.orphans.iter().map(|p| orphan_message(p)));
    out.fixes.extend(
        survey
            .stale_claims
            .iter()
            .map(|p| format!("remove the stale lock claim {}", p.display())),
    );
    out
}

fn apply_fix(env: &Env, rec: &Record, fix: Fix, out: &mut Findings) {
    let now = env.now();
    let note = |out: &mut Findings, text: String| out.attention.push(format!("{}: {text}", rec.id));
    match fix {
        Fix::Nothing => {}
        Fix::Attention(why) => note(out, why),
        Fix::Mark(state) => record_state(env, rec, state, out),
        Fix::Cleanup { remove, then } => {
            for path in remove.iter().filter(|p| exists(p)) {
                if let Err(err) = env.fs.remove(env.guard, path) {
                    return note(out, format!("could not remove {}: {err}", path.display()));
                }
            }
            record_state(env, rec, then, out);
        }
        Fix::FinishRenames => match rename_members(env, rec) {
            Ok(()) => {
                let missing = rec
                    .paths
                    .iter()
                    .filter(|p| !exists(p) && !exists(&rec.member_dest(p)))
                    .cloned()
                    .collect();
                let done = Record {
                    missing,
                    ..rec.with_state(State::Done, now)
                };
                record(env, rec, &done, out);
            }
            Err(_) => note(out, "could not finish the move".into()),
        },
    }
}

fn record_state(env: &Env, rec: &Record, state: State, out: &mut Findings) {
    record(env, rec, &rec.with_state(state, env.now()), out);
}

fn record(env: &Env, old: &Record, new: &Record, out: &mut Findings) {
    match env.append(new) {
        Ok(()) => out.fixes.push(describe(old, &Fix::Mark(new.state))),
        Err(err) => out
            .attention
            .push(format!("{}: cannot write the index: {err}", old.id)),
    }
}

/// Repairs what a crashed run left behind. Runs at the start of every apply
/// run. Never deletes an unknown entry in the tombstone folder.
pub fn reconcile(env: &Env) -> Findings {
    let survey = survey(env.registry, env.now());
    let mut out = Findings::default();
    for (rec, fix) in survey.fixes {
        apply_fix(env, &rec, fix, &mut out);
    }
    for claim in survey.stale_claims {
        match checked_remove_lock(env.registry.gc_dir(), &claim) {
            Ok(()) => out
                .fixes
                .push(format!("removed the stale lock claim {}", claim.display())),
            Err(err) => out.attention.push(err.to_string()),
        }
    }
    out.attention
        .extend(survey.orphans.iter().map(|p| orphan_message(p)));
    out
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::gc::guard::{GuardError, RealFs};
    use crate::gc::lock::NoHook;
    use crate::gc::{Action, FixedClock, Limits, Store};
    use std::cell::{Cell, RefCell};
    use std::time::{Duration, UNIX_EPOCH};

    pub(in crate::gc) const NOW: i64 = 1_800_000_000;
    pub(in crate::gc) const STALE: i64 = 3600;

    pub(in crate::gc) struct Fx {
        pub(in crate::gc) root: PathBuf,
        pub(in crate::gc) gc: PathBuf,
        pub(in crate::gc) registry: Registry,
        pub(in crate::gc) fs: TestFs,
    }

    type AfterCopy = Box<dyn Fn(&Path)>;

    pub(in crate::gc) struct TestFs {
        pub(in crate::gc) real: RealFs,
        pub(in crate::gc) rename_error: Cell<Option<io::ErrorKind>>,
        pub(in crate::gc) rename_error_once: Cell<bool>,
        pub(in crate::gc) copy_fails: Cell<bool>,
        pub(in crate::gc) append_fails: Cell<bool>,
        pub(in crate::gc) after_copy: RefCell<Option<AfterCopy>>,
    }

    impl FsOps for TestFs {
        fn rename(&self, guard: &RunGuard, from: &Path, to: &Path) -> io::Result<()> {
            if let Some(kind) = self.rename_error.get() {
                if self.rename_error_once.get() {
                    self.rename_error.set(None);
                }
                return Err(io::Error::from(kind));
            }
            self.real.rename(guard, from, to)
        }

        fn copy(&self, guard: &RunGuard, from: &Path, to: &Path) -> io::Result<()> {
            if self.copy_fails.get() {
                fs::create_dir_all(to)?;
                return Err(io::Error::other("copy failed midway"));
            }
            self.real.copy(guard, from, to)
        }

        fn after_copy(&self, source: &Path) {
            if let Some(hook) = self.after_copy.borrow().as_ref() {
                hook(source);
            }
        }

        fn append_index(&self, index: &Path, line: &str) -> io::Result<()> {
            if self.append_fails.get() {
                return Err(io::Error::other("no space left on device"));
            }
            self.real.append_index(index, line)
        }

        fn remove(&self, guard: &RunGuard, path: &Path) -> Result<(), GuardError> {
            self.real.remove(guard, path)
        }
    }

    pub(in crate::gc) fn fx(tag: &str) -> Fx {
        let base = crate::common::test_support::scratch_dir(tag);
        let root = base.join("root");
        let gc = base.join("gc");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&gc).unwrap();
        let store = Store {
            name: "test",
            roots: vec![root.clone()],
            scan: |_, _| Vec::new(),
            action: Action::Tombstone,
            limits: Limits::default(),
        };
        let registry = Registry::new(gc.clone()).with_store(store);
        let fs = TestFs {
            real: RealFs::new(&registry),
            rename_error: Cell::new(None),
            rename_error_once: Cell::new(false),
            copy_fails: Cell::new(false),
            append_fails: Cell::new(false),
            after_copy: RefCell::new(None),
        };
        Fx {
            root,
            gc,
            registry,
            fs,
        }
    }

    impl Fx {
        pub(in crate::gc) fn run<T>(&self, now: i64, f: impl FnOnce(&Env) -> T) -> T {
            let clock = FixedClock(now);
            let ran = with_run_lock(&self.gc, Duration::ZERO, &clock, &NoHook, |guard| {
                f(&Env {
                    guard,
                    registry: &self.registry,
                    fs: &self.fs,
                })
            });
            match ran.unwrap() {
                RunOutcome::Ran(value) => value,
                RunOutcome::AlreadyRunning => panic!("lock busy"),
            }
        }

        pub(in crate::gc) fn file(&self, name: &str, body: &str) -> PathBuf {
            let path = self.root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();
            path
        }

        pub(in crate::gc) fn mv(&self, paths: &[&Path], mode: Option<Mode>) -> MoveOutcome {
            let req = MoveRequest {
                paths: paths.iter().map(|p| p.to_path_buf()).collect(),
                reason: "idle".into(),
                retain_days: 7,
            };
            self.run(NOW, |env| move_locked(env, &req, mode))
        }

        pub(in crate::gc) fn records(&self) -> Vec<Record> {
            index::latest(index::read(&index::index_path(&self.gc)))
        }

        pub(in crate::gc) fn state_of(&self, id: &str) -> State {
            self.records()
                .into_iter()
                .find(|r| r.id == id)
                .unwrap()
                .state
        }

        /// Writes a record by hand, as a crashed run would have left it.
        pub(in crate::gc) fn plant(
            &self,
            id: &str,
            state: State,
            mode: Mode,
            paths: &[&Path],
            at: i64,
        ) -> Record {
            let dest = self.registry.tombstones_dir().join("2027-01-01").join(id);
            let rec = Record {
                id: id.into(),
                state,
                at,
                paths: paths.iter().map(|p| p.to_path_buf()).collect(),
                dest,
                size: 1,
                reason: "idle".into(),
                tombstoned_at: at,
                retain_days: 7,
                mode,
                source_mtime: 1,
                fingerprint: None,
                missing: Vec::new(),
            };
            index::append_line(&index::index_path(&self.gc), &index::encode(&rec)).unwrap();
            rec
        }

        pub(in crate::gc) fn reconcile(&self, now: i64) -> Findings {
            self.run(now, reconcile)
        }
    }

    pub(in crate::gc) fn set_mtime(path: &Path, secs: i64) {
        let when = UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).unwrap());
        fs::File::open(path).unwrap().set_modified(when).unwrap();
    }

    fn moved_id(outcome: MoveOutcome) -> String {
        match outcome {
            MoveOutcome::Moved { id } => id,
            other => panic!("expected a move, got {other:?}"),
        }
    }

    #[test]
    fn tombstone_move_writes_done_record() {
        let fx = fx("tomb-done");
        let file = fx.file("a.txt", "hello");

        let id = moved_id(fx.mv(&[&file], None));

        let rec = fx.records().remove(0);
        assert_eq!((rec.id.as_str(), rec.state), (id.as_str(), State::Done));
        assert_eq!(
            (rec.reason.as_str(), rec.size, rec.tombstoned_at),
            ("idle", 5, NOW)
        );
        assert_eq!(rec.paths, std::slice::from_ref(&file));
        assert_eq!(fs::read_to_string(rec.member_dest(&file)).unwrap(), "hello");
        assert!(!file.exists());
    }

    #[test]
    fn move_refuses_path_outside_registered_root() {
        let fx = fx("tomb-outside");
        let outside = fx.gc.parent().unwrap().join("elsewhere.txt");
        fs::write(&outside, "x").unwrap();

        let outcome = fx.mv(&[&outside], None);

        assert!(matches!(outcome, MoveOutcome::Refused(_)), "{outcome:?}");
        assert!(outside.exists() && fx.records().is_empty());
    }

    #[test]
    fn blocked_destination_keeps_source() {
        let fx = fx("tomb-blocked");
        let file = fx.file("a.txt", "x");
        fx.fs
            .rename_error
            .set(Some(io::ErrorKind::PermissionDenied));

        let outcome = fx.mv(&[&file], Some(Mode::Rename));

        assert!(matches!(outcome, MoveOutcome::Failed(_)), "{outcome:?}");
        assert!(file.exists());
    }

    #[test]
    fn id_collision_does_not_overwrite() {
        let fx = fx("tomb-collision");
        let file = fx.file("a.txt", "first");
        let first = moved_id(fx.mv(&[&file], None));
        fx.file("a.txt", "second");

        let second = moved_id(fx.mv(&[&file], None));

        assert_ne!(first, second);
        let bodies: Vec<String> = fx
            .records()
            .iter()
            .map(|r| fs::read_to_string(r.member_dest(&file)).unwrap())
            .collect();
        assert_eq!(bodies, ["first", "second"]);
    }

    #[test]
    fn index_append_error_keeps_source() {
        let fx = fx("tomb-append");
        let file = fx.file("a.txt", "x");
        fx.fs.append_fails.set(true);

        let outcome = fx.mv(&[&file], None);

        assert!(matches!(outcome, MoveOutcome::Refused(_)), "{outcome:?}");
        assert!(file.exists());
    }

    #[test]
    fn copy_mode_move_completes() {
        let fx = fx("tomb-copy");
        let dir = fx.file("d/inner.txt", "deep");
        let folder = dir.parent().unwrap().to_path_buf();

        let id = moved_id(fx.mv(&[&folder], Some(Mode::Copy)));

        let rec = fx.records().into_iter().find(|r| r.id == id).unwrap();
        assert_eq!(rec.state, State::Done);
        assert_eq!(
            fs::read_to_string(rec.member_dest(&folder).join("inner.txt")).unwrap(),
            "deep"
        );
        assert!(!folder.exists() && !rec.tmp_dest().exists());
    }

    #[cfg(unix)]
    #[test]
    fn copy_mode_moves_a_read_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let fx = fx("tomb-copy-readonly");
        let file = fx.file("ro.txt", "locked");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();

        let id = moved_id(fx.mv(&[&file], Some(Mode::Copy)));

        let rec = fx.records().into_iter().find(|r| r.id == id).unwrap();
        let copy = rec.member_dest(&file);
        assert_eq!(fs::read_to_string(&copy).unwrap(), "locked");
        assert_eq!(
            fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
            0o444
        );
        assert!(!file.exists());
    }

    #[test]
    fn a_path_inside_the_tombstone_folder_is_not_a_move_source() {
        let fx = fx("tomb-source-in-tombstones");
        let inside = fx.registry.tombstones_dir().join("2026-11-04").join("x");
        fs::create_dir_all(&inside).unwrap();

        let outcome = fx.mv(&[&inside], None);

        assert!(matches!(outcome, MoveOutcome::Refused(_)), "{outcome:?}");
        assert!(inside.exists());
    }

    #[test]
    fn failed_cross_fs_copy_keeps_source() {
        let fx = fx("tomb-copy-fail");
        let file = fx.file("a.txt", "x");
        fx.fs.copy_fails.set(true);

        let outcome = fx.mv(&[&file], Some(Mode::Copy));

        assert!(matches!(outcome, MoveOutcome::Failed(_)), "{outcome:?}");
        let rec = fx.records().remove(0);
        assert!(file.exists() && !rec.tmp_dest().exists());
    }

    #[test]
    fn copy_aborts_when_source_changes() {
        let fx = fx("tomb-copy-changes");
        let file = fx.file("a.txt", "x");
        *fx.fs.after_copy.borrow_mut() = Some(Box::new(|source| {
            fs::write(source, "grown while copying").unwrap();
            set_mtime(source, NOW + 5);
        }));

        let outcome = fx.mv(&[&file], Some(Mode::Copy));

        assert!(matches!(outcome, MoveOutcome::Failed(_)), "{outcome:?}");
        let rec = fx.records().remove(0);
        assert!(file.exists() && !rec.dest.exists() && !rec.tmp_dest().exists());
    }

    #[test]
    fn copy_mode_rechecks_token_before_source_removal() {
        let fx = fx("tomb-copy-token");
        let file = fx.file("a.txt", "x");
        let lock = fx.gc.join(crate::gc::lock::LOCK_DIR);
        *fx.fs.after_copy.borrow_mut() = Some(Box::new(move |_| {
            let token = lock.join(crate::gc::lock::TOKEN_FILE);
            fs::rename(&token, token.with_extension("gone")).unwrap();
        }));

        let outcome = fx.mv(&[&file], Some(Mode::Copy));

        assert!(matches!(outcome, MoveOutcome::Refused(_)), "{outcome:?}");
        assert!(file.exists());
    }

    #[test]
    fn exdev_rename_falls_back_to_copy() {
        let fx = fx("tomb-exdev");
        let file = fx.file("a.txt", "x");
        fx.fs.rename_error.set(Some(io::ErrorKind::CrossesDevices));
        fx.fs.rename_error_once.set(true);

        let id = moved_id(fx.mv(&[&file], Some(Mode::Rename)));

        let rec = fx.records().into_iter().find(|r| r.id == id).unwrap();
        assert_eq!((rec.state, rec.mode), (State::Done, Mode::Copy));
        assert!(!file.exists() && rec.member_dest(&file).exists());
    }

    #[test]
    fn move_to_tombstone_reports_busy_while_locked() {
        let fx = fx("tomb-busy");
        let file = fx.file("a.txt", "x");
        let req = MoveRequest {
            paths: vec![file.clone()],
            reason: "idle".into(),
            retain_days: 7,
        };

        // The lock folder carries the real file time, so the clock must match it.
        let real_now = crate::common::time::now_secs();
        let outcome = fx.run(real_now, |_| {
            move_to_tombstone(
                &fx.registry,
                &fx.fs,
                (&FixedClock(real_now), &NoHook),
                Duration::ZERO,
                &req,
                None,
            )
        });

        assert_eq!(outcome, MoveOutcome::Busy);
        assert!(file.exists());
    }

    #[test]
    fn crash_after_pending_keeps_source() {
        let fx = fx("rec-pending");
        let file = fx.file("a.txt", "x");
        let rec = fx.plant("p1", State::Pending, Mode::Rename, &[&file], NOW - STALE);

        fx.reconcile(NOW);

        assert_eq!(fx.state_of("p1"), State::Dropped);
        assert!(file.exists() && !rec.dest.exists());
    }

    #[test]
    fn crash_before_done_is_reconciled() {
        let fx = fx("rec-before-done");
        let file = fx.file("a.txt", "x");
        let rec = fx.plant("p1", State::Pending, Mode::Rename, &[&file], NOW - STALE);
        fs::create_dir_all(&rec.dest).unwrap();
        fs::rename(&file, rec.member_dest(&file)).unwrap();

        fx.reconcile(NOW);

        assert_eq!(fx.state_of("p1"), State::Done);
    }

    #[test]
    fn rename_mode_keeps_destination_when_source_recreated() {
        let fx = fx("rec-recreated");
        let file = fx.file("a.txt", "old");
        let rec = fx.plant("p1", State::Pending, Mode::Rename, &[&file], NOW - STALE);
        fs::create_dir_all(&rec.dest).unwrap();
        fs::rename(&file, rec.member_dest(&file)).unwrap();
        fx.file("a.txt", "new");

        fx.reconcile(NOW);

        assert_eq!(fx.state_of("p1"), State::Done);
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        assert_eq!(fs::read_to_string(rec.member_dest(&file)).unwrap(), "old");
    }

    #[test]
    fn reconcile_skips_young_pending() {
        let fx = fx("rec-young");
        let file = fx.file("a.txt", "x");
        fx.plant(
            "p1",
            State::Pending,
            Mode::Rename,
            &[&file],
            NOW - STALE + 1,
        );

        fx.reconcile(NOW);

        assert_eq!(fx.state_of("p1"), State::Pending);
    }

    #[test]
    fn copy_pending_both_present_drops_destination() {
        let fx = fx("rec-copy-both");
        let file = fx.file("a.txt", "x");
        let rec = fx.plant("p1", State::Pending, Mode::Copy, &[&file], NOW - STALE);
        fs::create_dir_all(&rec.dest).unwrap();
        fs::write(rec.member_dest(&file), "x").unwrap();

        fx.reconcile(NOW);

        assert_eq!(fx.state_of("p1"), State::Dropped);
        assert!(file.exists() && !rec.dest.exists());
    }

    #[test]
    fn copy_pending_source_only_drops_tmp() {
        let fx = fx("rec-copy-src");
        let file = fx.file("a.txt", "x");
        let rec = fx.plant("p1", State::Pending, Mode::Copy, &[&file], NOW - STALE);

        fx.reconcile(NOW);

        assert_eq!(fx.state_of("p1"), State::Dropped);
        assert!(file.exists() && !rec.tmp_dest().exists());
    }

    #[test]
    fn orphan_tmp_is_removed() {
        let fx = fx("rec-orphan-tmp");
        let file = fx.file("a.txt", "x");
        let rec = fx.plant("p1", State::Pending, Mode::Copy, &[&file], NOW - STALE);
        fs::create_dir_all(rec.tmp_dest()).unwrap();
        fs::write(rec.tmp_dest().join("a.txt"), "partial").unwrap();

        fx.reconcile(NOW);

        assert!(!rec.tmp_dest().exists() && file.exists());
    }

    #[test]
    fn copy_pending_destination_only_is_done() {
        let fx = fx("rec-copy-dst");
        let file = fx.file("a.txt", "x");
        let rec = fx.plant("p1", State::Pending, Mode::Copy, &[&file], NOW - STALE);
        fs::create_dir_all(&rec.dest).unwrap();
        fs::rename(&file, rec.member_dest(&file)).unwrap();

        fx.reconcile(NOW);

        assert_eq!(fx.state_of("p1"), State::Done);
    }

    #[test]
    fn copied_with_partial_source_finishes_the_move() {
        let fx = fx("rec-copied-partial");
        let one = fx.file("d/one.txt", "1");
        let folder = one.parent().unwrap().to_path_buf();
        fx.file("d/two.txt", "2");
        let mut rec = fx.plant("p1", State::Copied, Mode::Copy, &[&folder], NOW - 10);
        rec.fingerprint = Some(measure([&folder]));
        index::append_line(&index::index_path(&fx.gc), &index::encode(&rec)).unwrap();
        fs::create_dir_all(&rec.dest).unwrap();
        fx.run(NOW, |env| {
            copy_tree(env.guard, &folder, &rec.member_dest(&folder))
        })
        .unwrap();
        fs::rename(&one, fx.gc.join("moved-away")).unwrap();

        fx.reconcile(NOW);

        assert_eq!(fx.state_of("p1"), State::Done);
        assert!(!folder.exists());
        assert!(rec.member_dest(&folder).join("two.txt").exists());
    }

    #[test]
    fn copied_source_changed_is_kept() {
        let fx = fx("rec-copied-changed");
        let file = fx.file("a.txt", "x");
        let mut rec = fx.plant("p1", State::Copied, Mode::Copy, &[&file], NOW - 10);
        rec.fingerprint = Some(Fingerprint {
            bytes: 1,
            mtime: NOW - 100,
        });
        index::append_line(&index::index_path(&fx.gc), &index::encode(&rec)).unwrap();
        fs::create_dir_all(&rec.dest).unwrap();
        fs::write(rec.member_dest(&file), "x").unwrap();
        set_mtime(&file, NOW - 5);

        let found = fx.reconcile(NOW);

        assert!(file.exists() && fx.state_of("p1") == State::Copied);
        assert_eq!(found.attention.len(), 1);
    }

    #[test]
    fn missing_destination_is_marked_lost() {
        let fx = fx("rec-lost");
        let file = fx.file("a.txt", "x");
        fx.plant("p1", State::Done, Mode::Rename, &[&file], NOW - 10);

        fx.reconcile(NOW);

        assert_eq!(fx.state_of("p1"), State::Lost);
    }

    #[test]
    fn crash_between_expiring_and_expired_is_not_lost() {
        let fx = fx("rec-expiring-gone");
        let file = fx.file("a.txt", "x");
        fx.plant("p1", State::Expiring, Mode::Rename, &[&file], NOW - 10);

        fx.reconcile(NOW);

        assert_eq!(fx.state_of("p1"), State::Expired);
    }

    #[test]
    fn expiring_with_data_retries_delete() {
        let fx = fx("rec-expiring-data");
        let file = fx.file("a.txt", "x");
        let rec = fx.plant("p1", State::Expiring, Mode::Rename, &[&file], NOW - 10);
        fs::create_dir_all(&rec.dest).unwrap();
        fs::write(rec.member_dest(&file), "x").unwrap();

        fx.reconcile(NOW);

        assert_eq!(fx.state_of("p1"), State::Expired);
        assert!(!rec.dest.exists());
    }

    #[test]
    fn group_partial_crash_finishes_all_members() {
        let fx = fx("rec-group");
        let paths = [
            fx.file("x.db", "1"),
            fx.file("x.db-wal", "2"),
            fx.file("x.db-shm", "3"),
        ];
        let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        let rec = fx.plant("g1", State::Pending, Mode::Rename, &refs, NOW - STALE);
        fs::create_dir_all(&rec.dest).unwrap();
        fs::rename(&paths[0], rec.member_dest(&paths[0])).unwrap();

        fx.reconcile(NOW);

        assert_eq!(fx.state_of("g1"), State::Done);
        assert!(paths
            .iter()
            .all(|p| !p.exists() && rec.member_dest(p).exists()));
    }

    #[test]
    fn group_member_missing_finishes() {
        let fx = fx("rec-group-missing");
        let here = fx.file("x.db", "1");
        let gone = fx.root.join("x.db-wal");
        let rec = fx.plant(
            "g1",
            State::Pending,
            Mode::Rename,
            &[&here, &gone],
            NOW - STALE,
        );
        fs::create_dir_all(&rec.dest).unwrap();
        fs::rename(&here, rec.member_dest(&here)).unwrap();

        fx.reconcile(NOW);

        let done = fx.records().remove(0);
        assert_eq!((done.state, done.missing), (State::Done, vec![gone]));
    }

    #[test]
    fn orphan_destination_without_record_is_reported() {
        let fx = fx("rec-orphan");
        let orphan = fx.registry.tombstones_dir().join("2027-01-01/unknown-id");
        fs::create_dir_all(&orphan).unwrap();

        let found = fx.reconcile(NOW);

        assert!(orphan.exists());
        assert_eq!(found.attention.len(), 1);
    }

    #[test]
    fn stale_claim_folder_is_cleaned() {
        let fx = fx("rec-stale-claim");
        let old = fx.gc.join(".lock.stale-old");
        let young = fx.gc.join(".lock.stale-young");
        for (dir, age) in [(&old, STALE), (&young, STALE - 1)] {
            fs::create_dir_all(dir).unwrap();
            set_mtime(dir, NOW - age);
        }

        fx.reconcile(NOW);

        assert!(!old.exists() && young.exists());
    }

    #[test]
    fn dry_run_reconcile_changes_nothing() {
        let fx = fx("rec-dry");
        let file = fx.file("a.txt", "x");
        fx.plant("p1", State::Done, Mode::Rename, &[&file], NOW - 10);
        let before = fs::read(index::index_path(&fx.gc)).unwrap();

        let found = anomalies(&fx.registry, NOW);

        assert_eq!(found.fixes.len(), 1);
        assert_eq!(fs::read(index::index_path(&fx.gc)).unwrap(), before);
    }
}
