// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `playbook sync` command names, shared by `playbook` and `pbsync` so the
//! two cannot drift.

/// Names `playbook sync` passes to `pbsync` with the words after them.
pub const FORWARDED: &[&str] = &[
    "enable",
    "disable",
    "status",
    "push",
    "pull",
    "run",
    "purge",
    "config push",
    "config pull",
];

/// Names `playbook sync` runs itself.
pub const IN_PROCESS: &[&str] = &["config apply", "config refresh"];
