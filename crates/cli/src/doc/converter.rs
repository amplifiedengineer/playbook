// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The one place that knows which document converter runs. It finds the
//! converter (the configured `docs.anydocPath`, else the bundled `pbdoc` next
//! to the playbook binary, else `anydoc` on PATH), says where it came from,
//! reports its versions, words the `doc status` lines, the doctor row and the
//! install hints, and owns the input size ceiling. `doc read`, `doc status`,
//! `doctor` and the read hint hook all go through it.

use super::convert::{output_of, version_of};
pub use crate::config::keys::HARD_MAX_INPUT_BYTES;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

/// The converter version these safeguards were tested with.
pub const TESTED_VERSION: &str = "0.2";

const BUNDLED_NAME: &str = "pbdoc";
const EXTERNAL_NAME: &str = "anydoc";
const CONFIG_KEY: &str = "docs.anydocPath";
const FLAG_CONVERTER_VERSION: &str = "--converter-version";
const UNKNOWN: &str = "unknown";

/// Shown when no converter is found.
pub const INSTALL_HINT: &str = "the bundled converter pbdoc is missing next to the playbook \
binary. Reinstall playbook with install.sh or `brew reinstall playbook`, or run `playbook upgrade`.";

/// Shown when a bundled converter exits 2 (it rejected its arguments).
pub const OUT_OF_DATE_MESSAGE: &str =
    "the bundled converter pbdoc is out of date for this playbook. Run `playbook upgrade`.";

pub const STATUS_BUNDLED_PREFIX: &str = "converter: pbdoc (bundled) at ";
pub const STATUS_VERSION_PREFIX: &str = "version: pbdoc ";
pub const STATUS_ANYDOC_PREFIX: &str = "anydoc: ";
pub const STATUS_EXTERNAL_FOUND_PREFIX: &str = "anydoc: found at ";
pub const STATUS_EXTERNAL_VERSION_PREFIX: &str = "version: ";
pub const STATUS_NOT_FOUND_PREFIX: &str = "converter: not found. ";
pub const STATUS_EXTERNAL_NOT_FOUND_PREFIX: &str = "anydoc: not found. ";
pub const STATUS_OVERRIDE_NOTE: &str = "note: override active: the configured converter runs \
instead of the bundled one, and playbook does not check that it is offline";
pub const STATUS_NPM_NOTE: &str = "note: the bundled converter is used; you can remove the npm \
package with npm uninstall --global @firecrawl/anydoc";

pub const DOCTOR_ROW_BUNDLED: &str = "doc converter: pbdoc (bundled)";
pub const DOCTOR_ROW_EXTERNAL: &str =
    "doc converter: external anydoc, playbook does not check that it is offline";
pub const DOCTOR_ROW_NONE: &str = "doc converter: not found";
pub const DOCTOR_ROW_SKEW: &str = "doc converter: pbdoc version differs from playbook";

/// The `docs.anydocPath` value, empty when unset.
pub fn configured_path(home: &Path, slug: Option<&str>) -> String {
    crate::config::resolve_valid(CONFIG_KEY, home, slug)
        .ok()
        .and_then(|(v, _, _)| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The bare version in the output of `pbdoc --version` (`pbdoc 1.2.3`).
pub fn pbdoc_number(version_output: &str) -> &str {
    version_output
        .strip_prefix(BUNDLED_NAME)
        .map_or(version_output, str::trim_start)
}

/// The status note for a bundled converter whose version is not playbook's.
pub fn version_skew_note(pbdoc: &str, playbook: &str) -> String {
    format!("note: pbdoc {pbdoc} does not match playbook {playbook}. Run playbook upgrade.")
}

/// Where a converter came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bundled,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Converter {
    pub path: PathBuf,
    pub kind: Kind,
}

/// Why no converter was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    /// `docs.anydocPath` is set but names no executable file. The search does
    /// not fall back, so a typo never silently runs another converter.
    ConfiguredNotRunnable(PathBuf),
    NotInstalled,
}

impl Missing {
    /// A plain sentence that says what to do.
    pub fn message(&self) -> String {
        match self {
            Self::ConfiguredNotRunnable(path) => format!(
                "{CONFIG_KEY} is set to {} but that is not an executable file. Fix the path, or \
                 clear the key to use the bundled converter.",
                path.display()
            ),
            Self::NotInstalled => INSTALL_HINT.to_string(),
        }
    }
}

