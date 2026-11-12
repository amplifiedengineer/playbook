// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `pr.draft` setting for `pr create`.

use std::path::Path;

/// Whether `pr.draft` says to open the PR as a draft; only a resolved `false`
/// opens it ready, so an unreadable or non-bool value stays a draft.
pub fn draft_setting(home: &Path, repo_slug: Option<&str>) -> bool {
    crate::config::resolve_valid("pr.draft", home, repo_slug)
        .ok()
        .and_then(|(value, _, _)| value.as_bool())
        .unwrap_or(true)
}
