// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The interactive Agent Budget Dashboard (`playbook usage`). `data` is the snapshot it
//! draws, `worker` keeps it fresh in the background, `app` is the state and
//! keys, `view` draws a frame, and `run` is the terminal loop around them.

#[cfg(test)]
mod a11y;
pub mod account;
pub mod app;
pub mod arrange;
pub mod budget;
pub mod budget_popup;
pub mod burnup;
pub mod chart;
pub mod data;
pub mod dots;
pub mod editor;
pub mod editor_view;
pub mod fmt;
pub mod icon;
pub mod keys;
pub mod layout;
pub mod machine;
pub mod notice;
pub mod panels;
#[cfg(test)]
mod parity;
pub mod projection;
pub mod reset_popup;
pub mod scale;
#[cfg(test)]
mod screenshot;
pub mod sessions_view;
#[cfg(test)]
mod snapshots;
pub mod sort;
pub mod theme;
pub mod ui_state;
pub mod view;
pub mod worker;

use super::query::Range;
use super::run::Paths;
use app::{App, Effect};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use serde_json::Value;
use sessions_view::SessionsView;
use std::path::Path;
use std::time::{Duration, Instant};
use theme::{ColorDepth, Theme};
use ui_state::{Autosave, Flushed};
use worker::{Update, Worker};

/// The config key that holds whether the account email is masked.
const MASK_ACCOUNT_KEY: &str = "usage.maskAccount";

/// How long the loop waits for a key before it checks for new data.
const TICK: Duration = Duration::from_millis(200);

/// The theme to start with: `NO_COLOR` forces `mono`, else the `usage.theme`
/// config key, which is `default` unless set.
fn start_theme(no_color: bool) -> Theme {
    if no_color {
        return Theme::Mono;
    }
    crate::host::config_value_now("usage.theme")
        .and_then(|value| value.as_str().and_then(Theme::parse))
        .unwrap_or_default()
}

/// Whether the header masks an unnamed email account: the choice pbtop stored
/// (app tier), else the `usage.maskAccount` config key, which is off unless set.
pub(crate) fn start_mask_account(home: &Path) -> bool {
    crate::config::resolve(home, MASK_ACCOUNT_KEY)
        .and_then(|value| value.as_bool())
        .unwrap_or_default()
}

/// Stores the mask choice. A missing config store keeps it for this run only,
/// with nothing to tell the user; a failed write is the one thing worth a note.
fn save_mask_account(home: &Path, mask: bool) -> Option<String> {
    crate::config::save_app(home, MASK_ACCOUNT_KEY, &Value::from(mask))
        .err()
        .map(|e| format!("Account mask not saved: {e}"))
}

/// The range to open on: the `--range` flag, else the stored one, else this
/// week.
fn start_range(
    flag: Option<Range>,
    saved: &ui_state::UiState,
    week: crate::query::WeekStart,
) -> Range {
    flag.or_else(|| {
        saved
            .range
            .as_deref()
            .and_then(|r| Range::parse_with(Some(r), week))
    })
    .unwrap_or(Range::ThisWeek(week))
}

/// The state to store. A range that only came from `--range` is not stored
/// until the user changes it, so a one-off flag does not stick.
fn state_to_save(app: &App, flag: Option<Range>, saved: &ui_state::UiState) -> ui_state::UiState {
    let mut state = app.ui_state();
    if flag == Some(app.range) {
        state.range = saved.range.clone();
    }
    state
}

/// What the loop keeps beside the app, so a finished reset can bring it back
/// in step with the cleared store.
struct Reset<'a> {
    app: &'a mut App,
    autosave: &'a mut Autosave,
    flag: &'a mut Option<Range>,
    saved: &'a mut ui_state::UiState,
    last_saved: &'a mut ui_state::UiState,
}

impl Reset<'_> {
    /// Applies the result of clearing the store. On success the pending layout
    /// text is dropped and the defaults are not written back, and a `--range`
    /// flag cannot bring the old range back. On a failure nothing here changes
    /// except the footer note.
    fn finish(self, cleared: Result<(), String>) -> Effect {
        let ok = cleared.is_ok();
        let effect = self.app.reset_done(cleared);
        if ok {
            self.autosave.discard();
            *self.flag = None;
            *self.saved = ui_state::UiState::default();
            *self.last_saved = state_to_save(self.app, *self.flag, self.saved);
        }
        effect
    }
}