/// What the doctor row says about the converter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Ready,
    VersionSkew,
    External,
    Missing,
}

impl Health {
    /// The doctor row text.
    pub fn message(&self) -> &'static str {
        match self {
            Self::Ready => DOCTOR_ROW_BUNDLED,
            Self::VersionSkew => DOCTOR_ROW_SKEW,
            Self::External => DOCTOR_ROW_EXTERNAL,
            Self::Missing => DOCTOR_ROW_NONE,
        }
    }
}

/// The output of `doc status`.
#[derive(Debug, PartialEq, Eq)]
pub struct StatusReport {
    pub lines: Vec<String>,
    pub exit_code: i32,
}

/// The inputs of a converter lookup. Tests build one by hand, the commands
/// build it from the running process.
#[derive(Debug, Clone)]
pub struct Search {
    configured: String,
    exe: Option<PathBuf>,
    path_var: Option<OsString>,
}

impl Search {
    pub fn new(configured: &str, exe: Option<&Path>, path_var: Option<&OsStr>) -> Self {
        Self {
            configured: configured.to_string(),
            exe: exe.map(Path::to_path_buf),
            path_var: path_var.map(OsStr::to_os_string),
        }
    }

    /// The playbook binary and PATH of this process.
    pub fn process(configured: &str) -> Self {
        Self::new(
            configured,
            std::env::current_exe().ok().as_deref(),
            std::env::var_os("PATH").as_deref(),
        )
    }

    /// The same search with another PATH value.
    pub fn with_path(mut self, path_var: &OsStr) -> Self {
        self.path_var = Some(path_var.to_os_string());
        self
    }

    /// The converter to run: the configured path when set, else `pbdoc` next
    /// to the playbook binary, else the first executable `anydoc` on PATH.
    pub fn locate(&self) -> Result<Converter, Missing> {
        if !self.configured.is_empty() {
            let path = PathBuf::from(&self.configured);
            return if is_executable(&path) {
                Ok(Converter {
                    path,
                    kind: Kind::External,
                })
            } else {
                Err(Missing::ConfiguredNotRunnable(path))
            };
        }
        self.bundled()
            .or_else(|| self.on_path())
            .ok_or(Missing::NotInstalled)
    }

    /// Whether any converter can run.
    pub fn is_available(&self) -> bool {
        self.locate().is_ok()
    }

    /// The `pbdoc` beside the binary as invoked, else beside its canonical
    /// location, so a symlinked `playbook` still finds the helper.
    fn bundled(&self) -> Option<Converter> {
        let exe = self.exe.as_deref()?;
        let as_invoked = exe.parent().map(Path::to_path_buf);
        let canonical = fs::canonicalize(exe)
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        as_invoked
            .into_iter()
            .chain(canonical)
            .map(|dir| dir.join(BUNDLED_NAME))
            .find(|p| is_executable(p))
            .map(|path| Converter {
                path,
                kind: Kind::Bundled,
            })
    }

    /// The first executable `anydoc` on PATH.
    fn on_path(&self) -> Option<Converter> {
        std::env::split_paths(self.path_var.as_deref()?)
            .map(|dir| dir.join(EXTERNAL_NAME))
            .find(|p| is_executable(p))
            .map(|path| Converter {
                path,
                kind: Kind::External,
            })
    }

    /// The doctor row state, which runs the converter once for its version.
    pub fn health(&self) -> Health {
        match self.locate() {
            Ok(found) if found.kind == Kind::Bundled => {
                let version = version_of(&found.path, &std::env::temp_dir());
                if pbdoc_number(&version) == env!("CARGO_PKG_VERSION") {
                    Health::Ready
                } else {
                    Health::VersionSkew
                }
            }
            Ok(_) => Health::External,
            Err(_) => Health::Missing,
        }
    }

    /// The lines `doc status` prints and the exit code that goes with them.
    pub fn status(&self) -> StatusReport {
        let found = match self.locate() {
            Ok(found) => found,
            Err(missing) => return Self::not_found(&missing),
        };
        let scratch = std::env::temp_dir();
        let version = version_of(&found.path, &scratch);
        let lines = match found.kind {
            Kind::Bundled => self.bundled_lines(&found, &version),
            Kind::External => self.external_lines(&found, &version),
        };
        StatusReport {
            lines,
            exit_code: 0,
        }
    }

