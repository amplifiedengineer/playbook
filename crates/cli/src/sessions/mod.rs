// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The session management core (ADR-0024, stage 1): one derived record per
//! recent agent session, with its repo, branch, worktree, linked issue or PR,
//! state, last activity, context size and cost. Read only. It writes nothing,
//! stores no prompt text and never acts on a session: the hint on a record is a
//! suggestion for the person reading it.
//!
//! The pure parts (`build`, `state_of`, `issue_from_branch`, the renderers)
//! take their inputs as arguments so tests need no git, `gh` or terminal. The
//! impure part (`gather`) looks up the working directory, the worktree and the
//! pull request, each bounded by a timeout, and reads `unknown` on failure.

pub(crate) mod activity;
pub mod links;
pub(crate) mod state;
#[allow(dead_code)] // The hooks that write the store are not wired yet.
pub(crate) mod store;
mod sweep;

pub use crate::usage::session_view::{
    age, link_label, tokens, worktree_label, Place, Pr, PrKind, Record, State,
};
use crate::usage::UsageEvent;
use serde_json::{json, Value};

/// Seconds of silence after which a session no longer counts as active.
pub(crate) const ACTIVE_SECONDS: i64 = 900;

/// Seconds of silence under which a session reads `working`.
pub const WORKING_SECONDS: i64 = 120;
const DAY: i64 = 86_400;

/// Days without activity after which an unfinished session reads `stale`.
pub const STALE_AFTER_DAYS: i64 = 3;
/// How many days back `playbook sessions` looks for sessions.
pub const RECENT_DAYS: i64 = 7;

/// The limits that decide a state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub stale_after_days: i64,
    pub recent_days: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            stale_after_days: STALE_AFTER_DAYS,
            recent_days: RECENT_DAYS,
        }
    }
}

/// The first number in a branch name that looks like an issue, or `None`.
/// `feat/536-foo` and `fix/issue-536` give 536. A year (1990 to 2100) is not
/// an issue.
pub fn issue_from_branch(branch: &str) -> Option<u64> {
    branch
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|s| s.parse::<u64>().ok())
        .find(|n| *n > 0 && !(1990..=2100).contains(n))
}

/// Whether a branch is a repo's trunk, which never counts as a session's work.
pub fn is_trunk(branch: &str) -> bool {
    matches!(branch, "main" | "master" | "trunk" | "develop" | "")
}

/// The state of a session: `derive` for a session with a stored row, the
/// clock rules for one without. A merged or closed PR means done, whatever the
/// clock says.
pub(crate) fn state_of(
    row: Option<&state::Row>,
    last: i64,
    now: i64,
    pr: Option<Pr>,
    cfg: Settings,
) -> State {
    if let Some(row) = row {
        return state::derive(row, pr, now, cfg);
    }
    if matches!(
        pr,
        Some(Pr {
            kind: PrKind::Merged | PrKind::Closed,
            ..
        })
    ) {
        return State::Done;
    }
    let age = now - last;
    if age < WORKING_SECONDS {
        State::Working
    } else if age >= cfg.stale_after_days * DAY {
        State::Stale
    } else {
        State::Idle
    }
}

/// The suggestion for a state, and whether it needs attention.
fn advice(state: State, dirty: Option<bool>) -> (bool, String) {
    let keeps = dirty == Some(true);
    match (state, keeps) {
        (State::Done, true) => (
            true,
            "work landed but the worktree has uncommitted changes: commit or discard them by hand, nothing is removed".into(),
        ),
        (State::Done, false) => (
            false,
            "work landed: close this session, `playbook worktree sweep` removes a clean worktree".into(),
        ),
        (State::Stale, true) => (
            true,
            "stale with uncommitted changes: resume it or commit the work".into(),
        ),
        (State::Stale, false) => (false, "stale: archive or close this session".into()),
        (State::Waiting, _) => (
            false,
            "waiting for you: answer it in its terminal".into(),
        ),
        (State::Crashed, _) => (
            true,
            "the agent exited without a clean end: resume it or close the session".into(),
        ),
        (State::Working | State::Idle | State::Closed, _) => (false, String::new()),
    }
}

