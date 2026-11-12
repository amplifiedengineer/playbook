// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The end-to-end evaluation: the same tasks run with and without the code
//! index, a grader with no model scores the answers, and a fixed rule decides
//! whether the brief facts default may change. The model and the cost meter
//! are injected, so every test runs with stubs.

pub mod model;
pub mod report;
pub mod rule;
pub mod run;

use crate::tasks::TaskSet;
use serde::{Deserialize, Serialize};

/// The noninferiority margin, 3.0 accuracy points, in basis points.
pub const MARGIN_BP: i64 = 300;
/// Repeats per task and arm.
pub const REPEATS: u32 = 5;
/// The dollar cap of one run, 50 USD, in cents. The run stops above it.
pub const COST_CAP_CENTS: u64 = 5_000;
/// Lines an answer may be off and still score.
pub const LINE_TOLERANCE: u32 = 3;
/// A query kind with fewer tasks is listed and never blocks.
pub const MIN_TASKS_PER_KIND: u32 = 5;
/// Tasks drawn from the frozen set, or all of it when it is smaller.
pub const TASK_COUNT: usize = 30;
/// The model, pinned by dated snapshot id.
pub const MODEL_SNAPSHOT: &str = "claude-sonnet-4-5-20250929";
/// The fixed temperature, in whole units.
pub const TEMPERATURE: u32 = 0;
/// The value `--facts-tokens` takes when the rule holds.
pub const BRIEF_FACTS_TOKENS_ON: u32 = 1_500;
/// The value it keeps otherwise.
pub const BRIEF_FACTS_TOKENS_OFF: u32 = 0;

/// What a question asks of the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Symbol,
    Refs,
    Definition,
    Context,
    Phrase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct E2eTask {
    pub id: String,
    pub repo: String,
    pub kind: Kind,
    pub question: String,
    /// The oracle answer. A task without one is excluded and counted.
    pub expected: Option<Answer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    Without,
    With,
}

/// Draws `count` tasks from the frozen set, spread evenly over its sorted
/// ids, so the same set always gives the same tasks. Only declaration
/// questions have oracle truth today, so every drawn task is a symbol task.
pub fn draw(set: &TaskSet, count: usize) -> Vec<E2eTask> {
    let mut tasks: Vec<_> = set.tasks.iter().collect();
    tasks.sort_by(|a, b| a.id.cmp(&b.id));
    let total = tasks.len();
    let take = count.min(total);
    (0..take)
        .map(|i| {
            let t = tasks[i * total / take];
            E2eTask {
                id: t.id.clone(),
                repo: t.repo.clone(),
                kind: Kind::Symbol,
                question: format!(
                    "In the {} repo, where is the {} `{}` declared? Answer with only a JSON list of objects with the keys path and line.",
                    t.repo,
                    t.kind.as_str(),
                    t.name
                ),
                expected: Some(Answer {
                    path: t.path.clone(),
                    line: t.line,
                }),
            }
        })
        .collect()
}

fn normalize(path: &str) -> String {
    let slashed = path.replace('\\', "/");
    slashed.strip_prefix("./").unwrap_or(&slashed).to_string()
}

/// Scores a reply with no model: the reply must hold a JSON list with an
/// entry on the expected path (after normalizing) within the line tolerance.
pub fn grade(expected: &Answer, reply: &str) -> bool {
    let (Some(start), Some(end)) = (reply.find('['), reply.rfind(']')) else {
        return false;
    };
    let Ok(answers) = serde_json::from_str::<Vec<Answer>>(&reply[start..=end.max(start)]) else {
        return false;
    };
    let want = normalize(&expected.path);
    answers
        .iter()
        .any(|a| normalize(&a.path) == want && a.line.abs_diff(expected.line) <= LINE_TOLERANCE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected() -> Answer {
        Answer {
            path: "src/a.rs".into(),
            line: 100,
        }
    }

    fn reply(path: &str, line: u32) -> String {
        format!("[{{\"path\": \"{path}\", \"line\": {line}}}]")
    }

    #[test]
    fn a_line_off_by_tolerance_minus_one_and_tolerance_scores() {
        for line in [
            100 + LINE_TOLERANCE - 1,
            100 + LINE_TOLERANCE,
            100 - LINE_TOLERANCE,
        ] {
            assert!(grade(&expected(), &reply("src/a.rs", line)), "{line}");
        }
    }

    #[test]
    fn a_line_off_by_tolerance_plus_one_does_not_score() {
        assert!(!grade(
            &expected(),
            &reply("src/a.rs", 100 + LINE_TOLERANCE + 1)
        ));
        assert!(!grade(
            &expected(),
            &reply("src/a.rs", 100 - LINE_TOLERANCE - 1)
        ));
    }

    #[test]
    fn paths_are_normalized() {
        assert!(grade(&expected(), &reply("./src/a.rs", 100)));
        assert!(grade(&expected(), &reply("src\\\\a.rs", 100)));
        assert!(!grade(&expected(), &reply("src/b.rs", 100)));
    }

    #[test]
    fn prose_around_a_fenced_list_still_parses() {
        let text = format!("Here:\n```json\n{}\n```", reply("src/a.rs", 99));
        assert!(grade(&expected(), &text));
    }

    #[test]
    fn text_without_a_list_scores_zero() {
        assert!(!grade(&expected(), "I do not know"));
        assert!(!grade(&expected(), "] [ not json"));
    }

    #[test]
    fn the_draw_is_deterministic_and_has_the_requested_size() {
        let set = TaskSet::bundled().unwrap();
        let a = draw(&set, TASK_COUNT);
        assert_eq!(a.len(), TASK_COUNT.min(set.tasks.len()));
        assert_eq!(a, draw(&set, TASK_COUNT));
        assert!(a.iter().all(|t| t.expected.is_some()));
        let ids: std::collections::BTreeSet<_> = a.iter().map(|t| &t.id).collect();
        assert_eq!(ids.len(), a.len());
    }
}
