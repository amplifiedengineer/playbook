// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Session handoffs: the markdown one session leaves for the next. The write
//! side (`playbook handoff save`) and the read side (the SessionStart hook)
//! both key a handoff through `is_handoff_for`, so they can never disagree.
//!
//! Files live in `repos/<owner>/<repo>/handoff/<key>_<epoch>_<pid>.md`, or in
//! `runtime_root()/handoff/` outside a repo with an `origin` remote. A handoff
//! the hook has loaded is tombstoned by gc for the handoff grace period, so `show`
//! can still print it after the session start consumed it.

pub mod compose;

use crate::common::atomic::ensure_private_dir;
use crate::gc::guard::{FsOps, RealFs};
use crate::gc::index::{self, Record, State};
use crate::gc::lock::NoHook;
use crate::gc::tombstone::{self, MoveOutcome, MoveRequest};
use crate::gc::{Clock, Registry};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Age past which a handoff is stale: dropped without injecting.
const MAX_AGE_DAYS: u64 = 14;

/// Most handoffs one SessionStart injects, so a busy directory cannot flood
/// the context.
const MAX_INJECTED: usize = 3;

/// How long a start waits for a busy gc run before leaving the handoff for the
/// next start.
const LOCK_WAIT: Duration = Duration::from_millis(500);
const SECS_PER_DAY: u64 = 86_400;
const OTHER_DIRS_SHOWN: usize = 5;

/// The handoff folder for the repo this process runs in, or the old global
/// folder outside a repo with an `origin` remote. See [`crate::storage`].
pub fn root() -> PathBuf {
    crate::storage::handoff_root(&current_slug(), None)
}

/// The folder for the directory `dir` names, which may sit in another repo
/// than the current one.
fn root_for_dir(dir: &str, slug: &str) -> PathBuf {
    let given = Path::new(dir);
    let absolute = if given.is_absolute() {
        given.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(given)
    };
    crate::storage::handoff_root(slug, Some(&absolute))
}

/// Longest readable slug kept in a key. The file name adds the hash, an epoch,
/// a pid, a tag, and `.md`, and a file name tops out at 255 bytes, so a very
/// deep path keeps its tail, the most specific part.
const MAX_SLUG_LEN: usize = 150;

/// Separates the parts of a handoff file name. A slug holds only letters,
/// digits, and `-`, so it can never contain this character, and neither can
/// the hash, the epoch, the pid, or the tag.
const NAME_SEPARATOR: char = '_';

/// Hex characters of the path hash that end a key.
const HASH_HEX_LEN: usize = 8;

/// `<slug>_<hash>`: the readable slug of `path` plus a short hash of the full
/// path. Two directories whose names differ only in punctuation share a slug
/// but never a hash. Both sides go through `key`, so they still agree. An
/// empty path has no key.
fn key(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let slug = crate::cc::project_slug(path);
    let tail = &slug[slug.len().saturating_sub(MAX_SLUG_LEN)..];
    let digest = Sha256::digest(path.as_bytes());
    let hash: String = digest
        .iter()
        .take(HASH_HEX_LEN / 2)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{tail}{NAME_SEPARATOR}{hash}")
}

/// The slug that older versions used as the whole key. A handoff an older
/// version saved is still read under it, so an upgrade strands nothing.
pub(crate) fn legacy_slug(key: &str) -> &str {
    key.rsplit_once(NAME_SEPARATOR)
        .map_or(key, |(slug, _)| slug)
}

/// The key for the directory this process runs in (`$PWD` first, like Claude Code).
pub fn current_slug() -> String {
    key(&crate::cc::logical_cwd())
}

/// The key for `--dir`: the path as given made absolute, never canonicalized,
/// so it matches the `$PWD` a session in that directory carries.
fn slug_for_dir(dir: &str) -> Result<String, String> {
    let given = Path::new(dir);
    let absolute = if given.is_absolute() {
        given.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("--dir {dir}: {e}"))?
            .join(given)
    };
    let tidy: PathBuf = absolute.components().collect();
    if !tidy.is_dir() {
        return Err(format!("--dir {dir} is not a directory"));
    }
    Ok(key(&tidy.to_string_lossy()))
}

/// How long a session-tagged handoff stays eligible, and how long a clear
/// marker does, before the next start removes it. The tagged handoff is one a
/// context hygiene nudge asked for, meant for the next `/clear` or `/compact`
/// of that session.
const TAGGED_MAX_AGE_SECS: u64 = SECS_PER_DAY;
const MARKER_MAX_AGE_SECS: u64 = 3600;
const CLEARED_DIR: &str = "cleared";
const MAX_TAG_LEN: usize = 40;

/// A session id as a file name part: letters and digits only, so it can
/// never hold a dash or a path separator.
pub fn session_tag(session_id: &str) -> String {
    session_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(MAX_TAG_LEN)
        .collect()
}

fn all_digits(part: &str) -> bool {
    !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())
}

/// `<key>_<epoch>_<pid>.md`, or `<key>_<epoch>_<pid>_<tag>.md` for a handoff
/// saved on behalf of one session. `Some(None)` is the first form,
/// `Some(Some(tag))` the second. The key ends in a hash and the separator
/// cannot occur inside a part, so a sibling or child directory never matches,
/// whatever its name ends in.
///
/// A name from an older version, `<slug>-<epoch>-<pid>.md`, also matches
/// under the legacy slug. Only that untagged form does: an older tagged name
/// cannot be told from a sibling directory's, and a tagged handoff expires
/// after a day anyway.
fn parse_name(name: &str, key: &str) -> Option<Option<String>> {
    let stem = name.strip_suffix(".md")?;
    if let Some(rest) = stem
        .strip_prefix(key)
        .and_then(|r| r.strip_prefix(NAME_SEPARATOR))
    {
        let mut parts = rest.split(NAME_SEPARATOR);
        if !(parts.next().is_some_and(all_digits) && parts.next().is_some_and(all_digits)) {
            return None;
        }
        let tag = match parts.next() {
            None => return Some(None),
            Some(t) if !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric()) => t,
            Some(_) => return None,
        };
        return parts.next().is_none().then(|| Some(tag.to_string()));
    }
    let legacy = legacy_slug(key);
    let rest = stem.strip_prefix(legacy)?.strip_prefix('-')?;
    let (epoch, pid) = rest.split_once('-')?;
    (!legacy.is_empty() && all_digits(epoch) && all_digits(pid)).then_some(None)
}