/// One record per session in `events` (all of a session's events, any order),
/// newest activity first. `lookup` supplies the place and the PR of a session's
/// newest event. Sessions whose newest event is older than `recent_days` are
/// left out unless they are not finished.
pub fn build(
    events: &[UsageEvent],
    now: i64,
    cfg: Settings,
    lookup: &dyn Fn(&UsageEvent) -> (Place, Option<Pr>),
) -> Vec<Record> {
    let mut by_session: std::collections::BTreeMap<&str, Vec<&UsageEvent>> = Default::default();
    for e in events {
        by_session.entry(e.session_id.as_str()).or_default().push(e);
    }
    let mut records: Vec<Record> = by_session
        .into_values()
        .filter_map(|group| {
            let newest = *group.iter().max_by_key(|e| e.timestamp)?;
            if now - newest.timestamp >= cfg.recent_days * DAY {
                return None;
            }
            let (place, pr) = lookup(newest);
            let state = state_of(None, newest.timestamp, now, pr, cfg);
            let (needs_attention, hint) = advice(state, place.dirty);
            Some(Record {
                id: newest.session_id.clone(),
                agent: newest.agent.clone(),
                repo: newest.repo.clone(),
                branch: newest.branch.clone(),
                place,
                issue: issue_from_branch(&newest.branch),
                pr,
                state,
                last_activity: newest.timestamp,
                context_tokens: newest.input_tokens
                    + newest.cache_read_tokens
                    + newest.cache_creation_tokens,
                cost_usd: group.iter().map(|e| e.cost_usd).sum(),
                unpriced: group.iter().filter(|e| e.unpriced).count(),
                messages: group.len(),
                needs_attention,
                hint,
            })
        })
        .collect();
    records.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.id.cmp(&b.id))
    });
    records
}

/// The table `playbook sessions` prints. Times are relative to `now`, which
/// the footer states in UTC.
pub fn render_text(records: &[Record], now: i64) -> String {
    if records.is_empty() {
        return "No recent sessions.\n".into();
    }
    let header = [
        "STATE", "SESSION", "REPO", "BRANCH", "WORKTREE", "LINK", "LAST", "CONTEXT", "COST",
    ];
    let rows: Vec<[String; 9]> = records
        .iter()
        .map(|r| {
            [
                r.state.label().to_string(),
                crate::usage::query::short_id(&r.id),
                r.repo.clone(),
                r.branch.clone(),
                worktree_label(r),
                link_label(r),
                age(r.last_activity, now),
                tokens(r.context_tokens),
                format!("${:.2}", r.cost_usd),
            ]
        })
        .collect();
    let mut widths = header.map(str::len);
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                out.push_str("  ");
            }
            out.push_str(&format!("{:<w$}", cell, w = widths[i]));
        }
        out.trim_end().to_string()
    };
    let mut out = line(header.to_vec());
    out.push('\n');
    for row in &rows {
        out.push_str(&line(row.iter().map(String::as_str).collect()));
        out.push('\n');
    }
    let attention: Vec<&Record> = records.iter().filter(|r| !r.hint.is_empty()).collect();
    if !attention.is_empty() {
        out.push_str("\nSuggestions (nothing is done for you):\n");
        for r in attention {
            let mark = if r.needs_attention { "!" } else { "-" };
            out.push_str(&format!(
                "{mark} {} {}: {}\n",
                crate::usage::query::short_id(&r.id),
                r.branch,
                r.hint
            ));
        }
    }
    out.push_str(&format!(
        "\n{} sessions, as of {} UTC. Issue links marked ? are guessed from the branch name.\n",
        records.len(),
        crate::usage::query::clock(now)
    ));
    out
}

/// The `--json` document.
pub fn render_json(records: &[Record], now: i64) -> Value {
    let rows: Vec<Value> = records
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "agent": r.agent,
                "repo": r.repo,
                "branch": r.branch,
                "cwd": r.place.cwd,
                "worktree": r.place.worktree,
                "worktree_convention": r.place.convention,
                "worktree_gone": r.place.gone,
                "uncommitted": r.place.dirty,
                "issue_inferred": r.issue,
                "pr": r.pr.map(|p| json!({"number": p.number, "state": p.kind.label()})),
                "state": r.state.label(),
                "last_activity": r.last_activity,
                "context_tokens": r.context_tokens,
                "cost_usd": r.cost_usd,
                "unpriced_messages": r.unpriced,
                "messages": r.messages,
                "needs_attention": r.needs_attention,
                "hint": r.hint,
            })
        })
        .collect();
    json!({"generated_at": now, "sessions": rows})
}

