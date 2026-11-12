// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The acceptance rule, in integer basis points. The brief facts default
//! flips only when every part holds on a complete report.

use super::report::Report;
use super::{Kind, BRIEF_FACTS_TOKENS_OFF, BRIEF_FACTS_TOKENS_ON, MARGIN_BP, MIN_TASKS_PER_KIND};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Partial,
    UnequalArms,
    LowerBound,
    PointEstimate,
    TokensNotFalling,
    KindLoses(Kind),
}

/// Every part of the rule that does not hold. Kinds below the minimum task
/// count are never listed.
pub fn failures(r: &Report) -> Vec<Failure> {
    let mut out = Vec::new();
    if !r.complete {
        out.push(Failure::Partial);
    }
    if r.trials_with != r.trials_without {
        out.push(Failure::UnequalArms);
    }
    if r.lower_bp < -MARGIN_BP {
        out.push(Failure::LowerBound);
    }
    if r.difference_bp < 0 {
        out.push(Failure::PointEstimate);
    }
    if r.with_tokens_per_task >= r.without_tokens_per_task {
        out.push(Failure::TokensNotFalling);
    }
    out.extend(
        r.kinds
            .iter()
            .filter(|k| k.tasks >= MIN_TASKS_PER_KIND && k.difference_bp < -MARGIN_BP)
            .map(|k| Failure::KindLoses(k.kind)),
    );
    out
}

/// The value `--facts-tokens` should take for this report.
pub fn default_tokens(r: &Report) -> u32 {
    if failures(r).is_empty() {
        BRIEF_FACTS_TOKENS_ON
    } else {
        BRIEF_FACTS_TOKENS_OFF
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2e::report::KindReport;

    fn passing() -> Report {
        Report {
            source: "test".into(),
            model: "m".into(),
            repeats: 5,
            tasks: 30,
            excluded: 0,
            complete: true,
            spent_cents: 0,
            trials_with: 150,
            trials_without: 150,
            with_accuracy_bp: 9_000,
            without_accuracy_bp: 9_000,
            difference_bp: 0,
            lower_bp: -MARGIN_BP,
            upper_bp: 300,
            with_repeat_variance: 0,
            without_repeat_variance: 0,
            with_tokens_per_task: 50,
            without_tokens_per_task: 100,
            kinds: vec![KindReport {
                kind: Kind::Symbol,
                tasks: 30,
                difference_bp: -MARGIN_BP,
            }],
        }
    }

    fn kind(tasks: u32, difference_bp: i64) -> impl Fn(&mut Report) {
        move |r| {
            r.kinds = vec![KindReport {
                kind: Kind::Refs,
                tasks,
                difference_bp,
            }]
        }
    }

    #[test]
    fn each_case_decides_the_default() {
        type Edit = Box<dyn Fn(&mut Report)>;
        let cases: Vec<(&str, Edit, u32)> = vec![
            ("meets the rule", Box::new(|_| {}), BRIEF_FACTS_TOKENS_ON),
            (
                "lower bound one basis point lower",
                Box::new(|r| r.lower_bp = -MARGIN_BP - 1),
                0,
            ),
            (
                "point estimate exactly zero",
                Box::new(|r| r.difference_bp = 0),
                BRIEF_FACTS_TOKENS_ON,
            ),
            (
                "point estimate minus one",
                Box::new(|r| r.difference_bp = -1),
                0,
            ),
            (
                "tokens equal",
                Box::new(|r| r.with_tokens_per_task = 100),
                0,
            ),
            ("tokens rise", Box::new(|r| r.with_tokens_per_task = 101), 0),
            ("partial report", Box::new(|r| r.complete = false), 0),
            ("arms differ", Box::new(|r| r.trials_with = 149), 0),
            (
                "kind loses exactly the margin",
                Box::new(kind(5, -300)),
                BRIEF_FACTS_TOKENS_ON,
            ),
            ("kind loses one more", Box::new(kind(5, -301)), 0),
            (
                "kind of 4 tasks loses 10 points",
                Box::new(kind(4, -1_000)),
                BRIEF_FACTS_TOKENS_ON,
            ),
            (
                "kind of 5 tasks loses 10 points",
                Box::new(kind(5, -1_000)),
                0,
            ),
        ];
        for (name, edit, want) in cases {
            let mut r = passing();
            edit(&mut r);
            assert_eq!(default_tokens(&r), want, "{name}");
        }
    }

    #[test]
    fn a_small_kind_is_not_listed_and_a_large_one_is() {
        let mut r = passing();
        kind(4, -1_000)(&mut r);
        assert!(failures(&r).is_empty());
        kind(5, -1_000)(&mut r);
        assert_eq!(failures(&r), [Failure::KindLoses(Kind::Refs)]);
    }
}