pub(crate) fn is_handoff_for(name: &str, slug: &str) -> bool {
    parse_name(name, slug).is_some()
}

fn tag_of(path: &Path, slug: &str) -> Option<String> {
    parse_name(path.file_name()?.to_str()?, slug)?
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub modified: SystemTime,
}

/// Regular `.md` files in `dir` whose name passes `keep`, freshest first.
fn list(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<Entry> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Entry> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !name.ends_with(".md") || !keep(&name) {
                return None;
            }
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some(Entry {
                path: entry.path(),
                modified: meta.modified().ok()?,
            })
        })
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.modified));
    out
}

/// Handoffs for `slug` not yet loaded by a session start, freshest first.
pub fn unread(dir: &Path, slug: &str) -> Vec<Entry> {
    list(dir, |name| is_handoff_for(name, slug))
}

pub fn freshest_mtime(dir: &Path, slug: &str) -> Option<SystemTime> {
    unread(dir, slug).first().map(|e| e.modified)
}

/// The tombstone records of handoffs for `slug` that were loaded from `dir`,
/// newest first, in the given `state`.
fn tombstoned(dir: &Path, slug: &str, index: &Path, state: State) -> Vec<(Record, PathBuf)> {
    let mut found: Vec<(Record, PathBuf)> = index::latest(index::read(index))
        .into_iter()
        .filter(|rec| rec.state == state)
        .filter_map(|rec| {
            // A handoff that an older version parked in its old "used" folder was moved to
            // a tombstone by the init migration, with that path as its origin.
            let used = dir.join("used");
            let original = rec.paths.iter().find(|path| {
                (path.parent() == Some(dir) || path.parent() == Some(used.as_path()))
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| is_handoff_for(name, slug))
            })?;
            let at = rec.member_dest(original);
            Some((rec.clone(), at))
        })
        .collect();
    found.sort_by_key(|(rec, _)| std::cmp::Reverse(rec.source_mtime));
    found
}

/// Handoffs for `slug` that a session start already loaded and gc still keeps,
/// freshest first.
fn loaded(dir: &Path, slug: &str, index: &Path) -> Vec<Entry> {
    tombstoned(dir, slug, index, State::Done)
        .into_iter()
        .filter(|(_, at)| at.is_file())
        .map(|(rec, at)| Entry {
            path: at,
            modified: UNIX_EPOCH
                + Duration::from_secs(u64::try_from(rec.source_mtime).unwrap_or(0)),
        })
        .collect()
}

fn cutoff(now: SystemTime) -> SystemTime {
    now.checked_sub(Duration::from_secs(MAX_AGE_DAYS * SECS_PER_DAY))
        .unwrap_or(UNIX_EPOCH)
}

/// Writes `text` for `slug`. A temp file in the same directory plus a rename
/// keeps a reader from ever seeing half a handoff. Never overwrites: a name
/// already taken moves the epoch forward by one second.
fn save_in(
    dir: &Path,
    slug: &str,
    text: &str,
    epoch: u64,
    pid: u32,
    tag: &str,
) -> Result<PathBuf, String> {
    if text.trim().is_empty() {
        return Err(
            "empty handoff: pipe the handoff markdown to this command on stdin".to_string(),
        );
    }
    if slug.is_empty() {
        return Err("could not work out which directory this handoff belongs to".to_string());
    }
    ensure_private_dir(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let mut stamp = epoch;
    let target = loop {
        let suffix = if tag.is_empty() {
            String::new()
        } else {
            format!("{NAME_SEPARATOR}{tag}")
        };
        let candidate = dir.join(format!(
            "{slug}{NAME_SEPARATOR}{stamp}{NAME_SEPARATOR}{pid}{suffix}.md"
        ));
        if !candidate.exists() {
            break candidate;
        }
        stamp += 1;
    };
    crate::common::atomic::write_atomic_private(&target, text)
        .map_err(|e| format!("could not save {}: {e}", target.display()))?;
    Ok(target)
}

/// Whether `session_id` has a handoff for `slug` in `dir` saved at or after
/// `since` that no session start has loaded yet.
pub fn saved_since(dir: &Path, slug: &str, session_id: &str, since: SystemTime) -> bool {
    let wanted = session_tag(session_id);
    !wanted.is_empty()
        && unread(dir, slug).iter().any(|entry| {
            entry.modified >= since && tag_of(&entry.path, slug).as_deref() == Some(wanted.as_str())
        })
}

/// Saves `text` in `dir` as the handoff of `session_id`, tagged with it.
pub fn save_for_session(
    dir: &Path,
    slug: &str,
    text: &str,
    session_id: &str,
) -> Result<PathBuf, String> {
    save_in(
        dir,
        slug,
        text,
        crate::common::time::now_epoch_secs(),
        std::process::id(),
        &session_tag(session_id),
    )
}

pub struct Taken {
    pub contents: Vec<String>,
    pub total: usize,
}

/// Where handoffs go when they are loaded: the gc tombstone folder.
pub struct Archive<'a> {
    pub registry: &'a Registry,
    pub fs: &'a dyn FsOps,
    pub clock: &'a dyn Clock,
    /// Days a loaded handoff stays restorable.
    pub grace_days: u32,
}

enum Moved {
    To(PathBuf),
    /// Another gc run holds the lock. Nothing changed.
    Busy,
    /// The tombstone folder cannot be used at all.
    NoFolder,
    /// The move was refused or went wrong. The file is still where it was.
    Failed,
}