/// Reads the usage store (after ingesting new history), looks up each
/// session's place and PR, and returns the records with the time they were
/// read at.
pub fn load(
    paths: &crate::usage::run::Paths,
    home: &std::path::Path,
) -> Result<(Vec<Record>, i64), String> {
    let now = crate::common::time::now_secs();
    let cfg = Settings::default();
    let (conn, _) = crate::usage::run::ingest_new(paths)?;
    let events = crate::usage::db::load_usage_events_since(&conn, now - cfg.recent_days * DAY)?;
    let mut newest: std::collections::BTreeMap<&str, &UsageEvent> = Default::default();
    for e in &events {
        let slot = newest.entry(e.session_id.as_str()).or_insert(e);
        if e.timestamp > slot.timestamp {
            *slot = e;
        }
    }
    let heads: Vec<&UsageEvent> = newest.into_values().collect();
    let found = links::gather(&heads, paths, home);
    let lookup = |e: &UsageEvent| found.get(&e.session_id).cloned().unwrap_or_default();
    let all = crate::usage::db::load_usage_events_of_sessions(
        &conn,
        &heads
            .iter()
            .map(|e| e.session_id.clone())
            .collect::<Vec<_>>(),
    )?;
    Ok((build(&all, now, cfg, &lookup), now))
}

/// `playbook sessions`: the text or JSON of [`load`].
pub fn run(
    json: bool,
    paths: &crate::usage::run::Paths,
    home: &std::path::Path,
) -> Result<String, String> {
    let (records, now) = load(paths, home)?;
    if json {
        serde_json::to_string_pretty(&render_json(&records, now))
            .map_err(|e| format!("failed to encode sessions: {e}"))
    } else {
        Ok(render_text(&records, now))
    }
}

