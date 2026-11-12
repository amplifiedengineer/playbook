// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The sync settings file `sync.json` in the playbook data folder. It never
//! holds the token: it names the environment variable that does.

use std::fmt;
use std::path::{Path, PathBuf};

use playbook_common::atomic::{ensure_private_dir, write_atomic_private};
use serde_json::{json, Map, Value};

use crate::token::DEFAULT_TOKEN_ENV;

pub const SETTINGS_FILE: &str = "sync.json";
pub const SETTINGS_VERSION: u64 = 1;
pub const DEFAULT_INTERVAL_SECONDS: u64 = 900;

const KEY_VERSION: &str = "version";
const KEY_ENABLED: &str = "enabled";
const KEY_URL: &str = "url";
const KEY_ACCOUNTS: &str = "accounts";
const KEY_TOKEN_ENV: &str = "token_env";
const KEY_INTERVAL: &str = "interval_seconds";
const KEY_ENABLED_AT: &str = "enabled_at";
const KEY_CONSENTED: &str = "consented";
const KEY_RESET: &str = "reset_watermark";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub url: Option<String>,
    /// Local account keys that may sync. Empty means nothing syncs.
    pub accounts: Vec<String>,
    pub token_env: String,
    pub interval_seconds: u64,
    pub enabled_at: Option<i64>,
    pub consented: bool,
    /// Set when the account list changed: the next push re-sends every row.
    pub reset_watermark: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            url: None,
            accounts: Vec::new(),
            token_env: DEFAULT_TOKEN_ENV.to_string(),
            interval_seconds: DEFAULT_INTERVAL_SECONDS,
            enabled_at: None,
            consented: false,
            reset_watermark: false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SettingsError {
    Corrupt {
        path: PathBuf,
        reason: String,
    },
    UnsupportedVersion {
        path: PathBuf,
        version: u64,
    },
    Io {
        path: PathBuf,
        reason: String,
    },
    /// A network command ran while sync is off.
    Disabled,
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt { path, reason } => write!(
                f,
                "{} is not valid sync settings ({reason}): fix or delete the file; it was left unchanged",
                path.display()
            ),
            Self::UnsupportedVersion { path, version } => write!(
                f,
                "{} has settings version {version} but this pbsync understands {SETTINGS_VERSION}: update pbsync",
                path.display()
            ),
            Self::Io { path, reason } => write!(f, "cannot access {}: {reason}", path.display()),
            Self::Disabled => f.write_str(
                "sync is off: run `playbook sync enable --url URL` first; nothing was sent",
            ),
        }
    }
}

impl std::error::Error for SettingsError {}

/// The gate every network command passes before it opens a connection.
pub fn require_enabled(settings: &Settings) -> Result<(), SettingsError> {
    if settings.enabled {
        Ok(())
    } else {
        Err(SettingsError::Disabled)
    }
}

pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SETTINGS_FILE)
}

impl Settings {
    /// A missing file means everything is off. A corrupt file is an error and
    /// is never rewritten.
    pub fn load(data_dir: &Path) -> Result<Self, SettingsError> {
        let path = settings_path(data_dir);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(SettingsError::Io {
                    path,
                    reason: e.to_string(),
                })
            }
        };
        Self::parse(&text, &path)
    }

    /// Writes atomically; a new file is owner-only on unix.
    pub fn save(&self, data_dir: &Path) -> Result<(), SettingsError> {
        let path = settings_path(data_dir);
        let io = |e: std::io::Error| SettingsError::Io {
            path: path.clone(),
            reason: e.to_string(),
        };
        ensure_private_dir(data_dir).map_err(io)?;
        let text = serde_json::to_string_pretty(&self.to_json()).unwrap_or_default();
        write_atomic_private(&path, text).map_err(io)
    }

    fn to_json(&self) -> Value {
        json!({
            KEY_VERSION: SETTINGS_VERSION,
            KEY_ENABLED: self.enabled,
            KEY_URL: self.url,
            KEY_ACCOUNTS: self.accounts,
            KEY_TOKEN_ENV: self.token_env,
            KEY_INTERVAL: self.interval_seconds,
            KEY_ENABLED_AT: self.enabled_at,
            KEY_CONSENTED: self.consented,
            KEY_RESET: self.reset_watermark,
        })
    }

    fn parse(text: &str, path: &Path) -> Result<Self, SettingsError> {
        let corrupt = |reason: String| SettingsError::Corrupt {
            path: path.to_path_buf(),
            reason,
        };
        let value: Value = serde_json::from_str(text).map_err(|e| corrupt(e.to_string()))?;
        let object = value
            .as_object()
            .ok_or_else(|| corrupt("not a JSON object".to_string()))?;
        let version = object
            .get(KEY_VERSION)
            .and_then(Value::as_u64)
            .ok_or_else(|| corrupt(format!("missing or invalid {KEY_VERSION}")))?;
        if version != SETTINGS_VERSION {
            return Err(SettingsError::UnsupportedVersion {
                path: path.to_path_buf(),
                version,
            });
        }
        Self::from_object(object).map_err(corrupt)
    }

    fn from_object(object: &Map<String, Value>) -> Result<Self, String> {
        let defaults = Self::default();
        let wrong = |key: &str| format!("{key} has the wrong type");
        let field = |key: &str| object.get(key).filter(|v| !v.is_null());
        let text = |key: &str| {
            field(key)
                .map(|v| v.as_str().map(str::to_string).ok_or_else(|| wrong(key)))
                .transpose()
        };
        let flag = |key: &str| {
            field(key)
                .map(|v| v.as_bool().ok_or_else(|| wrong(key)))
                .transpose()
        };
        let accounts = match field(KEY_ACCOUNTS) {
            None => defaults.accounts,
            Some(v) => v
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .map(|i| i.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| wrong(KEY_ACCOUNTS))?,
        };
        Ok(Self {
            enabled: flag(KEY_ENABLED)?.unwrap_or(defaults.enabled),
            url: text(KEY_URL)?,
            accounts,
            token_env: text(KEY_TOKEN_ENV)?.unwrap_or(defaults.token_env),
            interval_seconds: field(KEY_INTERVAL)
                .map(|v| v.as_u64().ok_or_else(|| wrong(KEY_INTERVAL)))
                .transpose()?
                .unwrap_or(defaults.interval_seconds),
            enabled_at: field(KEY_ENABLED_AT)
                .map(|v| v.as_i64().ok_or_else(|| wrong(KEY_ENABLED_AT)))
                .transpose()?,
            consented: flag(KEY_CONSENTED)?.unwrap_or(defaults.consented),
            reset_watermark: flag(KEY_RESET)?.unwrap_or(defaults.reset_watermark),
        })
    }
}