impl Archive<'_> {
    fn tombstone(&self, path: &Path) -> Moved {
        let req = MoveRequest {
            paths: vec![path.to_path_buf()],
            reason: "loaded by a session start".into(),
            retain_days: self.grace_days,
        };
        let outcome = tombstone::move_to_tombstone(
            self.registry,
            self.fs,
            (self.clock, &NoHook),
            LOCK_WAIT,
            &req,
            None,
        );
        match outcome {
            MoveOutcome::Moved { id } => {
                self.destination(&id, path).map_or(Moved::Failed, Moved::To)
            }
            MoveOutcome::Busy => Moved::Busy,
            MoveOutcome::Unavailable(_) => Moved::NoFolder,
            MoveOutcome::Refused(_) | MoveOutcome::Failed(_) => Moved::Failed,
        }
    }

    fn destination(&self, id: &str, original: &Path) -> Option<PathBuf> {
        let records = index::read(&index::index_path(self.registry.gc_dir()));
        let rec = index::latest(records)
            .into_iter()
            .find(|rec| rec.id == id)?;
        Some(rec.member_dest(original))
    }
}

/// Runs `f` with the production archive: the registry under the real home and
/// the configured grace period.
fn with_production_archive<T>(f: impl FnOnce(&Archive) -> T) -> T {
    let home = crate::common::home_dir();
    let registry = crate::gc::registry(&home);
    let fs = RealFs::new(&registry);
    let clock = crate::gc::default_clock();
    f(&Archive {
        registry: &registry,
        fs: &fs,
        clock: &*clock,
        grace_days: crate::gc::HANDOFF_GRACE_DAYS,
    })
}

/// Tombstones a loaded handoff. Whatever goes wrong, the file stays where it
/// is and the next start tries again: the injected text may have been cut to
/// fit, so the file is the only full copy. A kept file expires with the rest
/// after `MAX_AGE_DAYS`.
fn archive(a: &Archive, path: &Path) {
    let _ = a.tombstone(path);
}

/// The read side of a session start: up to `MAX_INJECTED` fresh handoffs for
/// `slug`. Loaded ones are tombstoned; stale and over-cap ones are removed.
/// Never panics, and any failure just yields fewer handoffs.
pub fn take_in(dir: &Path, slug: &str, now: SystemTime) -> Taken {
    with_production_archive(|a| take_in_with(a, dir, slug, now))
}

fn take_in_with(a: &Archive, dir: &Path, slug: &str, now: SystemTime) -> Taken {
    prune_hygiene(dir, slug, now);
    let found: Vec<Entry> = unread(dir, slug)
        .into_iter()
        .filter(|e| tag_of(&e.path, slug).is_none())
        .collect();
    let total = found.len();
    let limit = cutoff(now);
    let mut contents = Vec::new();
    for entry in &found {
        let mut keep = false;
        if contents.len() < MAX_INJECTED && entry.modified >= limit {
            if let Ok(text) = fs::read_to_string(&entry.path) {
                if !text.is_empty() {
                    contents.push(text);
                    keep = true;
                }
            }
        }
        if keep {
            archive(a, &entry.path);
        } else {
            let _ = fs::remove_file(&entry.path);
        }
    }
    Taken { contents, total }
}

fn older_than(now: SystemTime, then: SystemTime, secs: u64) -> bool {
    now.duration_since(then).is_ok_and(|d| d.as_secs() > secs)
}

/// Removes session-tagged handoffs for `slug` past `TAGGED_MAX_AGE_SECS` and
/// clear markers past `MARKER_MAX_AGE_SECS`, so neither lingers.
fn prune_hygiene(dir: &Path, slug: &str, now: SystemTime) {
    for entry in unread(dir, slug) {
        if tag_of(&entry.path, slug).is_some()
            && older_than(now, entry.modified, TAGGED_MAX_AGE_SECS)
        {
            let _ = fs::remove_file(&entry.path);
        }
    }
    let markers = dir.join(CLEARED_DIR);
    if let Ok(entries) = fs::read_dir(&markers) {
        for entry in entries.flatten() {
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| older_than(now, t, MARKER_MAX_AGE_SECS));
            if stale {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Records that the session with `session_id` ended with `/clear`, so the
/// handoff saved for it may load into the session that replaces it. Nothing
/// else can tell a start which handoff is its own: the new session has a new
/// id, and a session that is still running never gets a marker. Never panics.
pub fn mark_cleared(session_id: &str) {
    mark_cleared_in(&root(), session_id);
}

fn mark_cleared_in(dir: &Path, session_id: &str) {
    let tag = session_tag(session_id);
    if tag.is_empty() {
        return;
    }
    let markers = dir.join(CLEARED_DIR);
    if ensure_private_dir(&markers).is_ok() {
        let _ = fs::write(markers.join(tag), "");
    }
}

/// A session-tagged handoff taken for a start after `/clear` or `/compact`.
pub struct Resumed {
    pub text: String,
    /// Where the full handoff now lives: in the tombstone folder, or where it
    /// was saved when the move failed.
    pub path: PathBuf,
}

/// The read side of a start after `/clear` for one session's own handoff: of
/// the tagged handoffs for `slug` whose session left a fresh clear marker,
/// the one with the freshest marker. Its marker is consumed and the file
/// is tombstoned, so it does not load twice. If the move fails the file
/// stays, but the consumed marker still stops a second load. A tagged handoff whose
/// session never cleared, such as a parallel session still running, is left
/// alone. Never panics.
pub fn take_cleared_in(dir: &Path, slug: &str, now: SystemTime) -> Option<Resumed> {
    with_production_archive(|a| take_cleared_with(a, dir, slug, now))
}

fn take_cleared_with(a: &Archive, dir: &Path, slug: &str, now: SystemTime) -> Option<Resumed> {
    prune_hygiene(dir, slug, now);
    let markers = dir.join(CLEARED_DIR);
    let best = unread(dir, slug)
        .into_iter()
        .filter_map(|entry| {
            let tag = tag_of(&entry.path, slug)?;
            let marked = fs::metadata(markers.join(&tag))
                .and_then(|m| m.modified())
                .ok()?;
            Some((marked, tag, entry))
        })
        .max_by_key(|(marked, _, entry)| (*marked, entry.modified))?;
    let (_, tag, entry) = best;
    let _ = fs::remove_file(markers.join(tag));
    load_and_archive(a, entry)
}

/// The read side of a start after `/compact`. A compact keeps the session id,
/// so the handoff tagged with it is this session's own and needs no marker.
/// The freshest unread one is tombstoned, so it loads once. If that move
/// fails the file stays and loads again on a later compact until it expires
/// after a day. Older ones
/// with the same tag are tombstoned unread because the freshest replaces them. A
/// handoff tagged for another session, an untagged one and an empty id are
/// left alone. Never panics.
pub fn take_session_in(
    dir: &Path,
    slug: &str,
    session_id: &str,
    now: SystemTime,
) -> Option<Resumed> {
    with_production_archive(|a| take_session_with(a, dir, slug, session_id, now))
}

fn take_session_with(
    a: &Archive,
    dir: &Path,
    slug: &str,
    session_id: &str,
    now: SystemTime,
) -> Option<Resumed> {
    let wanted = session_tag(session_id);
    if wanted.is_empty() {
        return None;
    }
    prune_hygiene(dir, slug, now);
    let mut own = unread(dir, slug)
        .into_iter()
        .filter(|entry| tag_of(&entry.path, slug).as_deref() == Some(wanted.as_str()));
    let freshest = own.next()?;
    for superseded in own {
        archive(a, &superseded.path);
    }
    load_and_archive(a, freshest)
}

/// Reads a tagged handoff and tombstones it. An empty file is removed and
/// yields nothing. When the move fails or gc is busy the file stays where it
/// is, so the pointer to the full text stays valid; the tag expires it after a
/// day.
fn load_and_archive(a: &Archive, entry: Entry) -> Option<Resumed> {
    let text = fs::read_to_string(&entry.path)
        .ok()
        .filter(|t| !t.trim().is_empty());
    let Some(text) = text else {
        let _ = fs::remove_file(&entry.path);
        return None;
    };
    let path = match a.tombstone(&entry.path) {
        Moved::To(dest) => dest,
        Moved::Busy | Moved::NoFolder | Moved::Failed => entry.path,
    };
    Some(Resumed { text, path })
}

fn age(now: SystemTime, then: SystemTime) -> String {
    let secs = now.duration_since(then).unwrap_or_default().as_secs();
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / SECS_PER_DAY),
    }
}

/// The topic line: the first markdown heading, else the first non-empty line.
fn heading(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with('#'))
        .or_else(|| text.lines().find(|l| !l.trim().is_empty()))
        .unwrap_or("(empty)");
    line.trim().trim_start_matches('#').trim().to_string()
}

