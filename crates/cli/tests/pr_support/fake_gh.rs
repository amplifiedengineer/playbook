// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A scripted `GhClient` for the `pr` integration tests.

use playbook::pr::shared::{ExistingPr, GhClient};
use std::cell::RefCell;

/// A scripted `GhClient` that records every call it receives.
#[derive(Default)]
pub(crate) struct FakeGh {
    pub existing: Option<ExistingPr>,
    pub default_branch: Option<String>,
    /// Head branches of open PRs, for stack detection.
    pub heads: Vec<String>,
    pub calls: RefCell<Vec<String>>,
}

impl FakeGh {
    pub(crate) fn count(&self, prefix: &str) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }
}

impl GhClient for FakeGh {
    fn pr_view(&self, _branch: &str) -> Result<Option<ExistingPr>, String> {
        self.calls.borrow_mut().push("pr_view".into());
        Ok(self.existing.clone())
    }
    fn open_pr_heads(&self) -> Vec<String> {
        self.calls.borrow_mut().push("open_pr_heads".into());
        self.heads.clone()
    }
    fn repo_default_branch(&self) -> Result<Option<String>, String> {
        self.calls.borrow_mut().push("repo_default_branch".into());
        Ok(self.default_branch.clone())
    }
}
