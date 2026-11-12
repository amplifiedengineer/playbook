// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Run the external `anydoc` converter in a bounded child process. The child
//! gets a scrubbed environment, no network where the OS can give that, a
//! wall clock timeout, a resident memory watchdog and a file size limit. The
//! converter is never given an OCR or hosted flag: its default is to refuse
//! pages that need OCR, which is the behavior wanted here.

use super::converter::{
    Converter, Kind, Missing, HARD_MAX_INPUT_BYTES, INSTALL_HINT, OUT_OF_DATE_MESSAGE,
};
use super::tmp::TempDir;
use rustix::process::{Pid, Signal};
use signal_hook::consts::{SIGINT, SIGTERM};
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Formats the converter supports, by lowercase extension.
pub const FORMATS: &[&str] = &[
    "pdf", "docx", "doc", "odt", "rtf", "epub", "pptx", "ppt", "odp", "xlsx", "ods", "csv",
];

/// Hard ceiling on the bytes read back from the converter's output file.
const READ_CEILING: u64 = 32 * 1024 * 1024;
/// Hard ceiling on any file the child may write.
const FILE_SIZE_LIMIT: u64 = 64 * 1024 * 1024;
/// Longest version text kept.
const VERSION_CHARS: usize = 32;
const POLL: Duration = Duration::from_millis(20);
const RSS_EVERY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_input_bytes: u64,
    pub timeout: Duration,
    pub max_output_chars: usize,
    pub max_rss_kb: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DocError {
    Absent(Missing),
    UnsupportedExtension(String),
    NotAFile,
    TooLarge { bytes: u64, max: u64 },
    NeedsOcr,
    Timeout(u64),
    MemoryLimit(u64),
    ResourceLimit(String),
    Interrupted(i32),
    OutOfDate,
    Failed(String),
    Io(String),
}

impl DocError {
    /// A plain sentence for the person or model reading the result.
    pub fn message(&self) -> String {
        match self {
            Self::Absent(Missing::NotInstalled) => {
                format!("no document converter found. {INSTALL_HINT}")
            }
            Self::Absent(why) => why.message(),
            Self::UnsupportedExtension(ext) => format!(
                "unsupported file type `{ext}`. Supported: {}",
                FORMATS.join(", ")
            ),
            Self::NotAFile => "the path is not a regular file".to_string(),
            Self::TooLarge { bytes, max } => {
                format!("the file is {bytes} bytes, over the limit of {max}")
            }
            Self::NeedsOcr => {
                "needs OCR, not supported locally. The document has pages with no text layer. \
                 Playbook never sends documents to a hosted service."
                    .to_string()
            }
            Self::Timeout(s) => {
                format!("conversion stopped after {s} seconds")
            }
            Self::MemoryLimit(mb) => {
                format!("conversion stopped, it used more than {mb} MB of memory")
            }
            Self::ResourceLimit(reason) => format!("the document hit a safety limit: {reason}"),
            Self::Interrupted(_) => "conversion interrupted".to_string(),
            Self::OutOfDate => OUT_OF_DATE_MESSAGE.to_string(),
            Self::Failed(why) => format!("conversion failed: {why}"),
            Self::Io(why) => format!("could not prepare the conversion: {why}"),
        }
    }

    /// Short stable code for `--json`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Absent(_) => "absent",
            Self::UnsupportedExtension(_) => "unsupported",
            Self::NotAFile => "not_a_file",
            Self::TooLarge { .. } => "too_large",
            Self::NeedsOcr => "needs_ocr",
            Self::Timeout(_) => "timeout",
            Self::MemoryLimit(_) => "memory_limit",
            Self::ResourceLimit(_) => "resource_limit",
            Self::Interrupted(_) => "interrupted",
            Self::OutOfDate => "out_of_date",
            Self::Failed(_) => "failed",
            Self::Io(_) => "io",
        }
    }
}

/// How the child was kept off the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    MacosSandbox,
    LinuxNamespace,
    /// No sandbox tool worked here. Only the environment was scrubbed.
    EnvironmentOnly,
}

impl Isolation {
    pub fn label(self) -> &'static str {
        match self {
            Self::MacosSandbox => "network blocked by the macOS sandbox",
            Self::LinuxNamespace => "network blocked by a Linux network namespace",
            Self::EnvironmentOnly => {
                "network isolation unavailable here, only the environment was scrubbed"
            }
        }
    }
}

pub struct Converted {
    pub markdown: String,
    pub total_chars: usize,
    pub truncated: bool,
    pub isolation: Isolation,
    pub version: String,
}

/// The extension in lowercase when it is a supported format.
pub fn format_of(path: &Path) -> Result<&'static str, DocError> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    FORMATS
        .iter()
        .find(|f| **f == ext)
        .copied()
        .ok_or(DocError::UnsupportedExtension(ext))
}