fn render(entry: &Entry, label: &str, now: SystemTime) -> String {
    let text = fs::read_to_string(&entry.path).unwrap_or_default();
    format!(
        "{label}, saved {} ({}):\n\n{}",
        age(now, entry.modified),
        entry.path.display(),
        text.trim_end()
    )
}

/// What `playbook handoff show` prints. Never deletes or moves anything.
fn show_in(dir: &Path, slug: &str, index: &Path, all: bool, now: SystemTime) -> String {
    let waiting = unread(dir, slug);
    if !waiting.is_empty() {
        let shown = if all { waiting.len() } else { 1 };
        let mut parts: Vec<String> = waiting
            .iter()
            .take(shown)
            .map(|e| render(e, "Handoff for this directory", now))
            .collect();
        if waiting.len() > shown {
            parts.push(format!(
                "({} more waiting for this directory; run `playbook handoff show --all` to print them.)",
                waiting.len() - shown
            ));
        }
        return parts.join("\n\n---\n\n");
    }
    if let Some(entry) = loaded(dir, slug, index).first() {
        return render(
            entry,
            "Handoff for this directory, already loaded by the last session start",
            now,
        );
    }
    let mut out = String::from("No handoff saved for this directory.");
    if !tombstoned(dir, slug, index, State::Expired).is_empty() {
        out.push_str(" The last one was loaded and has expired.");
    }
    let others = list(dir, |name| !is_handoff_for(name, slug));
    if !others.is_empty() {
        out.push_str(
            "\n\nHandoffs saved for other directories (a handoff belongs to the directory its session ran in):",
        );
        for entry in others.iter().take(OTHER_DIRS_SHOWN) {
            out.push_str(&format!(
                "\n- {}: {} ({})",
                age(now, entry.modified),
                heading(&entry.path),
                entry.path.display()
            ));
        }
    }
    out
}

/// CLI entry for `handoff save`.
pub fn run_save(dir: Option<&str>, text: &str, session: Option<&str>) -> Result<String, String> {
    let (slug, root) = match dir {
        Some(d) => {
            let slug = slug_for_dir(d)?;
            let root = root_for_dir(d, &slug);
            (slug, root)
        }
        None => (current_slug(), root()),
    };
    let path = save_in(
        &root,
        &slug,
        text,
        crate::common::time::now_epoch_secs(),
        std::process::id(),
        &session.map(session_tag).unwrap_or_default(),
    )?;
    Ok(format!("handoff saved: {}", path.display()))
}

/// CLI entry for `handoff show`.
pub fn run_show(dir: Option<&str>, all: bool) -> Result<String, String> {
    let (slug, root) = match dir {
        Some(d) => {
            let slug = slug_for_dir(d)?;
            let root = root_for_dir(d, &slug);
            (slug, root)
        }
        None => (current_slug(), root()),
    };
    Ok(show_in(
        &root,
        &slug,
        &production_index(),
        all,
        SystemTime::now(),
    ))
}

const LOG_FILE: &str = "session-start.log";
const LOG_TRIM_AT: usize = 300;
const LOG_KEEP: usize = 200;
const STATUS_ROWS: usize = 5;

pub fn log_path() -> PathBuf {
    crate::common::paths::runtime_root().join(LOG_FILE)
}

fn field(value: &str) -> String {
    let clean: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        "-".to_string()
    } else {
        clean.to_string()
    }
}