/// Runs the view until the user quits. Restores the terminal on every exit,
/// including a panic (ratatui installs the hook).
/// Opens the view on `range` and, when given, on the account `account_key`.
pub fn run(paths: &Paths, range: Option<Range>, account_key: Option<String>) -> Result<(), String> {
    // Nothing may write to stderr while the screen is drawn.
    crate::say::hold();
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, paths, range, account_key);
    ratatui::restore();
    crate::say::release();
    result.map_err(|e| format!("terminal error: {e}"))
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    paths: &Paths,
    range: Option<Range>,
    account_key: Option<String>,
) -> std::io::Result<()> {
    let home = crate::host::home_dir();
    let mut saved = ui_state::load(&home);
    let week = super::entry::configured_week_start();
    let mut flag = range;
    let range = start_range(flag, &saved, week);
    let mut app = App::new(range);
    let (layout, layout_warning) = ui_state::load_layout(&home);
    app.layout = layout;
    app.apply_ui_state(&saved);
    if let Some(key) = account_key {
        app.account = account::AccountSel::Key(key);
    }
    app.note = layout_warning;
    let mut last_saved = state_to_save(&app, flag, &saved);
    let mut autosave = Autosave::default();
    app.sources = paths.claude_projects.display().to_string();
    app.week_start = super::entry::configured_week_start();
    // NO_COLOR counts when it is set to anything but empty.
    app.no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    app.theme = start_theme(app.no_color);
    app.mask_account = start_mask_account(&home);
    let env = |k: &str| std::env::var(k).ok();
    app.icon = icon::Icon::detect(
        app.no_color,
        env("TERM").as_deref(),
        [
            env("LC_ALL").as_deref(),
            env("LC_CTYPE").as_deref(),
            env("LANG").as_deref(),
        ],
        env("PBTOP_ICON_WIDTH").as_deref(),
    );
    app.dots_off = env("PBTOP_DOTS").is_some_and(|v| v.trim() == "0");
    app.depth = ColorDepth::detect(
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    );
    app.telemetry = Some(super::telemetry_status::detect_here(paths));
    let worker = Worker::spawn(
        paths.clone(),
        app.range,
        app.filter.clone(),
        app.account.clone(),
        app.machine.clone(),
        app.refresh_every,
    );
    let mut sessions = SessionsLoop::new(paths.clone());
    while !app.quit {
        app.area = terminal.size().map(Into::into)?;
        sessions.poll();
        terminal.draw(|frame| match &sessions.view {
            Some(v) => sessions_view::render(
                frame,
                v,
                &app.theme.palette(app.depth),
                crate::util::time::now_secs(),
                app.icon,
            ),
            None => view::render(frame, &app),
        })?;
        if event::poll(TICK)? {
            // A resize needs no handling: the next draw reads the new size.
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press && sessions.view.is_some() => {
                    if sessions.key(key) == sessions_view::Action::Quit {
                        app.quit = true;
                    }
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => match app.key(key) {
                    Effect::Requery => {
                        worker.ask(app.range, &app.filter, &app.account, &app.machine)
                    }
                    Effect::Sessions => sessions.open(),
                    Effect::SaveLayout(text) => autosave.push(text),
                    Effect::SaveBudget(usd) => {
                        app.note = Some(save_budget(&home, usd));
                        worker.ask(app.range, &app.filter, &app.account, &app.machine);
                    }
                    Effect::SaveMaskAccount(mask) => {
                        if let Some(why) = save_mask_account(&home, mask) {
                            app.note = Some(why);
                        }
                    }
                    Effect::ResetUsage => worker.reset(),
                    Effect::ResetPrefs => {
                        let cleared = ui_state::reset(&home).map(|_| ());
                        let reset = Reset {
                            app: &mut app,
                            autosave: &mut autosave,
                            flag: &mut flag,
                            saved: &mut saved,
                            last_saved: &mut last_saved,
                        };
                        if let Effect::Requery = reset.finish(cleared) {
                            worker.ask(app.range, &app.filter, &app.account, &app.machine);
                        }
                    }
                    Effect::None => {}
                },
                _ => {}
            }
        }
        // Pending layout changes go out every 200 ms while the editor is open,
        // and at once when it closes or the program quits.
        let force = app.editor.is_none() || app.quit;
        match autosave.flush(Instant::now(), force, |text| {
            ui_state::save_layout(&home, text)
        }) {
            Flushed::Idle | Flushed::Saved => {}
            Flushed::Notice(text) | Flushed::Failed(text) => app.note = Some(text),
        }
        let now_state = state_to_save(&app, flag, &saved);
        if now_state != last_saved {
            ui_state::save(&home, &now_state);
            last_saved = now_state;
        }
        while let Ok(update) = worker.updates.try_recv() {
            match update {
                Update::Data(data) => {
                    // A reply for an earlier range or time zone may still be in flight.
                    if data.range == app.range
                        && data.filter == app.filter
                        && data.zone == crate::tz::current()
                    {
                        app.refresh(*data);
                        app.telemetry = Some(super::telemetry_status::detect_here(paths));
                    }
                }
                Update::Error(e) => app.error = Some(e),
                Update::Indexing(progress) => app.indexing = progress,
            }
        }
    }
    if let Some(why) = autosave.finish(|text| ui_state::save_layout(&home, text)) {
        crate::say::notice(&why);
    }
    Ok(())
}

