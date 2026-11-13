// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The renames and removals that have landed. Each rename or removal PR adds
//! its row here, and the checks in `renames.rs` and `size_budget.rs` follow it.

/// What a rename applies to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Command,
    Skill,
}

/// One rename: the old name stops existing and the new one takes its place.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rename {
    pub(crate) kind: Kind,
    pub(crate) old: &'static str,
    pub(crate) new: &'static str,
}

/// Renames that have landed. Each rename PR adds its row.
pub(crate) const RENAMES: &[Rename] = &[
    Rename {
        kind: Kind::Command,
        old: "commit-and-push",
        new: "commit",
    },
    Rename {
        kind: Kind::Command,
        old: "create-pull-request",
        new: "pr-create",
    },
    Rename {
        kind: Kind::Command,
        old: "address-pr-comments",
        new: "pr-comments",
    },
    Rename {
        kind: Kind::Command,
        old: "learn-project",
        new: "learn",
    },
    Rename {
        kind: Kind::Command,
        old: "repo-audit",
        new: "audit",
    },
];
