// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook segment unit`: the worktree recipes for one Work Unit of a
//! parallel wave. `start` adds its worktree at the base commit, `integrate`
//! squashes its checkpoints into one signed commit of the listed files and
//! cherry-picks it onto the Segment branch, and `resume` reads the
//! checkpoint commits to say which TDD step comes next.

use super::git;
use crate::commit::run::signing_flags;
use std::path::Path;

/// Written into a kept worktree so a worktree sweep leaves it alone while a
/// person resolves the conflict. Line 1 is the reason, line 2 a UTC timestamp.
const CONFLICT_MARKER_FILE: &str = ".playbook-conflict-stop";

/// The TDD steps a checkpoint commit names, in the order they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Red,
    Green,
    Refactor,
}

impl Step {
    fn parse(word: &str) -> Option<Self> {
        match word {
            "red" => Some(Self::Red),
            "green" => Some(Self::Green),
            "refactor" => Some(Self::Refactor),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Green => "green",
            Self::Refactor => "refactor",
        }
    }

    /// The step after this one; a finished scenario starts the next at red.
    fn next(self) -> Self {
        match self {
            Self::Red => Self::Green,
            Self::Green => Self::Refactor,
            Self::Refactor => Self::Red,
        }
    }
}

/// The `key=value` fields a verb reports, printed on one line or as JSON.
pub struct Report(Vec<(&'static str, String)>);

impl Report {
    fn new(fields: &[(&'static str, &str)]) -> Self {
        Self(fields.iter().map(|(k, v)| (*k, (*v).to_string())).collect())
    }

    pub fn render(&self) -> String {
        let pairs: Vec<String> = self.0.iter().map(|(k, v)| format!("{k}={v}")).collect();
        pairs.join(" ")
    }

    fn fields(&self) -> serde_json::Value {
        serde_json::Value::Object(
            self.0
                .iter()
                .map(|(k, v)| ((*k).to_string(), serde_json::Value::String(v.clone())))
                .collect(),
        )
    }
}

/// The shared `--json` envelope for a verb's outcome; a failure is code 1.
pub fn to_envelope(result: &Result<Report, String>) -> serde_json::Value {
    match result {
        Ok(report) => crate::json::envelope::wrap(0, "", report.fields()),
        Err(error) => crate::json::envelope::wrap(1, error, serde_json::Value::Null),
    }
}

/// Add the unit's worktree at `base`, or report the one already there.
/// Prints `worktree=<path> base=<sha>`.
pub fn start(dir: &Path, base: &str, path: &Path) -> Result<Report, String> {
    let sha = git(
        dir,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
    )?;
    if path.exists() {
        git(path, &["rev-parse", "--is-inside-work-tree"])
            .map_err(|e| format!("{} exists but is not a worktree: {e}", path.display()))?;
    } else {
        let target = path.to_string_lossy();
        git(dir, &["worktree", "add", "--detach", &target, &sha])?;
    }
    Ok(Report::new(&[
        ("worktree", &path.to_string_lossy()),
        ("base", &sha),
    ]))
}

/// Everything `integrate` needs to know.
pub struct IntegrateArgs<'a> {
    pub base: &'a str,
    pub path: &'a Path,
    pub files: &'a [String],
    pub branch: &'a str,
    pub message: &'a str,
    pub config_signoff: bool,
}

/// Squash the unit's checkpoints into one commit of `files`, cherry-pick it
/// onto `branch` (the checkout in `dir`), and remove the worktree. On a
/// cherry-pick conflict the checkout is restored, the worktree is kept with a
/// conflict marker, and the result is an error. Prints
/// `integrated=<sha> branch=<branch>`.
pub fn integrate(dir: &Path, a: &IntegrateArgs<'_>) -> Result<Report, String> {
    let current = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if current != a.branch {
        return Err(format!(
            "the checkout is on {current}, not the Segment branch {}; switch to it first",
            a.branch
        ));
    }
    let commit = squash(a)?;
    cherry_pick(dir, a, &commit)?;
    git(
        dir,
        &["worktree", "remove", "--force", &a.path.to_string_lossy()],
    )?;
    Ok(Report::new(&[
        ("integrated", &commit),
        ("branch", a.branch),
    ]))
}

/// Soft reset the worktree to the base, then commit only the listed files.
/// Changes to any other file stay in the worktree uncommitted.
fn squash(a: &IntegrateArgs<'_>) -> Result<String, String> {
    let wt = a.path;
    git(wt, &["reset", "--soft", a.base])?;
    git(wt, &["reset", "--quiet"])?;
    let mut add = vec!["add", "--"];
    add.extend(a.files.iter().map(String::as_str));
    git(wt, &add)?;
    if git(wt, &["diff", "--cached", "--name-only"])?.is_empty() {
        return Err("none of the listed files changed since the base; nothing to commit".into());
    }
    let mut commit = vec!["commit", "--quiet"];
    commit.extend(signing_flags(wt, a.message, false, a.config_signoff));
    commit.extend(["-m", a.message]);
    git(wt, &commit)?;
    git(wt, &["rev-parse", "HEAD"])
}

fn cherry_pick(dir: &Path, a: &IntegrateArgs<'_>, commit: &str) -> Result<(), String> {
    let mut pick = vec!["cherry-pick"];
    // The commit already carries the sign-off trailer; only the signature is new.
    pick.extend(signing_flags(dir, a.message, true, false));
    pick.push(commit);
    let Err(failure) = git(dir, &pick) else {
        return Ok(());
    };
    if git(dir, &["diff", "--name-only", "--diff-filter=U"])?.is_empty() {
        return Err(failure);
    }
    git(dir, &["cherry-pick", "--abort"])?;
    let reason = format!("cherry-pick of {commit} onto {} conflicted", a.branch);
    let stamp = crate::common::time::iso_utc(crate::common::time::now_secs());
    std::fs::write(
        a.path.join(CONFLICT_MARKER_FILE),
        format!("{reason}\n{stamp}\n"),
    )
    .map_err(|e| format!("could not write the conflict marker: {e}"))?;
    Err(format!(
        "{reason}: the worktree {} was kept and the branch checkout is unchanged; resolve by hand",
        a.path.display()
    ))
}

/// The checkpoint commits of `wu` since `base`, newest first, as
/// `(step, scenario)`.
fn checkpoints(dir: &Path, wu: &str, base: &str) -> Result<Vec<(Step, String)>, String> {
    let log = git(dir, &["log", "--format=%s", &format!("{base}..HEAD")])?;
    let prefix = format!("wip({}): ", wu.to_lowercase());
    Ok(log
        .lines()
        .filter_map(|subject| {
            let rest = subject
                .to_lowercase()
                .starts_with(&prefix)
                .then(|| subject.get(prefix.len()..))??;
            let (step, scenario) = rest.split_once(" - ")?;
            Some((Step::parse(step)?, scenario.trim().to_string()))
        })
        .collect())
}

/// Say which TDD step comes next from the unit's latest checkpoint. Prints
/// `last=<step> scenario=<id> next=<step>`, or `last=none next=red` when no
/// checkpoint exists. After a refactor `next=red` starts the following
/// scenario.
pub fn resume(dir: &Path, wu: &str, base: &str) -> Result<Report, String> {
    Ok(match checkpoints(dir, wu, base)?.first() {
        None => Report::new(&[("last", "none"), ("next", Step::Red.name())]),
        Some((step, scenario)) => Report::new(&[
            ("last", step.name()),
            ("scenario", scenario),
            ("next", step.next().name()),
        ]),
    })
}