/// Stores the manual monthly budget and says what happened.
fn save_budget(home: &std::path::Path, usd: f64) -> String {
    save_budget_with(usd, |value| {
        crate::host::set_config_value(budget_popup::KEY, value, home)
    })
}

/// [`save_budget`] with the write given, so the words can be tested.
fn save_budget_with(
    usd: f64,
    write: impl FnOnce(&serde_json::Value) -> Result<(), String>,
) -> String {
    match write(&serde_json::Value::from(usd)) {
        Ok(()) if usd > 0.0 => format!("Manual monthly budget set to {}", fmt::money(usd)),
        Ok(()) => "Manual monthly budget cleared".to_string(),
        Err(why) => format!("Budget not saved: {why}"),
    }
}

type Loaded = Result<(Vec<crate::session_view::Record>, i64), String>;

/// The Sessions management view and the thread that reads it. A read asks
/// `gh` for each branch, so it runs off the draw loop and the screen never
/// waits on it. At most one read is in flight.
struct SessionsLoop {
    paths: Paths,
    view: Option<SessionsView>,
    reply: Option<std::sync::mpsc::Receiver<Loaded>>,
    last_read: Option<std::time::Instant>,
}

impl SessionsLoop {
    fn new(paths: Paths) -> Self {
        SessionsLoop {
            paths,
            view: None,
            reply: None,
            last_read: None,
        }
    }

    fn open(&mut self) {
        self.view = Some(SessionsView::default());
        self.start();
    }

    fn start(&mut self) {
        if self.reply.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let paths = self.paths.clone();
        std::thread::spawn(move || {
            let home = crate::host::home_dir();
            let _ = tx.send(crate::host::load_sessions(&paths, &home));
        });
        self.reply = Some(rx);
        self.last_read = Some(std::time::Instant::now());
    }

    fn key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> sessions_view::Action {
        let action = match self.view.as_mut() {
            Some(v) => v.key(key),
            None => sessions_view::Action::None,
        };
        match action {
            sessions_view::Action::Close => self.view = None,
            sessions_view::Action::Refresh => self.start(),
            _ => {}
        }
        action
    }

