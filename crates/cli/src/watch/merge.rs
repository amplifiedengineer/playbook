// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `merge_when_green` action: merges a pull request once it is green, has
//! a human approval of the current head, no open change request or review
//! request, and comes from an author you allowed, and only when
//! `pr.merge.enabled` is on. Merging is on the list of things that are unsafe
//! unattended, so the action stays at `propose`.

use super::clock::Clock;
use super::engine::{Engine, GithubWrite, MergeCheck, Step};
use super::store::WorkRow;
use super::trust::{classify, Trust};

impl<W: GithubWrite, C: Clock> Engine<'_, W, C> {
    /// Carries out `merge_when_green` for `row`. A pull request that is not
    /// ready goes back to a proposal with the reason, so you can approve it
    /// again later.
    pub(super) fn merge_when_green(&mut self, row: &WorkRow) -> Result<Step, String> {
        if !self.auto_merge {
            return Ok(Step::Demote("auto_merge_off"));
        }
        let check = self.write.merge_check(&row.repo, row.object)?;
        if let Some(reason) = self.not_ready(&check) {
            return Ok(Step::Demote(reason));
        }
        // The head the check read, so a push after it is not merged.
        self.write.merge(&row.repo, row.object, &check.head)?;
        Ok(Step::Done(0.0))
    }

    /// Why the pull request may not be merged yet, if it may not.
    fn not_ready(&self, check: &MergeCheck) -> Option<&'static str> {
        let author_allowed = check.author == self.me || self.allowed.contains(&check.author);
        if !author_allowed {
            return Some("author_not_allowed");
        }
        if !check.green {
            return Some("not_green");
        }
        if check.changes_requested {
            return Some("changes_requested");
        }
        if check.review_requested {
            return Some("review_requested");
        }
        let human_approval = check.approvers.iter().any(|(login, association)| {
            login != &check.author
                && matches!(
                    classify(login, association, self.me, self.allowed),
                    Trust::Trusted | Trust::SelfAuthored
                )
        });
        (!human_approval).then_some("no_approval")
    }
}