const FLAG_OUTPUT: &str = "-o";
const FLAG_END_OF_FLAGS: &str = "--";
const FLAG_MAX_INPUT_BYTES: &str = "--max-input-bytes";
const FLAG_MAX_MEMORY_MB: &str = "--max-memory-mb";
const KB_PER_MB: u64 = 1024;
const BYTES_PER_KB: u64 = 1024;
/// The unit of `ulimit -f`.
const FILE_BLOCK_BYTES: u64 = 512;
/// A converter exits with this code when it rejects its arguments.
const EXIT_USAGE: i32 = 2;
const EXIT_NEEDS_OCR: i32 = 3;
/// The bundled helper exits with this code when it hits a safety limit.
const EXIT_RESOURCE_LIMIT: i32 = 4;
/// Added to the memory limit for the address space cap, so mapped libraries
/// and thread stacks do not trip it before the memory cap does.
const ADDRESS_SPACE_HEADROOM: u64 = 512 * 1024 * 1024;
/// The line pbdoc's own watchdog prints. The outer watchdog reports the same
/// reason, so the message does not depend on which of the two fires first.
const REASON_MEMORY: &str = "memory limit reached";
const REASON_CRASH: &str = "memory limit or crash";

/// The signals a child gets when it runs out of address space or memory.
fn is_memory_crash(sig: i32) -> bool {
    [Signal::ABORT, Signal::SEGV, Signal::KILL]
        .iter()
        .any(|s| s.as_raw() == sig)
}

/// The arguments passed to the converter. Output goes to a file in the
/// private directory. No OCR, API key or URL flag is ever added here. The
/// bundled helper also gets the size and memory limits.
pub fn argv(kind: Kind, limits: &Limits, out: &Path, input: &Path) -> Vec<OsString> {
    let mut args = Vec::new();
    if kind == Kind::Bundled {
        args.extend([
            OsString::from(FLAG_MAX_INPUT_BYTES),
            OsString::from(limits.max_input_bytes.to_string()),
            OsString::from(FLAG_MAX_MEMORY_MB),
            OsString::from((limits.max_rss_kb / KB_PER_MB).to_string()),
        ]);
    }
    args.extend([
        OsString::from(FLAG_OUTPUT),
        out.as_os_str().to_os_string(),
        OsString::from(FLAG_END_OF_FLAGS),
        input.as_os_str().to_os_string(),
    ]);
    args
}