    /// Takes a finished read and starts the next one when the view is open and
    /// the last one is old.
    fn poll(&mut self) {
        let Some(view) = self.view.as_mut() else {
            self.reply = None;
            return;
        };
        if let Some(rx) = &self.reply {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok((records, now)) => view.refresh(records, now),
                    Err(e) => view.fail(e),
                }
                self.reply = None;
            }
        }
        let due = self
            .last_read
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(sessions_view::REFRESH_SECONDS));
        if due {
            self.start();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::WeekStart;

    fn saved(range: Option<&str>) -> ui_state::UiState {
        ui_state::UiState {
            range: range.map(str::to_string),
            ..Default::default()
        }
    }

    fn store(tag: &str) -> std::path::PathBuf {
        let home = crate::util::scratch_dir(tag);
        let data = crate::config::root(&home).join("data");
        std::fs::create_dir_all(&data).unwrap();
        let conn = playbook_store::Connection::open(playbook_data::config::db_in(&data)).unwrap();
        conn.execute_batch(playbook_data::config::SCHEMA_SQL)
            .unwrap();
        home
    }

    #[test]
    fn the_mask_is_off_when_nothing_is_set() {
        let home = store("mask-default");

        assert!(!start_mask_account(&home));
    }

    #[test]
    fn the_config_key_turns_the_mask_on() {
        let home = store("mask-config");
        let db = playbook_data::config::db_in(&crate::config::root(&home).join("data"));
        playbook_store::Connection::open(db)
            .unwrap()
            .execute(
                "INSERT INTO config VALUES ('global','',?1,'true',1)",
                [MASK_ACCOUNT_KEY],
            )
            .unwrap();

        assert!(start_mask_account(&home));
    }

    #[test]
    fn a_toggle_is_stored_and_wins_over_the_config_key_on_the_next_start() {
        let home = store("mask-toggle");
        let db = playbook_data::config::db_in(&crate::config::root(&home).join("data"));
        playbook_store::Connection::open(db)
            .unwrap()
            .execute(
                "INSERT INTO config VALUES ('global','',?1,'true',1)",
                [MASK_ACCOUNT_KEY],
            )
            .unwrap();
        let mut app = App::new(Range::All);
        app.mask_account = start_mask_account(&home);

        let Effect::SaveMaskAccount(mask) = app.key(ratatui::crossterm::event::KeyEvent::from(
            ratatui::crossterm::event::KeyCode::Char('o'),
        )) else {
            panic!("the mask key must ask to store the choice");
        };
        let note = save_mask_account(&home, mask);

        assert_eq!(note, None);
        assert!(!app.mask_account);
        assert!(!start_mask_account(&home));
    }

    #[test]
    fn without_a_config_store_the_toggle_writes_nothing_and_says_nothing() {
        let home = crate::util::scratch_dir("mask-no-store");

        let note = save_mask_account(&home, true);

        assert_eq!(note, None);
        assert!(!crate::config::root(&home).join("data").exists());
        assert!(!start_mask_account(&home));
    }

    #[test]
    fn a_failed_write_says_the_mask_was_not_saved() {
        let home = crate::util::scratch_dir("mask-failed-write");
        let data = crate::config::root(&home).join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            playbook_data::config::db_in(&data),
            b"not a database at all",
        )
        .unwrap();

        let note = save_mask_account(&home, true);

        assert!(
            note.is_some_and(|n| n.starts_with("Account mask not saved:")),
            "a store that cannot be opened must say so"
        );
    }

    #[test]
    fn the_budget_note_says_set_cleared_or_not_saved() {
        let set = save_budget_with(1000.0, |_| Ok(()));
        let cleared = save_budget_with(0.0, |_| Ok(()));
        let failed = save_budget_with(5.0, |_| Err("no config store yet".to_string()));

        assert_eq!(set, "Manual monthly budget set to $1,000.00");
        assert_eq!(cleared, "Manual monthly budget cleared");
        assert_eq!(failed, "Budget not saved: no config store yet");
    }

    #[test]
    fn the_range_flag_beats_the_stored_range() {
        let got = start_range(Some(Range::Today), &saved(Some("30d")), WeekStart::Monday);

        assert_eq!(got, Range::Today);
    }

    #[test]
    fn the_stored_range_is_used_when_there_is_no_flag() {
        let got = start_range(None, &saved(Some("30d")), WeekStart::Monday);

        assert_eq!(got, Range::LastDays(30));
    }

    #[test]
    fn a_missing_or_invalid_stored_range_opens_on_this_week() {
        let week = Range::ThisWeek(WeekStart::Monday);

        assert_eq!(start_range(None, &saved(None), WeekStart::Monday), week);
        assert_eq!(
            start_range(None, &saved(Some("nonsense")), WeekStart::Monday),
            week
        );
    }

    #[test]
    fn a_flag_range_is_not_stored_until_the_user_changes_it() {
        let mut app = App::new(Range::Today);
        let stored = saved(Some("30d"));

        let untouched = state_to_save(&app, Some(Range::Today), &stored);
        app.range = Range::All;
        let changed = state_to_save(&app, Some(Range::Today), &stored);

        assert_eq!(untouched.range.as_deref(), Some("30d"));
        assert_eq!(changed.range.as_deref(), Some("all"));
    }

    fn finish_reset(
        app: &mut App,
        flag: Option<Range>,
        cleared: Result<(), String>,
    ) -> (
        Effect,
        Autosave,
        Option<Range>,
        ui_state::UiState,
        ui_state::UiState,
    ) {
        let mut autosave = Autosave::default();
        autosave.push("Old=spend".into());
        let mut flag = flag;
        let mut stored = saved(Some("30d"));
        let mut last_saved = saved(Some("30d"));
        let effect = Reset {
            app,
            autosave: &mut autosave,
            flag: &mut flag,
            saved: &mut stored,
            last_saved: &mut last_saved,
        }
        .finish(cleared);
        (effect, autosave, flag, stored, last_saved)
    }

    #[test]
    fn a_cleared_store_drops_the_pending_layout_the_flag_and_the_stored_state() {
        // Arrange
        let mut app = App::new(Range::Today);

        // Act
        let (_, mut autosave, flag, stored, last_saved) =
            finish_reset(&mut app, Some(Range::Today), Ok(()));

        // Assert
        let flushed = autosave.flush(Instant::now(), true, |_| Err("unused".into()));
        assert_eq!(flushed, Flushed::Idle);
        assert_eq!(flag, None);
        assert_eq!(stored, ui_state::UiState::default());
        assert_eq!(last_saved, state_to_save(&app, None, &stored));
    }

    #[test]
    fn a_failed_clear_keeps_the_pending_layout_the_flag_and_the_stored_state() {
        // Arrange
        let mut app = App::new(Range::Today);

        // Act
        let (effect, mut autosave, flag, stored, last_saved) =
            finish_reset(&mut app, Some(Range::Today), Err("locked".into()));

        // Assert
        let flushed = autosave.flush(Instant::now(), true, |_| Ok(true));
        assert_eq!(flushed, Flushed::Saved);
        assert_eq!(effect, Effect::None);
        assert_eq!(flag, Some(Range::Today));
        assert_eq!(stored, saved(Some("30d")));
        assert_eq!(last_saved, saved(Some("30d")));
    }
}