/// Appends one line per SessionStart: time, source, session id, directory
/// key, handoffs injected. Best-effort and owner-only; the file is cut back
/// to the last 200 lines once it passes 300. Never fails the hook.
fn log_start_in(
    path: &Path,
    now_secs: u64,
    source: &str,
    session_id: &str,
    slug: &str,
    injected: usize,
) {
    let line = format!(
        "{}\t{}\t{}\t{}\t{injected}\n",
        crate::common::time::iso_utc(i64::try_from(now_secs).unwrap_or(i64::MAX)),
        field(source),
        field(session_id),
        field(slug)
    );
    let Some(parent) = path.parent() else { return };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let appended = open_log_for_append(path).and_then(|mut f| f.write_all(line.as_bytes()));
    if appended.is_err() {
        return;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > LOG_TRIM_AT {
        let kept = lines[lines.len() - LOG_KEEP..].join("\n") + "\n";
        let _ = crate::common::atomic::write_atomic_private(path, kept);
    }
}

#[cfg(unix)]
fn open_log_for_append(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_log_for_append(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new().create(true).append(true).open(path)
}

/// Called by the SessionStart hook; see `log_start_in`.
pub fn log_start(source: &str, session_id: &str, slug: &str, injected: usize) {
    log_start_in(
        &log_path(),
        crate::common::time::now_epoch_secs(),
        source,
        session_id,
        slug,
        injected,
    );
}

/// What `playbook handoff status` prints: the last few SessionStart events
/// and how many handoffs wait or were loaded for this directory.
fn status_in(dir: &Path, index: &Path, log: &Path, slug: &str) -> String {
    let text = fs::read_to_string(log).unwrap_or_default();
    let rows: Vec<Vec<&str>> = text
        .lines()
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .filter(|f| f.len() >= 5)
        .collect();
    let mut out = String::new();
    if rows.is_empty() {
        out.push_str("No SessionStart has been logged yet.\n");
    } else {
        out.push_str("Last SessionStart events (newest last):\n");
        out.push_str(&format!(
            "  {:<21} {:<8} {}\n",
            "time (UTC)", "source", "handoffs injected"
        ));
        for f in rows.iter().skip(rows.len().saturating_sub(STATUS_ROWS)) {
            out.push_str(&format!("  {:<21} {:<8} {}\n", f[0], f[1], f[4]));
        }
    }
    out.push_str(&format!(
        "\nThis directory: {} unread, {} already loaded.\n",
        unread(dir, slug).len(),
        loaded(dir, slug, index).len()
    ));
    out.push_str(
        "If you ran /clear and no `clear` row appears above, Claude Code did not run the hook: run /playbook:handoff-show to load the handoff on demand.",
    );
    out
}

/// CLI entry for `handoff status`.
pub fn run_status() -> String {
    status_in(&root(), &production_index(), &log_path(), &current_slug())
}

fn production_index() -> PathBuf {
    index::index_path(&crate::common::paths::playbook_root().join(crate::gc::GC_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use crate::gc::lock::with_run_lock;
    use crate::gc::tombstone::Env;
    use crate::gc::{Action, FixedClock, Limits, Store, SystemClock};

    const SLUG: &str = "-Users-me-repo_0a1b2c3d";
    const GRACE_DAYS: u32 = 14;

    /// The gc folder that goes with the handoff folder `dir`.
    fn gc_of(dir: &Path) -> PathBuf {
        let name = dir.file_name().unwrap().to_string_lossy();
        dir.with_file_name(format!("{name}-gc"))
    }

    fn index_of(dir: &Path) -> PathBuf {
        index::index_path(&gc_of(dir))
    }

    /// A registry whose only root is the handoff folder `dir`.
    fn registry_for(dir: &Path) -> Registry {
        Registry::new(gc_of(dir)).with_store(Store {
            name: "handoffs",
            roots: vec![dir.to_path_buf()],
            scan: |_, _| Vec::new(),
            action: Action::ReportOnly,
            limits: Limits::default(),
        })
    }

    fn with_archive<T>(dir: &Path, clock: &dyn Clock, f: impl FnOnce(&Archive) -> T) -> T {
        let registry = registry_for(dir);
        let fs = RealFs::new(&registry);
        f(&Archive {
            registry: &registry,
            fs: &fs,
            clock,
            grace_days: GRACE_DAYS,
        })
    }

    fn take_in(dir: &Path, slug: &str, now: SystemTime) -> Taken {
        with_archive(dir, &SystemClock, |a| {
            super::take_in_with(a, dir, slug, now)
        })
    }

    fn take_cleared_in(dir: &Path, slug: &str, now: SystemTime) -> Option<Resumed> {
        with_archive(dir, &SystemClock, |a| {
            super::take_cleared_with(a, dir, slug, now)
        })
    }

    fn take_session_in(dir: &Path, slug: &str, sid: &str, now: SystemTime) -> Option<Resumed> {
        with_archive(dir, &SystemClock, |a| {
            super::take_session_with(a, dir, slug, sid, now)
        })
    }

    fn loaded(dir: &Path, slug: &str) -> Vec<Entry> {
        super::loaded(dir, slug, &index_of(dir))
    }

    fn show_in(dir: &Path, slug: &str, all: bool, now: SystemTime) -> String {
        super::show_in(dir, slug, &index_of(dir), all, now)
    }

    fn status_in(dir: &Path, log: &Path, slug: &str) -> String {
        super::status_in(dir, &index_of(dir), log, slug)
    }

    /// Makes the tombstone folder impossible to create.
    fn block_tombstones(dir: &Path) {
        fs::create_dir_all(gc_of(dir)).unwrap();
        fs::write(gc_of(dir).join("tombstones"), "blocks the folder").unwrap();
    }

    fn day(n: u64) -> Duration {
        Duration::from_secs(n * SECS_PER_DAY)
    }

    fn backdate(path: &Path, age_days: u64) {
        let when = SystemTime::now() - day(age_days);
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    #[test]
    fn a_very_deep_path_still_gives_a_key_that_fits_a_file_name() {
        let deep = format!("/{}", "segment/".repeat(60));

        let k = key(&deep);

        assert_eq!(k.len(), MAX_SLUG_LEN + 1 + HASH_HEX_LEN);
        assert!(format!("{k}_1790000000_99999_{}.md", "a".repeat(MAX_TAG_LEN)).len() < 255);
        assert_eq!(key(&deep), k, "the key is stable");
        assert!(is_handoff_for(&format!("{k}_1790000000_99999.md"), &k));
    }

    #[test]
    fn only_the_exact_directory_key_matches() {
        assert!(is_handoff_for(
            "-Users-me-repo_0a1b2c3d_1790000000_42.md",
            SLUG
        ));
        assert!(is_handoff_for(
            "-Users-me-repo_0a1b2c3d_1790000000_42_abc123.md",
            SLUG
        ));
        assert!(!is_handoff_for(
            "-Users-me-repo-sub_0a1b2c3d_1790000000_42.md",
            SLUG
        ));
        assert!(!is_handoff_for(
            "-Users-me-repo2_0a1b2c3d_1790000000_42.md",
            SLUG
        ));
        assert!(!is_handoff_for(
            "-Users-me-repo_ffffffff_1790000000_42.md",
            SLUG
        ));
        assert!(!is_handoff_for(
            "-Users-me-repo_0a1b2c3d_1790000000_test.md",
            SLUG
        ));
        assert!(!is_handoff_for(
            "-Users-me-repo_0a1b2c3d_1790000000_42.txt",
            SLUG
        ));
    }

    #[test]
    fn directories_that_differ_only_in_punctuation_get_different_keys() {
        let underscore = key("/x/my_app");
        let dash = key("/x/my-app");

        assert_ne!(underscore, dash);
        assert!(underscore.starts_with("-x-my-app_"), "{underscore}");
        let dir = scratch_dir("handoff-punctuation");
        save_in(&dir, &underscore, "# Underscore", 100, 7, "").unwrap();
        save_in(&dir, &dash, "# Dash", 100, 8, "").unwrap();

        let taken = take_in(&dir, &dash, SystemTime::now());

        assert_eq!(taken.contents, vec!["# Dash".to_string()]);
        assert_eq!(unread(&dir, &underscore).len(), 1, "the other one stays");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_sibling_whose_name_ends_in_digits_never_matches_as_a_tagged_handoff() {
        let repo = key("/x/repo");
        let sibling = key("/x/repo-2");
        let dir = scratch_dir("handoff-sibling-digits");
        let theirs = save_in(&dir, &sibling, "# Sibling", 100, 7, &session_tag(SESSION)).unwrap();
        backdate(&theirs, 3);

        assert!(!is_handoff_for(
            theirs.file_name().unwrap().to_str().unwrap(),
            &repo
        ));
        assert!(
            !is_handoff_for("-x-repo-2-1790000000-42-abc.md", legacy_slug(&repo)),
            "an older sibling name is not a tagged handoff either"
        );
        take_in(&dir, &repo, SystemTime::now());
        assert!(theirs.is_file(), "pruning must not touch a sibling's file");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_handoff_an_older_version_saved_still_loads_once() {
        let dir = scratch_dir("handoff-legacy-name");
        fs::create_dir_all(&dir).unwrap();
        let old = dir.join("-x-repo-1790000000-42.md");
        fs::write(&old, "# Written by the previous version").unwrap();
        let repo = key("/x/repo");

        let first = take_in(&dir, &repo, SystemTime::now());
        let second = take_in(&dir, &repo, SystemTime::now());

        assert_eq!(first.contents, vec!["# Written by the previous version"]);
        assert!(second.contents.is_empty(), "it loads once");
        assert!(!old.exists(), "it is tombstoned like any other");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn two_saves_in_one_second_do_not_overwrite_each_other() {
        let dir = scratch_dir("handoff-two");
        let a = save_in(&dir, SLUG, "first", 100, 7, "").unwrap();
        let b = save_in(&dir, SLUG, "second", 100, 7, "").unwrap();

        assert_ne!(a, b);
        assert_eq!(fs::read_to_string(a).unwrap(), "first");
        assert_eq!(fs::read_to_string(b).unwrap(), "second");
        let _ = fs::remove_dir_all(dir);
    }

    const SESSION: &str = "c6e91d16-99c2-4305-b9c5-2abc835f7f83";
    const OTHER_SESSION: &str = "0a1b2c3d-0000-4000-8000-000000000000";

    #[test]
    fn a_compact_loads_the_handoff_tagged_for_the_same_session_once() {
        let dir = scratch_dir("handoff-compact");
        save_in(&dir, SLUG, "# Mine", 100, 7, &session_tag(SESSION)).unwrap();

        let first = take_session_in(&dir, SLUG, SESSION, SystemTime::now()).unwrap();
        let second = take_session_in(&dir, SLUG, SESSION, SystemTime::now());

        assert_eq!(first.text, "# Mine");
        assert!(
            first.path.starts_with(gc_of(&dir).join("tombstones")),
            "{:?}",
            first.path
        );
        assert!(second.is_none(), "a handoff loads once");
        assert_eq!(unread(&dir, SLUG).len(), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_compact_leaves_the_handoff_of_another_session_alone() {
        let dir = scratch_dir("handoff-compact-other");
        save_in(&dir, SLUG, "# Theirs", 100, 7, &session_tag(OTHER_SESSION)).unwrap();

        let taken = take_session_in(&dir, SLUG, SESSION, SystemTime::now());

        assert!(taken.is_none());
        assert_eq!(
            unread(&dir, SLUG).len(),
            1,
            "the other session still has it"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_compact_does_not_take_an_untagged_handoff_or_an_empty_session_id() {
        let dir = scratch_dir("handoff-compact-untagged");
        save_in(&dir, SLUG, "# Untagged", 100, 7, "").unwrap();
        save_in(&dir, SLUG, "# Tagged", 101, 7, &session_tag(SESSION)).unwrap();

        assert!(take_session_in(&dir, SLUG, "", SystemTime::now()).is_none());
        let taken = take_session_in(&dir, SLUG, SESSION, SystemTime::now()).unwrap();

        assert_eq!(taken.text, "# Tagged");
        assert_eq!(
            unread(&dir, SLUG).len(),
            1,
            "the untagged one stays for take_in"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_compact_takes_the_newest_handoff_and_retires_the_older_ones_of_the_session() {
        let dir = scratch_dir("handoff-compact-two");
        let tag = session_tag(SESSION);
        let older = save_in(&dir, SLUG, "# Older", 100, 7, &tag).unwrap();
        save_in(&dir, SLUG, "# Newer", 101, 7, &tag).unwrap();
        let an_hour_ago = SystemTime::now() - Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(&older)
            .unwrap()
            .set_modified(an_hour_ago)
            .unwrap();

        let taken = take_session_in(&dir, SLUG, SESSION, SystemTime::now()).unwrap();

        assert_eq!(taken.text, "# Newer");
        assert_eq!(unread(&dir, SLUG).len(), 0, "the older one is retired too");
        assert!(take_session_in(&dir, SLUG, SESSION, SystemTime::now()).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_move_to_used_keeps_the_file_the_pointer_names() {
        let dir = scratch_dir("handoff-move-fails");
        let saved = save_in(&dir, SLUG, "# Long", 100, 7, &session_tag(SESSION)).unwrap();
        block_tombstones(&dir);

        let taken = take_session_in(&dir, SLUG, SESSION, SystemTime::now()).unwrap();

        assert_eq!(taken.text, "# Long");
        assert_eq!(taken.path, saved);
        assert!(taken.path.is_file(), "the pointer must name a real file");
        let again = take_session_in(&dir, SLUG, SESSION, SystemTime::now()).unwrap();
        assert_eq!(
            again.path, saved,
            "a kept file loads again until it expires"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_move_after_a_clear_keeps_the_file_and_loads_it_once() {
        let dir = scratch_dir("handoff-clear-move-fails");
        let saved = save_in(&dir, SLUG, "# Long", 100, 7, &session_tag(SESSION)).unwrap();
        mark_cleared_in(&dir, SESSION);
        block_tombstones(&dir);

        let taken = take_cleared_in(&dir, SLUG, SystemTime::now()).unwrap();

        assert_eq!(taken.path, saved);
        assert!(taken.path.is_file(), "the pointer must name a real file");
        assert!(
            take_cleared_in(&dir, SLUG, SystemTime::now()).is_none(),
            "the consumed marker stops a second load"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_compact_ignores_and_removes_a_stale_tagged_handoff() {
        let dir = scratch_dir("handoff-compact-stale");
        let path = save_in(&dir, SLUG, "# Old", 100, 7, &session_tag(SESSION)).unwrap();
        backdate(&path, 2);

        let taken = take_session_in(&dir, SLUG, SESSION, SystemTime::now());

        assert!(taken.is_none());
        assert!(!path.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_compact_removes_a_blank_tagged_file_and_loads_nothing() {
        let dir = scratch_dir("handoff-compact-blank");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{SLUG}_100_7_{}.md", session_tag(SESSION)));
        fs::write(&path, "  \n").unwrap();

        let taken = take_session_in(&dir, SLUG, SESSION, SystemTime::now());

        assert!(taken.is_none());
        assert!(!path.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_empty_handoff_is_refused_and_writes_nothing() {
        let dir = scratch_dir("handoff-empty");
        let err = save_in(&dir, SLUG, "  \n", 100, 7, "").unwrap_err();

        assert!(err.contains("empty handoff"), "{err}");
        assert_eq!(unread(&dir, SLUG).len(), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_directory_is_private_and_so_is_the_file() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch_dir("handoff-mode");
        let dir = root.join("handoff");
        let path = save_in(&dir, SLUG, "# x\n", 100, 7, "").unwrap();

        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_loaded_handoff_is_tombstoned_and_show_still_prints_it() {
        let dir = scratch_dir("handoff-take");
        save_in(&dir, SLUG, "# topic\nNEXT-STEP\n", 100, 7, "").unwrap();

        let taken = take_in(&dir, SLUG, SystemTime::now());

        assert_eq!(taken.contents.len(), 1);
        assert!(taken.contents[0].contains("NEXT-STEP"));
        assert_eq!(unread(&dir, SLUG).len(), 0);
        let shown = show_in(&dir, SLUG, false, SystemTime::now());
        assert!(
            shown.contains("already loaded by the last session start"),
            "{shown}"
        );
        assert!(shown.contains("NEXT-STEP"), "{shown}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_stale_handoff_is_removed_not_injected_or_kept() {
        let dir = scratch_dir("handoff-stale");
        let path = save_in(&dir, SLUG, "# old\n", 100, 7, "").unwrap();
        backdate(&path, MAX_AGE_DAYS + 1);

        let taken = take_in(&dir, SLUG, SystemTime::now());

        assert!(taken.contents.is_empty());
        assert!(!path.exists());
        assert!(loaded(&dir, SLUG).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    /// Expires the tombstones of `dir`'s gc folder as of `now_secs`.
    fn expire_at(dir: &Path, now_secs: i64) -> usize {
        let registry = registry_for(dir);
        let fs = RealFs::new(&registry);
        let clock = FixedClock(now_secs);
        let ran = with_run_lock(
            registry.gc_dir(),
            Duration::ZERO,
            &clock,
            &NoHook,
            |guard| {
                crate::gc::apply::expire(&Env {
                    guard,
                    registry: &registry,
                    fs: &fs,
                })
            },
        );
        match ran {
            Ok(crate::gc::lock::RunOutcome::Ran(count)) => count,
            _ => panic!("the gc lock should be free"),
        }
    }

    #[test]
    fn a_handoff_tombstoned_from_the_old_used_folder_still_counts_as_loaded() {
        // Arrange
        let dir = scratch_dir("handoff-from-used");
        let live = save_in(&dir, SLUG, "# a\n", 100, 7, "").unwrap();
        let used = dir.join("used");
        fs::create_dir_all(&used).unwrap();
        let parked = used.join(live.file_name().unwrap());
        fs::rename(&live, &parked).unwrap();

        // Act
        with_archive(&dir, &SystemClock, |a| super::archive(a, &parked));

        // Assert
        let found = loaded(&dir, SLUG);
        assert_eq!(found.len(), 1);
        assert_eq!(fs::read_to_string(&found[0].path).unwrap(), "# a\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn handoff_expires_at_grace_boundary() {
        // Arrange
        let dir = scratch_dir("handoff-expire");
        save_in(&dir, SLUG, "# a\n", 100, 7, "").unwrap();
        let loaded_at = 1_800_000_000;
        with_archive(&dir, &FixedClock(loaded_at), |a| {
            super::take_in_with(a, &dir, SLUG, SystemTime::now())
        });
        let kept = loaded(&dir, SLUG).remove(0).path;
        let grace = i64::from(GRACE_DAYS) * 86_400;

        // Act
        let early = expire_at(&dir, loaded_at + grace - 1);
        let still_there = kept.exists();
        let on_time = expire_at(&dir, loaded_at + grace);

        // Assert
        assert_eq!((early, on_time), (0, 1));
        assert!(still_there);
        assert!(!kept.exists());
        assert!(loaded(&dir, SLUG).is_empty());
        let shown = show_in(&dir, SLUG, false, SystemTime::now());
        assert!(shown.contains("has expired"), "{shown}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn archive_keeps_the_file_when_the_move_fails() {
        let dir = scratch_dir("handoff-archive-keeps");
        let path = save_in(&dir, SLUG, "# a\n", 100, 7, "").unwrap();
        block_tombstones(&dir);

        let taken = take_in(&dir, SLUG, SystemTime::now());

        assert_eq!(taken.contents.len(), 1);
        assert!(
            path.is_file(),
            "the only full copy must survive a failed move"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn guard_refusal_keeps_handoff_source() {
        let dir = scratch_dir("handoff-guard-refuses");
        let path = save_in(&dir, SLUG, "# a\n", 100, 7, "").unwrap();
        let elsewhere = scratch_dir("handoff-guard-elsewhere");
        let registry = registry_for(&elsewhere);
        let fs = RealFs::new(&registry);
        let archive = Archive {
            registry: &registry,
            fs: &fs,
            clock: &SystemClock,
            grace_days: GRACE_DAYS,
        };

        super::archive(&archive, &path);

        assert!(path.is_file(), "a refused move must leave the source");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn busy_lock_keeps_the_source_on_both_paths() {
        // Arrange
        let dir = scratch_dir("handoff-busy");
        let plain = save_in(&dir, SLUG, "# a\n", 100, 7, "").unwrap();
        let tagged = save_in(&dir, SLUG, "# b\n", 101, 7, &session_tag(SESSION)).unwrap();
        let registry = registry_for(&dir);
        let clock = SystemClock;

        // Act
        let held = with_run_lock(registry.gc_dir(), Duration::ZERO, &clock, &NoHook, |_| {
            let taken = take_in(&dir, SLUG, SystemTime::now());
            let resumed = take_session_in(&dir, SLUG, SESSION, SystemTime::now());
            (taken.contents.len(), resumed.map(|r| r.path))
        });

        // Assert
        let Ok(crate::gc::lock::RunOutcome::Ran((injected, pointer))) = held else {
            panic!("the test should hold the lock");
        };
        assert_eq!(injected, 1);
        assert_eq!(pointer, Some(tagged.clone()));
        assert!(plain.is_file() && tagged.is_file());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_cap_injects_the_freshest_and_removes_the_rest() {
        let dir = scratch_dir("handoff-cap");
        for (i, pid) in [1u32, 2, 3, 4].iter().enumerate() {
            let p = save_in(&dir, SLUG, &format!("# h{i}\n"), 100 + i as u64, *pid, "").unwrap();
            backdate(&p, 4 - i as u64);
        }

        let taken = take_in(&dir, SLUG, SystemTime::now());

        assert_eq!((taken.contents.len(), taken.total), (MAX_INJECTED, 4));
        assert!(taken.contents[0].contains("h3"));
        assert!(unread(&dir, SLUG).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn show_lists_handoffs_saved_for_other_directories() {
        let dir = scratch_dir("handoff-others");
        save_in(&dir, "-Users-me-other", "# Other topic\n", 100, 7, "").unwrap();

        let shown = show_in(&dir, SLUG, false, SystemTime::now());

        assert!(
            shown.starts_with("No handoff saved for this directory."),
            "{shown}"
        );
        assert!(shown.contains("Other topic"), "{shown}");
        assert_eq!(
            unread(&dir, "-Users-me-other").len(),
            1,
            "show never consumes"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn show_all_prints_every_waiting_handoff_and_default_notes_the_rest() {
        let dir = scratch_dir("handoff-all");
        save_in(&dir, SLUG, "# one\n", 100, 1, "").unwrap();
        save_in(&dir, SLUG, "# two\n", 100, 2, "").unwrap();

        let one = show_in(&dir, SLUG, false, SystemTime::now());
        let both = show_in(&dir, SLUG, true, SystemTime::now());

        assert!(one.contains("1 more waiting"), "{one}");
        assert!(both.contains("# one") && both.contains("# two"), "{both}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_log_line_has_five_tab_separated_fields() {
        let dir = scratch_dir("handoff-log-line");
        let log = dir.join("session-start.log");

        log_start_in(&log, 0, "clear", "sess\t1", SLUG, 2);

        assert_eq!(
            fs::read_to_string(&log).unwrap(),
            "1970-01-01T00:00:00Z\tclear\tsess 1\t-Users-me-repo_0a1b2c3d\t2\n"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_log_is_cut_back_to_the_last_200_lines_past_300() {
        let dir = scratch_dir("handoff-log-bound");
        let log = dir.join("session-start.log");

        for n in 0..=LOG_TRIM_AT {
            log_start_in(&log, n as u64, "startup", "s", SLUG, 0);
        }

        let lines = fs::read_to_string(&log).unwrap().lines().count();
        assert_eq!(lines, LOG_KEEP);
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_runtime_dir_never_breaks_the_log_call() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("handoff-log-ro");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();

        log_start_in(&dir.join("session-start.log"), 1, "clear", "s", SLUG, 1);

        assert!(!dir.join("session-start.log").exists() || dir.join("session-start.log").is_file());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn status_shows_recent_events_and_this_directorys_counts() {
        let dir = scratch_dir("handoff-status");
        let log = dir.join("session-start.log");
        log_start_in(&log, 1_788_252_682, "clear", "s1", SLUG, 1);
        save_in(&dir, SLUG, "# a\n", 100, 1, "").unwrap();

        let shown = status_in(&dir, &log, SLUG);

        assert!(shown.contains("2026-09-01T08:51:22Z"), "{shown}");
        assert!(shown.contains("clear"), "{shown}");
        assert!(shown.contains("1 unread, 0 already loaded"), "{shown}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn status_with_no_log_says_so() {
        let dir = scratch_dir("handoff-status-empty");

        let shown = status_in(&dir, &dir.join("none.log"), SLUG);

        assert!(
            shown.starts_with("No SessionStart has been logged yet."),
            "{shown}"
        );
        let _ = fs::remove_dir_all(dir);
    }
}