    fn not_found(missing: &Missing) -> StatusReport {
        let prefix = match missing {
            Missing::ConfiguredNotRunnable(_) => STATUS_EXTERNAL_NOT_FOUND_PREFIX,
            Missing::NotInstalled => STATUS_NOT_FOUND_PREFIX,
        };
        StatusReport {
            lines: vec![format!("{prefix}{}", missing.message())],
            exit_code: 1,
        }
    }

    fn bundled_lines(&self, found: &Converter, version: &str) -> Vec<String> {
        let own = pbdoc_number(version);
        let mut lines = vec![
            format!("{STATUS_BUNDLED_PREFIX}{}", found.path.display()),
            format!("{STATUS_VERSION_PREFIX}{own}"),
            format!(
                "{STATUS_ANYDOC_PREFIX}{}",
                anydoc_version(&found.path).unwrap_or_else(|| UNKNOWN.to_string())
            ),
        ];
        if own != env!("CARGO_PKG_VERSION") {
            lines.push(version_skew_note(own, env!("CARGO_PKG_VERSION")));
        }
        if self.on_path().is_some() {
            lines.push(STATUS_NPM_NOTE.to_string());
        }
        lines
    }

    fn external_lines(&self, found: &Converter, version: &str) -> Vec<String> {
        let shown = if version.is_empty() { UNKNOWN } else { version };
        let mut lines = vec![
            format!("{STATUS_EXTERNAL_FOUND_PREFIX}{}", found.path.display()),
            format!("{STATUS_EXTERNAL_VERSION_PREFIX}{shown}"),
        ];
        if !version.is_empty() && !version.starts_with(TESTED_VERSION) {
            lines.push(format!(
                "note: playbook was tested with {TESTED_VERSION}.x, other versions may behave \
                 differently"
            ));
        }
        if self.bundled().is_some() {
            lines.push(STATUS_OVERRIDE_NOTE.to_string());
        }
        lines
    }
}