/// The environment the child sees. Only what a Node or native program needs
/// to start. Proxy variables, API keys and `NODE_OPTIONS` are never passed.
fn child_env(scratch: &Path) -> Vec<(&'static str, OsString)> {
    let mut env: Vec<(&'static str, OsString)> = Vec::new();
    for key in ["PATH", "HOME", "LANG", "LC_ALL"] {
        if let Some(v) = std::env::var_os(key) {
            env.push((key, v));
        }
    }
    env.push(("TMPDIR", scratch.as_os_str().to_os_string()));
    env
}

const MACOS_PROFILE: &str = "(version 1)(allow default)(deny network*)";
const MACOS_SANDBOX: &str = "/usr/bin/sandbox-exec";

fn probe(mut cmd: Command) -> bool {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::common::run_with_timeout(&mut cmd, Duration::from_secs(5))
        .is_some_and(|o| o.status.success())
}

fn detect_isolation() -> Isolation {
    if cfg!(target_os = "macos") && Path::new(MACOS_SANDBOX).exists() {
        let mut c = Command::new(MACOS_SANDBOX);
        c.args(["-p", MACOS_PROFILE, "/usr/bin/true"]);
        if probe(c) {
            return Isolation::MacosSandbox;
        }
    }
    if cfg!(target_os = "linux") {
        let mut c = Command::new("unshare");
        c.args(["--net", "--map-root-user", "true"]);
        if probe(c) {
            return Isolation::LinuxNamespace;
        }
    }
    Isolation::EnvironmentOnly
}

/// Set by the SIGINT and SIGTERM handlers.
static INTERRUPTED: OnceLock<[(i32, Arc<AtomicBool>); 2]> = OnceLock::new();

/// Make SIGINT and SIGTERM set a flag the wait loop sees, so the child is
/// stopped and the scratch directory removed before the process exits.
pub fn install_signal_handlers() {
    let flags = INTERRUPTED.get_or_init(|| {
        [SIGINT, SIGTERM].map(|sig| {
            let flag = Arc::new(AtomicBool::new(false));
            let _ = signal_hook::flag::register(sig, flag.clone());
            (sig, flag)
        })
    });
    for (_, flag) in flags {
        flag.store(false, Ordering::SeqCst);
    }
}

/// The signal that arrived since the handlers were installed, or 0.
fn pending_signal() -> i32 {
    INTERRUPTED
        .get()
        .and_then(|flags| {
            flags
                .iter()
                .find(|(_, flag)| flag.load(Ordering::SeqCst))
                .map(|(sig, _)| *sig)
        })
        .unwrap_or(0)
}

/// Resident memory in KB of every process in group `pgid`.
fn group_rss_kb(pgid: u32) -> u64 {
    let out = Command::new("ps")
        .args(["-axo", "pgid=,rss="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else { return 0 };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let g: u32 = it.next()?.parse().ok()?;
            let r: u64 = it.next()?.parse().ok()?;
            (g == pgid).then_some(r)
        })
        .sum()
}

/// SIGKILL to every process in the group `pgid` that `process_group(0)` made.
fn kill_process_group(pgid: u32) {
    if let Some(pgid) = i32::try_from(pgid).ok().and_then(Pid::from_raw) {
        let _ = rustix::process::kill_process_group(pgid, Signal::KILL);
    }
}

fn kill_group(child: &mut std::process::Child) {
    kill_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

const FLAG_VERSION: &str = "--version";

/// The converter's version, or an empty string when it will not say.
pub fn version_of(anydoc: &Path, scratch: &Path) -> String {
    output_of(anydoc, FLAG_VERSION, scratch)
}

/// Only the bundled helper gets an address space cap, because Node reserves a
/// large virtual range and would never start under one. macOS accepts neither
/// RLIMIT_AS nor RLIMIT_DATA, so there the helper's own watchdog and the
/// resident size watchdog below are the memory limits.
fn address_space_cap(kind: Kind, limits: &Limits) -> Option<u64> {
    (cfg!(target_os = "linux") && kind == Kind::Bundled).then(|| {
        limits
            .max_rss_kb
            .saturating_mul(BYTES_PER_KB)
            .saturating_add(ADDRESS_SPACE_HEADROOM)
    })
}

/// Wraps `cmd` in `sh` so the limits are set before it runs, since std has no
/// safe way to call `setrlimit` between fork and exec. The shell caps the
/// size of any file the child writes, turns off core dumps and applies the
/// address space cap when there is one, then replaces itself with the command.
/// A limit the shell cannot set is skipped, as a failed `setrlimit` was.
fn with_limits(cmd: Command, address_space: Option<u64>) -> Command {
    // `ulimit -f` counts 512-byte blocks in POSIX mode, `-v` counts KiB.
    let mut script = format!(
        "ulimit -c 0 2>/dev/null; ulimit -f {} 2>/dev/null; ",
        FILE_SIZE_LIMIT / FILE_BLOCK_BYTES
    );
    if let Some(bytes) = address_space {
        script.push_str(&format!("ulimit -v {} 2>/dev/null; ", bytes / BYTES_PER_KB));
    }
    script.push_str("exec \"$@\"");
    let mut wrapped = Command::new("/bin/sh");
    wrapped
        .args(["-c", &script, "sh"])
        .arg(cmd.get_program())
        .args(cmd.get_args());
    wrapped
}

/// What the converter prints for `flag`, trimmed and short, or an empty
/// string when it fails or takes too long.
pub fn output_of(anydoc: &Path, flag: &str, scratch: &Path) -> String {
    let mut cmd = Command::new(anydoc);
    cmd.arg(flag).env_clear().stdin(Stdio::null());
    for (k, v) in child_env(scratch) {
        cmd.env(k, v);
    }
    crate::common::run_with_timeout(&mut cmd, Duration::from_secs(5))
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .chars()
                .take(VERSION_CHARS)
                .collect()
        })
        .unwrap_or_default()
}