/// `playbook sessions sweep --dry-run`: what a sweep would do with each
/// finished session's worktree, judged by the worktree sweep guards. Nothing
/// is removed.
pub fn sweep_dry_run(
    paths: &crate::usage::run::Paths,
    home: &std::path::Path,
) -> Result<String, String> {
    let (records, now) = load(paths, home)?;
    let judge = |path: &std::path::Path| {
        let slug = crate::common::repo::repo_slug_in(path);
        let slug = (!slug.is_empty()).then_some(slug.as_str());
        crate::worktree::would_remove(path, home, slug, path, now)
    };
    Ok(sweep::report(&records, now, &judge))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_788_400_000;

    #[test]
    fn the_default_limits_are_the_built_in_constants() {
        // Arrange
        let cfg = Settings::default();

        // Act
        let limits = (cfg.stale_after_days, cfg.recent_days);

        // Assert
        assert_eq!(limits, (STALE_AFTER_DAYS, RECENT_DAYS));
    }

    fn ev(session: &str, ts: i64, branch: &str, cost: f64) -> UsageEvent {
        UsageEvent {
            event_id: format!("{session}{ts}"),
            timestamp: ts,
            session_id: session.into(),
            agent: "claude-code".into(),
            repo: "playbook".into(),
            branch: branch.into(),
            input_tokens: 100,
            cache_read_tokens: 1000,
            cache_creation_tokens: 50,
            cost_usd: cost,
            ..UsageEvent::default()
        }
    }

    fn none(_: &UsageEvent) -> (Place, Option<Pr>) {
        (Place::default(), None)
    }

    #[test]
    fn an_issue_number_comes_from_the_branch_and_a_year_does_not() {
        assert_eq!(issue_from_branch("feat/536-sessions"), Some(536));
        assert_eq!(issue_from_branch("fix/issue-12-x"), Some(12));
        assert_eq!(issue_from_branch("docs/adr-session-management"), None);
        assert_eq!(issue_from_branch("release/2026"), None);
        assert_eq!(issue_from_branch("v2"), None);
        assert_eq!(issue_from_branch("main"), None);
    }

    #[test]
    fn a_session_is_working_idle_or_stale_by_its_last_message() {
        let cfg = Settings::default();
        assert_eq!(state_of(None, NOW - 30, NOW, None, cfg), State::Working);
        assert_eq!(state_of(None, NOW - 3600, NOW, None, cfg), State::Idle);
        assert_eq!(state_of(None, NOW - 3 * DAY, NOW, None, cfg), State::Stale);
        assert_eq!(
            state_of(None, NOW - 3 * DAY + 1, NOW, None, cfg),
            State::Idle
        );
    }

    #[test]
    fn a_session_with_a_row_takes_its_state_from_the_row() {
        let row = state::Row {
            last_stop_at: Some(NOW - 10),
            waiting_after_prompt: true,
            ..state::Row::default()
        };
        // The last message argument is old: the row wins.
        let got = state_of(Some(&row), NOW - 9 * DAY, NOW, None, Settings::default());
        assert_eq!(got, State::Waiting);
    }

    #[test]
    fn a_waiting_or_crashed_session_gets_its_own_hint() {
        let (attention, hint) = advice(State::Waiting, None);
        assert!(!attention && hint.contains("waiting"));
        let (attention, hint) = advice(State::Crashed, None);
        assert!(attention && hint.contains("without a clean end"));
        assert_eq!(advice(State::Closed, Some(true)), (false, String::new()));
    }

    #[test]
    fn a_merged_or_closed_pr_is_done_and_an_open_one_is_not() {
        let cfg = Settings::default();
        let pr = |kind| Some(Pr { number: 7, kind });
        assert_eq!(
            state_of(None, NOW - 10, NOW, pr(PrKind::Merged), cfg),
            State::Done
        );
        assert_eq!(
            state_of(None, NOW - 10, NOW, pr(PrKind::Closed), cfg),
            State::Done
        );
        assert_eq!(
            state_of(None, NOW - 10, NOW, pr(PrKind::Open), cfg),
            State::Working
        );
    }

    #[test]
    fn a_record_sums_the_session_and_sizes_the_context_from_the_newest_message() {
        let events = [
            ev("a", NOW - 1200, "feat/9-x", 0.5),
            ev("a", NOW - 600, "feat/9-x", 0.25),
        ];
        let records = build(&events, NOW, Settings::default(), &none);
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.cost_usd, 0.75);
        assert_eq!(r.messages, 2);
        assert_eq!(r.context_tokens, 1150);
        assert_eq!(r.issue, Some(9));
        assert_eq!(r.last_activity, NOW - 600);
        assert_eq!(r.state, State::Idle);
    }

    #[test]
    fn sessions_older_than_the_window_are_left_out_and_the_rest_sort_newest_first() {
        let events = [
            ev("old", NOW - 8 * DAY, "b", 1.0),
            ev("mid", NOW - 3600, "b", 1.0),
            ev("new", NOW - 10, "b", 1.0),
        ];
        let ids: Vec<String> = build(&events, NOW, Settings::default(), &none)
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, ["new", "mid"]);
    }

    #[test]
    fn a_finished_session_with_uncommitted_work_needs_attention_and_is_never_removed() {
        let events = [ev("a", NOW - 600, "feat/1-x", 0.1)];
        let lookup = |_: &UsageEvent| {
            (
                Place {
                    cwd: Some("/w/repo".into()),
                    worktree: Some("/w/repo".into()),
                    dirty: Some(true),
                    ..Place::default()
                },
                Some(Pr {
                    number: 4,
                    kind: PrKind::Merged,
                }),
            )
        };
        let r = &build(&events, NOW, Settings::default(), &lookup)[0];
        assert_eq!(r.state, State::Done);
        assert!(r.needs_attention);
        assert!(r.hint.contains("nothing is removed"));
    }

    #[test]
    fn a_clean_merged_session_gets_a_suggestion_not_attention() {
        let events = [ev("a", NOW - 600, "feat/1-x", 0.1)];
        let lookup = |_: &UsageEvent| {
            (
                Place {
                    dirty: Some(false),
                    ..Place::default()
                },
                Some(Pr {
                    number: 4,
                    kind: PrKind::Merged,
                }),
            )
        };
        let r = &build(&events, NOW, Settings::default(), &lookup)[0];
        assert!(!r.needs_attention);
        assert!(r.hint.contains("worktree sweep"));
    }

    #[test]
    fn the_text_has_every_column_the_suggestions_and_a_utc_footer() {
        let events = [ev("abcdef123456", NOW - 7200, "feat/536-x", 1.234)];
        let lookup = |_: &UsageEvent| {
            (
                Place {
                    cwd: Some("/w/playbook/536".into()),
                    worktree: Some("/w/playbook/536".into()),
                    convention: "cc",
                    dirty: Some(false),
                    ..Place::default()
                },
                Some(Pr {
                    number: 540,
                    kind: PrKind::Merged,
                }),
            )
        };
        let text = render_text(&build(&events, NOW, Settings::default(), &lookup), NOW);
        for want in [
            "STATE",
            "done",
            "abcdef12",
            "playbook",
            "feat/536-x",
            "536 (cc)",
            "PR #540 merged",
            "2h",
            "1k",
            "$1.23",
            "Suggestions",
            "UTC",
        ] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        assert_eq!(render_text(&[], NOW), "No recent sessions.\n");
    }

    #[test]
    fn the_json_names_the_guess_and_holds_no_prompt_text() {
        let events = [ev("a", NOW - 600, "fix/issue-3", 0.1)];
        let doc = render_json(&build(&events, NOW, Settings::default(), &none), NOW);
        let row = &doc["sessions"][0];
        assert_eq!(row["issue_inferred"], 3);
        assert_eq!(row["state"], "idle");
        assert!(row["pr"].is_null());
        assert_eq!(doc["generated_at"], NOW);
    }
}
