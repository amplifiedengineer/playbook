// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook doc read <file>`: turn a PDF or Office file into Markdown with
//! the bundled `pbdoc` converter (or a configured `anydoc`), locally and under
//! limits, and print it framed as untrusted document content (ADR-0027).
//! Nothing from the document is logged. A long document is also kept in a
//! private cache so later slices need no new conversion.

pub mod convert;
pub mod converter;
pub mod frame;
pub mod slice;
pub mod tmp;

use convert::{DocError, Limits};
use converter::{Converter, Search};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const KB_PER_MB: u64 = 1024;
const STALE_AFTER: Duration = Duration::from_secs(3600);
const CACHE_STALE_AFTER: Duration = Duration::from_secs(24 * 3600);

/// The largest file `doc read` opens, in bytes (25 MiB).
pub const MAX_BYTES: u64 = 26_214_400;
/// Seconds a conversion may run before it is stopped.
pub const TIMEOUT_SECONDS: u64 = 30;
/// Characters of converted text kept before the output is cut.
pub const MAX_OUTPUT_CHARS: usize = 200_000;
/// Resident megabytes a conversion may use before it is stopped.
pub const MAX_MEMORY_MB: u64 = 1024;
/// The most document characters printed at once.
pub const DISPLAY_CHARS: usize = 24_000;

// A built-in cap above the hard ceiling would make the ceiling the real cap.
const _: () = assert!(MAX_BYTES <= converter::HARD_MAX_INPUT_BYTES);

/// The converter limits, the converter path and the display size.
pub struct Settings {
    pub limits: Limits,
    pub anydoc_path: String,
    pub display_chars: usize,
}

/// Env var that lowers the limits in debug builds so tests can reach them,
/// as a JSON object of `Limits` field names.
#[cfg(debug_assertions)]
const TEST_LIMITS_VAR: &str = "PLAYBOOK_TEST_DOC_LIMITS";

pub fn settings(home: &Path) -> Settings {
    let slug = crate::common::repo_slug();
    let slug = (!slug.is_empty()).then_some(slug);
    let anydoc_path = converter::configured_path(home, slug.as_deref());
    let mut cfg = Settings {
        limits: Limits {
            max_input_bytes: MAX_BYTES,
            timeout: Duration::from_secs(TIMEOUT_SECONDS),
            max_output_chars: MAX_OUTPUT_CHARS,
            max_rss_kb: MAX_MEMORY_MB * KB_PER_MB,
        },
        anydoc_path,
        display_chars: DISPLAY_CHARS,
    };
    #[cfg(debug_assertions)]
    apply_test_limits(&mut cfg);
    cfg
}

#[cfg(debug_assertions)]
fn apply_test_limits(cfg: &mut Settings) {
    let Some(overrides) = std::env::var(TEST_LIMITS_VAR)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
    else {
        return;
    };
    let number = |field: &str| overrides.get(field).and_then(|v| v.as_u64());
    if let Some(n) = number("max_input_bytes") {
        cfg.limits.max_input_bytes = n;
    }
    if let Some(n) = number("timeout_seconds") {
        cfg.limits.timeout = Duration::from_secs(n);
    }
    if let Some(n) = number("max_output_chars") {
        cfg.limits.max_output_chars = n as usize;
    }
    if let Some(n) = number("max_memory_mb") {
        cfg.limits.max_rss_kb = n * KB_PER_MB;
    }
    if let Some(n) = number("display_chars") {
        cfg.display_chars = n as usize;
    }
}

/// The scratch root: `tmp` under the playbook state directory.
pub fn scratch_root(home: &Path) -> PathBuf {
    crate::common::paths::playbook_root_from(home).join("tmp")
}

/// Which part of the document to print. All `None` is the first slice.
#[derive(Debug, Default)]
pub struct SliceRequest {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    pub section: Option<usize>,
}

impl SliceRequest {
    fn is_default(&self) -> bool {
        self.offset.is_none() && self.limit.is_none() && self.section.is_none()
    }
}

/// Why a read failed: the conversion itself, or a section number that does
/// not exist.
enum Failure {
    Doc(DocError),
    BadSection(String),
}

impl From<DocError> for Failure {
    fn from(err: DocError) -> Self {
        Self::Doc(err)
    }
}

/// The document text a read works on, from a conversion or from the cache.
struct Document {
    markdown: String,
    total_chars: usize,
    truncated: bool,
    version: String,
    /// `None` when the text came from the cache and nothing ran.
    isolation: Option<convert::Isolation>,
    cache: Option<PathBuf>,
}

/// What `read` prints, as text or as JSON.
struct Shown {
    text: String,
    format: String,
    chars: usize,
    offset: usize,
    next_offset: Option<usize>,
    doc: Document,
}