/// The anydoc version inside the bundled helper, from the helper itself, so
/// the number has one source: the helper's pinned dependency.
fn anydoc_version(pbdoc: &Path) -> Option<String> {
    let said = output_of(pbdoc, FLAG_CONVERTER_VERSION, &std::env::temp_dir());
    let number = said
        .strip_prefix(EXTERNAL_NAME)
        .map_or(said.as_str(), str::trim);
    (!number.is_empty()).then(|| number.to_string())
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A scratch directory removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "playbook-find-converter-{}-{n}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            // Resolve the temp dir symlink (macOS /var) so canonical paths compare equal.
            Self(dir.canonicalize().unwrap())
        }

        fn file(&self, rel: &str, mode: u32) -> PathBuf {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
            p
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const EXEC: u32 = 0o755;
    const PLAIN: u32 = 0o644;

    fn bundled(path: PathBuf) -> Result<Converter, Missing> {
        Ok(Converter {
            path,
            kind: Kind::Bundled,
        })
    }

    fn external(path: PathBuf) -> Result<Converter, Missing> {
        Ok(Converter {
            path,
            kind: Kind::External,
        })
    }

    fn locate(
        configured: &str,
        exe: Option<&Path>,
        path_var: Option<&OsStr>,
    ) -> Result<Converter, Missing> {
        Search::new(configured, exe, path_var).locate()
    }

    #[test]
    fn a_configured_path_wins_over_a_bundled_sibling() {
        // Arrange
        let s = Scratch::new();
        let exe = s.file("bin/playbook", EXEC);
        s.file("bin/pbdoc", EXEC);
        let configured = s.file("custom/anydoc", EXEC);

        // Act
        let found = locate(configured.to_str().unwrap(), Some(&exe), None);

        // Assert
        assert_eq!(found, external(configured));
    }

    #[test]
    fn a_configured_path_that_is_not_executable_is_named_and_not_replaced() {
        // Arrange
        let s = Scratch::new();
        let exe = s.file("bin/playbook", EXEC);
        s.file("bin/pbdoc", EXEC);
        let configured = s.file("custom/anydoc", PLAIN);

        // Act
        let found = locate(configured.to_str().unwrap(), Some(&exe), None);

        // Assert
        let missing = found.unwrap_err();
        assert_eq!(missing, Missing::ConfiguredNotRunnable(configured.clone()));
        let message = missing.message();
        assert!(message.contains("docs.anydocPath"), "{message}");
        assert!(message.contains(configured.to_str().unwrap()), "{message}");
    }

    #[test]
    fn a_bundled_sibling_wins_over_anydoc_on_path() {
        // Arrange
        let s = Scratch::new();
        let exe = s.file("bin/playbook", EXEC);
        let pbdoc = s.file("bin/pbdoc", EXEC);
        s.file("npm/anydoc", EXEC);
        let path_var = s.0.join("npm");

        // Act
        let found = locate("", Some(&exe), Some(path_var.as_os_str()));

        // Assert
        assert_eq!(found, bundled(pbdoc));
    }

    #[test]
    fn a_sibling_that_is_not_executable_falls_back_to_anydoc_on_path() {
        // Arrange
        let s = Scratch::new();
        let exe = s.file("bin/playbook", EXEC);
        s.file("bin/pbdoc", PLAIN);
        let anydoc = s.file("npm/anydoc", EXEC);
        let path_var = s.0.join("npm");

        // Act
        let found = locate("", Some(&exe), Some(path_var.as_os_str()));

        // Assert
        assert_eq!(found, external(anydoc));
    }

    #[test]
    fn anydoc_on_path_is_found_when_the_binary_path_is_unknown() {
        // Arrange
        let s = Scratch::new();
        let anydoc = s.file("npm/anydoc", EXEC);
        let path_var = s.0.join("npm");

        // Act
        let found = locate("", None, Some(path_var.as_os_str()));

        // Assert
        assert_eq!(found, external(anydoc));
    }

    #[test]
    fn nothing_found_says_the_helper_is_not_installed() {
        // Arrange
        let s = Scratch::new();
        let exe = s.file("bin/playbook", EXEC);
        let empty_path = s.0.join("empty");

        // Act
        let found = locate("", Some(&exe), Some(empty_path.as_os_str()));

        // Assert
        assert_eq!(found, Err(Missing::NotInstalled));
        assert_eq!(Missing::NotInstalled.message(), INSTALL_HINT);
    }

    #[test]
    fn an_absolute_symlink_to_the_binary_finds_pbdoc_in_the_canonical_dir() {
        // Arrange
        let s = Scratch::new();
        let target = s.file("cellar/bin/playbook", EXEC);
        let pbdoc = s.file("cellar/bin/pbdoc", EXEC);
        fs::create_dir_all(s.0.join("bin")).unwrap();
        let link = s.0.join("bin/playbook");
        symlink(&target, &link).unwrap();

        // Act
        let found = locate("", Some(&link), None);

        // Assert
        assert_eq!(found, bundled(pbdoc));
    }

    #[test]
    fn a_relative_symlink_to_the_binary_finds_pbdoc_in_the_canonical_dir() {
        // Arrange
        let s = Scratch::new();
        s.file("cellar/bin/playbook", EXEC);
        let pbdoc = s.file("cellar/bin/pbdoc", EXEC);
        fs::create_dir_all(s.0.join("bin")).unwrap();
        let link = s.0.join("bin/playbook");
        symlink("../cellar/bin/playbook", &link).unwrap();

        // Act
        let found = locate("", Some(&link), None);

        // Assert
        assert_eq!(found, bundled(pbdoc));
    }

    #[test]
    fn a_pbdoc_next_to_the_symlink_is_found_before_the_canonical_dir() {
        // Arrange
        let s = Scratch::new();
        let target = s.file("cellar/bin/playbook", EXEC);
        s.file("cellar/bin/pbdoc", EXEC);
        fs::create_dir_all(s.0.join("bin")).unwrap();
        let link = s.0.join("bin/playbook");
        symlink(&target, &link).unwrap();
        let beside_link = s.file("bin/pbdoc", EXEC);

        // Act
        let found = locate("", Some(&link), None);

        // Assert
        assert_eq!(found, bundled(beside_link));
    }

    #[test]
    fn the_ceiling_equals_the_one_the_bundled_helper_enforces() {
        // Arrange
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pbdoc/src/limits.rs");
        let text = fs::read_to_string(source).unwrap();

        // Act
        let helper: u64 = text
            .lines()
            .find(|l| l.contains("const HARD_MAX_INPUT_BYTES"))
            .and_then(|l| l.split('=').nth(1))
            .map(|v| v.trim().trim_end_matches(';').replace('_', ""))
            .and_then(|v| v.parse().ok())
            .expect("pbdoc declares HARD_MAX_INPUT_BYTES");

        // Assert
        assert_eq!(helper, HARD_MAX_INPUT_BYTES);
    }
}
