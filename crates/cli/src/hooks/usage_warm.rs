// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! SessionStart and SessionEnd hook that warms the usage store. It starts a
//! detached, low priority, time bounded ingest and returns at once, with no
//! output, so it never slows or blocks a session. The plugin-disabled pause is
//! handled before any hook runs. See `usage::warm`.

use crate::common::payload::Payload;
use crate::usage::run::Paths;

pub fn run(payload: &Payload) {
    // A real hook call always names its session; a malformed one does nothing.
    if payload.field(".session_id").is_empty() {
        return;
    }
    let dir = crate::common::paths::usage_db_dir();
    let home = crate::common::home_dir();
    let mut paths = Paths::from_home(&home);
    // Only a session start sees the credential the session begins with. The
    // token itself is hashed here and never leaves this call. Without one the
    // session uses the signed in account, which is named now because a later
    // sign in must not relabel it.
    if payload.field(".hook_event_name") == "SessionStart" {
        let _ = std::fs::create_dir_all(&dir);
        crate::usage::session_account::record(
            &dir,
            &payload.field(".session_id"),
            session_key(
                crate::usage::session_account::env_fingerprint(),
                signed_in_label(&paths.claude_json),
            )
            .as_deref(),
            crate::common::time::now_secs(),
        );
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    paths.db = dir.join("usage.db");
    crate::usage::warm::maybe_spawn(&paths, &exe);
}

/// The account `~/.claude.json` is signed in to, or none when it names none.
fn signed_in_label(claude_json: &std::path::Path) -> Option<String> {
    Some(crate::usage::account::account_label(claude_json))
        .filter(|label| label != crate::usage::account::UNKNOWN_ACCOUNT)
}

/// The key a session is filed under: the token fingerprint when the session
/// has a token, else the signed in account.
fn session_key(token: Option<String>, signed_in: Option<String>) -> Option<String> {
    token.or(signed_in)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;

    const TOKEN_KEY: &str = "token:4243ddb1";
    const EMAIL: &str = "dev@example.com";

    #[test]
    fn a_token_alone_gives_the_token_key() {
        // Arrange
        let token = Some(TOKEN_KEY.to_string());
        // Act
        let key = session_key(token, None);
        // Assert
        assert_eq!(key.as_deref(), Some(TOKEN_KEY));
    }

    #[test]
    fn an_email_alone_gives_the_email() {
        // Arrange
        let signed_in = Some(EMAIL.to_string());
        // Act
        let key = session_key(None, signed_in);
        // Assert
        assert_eq!(key.as_deref(), Some(EMAIL));
    }

    #[test]
    fn a_token_and_an_email_give_the_token_key() {
        // Arrange
        let token = Some(TOKEN_KEY.to_string());
        let signed_in = Some(EMAIL.to_string());
        // Act
        let key = session_key(token, signed_in);
        // Assert
        assert_eq!(key.as_deref(), Some(TOKEN_KEY));
    }

    #[test]
    fn neither_gives_none() {
        // Arrange, Act
        let key = session_key(None, None);
        // Assert
        assert_eq!(key, None);
    }

    #[test]
    fn a_signed_in_file_with_a_token_gives_the_token_key_and_not_the_email() {
        // Arrange
        let dir = scratch_dir("usage-warm-key");
        std::fs::create_dir_all(&dir).unwrap();
        let claude_json = dir.join(".claude.json");
        std::fs::write(
            &claude_json,
            format!(r#"{{"oauthAccount":{{"emailAddress":"{EMAIL}"}}}}"#),
        )
        .unwrap();
        // Act
        let key = session_key(Some(TOKEN_KEY.to_string()), signed_in_label(&claude_json));
        // Assert
        let key = key.expect("a key");
        assert_eq!(key, TOKEN_KEY);
        assert!(!key.contains(EMAIL));
    }
}