/// Convert `input` and return the Markdown. The scratch directory lives under
/// `scratch_root` and is gone when this returns, whatever the outcome.
pub fn convert(
    converter: &Converter,
    input: &Path,
    limits: &Limits,
    scratch_root: &Path,
) -> Result<Converted, DocError> {
    format_of(input)?;
    let meta = fs::metadata(input).map_err(|e| DocError::Io(e.kind().to_string()))?;
    if !meta.is_file() {
        return Err(DocError::NotAFile);
    }
    let max_input = limits.max_input_bytes.min(HARD_MAX_INPUT_BYTES);
    if meta.len() > max_input {
        return Err(DocError::TooLarge {
            bytes: meta.len(),
            max: max_input,
        });
    }
    // Absolute but not resolved: the child runs in the scratch directory, and
    // a symlink named `data.csv` must keep the extension that sets the format.
    let input = std::path::absolute(input).map_err(|e| DocError::Io(e.kind().to_string()))?;
    let scratch = TempDir::create(scratch_root).map_err(|e| DocError::Io(e.to_string()))?;
    let out = scratch.path().join("out.md");
    let err_file = scratch.path().join("err.txt");
    let isolation = detect_isolation();
    let anydoc = converter.path.as_path();
    let version = version_of(anydoc, scratch.path());

    let mut cmd = match isolation {
        Isolation::MacosSandbox => {
            let mut c = Command::new(MACOS_SANDBOX);
            c.args(["-p", MACOS_PROFILE]).arg(anydoc);
            c
        }
        Isolation::LinuxNamespace => {
            let mut c = Command::new("unshare");
            c.args(["--net", "--map-root-user"]).arg(anydoc);
            c
        }
        Isolation::EnvironmentOnly => Command::new(anydoc),
    };
    cmd.args(argv(converter.kind, limits, &out, &input));
    let mut cmd = with_limits(cmd, address_space_cap(converter.kind, limits));
    cmd.env_clear();
    for (k, v) in child_env(scratch.path()) {
        cmd.env(k, v);
    }
    let stderr = fs::File::create(&err_file).map_err(|e| DocError::Io(e.to_string()))?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .current_dir(scratch.path())
        .process_group(0);
    let mut child = cmd.spawn().map_err(|e| DocError::Io(e.to_string()))?;

    let start = Instant::now();
    let mut last_rss = Instant::now();
    let status = loop {
        let sig = pending_signal();
        if sig != 0 {
            kill_group(&mut child);
            return Err(DocError::Interrupted(sig));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                kill_group(&mut child);
                return Err(DocError::Io(e.to_string()));
            }
        }
        if start.elapsed() >= limits.timeout {
            kill_group(&mut child);
            return Err(DocError::Timeout(limits.timeout.as_secs()));
        }
        if last_rss.elapsed() >= RSS_EVERY {
            last_rss = Instant::now();
            if group_rss_kb(child.id()) > limits.max_rss_kb {
                kill_group(&mut child);
                return Err(if converter.kind == Kind::Bundled {
                    DocError::ResourceLimit(REASON_MEMORY.to_string())
                } else {
                    DocError::MemoryLimit(limits.max_rss_kb / KB_PER_MB)
                });
            }
        }
        std::thread::sleep(POLL);
    };
    // Anything the converter left running in its group goes too.
    kill_process_group(child.id());

    if !status.success() {
        match status.code() {
            Some(EXIT_NEEDS_OCR) => return Err(DocError::NeedsOcr),
            Some(EXIT_RESOURCE_LIMIT) if converter.kind == Kind::Bundled => {
                return Err(DocError::ResourceLimit(first_line(&err_file)));
            }
            Some(EXIT_USAGE) if converter.kind == Kind::Bundled => return Err(DocError::OutOfDate),
            _ => {}
        }
        if converter.kind == Kind::Bundled && status.signal().is_some_and(is_memory_crash) {
            return Err(DocError::ResourceLimit(REASON_CRASH.to_string()));
        }
        return Err(DocError::Failed(first_line(&err_file)));
    }

    let meta = fs::symlink_metadata(&out).map_err(|_| DocError::Failed("no output".into()))?;
    if !meta.is_file() {
        return Err(DocError::Failed("the output is not a regular file".into()));
    }
    let mut bytes = Vec::new();
    fs::File::open(&out)
        .and_then(|f| f.take(READ_CEILING).read_to_end(&mut bytes))
        .map_err(|e| DocError::Io(e.to_string()))?;
    let text = String::from_utf8_lossy(&bytes);
    let total_chars = text.chars().count();
    let (markdown, truncated) = if total_chars > limits.max_output_chars {
        (text.chars().take(limits.max_output_chars).collect(), true)
    } else {
        (text.into_owned(), false)
    };
    Ok(Converted {
        markdown,
        total_chars,
        truncated: truncated || meta.len() > READ_CEILING,
        isolation,
        version,
    })
}

/// The prefix the bundled helper puts on every error line.
const FALLBACK_ERROR: &str = "the converter exited with an error";
const ERROR_PREFIX: &str = "error: ";

/// The first line of the converter's error output, printable and short, with
/// the helper's own `error: ` prefix dropped so a caller can add its own.
fn first_line(path: &Path) -> String {
    let mut buf = Vec::new();
    let _ = fs::File::open(path).and_then(|f| f.take(2048).read_to_end(&mut buf));
    let text = String::from_utf8_lossy(&buf);
    let first = text.lines().next().unwrap_or(FALLBACK_ERROR);
    let line: String = first
        .strip_prefix(ERROR_PREFIX)
        .unwrap_or(first)
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    if line.is_empty() {
        FALLBACK_ERROR.to_string()
    } else {
        line
    }
}