/// Run `playbook doc read`. Returns the process exit code: 0 on success, 2
/// for a section that does not exist, 3 when the document needs OCR, 1 for
/// every other failure.
pub fn read(home: &Path, file: &Path, json: bool, req: &SliceRequest) -> i32 {
    let cfg = settings(home);
    let root = scratch_root(home);
    tmp::sweep_stale(&root, STALE_AFTER);
    let cache_dir = slice::cache_dir(&crate::common::paths::playbook_root_from(home));
    slice::sweep_cache(&cache_dir, CACHE_STALE_AFTER);
    convert::install_signal_handlers();

    match run(&cfg, &root, &cache_dir, file, req) {
        Ok(shown) => {
            if json {
                println!("{}", shown_json(&shown));
            } else {
                print!("{}", shown.text);
            }
            0
        }
        Err(Failure::Doc(err)) => {
            let code = match err {
                DocError::NeedsOcr => 3,
                DocError::Interrupted(sig) => 128 + sig,
                _ => 1,
            };
            if json {
                println!(
                    "{}",
                    json!({"status": err.code(), "message": err.message()})
                );
            } else {
                eprintln!("doc read: {}", err.message());
            }
            code
        }
        Err(Failure::BadSection(message)) => {
            if json {
                println!("{}", json!({"status": "bad_section", "message": message}));
            } else {
                eprintln!("doc read: {message}");
            }
            2
        }
    }
}

fn shown_json(shown: &Shown) -> Value {
    let doc = &shown.doc;
    json!({
        "status": "ok",
        "format": shown.format,
        "chars": shown.chars,
        "total_chars": doc.total_chars,
        "offset": shown.offset,
        "next_offset": shown.next_offset,
        "tokens_estimate": frame::token_estimate(shown.chars),
        "truncated": doc.truncated,
        "converter_version": doc.version,
        "isolation": doc.isolation.map(convert::Isolation::label),
        "not_represented": frame::limitations(&shown.format),
        "content": shown.text,
    })
}

fn run(
    cfg: &Settings,
    root: &Path,
    cache_dir: &Path,
    file: &Path,
    req: &SliceRequest,
) -> Result<Shown, Failure> {
    let format = convert::format_of(file)?;
    let converter = Search::process(&cfg.anydoc_path)
        .locate()
        .map_err(DocError::Absent)?;
    let doc = match cached_document(cfg, cache_dir, &converter, file, req) {
        Some(doc) => doc,
        None => converted_document(cfg, root, &converter, file)?,
    };
    let source = frame::safe_name(file);
    let meta = frame::Meta {
        source: &source,
        format,
        id: &tmp::random_hex(12),
    };
    present(cfg, cache_dir, file, &meta, doc, req)
}

/// The cache key for `file`, or `None` when the file cannot be read or is
/// over the size limit, which the conversion then reports in its own words.
fn cache_name(cfg: &Settings, file: &Path, version: &str) -> Option<String> {
    let readable =
        fs::metadata(file).is_ok_and(|m| m.is_file() && m.len() <= cfg.limits.max_input_bytes);
    if !readable {
        return None;
    }
    slice::cache_name(file, version, cfg.limits.max_output_chars).ok()
}

/// The cached Markdown for a follow up read, or `None` on a miss, in which
/// case the caller converts again.
fn cached_document(
    cfg: &Settings,
    cache_dir: &Path,
    converter: &Converter,
    file: &Path,
    req: &SliceRequest,
) -> Option<Document> {
    if req.is_default() {
        return None;
    }
    let version = convert::version_of(&converter.path, &std::env::temp_dir());
    let path = cache_dir.join(cache_name(cfg, file, &version)?);
    let markdown = slice::read_cache(&path)?;
    let truncated_from = slice::read_truncation(&path);
    let total_chars = truncated_from.unwrap_or_else(|| markdown.chars().count());
    Some(Document {
        markdown,
        total_chars,
        truncated: truncated_from.is_some(),
        version,
        isolation: None,
        cache: Some(path),
    })
}

fn converted_document(
    cfg: &Settings,
    root: &Path,
    converter: &Converter,
    file: &Path,
) -> Result<Document, DocError> {
    let c = convert::convert(converter, file, &cfg.limits, root)?;
    Ok(Document {
        markdown: c.markdown,
        total_chars: c.total_chars,
        truncated: c.truncated,
        version: c.version,
        isolation: Some(c.isolation),
        cache: None,
    })
}

