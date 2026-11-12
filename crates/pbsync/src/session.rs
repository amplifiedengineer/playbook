// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! What every network command needs before it talks to the remote: a transport
//! built from the settings and the token, and the local `usage.db`.

use std::path::Path;
use std::time::Duration;

use playbook_store::{Connection, OpenFlags, OptionalExtension};

use crate::settings::Settings;
use crate::token::resolve_token;
use crate::transport::HttpTransport;

const USAGE_DB_FILE: &str = "usage.db";
const BUSY_WAIT: Duration = Duration::from_secs(5);
const READ_MACHINE_SQL: &str = "SELECT machine_id FROM machine WHERE id = 1";
const NO_URL: &str = "sync has no url: run `playbook sync enable --url URL`";
pub const NO_MACHINE: &str =
    "usage.db has no machine id yet: run any playbook command, then try again";
pub const NO_USAGE_DB: &str =
    "usage.db does not exist yet: run any playbook command, then try again";

/// The transport for the configured remote. The token comes from the
/// environment variable the settings name, and no error text carries it.
pub fn connect(
    settings: &Settings,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<HttpTransport, String> {
    let token = resolve_token(None, &settings.token_env, env).map_err(|error| error.to_string())?;
    let url = settings.url.as_deref().ok_or(NO_URL)?;
    HttpTransport::new(url, token).map_err(|error| error.to_string())
}

/// The `usage.db` in the data folder, or none when it does not exist. It is
/// never created here: playbook owns its layout.
pub fn open_local(data_dir: &Path) -> Result<Option<Connection>, String> {
    let path = data_dir.join(USAGE_DB_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let local_error = |error: playbook_store::Error| format!("the local database failed: {error}");
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(local_error)?;
    conn.busy_timeout(BUSY_WAIT).map_err(local_error)?;
    Ok(Some(conn))
}

pub fn machine_id(local: &Connection) -> Result<Option<String>, String> {
    local
        .query_row(READ_MACHINE_SQL, [], |r| r.get(0))
        .optional()
        .map_err(|error| format!("the local database failed: {error}"))
}