/// Print the whole document when it fits and nothing else was asked for,
/// otherwise one cached slice.
fn present(
    cfg: &Settings,
    cache_dir: &Path,
    file: &Path,
    meta: &frame::Meta,
    mut doc: Document,
    req: &SliceRequest,
) -> Result<Shown, Failure> {
    let chars = doc.markdown.chars().count();
    let fits = chars <= cfg.display_chars;
    if fits && req.is_default() {
        let text = frame::wrap(
            &doc.markdown,
            doc.truncated.then_some(doc.total_chars),
            doc.isolation,
            meta,
        );
        return Ok(Shown {
            text,
            format: meta.format.to_string(),
            chars,
            offset: 0,
            next_offset: None,
            doc,
        });
    }
    if !fits && doc.cache.is_none() {
        doc.cache = cache_name(cfg, file, &doc.version)
            .and_then(|name| write_entry(cache_dir, &name, &doc));
    }
    let limit = req
        .limit
        .unwrap_or(cfg.display_chars)
        .clamp(1, cfg.display_chars);
    let window = window_of(&doc.markdown, req, limit)?;
    let view = frame::SliceView {
        text: &window.text,
        offset: window.offset,
        total_chars: chars,
        truncated_from: doc.truncated.then_some(doc.total_chars),
        cache: doc.cache.as_deref(),
        sections: &slice::section_listing(&doc.markdown),
        next: window.next,
        file,
        isolation: doc.isolation,
    };
    let text = frame::wrap_slice(meta, &view);
    Ok(Shown {
        text,
        format: meta.format.to_string(),
        chars: window.text.chars().count(),
        offset: window.offset,
        next_offset: window.next,
        doc,
    })
}

/// Cache the Markdown, and for a truncated document the length it was cut
/// from, so later slices can still say it is incomplete.
fn write_entry(cache_dir: &Path, name: &str, doc: &Document) -> Option<PathBuf> {
    if doc.truncated {
        slice::write_truncation(cache_dir, name, doc.total_chars).ok()?;
    }
    slice::write_cache(cache_dir, name, &doc.markdown).ok()
}

struct Window {
    text: String,
    offset: usize,
    next: Option<usize>,
}

/// The slice a request asks for: a section, or `limit` characters from the
/// offset. After a section the next offset is where the following one starts.
fn window_of(md: &str, req: &SliceRequest, limit: usize) -> Result<Window, Failure> {
    let Some(k) = req.section else {
        let offset = req.offset.unwrap_or(0);
        let (text, next) = slice::slice(md, offset, limit);
        return Ok(Window { text, offset, next });
    };
    let index = slice::section_index(md);
    let Some((_, _, start)) = k.checked_sub(1).and_then(|i| index.get(i)) else {
        return Err(Failure::BadSection(section_range_message(k, index.len())));
    };
    let total = md.chars().count();
    let end = index.get(k).map_or(total, |(_, _, next_start)| *next_start);
    let section: String = md.chars().skip(*start).take(end - start).collect();
    let (text, next) = slice::slice(&section, 0, limit);
    let next = next.map(|n| start + n).or((end < total).then_some(end));
    Ok(Window {
        text,
        offset: *start,
        next,
    })
}

fn section_range_message(asked: usize, count: usize) -> String {
    if count == 0 {
        format!("section {asked} does not exist, this document has no sections, use --offset")
    } else {
        format!("section {asked} does not exist, choose a section from 1 to {count}")
    }
}

/// `playbook doc status`: whether the converter is available.
pub fn status(home: &Path) -> i32 {
    let cfg = settings(home);
    let report = Search::process(&cfg.anydoc_path).status();
    for line in &report.lines {
        println!("{line}");
    }
    report.exit_code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limits_are_the_built_in_values_whatever_is_stored() {
        // Arrange
        let home = crate::testing::scratch_root("doc-limits-fixed");
        let root = crate::common::paths::playbook_root_from(&home);
        for key in [
            "docs.maxBytes",
            "docs.timeoutSeconds",
            "docs.maxOutputChars",
            "docs.maxMemoryMb",
            "docs.displayChars",
        ] {
            crate::config::store::put(
                &root,
                crate::config::write::Tier::Global,
                None,
                key,
                &serde_json::json!(3),
            )
            .unwrap();
        }

        // Act
        let cfg = settings(&home);

        // Assert
        assert_eq!(cfg.limits.max_input_bytes, MAX_BYTES);
        assert_eq!(cfg.limits.timeout, Duration::from_secs(TIMEOUT_SECONDS));
        assert_eq!(cfg.limits.max_output_chars, MAX_OUTPUT_CHARS);
        assert_eq!(cfg.limits.max_rss_kb, MAX_MEMORY_MB * KB_PER_MB);
        assert_eq!(cfg.display_chars, DISPLAY_CHARS);
    }
}
