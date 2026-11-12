// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The terminal view's state and what each key does. Pure: no terminal, no
//! store, so every key is a unit test.

use super::account::AccountSel;
use super::chart::Plot;
use super::data::Data;
use super::keys::{self, Cmd};
use super::layout::PanelId;
use super::machine::MachineSel;
use super::panels;
use super::sort::{
    day_rows, group_rows, pair_rows, session_day_rows, session_rows, Panel, Sort, PANELS,
};
use super::theme::{ColorDepth, Theme};
use super::view::{self, help_height};
use super::worker::REFRESH_SECONDS;

use crate::period::pair_groups;
use crate::query::{Range, WeekStart};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use std::time::Duration;

/// Every range the view can show: the calendar ranges from the shortest to
/// the longest, then the rolling ones.
pub fn ranges(week: WeekStart) -> [Range; 9] {
    [
        Range::Today,
        Range::ThisWeek(week),
        Range::LastWeek(week),
        Range::CurrentMonth,
        Range::LastMonth,
        Range::LastDays(30),
        Range::LastDays(60),
        Range::LastDays(90),
        Range::All,
    ]
}

/// The two ranges `r` steps between. Both are periods, so the view shows each
/// against the one before it. The month is reached with `m`.
pub fn default_ranges(week: WeekStart) -> [Range; 2] {
    [Range::Today, Range::ThisWeek(week)]
}

/// What the spend panel shows. `v` steps through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpendView {
    /// Cost per day, one color along the gradient.
    #[default]
    Total,
    /// Cost per day stacked by model.
    ByModel,
    /// A sortable table with a row for each day.
    Daily,
}

impl SpendView {
    pub fn name(self) -> &'static str {
        match self {
            SpendView::Total => "total",
            SpendView::ByModel => "by model",
            SpendView::Daily => "daily table",
        }
    }

    pub fn next(self) -> SpendView {
        match self {
            SpendView::Total => SpendView::ByModel,
            SpendView::ByModel => SpendView::Daily,
            SpendView::Daily => SpendView::Total,
        }
    }
}

/// What the projects panel shows. `v` switches it while the panel is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProjectsView {
    #[default]
    Projects,
    /// Cost per agent, from telemetry.
    Agents,
    /// Cost per skill, from telemetry.
    Skills,
    /// Tool latency and error rate per tool, from telemetry.
    Tools,
    /// API errors, retries, cache hit rate and busy time, from telemetry.
    Health,
}

impl ProjectsView {
    pub fn name(self) -> &'static str {
        match self {
            ProjectsView::Projects => "projects",
            ProjectsView::Agents => "agents",
            ProjectsView::Skills => "skills",
            ProjectsView::Tools => "tools",
            ProjectsView::Health => "health",
        }
    }

    pub fn next(self) -> ProjectsView {
        match self {
            ProjectsView::Projects => ProjectsView::Agents,
            ProjectsView::Agents => ProjectsView::Skills,
            ProjectsView::Skills => ProjectsView::Tools,
            ProjectsView::Tools => ProjectsView::Health,
            ProjectsView::Health => ProjectsView::Projects,
        }
    }
}

#[derive(Debug)]
pub struct App {
    pub data: Option<Data>,
    pub range: Range,
    /// The day the chart cursor is on, if it was moved or hovered. Both
    /// charts share it, whatever their bar widths.
    pub cursor: Option<i64>,
    /// What the spend panel shows.
    pub spend_view: SpendView,
    /// What the projects panel shows.
    pub projects_view: ProjectsView,
    /// The first day of the week for the week ranges.
    pub week_start: WeekStart,
    pub filter: String,
    /// Which account the numbers belong to.
    pub account: AccountSel,
    /// The header shows an unnamed email account cut to its domain.
    pub mask_account: bool,
    /// Which machine the machine panels belong to.
    pub machine: MachineSel,
    /// The last store error, shown in the header until the next good refresh.
    pub error: Option<String>,
    pub quit: bool,
    pub theme: Theme,
    pub depth: ColorDepth,
    /// `NO_COLOR` is set: the theme stays `mono`.
    pub no_color: bool,
    /// How the header icon is drawn, or off for plain text.
    pub icon: super::icon::Icon,
    /// `PBTOP_DOTS=0` is set: charts keep their solid glyphs instead of Braille dots.
    pub dots_off: bool,
    /// True while the filter is being typed; keys then edit `draft`.
    pub editing: bool,
    pub draft: String,
    /// The panel the keys act on.
    pub focus: Panel,
    /// Which of the tab's slots showing `focus` has the focus, counting from
    /// the first. Two pinned views of one panel (`spend.total` beside
    /// `spend.model`) share a `Panel`, so `focus` alone cannot tell them apart.
    pub focus_nth: usize,
    /// The selected row of each panel, by `Panel::index`.
    pub selected: [usize; 6],
    pub sorts: [Sort; 6],
    /// The screen size, for laying out the charts the cursor keys move over.
    pub area: Rect,
    /// The help window, with the line it is scrolled to, when it is open.
    pub help: Option<usize>,
    /// The budget window, when it is open.
    pub budget_popup: Option<super::budget_popup::BudgetPopup>,
    /// The header's budget lines show the figures of every limit and the full notes.
    pub budget_detail: bool,
    /// True while the window that asks before deleting the usage history is open.
    pub reset_popup: bool,
    /// Files read and files to read while a first read runs.
    pub indexing: Option<(usize, usize)>,
    /// Where transcripts are read from, named in the empty states.
    pub sources: String,
    /// How telemetry is configured, named in the footer.
    pub telemetry: Option<crate::telemetry_status::Mode>,
    /// How often the snapshot is refreshed, shown in the footer.
    pub refresh_every: Duration,
    /// The tabs and the panels on each.
    pub layout: super::layout::Layout,
    /// The selected tab, an index into `layout.tabs`.
    pub tab: usize,
    /// The layout editor, while it is open.
    pub editor: Option<super::editor::Editor>,
    /// A line that replaces the footer hints until the next key.
    pub note: Option<String>,
}

/// What the loop must do after a key.
#[derive(Debug, PartialEq)]
pub enum Effect {
    None,
    /// The range or filter changed: re-query now.
    Requery,
    /// Open the Sessions view.
    Sessions,
    /// Store the layout text (empty means back to the default).
    SaveLayout(String),
    /// Store the manual monthly budget in dollars (0 clears it), then re-query.
    SaveBudget(f64),
    /// Clear the stored layout and screen setup, then call `reset_done`.
    ResetPrefs,
    /// Store the account mask choice, so the next start opens with it.
    SaveMaskAccount(bool),
    /// Delete the usage history and read the transcripts again.
    ResetUsage,
}

/// What `v` says on a panel pinned to one view.
const PINNED_NOTE: &str = "This panel shows one view. Press e to add an unpinned one";

impl App {
    pub fn new(range: Range) -> App {
        App {
            data: None,
            range,
            cursor: None,
            spend_view: SpendView::default(),
            projects_view: ProjectsView::default(),
            week_start: WeekStart::default(),
            filter: String::new(),
            account: AccountSel::default(),
            mask_account: false,
            machine: MachineSel::default(),
            error: None,
            quit: false,
            theme: Theme::default(),
            depth: ColorDepth::default(),
            no_color: false,
            icon: super::icon::Icon::default(),
            dots_off: false,
            editing: false,
            draft: String::new(),
            focus: Panel::Models,
            focus_nth: 0,
            selected: [0; 6],
            sorts: PANELS.map(Panel::default_sort),
            area: Rect::new(0, 0, 100, 30),
            help: None,
            budget_popup: None,
            budget_detail: false,
            reset_popup: false,
            indexing: None,
            sources: String::new(),
            telemetry: None,
            refresh_every: Duration::from_secs(REFRESH_SECONDS),
            layout: super::layout::Layout::classic(),
            tab: 0,
            editor: None,
            note: None,
        }
    }

    /// The screen setup to remember.
    pub fn ui_state(&self) -> super::ui_state::UiState {
        super::ui_state::UiState {
            tab: self.layout.tabs.get(self.tab).map(|t| t.name.clone()),
            range: Some(self.range.key().to_string()),
            spend_view: Some(self.spend_view),
            projects_view: Some(self.projects_view),
            sorts: self.sorts.iter().map(|s| Some(*s)).collect(),
            filter: Some(self.filter.clone()),
            machine: Some(self.machine.clone()),
        }
    }

    /// Applies a remembered screen setup. The range is not part of it: the
    /// caller picks the range, since a command line flag outranks the stored
    /// one. A tab that no longer exists leaves the first tab selected.
    pub fn apply_ui_state(&mut self, state: &super::ui_state::UiState) {
        if let Some(name) = &state.tab {
            if let Some(i) = self.layout.tabs.iter().position(|t| &t.name == name) {
                self.tab = i;
            }
        }
        if let Some(v) = state.spend_view {
            self.spend_view = v;
        }
        if let Some(v) = state.projects_view {
            self.projects_view = v;
        }
        for panel in PANELS {
            if let Some(sort) = state.sort_for(panel, true) {
                self.sorts[panel.index()] = sort;
            }
        }
        if let Some(filter) = &state.filter {
            self.filter = filter.clone();
        }
        if let Some(machine) = &state.machine {
            self.machine = machine.clone();
        }
        self.fix_focus();
    }

    pub fn sort(&self, panel: Panel) -> Sort {
        self.sorts[panel.index()]
    }

    /// Whether the range is a period shown against the one before it.
    pub fn compared(&self) -> bool {
        self.data.as_ref().is_some_and(|d| d.comparison.is_some())
    }

    /// How many rows the panel's table has.
    fn rows(&self, panel: Panel) -> usize {
        let Some(d) = &self.data else { return 0 };
        match panel {
            Panel::Models => pair_groups(&d.models, &d.models_before).len(),
            Panel::Projects if self.projects_slot_view() == ProjectsView::Agents => {
                d.agents.rows.len()
            }
            Panel::Projects if self.projects_slot_view() == ProjectsView::Skills => {
                d.skills.rows.len()
            }
            Panel::Projects if self.projects_slot_view() == ProjectsView::Tools => {
                d.health.tools.len()
            }
            Panel::Projects if self.projects_slot_view() == ProjectsView::Health => {
                panels::HEALTH_ROWS
            }
            Panel::Projects => d.repos.len(),
            Panel::Live => d.live.active.len(),
            Panel::Sessions => d.sessions.len(),
            Panel::Spend if self.spend_slot_view() == SpendView::Daily => {
                let today = crate::tz::day_of(d.now);
                d.series.iter().filter(|p| p.day <= today).count()
            }
            Panel::Spend | Panel::Tokens => 0,
        }
    }

    /// The Breakdown view the keys act on: the focused slot's, or the first
    /// Breakdown slot of the tab while another panel has the focus.
    fn projects_slot_view(&self) -> ProjectsView {
        let slot = if self.focus == Panel::Projects {
            self.focus_slot()
        } else {
            self.tab_ids()
                .iter()
                .copied()
                .find(|id| id.panel() == Some(Panel::Projects))
        };
        slot.and_then(PanelId::projects_view)
            .unwrap_or(self.projects_view)
    }

    /// The Spend view of the slot `id`: its pinned view, else the switcher's.
    fn spend_view_of(&self, id: PanelId) -> SpendView {
        id.spend_view().unwrap_or(self.spend_view)
    }

    /// The Spend view the keys act on: the focused slot's, or the first
    /// Spend slot of the tab while another panel has the focus.
    fn spend_slot_view(&self) -> SpendView {
        let slot = if self.focus == Panel::Spend {
            self.focus_slot()
        } else {
            self.tab_ids()
                .iter()
                .copied()
                .find(|id| id.panel() == Some(Panel::Spend))
        };
        slot.map_or(self.spend_view, |id| self.spend_view_of(id))
    }

    /// Whether the panel is showing a table right now.
    pub fn is_table(&self, panel: Panel) -> bool {
        match panel {
            Panel::Spend => self.spend_slot_view() == SpendView::Daily,
            Panel::Tokens => false,
            _ => true,
        }
    }

    /// The selected row of `panel`, kept inside its rows.
    pub fn selection(&self, panel: Panel) -> usize {
        self.selected[panel.index()].min(self.rows(panel).saturating_sub(1))
    }

    fn select(&mut self, panel: Panel, to: usize) {
        self.selected[panel.index()] = to.min(self.rows(panel).saturating_sub(1));
    }

    fn step(&mut self, panel: Panel, by: isize) {
        let at = self.selection(panel) as isize;
        self.select(panel, (at + by).max(0) as usize);
    }

    /// The row keys of a table in the order it is drawn: what a row is, so a
    /// refresh can find the selected row again after the rows moved.
    fn row_ids(&self, panel: Panel) -> Vec<String> {
        let Some(d) = &self.data else {
            return Vec::new();
        };
        let sort = self.sort(panel);
        match panel {
            Panel::Models => pair_rows(&d.models, &d.models_before, sort)
                .into_iter()
                .map(|p| p.key)
                .collect(),
            Panel::Projects => {
                let groups = match self.projects_slot_view() {
                    ProjectsView::Agents => &d.agents.rows,
                    ProjectsView::Skills => &d.skills.rows,
                    ProjectsView::Projects => &d.repos,
                    // Fixed order, one row per tool or figure.
                    ProjectsView::Tools => {
                        return d.health.tools.iter().map(|t| t.name.clone()).collect()
                    }
                    ProjectsView::Health => return Vec::new(),
                };
                group_rows(groups, sort)
                    .iter()
                    .map(|g| g.key.clone())
                    .collect()
            }
            Panel::Live => session_rows(&d.live.active, sort)
                .iter()
                .map(|s| s.id.clone())
                .collect(),
            Panel::Sessions => session_day_rows(&d.sessions, sort)
                .iter()
                .map(|s| format!("{}/{}", s.day, s.id))
                .collect(),
            Panel::Spend if self.spend_slot_view() == SpendView::Daily => {
                let today = crate::tz::day_of(d.now);
                day_rows(&d.series, today, sort)
                    .iter()
                    .map(|p| p.day.to_string())
                    .collect()
            }
            Panel::Spend | Panel::Tokens => Vec::new(),
        }
    }

    /// The text that filters the view to the selected row: its model, project
    /// or session id.
    fn selected_key(&self) -> Option<String> {
        let d = self.data.as_ref()?;
        let panel = self.focus;
        let at = self.selection(panel);
        let sort = self.sort(panel);
        match panel {
            Panel::Models => pair_rows(&d.models, &d.models_before, sort)
                .into_iter()
                .nth(at)
                .map(|p| p.key),
            // An agent, skill, tool or figure is not a field of a usage event, so it cannot filter.
            Panel::Projects if self.projects_slot_view() != ProjectsView::Projects => None,
            Panel::Projects => group_rows(&d.repos, sort).get(at).map(|g| g.key.clone()),
            Panel::Live => session_rows(&d.live.active, sort)
                .get(at)
                .map(|s| s.id.clone()),
            Panel::Sessions => session_day_rows(&d.sessions, sort)
                .get(at)
                .map(|s| s.id.clone()),
            Panel::Spend | Panel::Tokens => None,
        }
    }

    /// Takes a fresh snapshot without moving anything the user set: each
    /// table's selected row is found again by what it is, so the selection
    /// and the scroll that follows it stay on the same row when the rows
    /// reorder. The cursor, focus, sorts and view are not touched.
    pub fn refresh(&mut self, data: Data) {
        let kept: Vec<Option<String>> = PANELS
            .iter()
            .map(|p| self.row_ids(*p).into_iter().nth(self.selection(*p)))
            .collect();
        self.data = Some(data);
        self.error = None;
        for (panel, id) in PANELS.iter().zip(kept) {
            if let Some(at) = id.and_then(|id| self.row_ids(*panel).iter().position(|r| *r == id)) {
                self.selected[panel.index()] = at;
            }
        }
    }

    /// The panel ids of the selected tab. The layout is never empty, but a
    /// stale index falls back to the first tab.
    pub fn tab_ids(&self) -> &[super::layout::PanelId] {
        self.layout
            .tabs
            .get(self.tab)
            .or_else(|| self.layout.tabs.first())
            .map_or(&[][..], |t| t.panels.as_slice())
    }

    /// The panel count of each row of the selected tab, `None` for an
    /// automatic tab.
    pub fn tab_rows(&self) -> Option<&[u8]> {
        self.layout
            .tabs
            .get(self.tab)
            .or_else(|| self.layout.tabs.first())
            .and_then(|t| t.rows.as_deref())
    }

    /// The focusable panels of the selected tab, in the order they are drawn.
    /// A panel drawn twice, like `spend` beside `spend.total`, counts once.
    fn tab_panels(&self) -> Vec<Panel> {
        let mut panels: Vec<Panel> = Vec::new();
        for panel in self.tab_ids().iter().filter_map(|id| id.panel()) {
            if !panels.contains(&panel) {
                panels.push(panel);
            }
        }
        panels
    }

    /// The slots of the selected tab that take focus, in the order they are
    /// drawn. Unlike `tab_panels`, two views of one panel are two slots.
    fn tab_slots(&self) -> Vec<PanelId> {
        self.tab_ids()
            .iter()
            .copied()
            .filter(|id| id.panel().is_some())
            .collect()
    }

    /// The slot that has the focus, `None` when the tab shows no focusable panel.
    pub fn focus_slot(&self) -> Option<PanelId> {
        let same: Vec<PanelId> = self
            .tab_ids()
            .iter()
            .copied()
            .filter(|id| id.panel() == Some(self.focus))
            .collect();
        same.get(self.focus_nth).or(same.first()).copied()
    }

    /// Moves the focus to `slot` of the selected tab.
    fn focus_on(&mut self, slot: PanelId) {
        let panel = slot.panel();
        self.focus_nth = self
            .tab_slots()
            .iter()
            .take_while(|id| **id != slot)
            .filter(|id| id.panel() == panel)
            .count();
        if let Some(panel) = panel {
            self.focus = panel;
        }
    }

    /// Keeps the focus on a panel the selected tab shows.
    fn fix_focus(&mut self) {
        let shown = self.tab_slots();
        // Re-count the slot so a stale index from another tab cannot linger.
        match self.focus_slot() {
            Some(slot) => self.focus_on(slot),
            None => {
                if let Some(first) = shown.first() {
                    self.focus_on(*first);
                }
            }
        }
    }

    /// Shows tab `to`, wrapping around the ends.
    pub fn go_tab(&mut self, to: isize) {
        let n = self.layout.tabs.len().max(1) as isize;
        self.tab = to.rem_euclid(n) as usize;
        self.fix_focus();
    }

    fn focus_next(&mut self, by: isize) {
        let shown = self.tab_slots();
        if shown.is_empty() {
            return;
        }
        let at = self
            .focus_slot()
            .and_then(|slot| shown.iter().position(|id| *id == slot))
            .unwrap_or(0) as isize;
        self.focus_on(shown[(at + by).rem_euclid(shown.len() as isize) as usize]);
    }

    /// The slot whose chart the cursor keys move: the focused Spend slot when
    /// it charts, else the first slot of the tab that charts `panel`. A Spend
    /// slot on the daily table has no chart.
    fn chart_slot(&self, panel: Panel) -> Option<PanelId> {
        let charts = |id: &PanelId| {
            id.panel() == Some(panel)
                && (panel != Panel::Spend || self.spend_view_of(*id) != SpendView::Daily)
        };
        self.focus_slot()
            .filter(charts)
            .or_else(|| self.tab_ids().iter().copied().find(charts))
    }

    /// The laid out chart of a chart panel, once there is data to chart.
    fn plot(&self, panel: Panel) -> Option<Plot> {
        let data = self.data.as_ref()?;
        if data.series.is_empty()
            || self.area.width < view::MIN_WIDTH
            || self.area.height < view::MIN_HEIGHT
        {
            return None;
        }
        let id = self.chart_slot(panel)?;
        let area = view::panel_rect(
            self.area,
            view::header_height(self, self.area.width),
            self.tab_ids(),
            self.tab_rows(),
            self.layout.tabs.len() > 1,
            id,
        )?;
        Some(panels::chart_plot(area, data, panel))
    }

    /// Moves the chart cursor by `by` bars, or to an end of the chart with
    /// `isize::MIN` or `isize::MAX` steps. With no cursor yet it starts from today's bar.
    fn move_cursor(&mut self, by: isize) {
        let shown = self.tab_panels();
        // The daily table owns the cursor keys while it has the focus, even
        // when another Spend slot of the tab charts; otherwise the chart that
        // is shown moves the shared cursor.
        if self.focus == Panel::Spend && self.is_table(Panel::Spend) {
            return;
        }
        let chart = if shown.contains(&Panel::Spend)
            && (!self.is_table(Panel::Spend)
                || self.focus == Panel::Spend
                || !shown.contains(&Panel::Tokens))
        {
            Panel::Spend
        } else {
            Panel::Tokens
        };
        let Some(plot) = self.plot(chart) else {
            return;
        };
        let last = plot.bars.len().saturating_sub(1);
        let today = self.data.as_ref().map_or(0, |d| crate::tz::day_of(d.now));
        let at = self
            .cursor
            .and_then(|d| plot.index_of(d))
            .or_else(|| plot.default_index(today))
            .unwrap_or(last);
        let to = at.saturating_add_signed(by).min(last);
        self.cursor = plot.bars.get(to).map(|b| b.first);
    }

    /// Switches the range, re-querying only when it changed.
    fn set_range(&mut self, range: Range) -> Effect {
        if range == self.range {
            return Effect::None;
        }
        self.range = range;
        self.cursor = None;
        Effect::Requery
    }

    pub fn key(&mut self, key: KeyEvent) -> Effect {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        self.note = None;
        if self.editing {
            return self.edit(key.code, ctrl);
        }
        if let Some(popup) = self.budget_popup.as_mut() {
            if ctrl && key.code == KeyCode::Char('c') {
                self.quit = true;
                return Effect::None;
            }
            // A Ctrl chord is not text.
            if ctrl {
                return Effect::None;
            }
            return match popup.key(key.code) {
                super::budget_popup::Outcome::Stay => Effect::None,
                super::budget_popup::Outcome::Close => {
                    self.budget_popup = None;
                    Effect::None
                }
                super::budget_popup::Outcome::Save(usd) => {
                    self.budget_popup = None;
                    Effect::SaveBudget(usd)
                }
            };
        }
        if self.reset_popup {
            return self.reset_popup_key(key.code, ctrl);
        }
        if self.help.is_some() {
            return self.help_key(key.code, ctrl);
        }
        if self.editor.is_some() {
            return self.editor_key(key.code, ctrl);
        }
        match keys::lookup(key.code, ctrl) {
            Some(binding) => self.run(binding.cmd, key.code),
            None => Effect::None,
        }
    }

    /// Keys while the reset window is open: `y` deletes, Ctrl-C quits and any
    /// other key cancels.
    fn reset_popup_key(&mut self, code: KeyCode, ctrl: bool) -> Effect {
        if ctrl && code == KeyCode::Char('c') {
            self.quit = true;
            return Effect::None;
        }
        self.reset_popup = false;
        if ctrl || !super::reset_popup::confirms(code) {
            return Effect::None;
        }
        self.error = None;
        self.note = Some("Reading the transcripts again".to_string());
        Effect::ResetUsage
    }

    /// Keys while the layout editor is open. Ctrl-C quits.
    fn editor_key(&mut self, code: KeyCode, ctrl: bool) -> Effect {
        if ctrl && code == KeyCode::Char('c') {
            self.quit = true;
            return Effect::None;
        }
        let Some(editor) = self.editor.as_mut() else {
            return Effect::None;
        };
        match editor.key(code) {
            super::editor::Outcome::Close => {
                self.editor = None;
                Effect::None
            }
            super::editor::Outcome::ResetAll => Effect::ResetPrefs,
            super::editor::Outcome::Stay if editor.layout == self.layout => Effect::None,
            super::editor::Outcome::Stay => {
                let (edited, tab) = (editor.layout.clone(), editor.tab);
                self.apply_edit(edited, tab)
            }
        }
    }

    /// Shows the edited layout behind the editor at once and asks for it to be
    /// stored. A layout the text form cannot hold is refused in the editor.
    fn apply_edit(&mut self, edited: super::layout::Layout, tab: usize) -> Effect {
        let text = edited.to_text();
        if let Err(why) = super::layout::Layout::parse(&text) {
            if let Some(editor) = self.editor.as_mut() {
                editor.message = Some(why);
            }
            return Effect::None;
        }
        self.use_layout(edited, tab);
        Effect::SaveLayout(text)
    }

    /// Finishes a reset once the stored layout and setup were cleared. On
    /// success the layout, tab, range, views, sorts and filter go back to the
    /// defaults and the editor closes. On a failure nothing changes here.
    pub fn reset_done(&mut self, cleared: Result<(), String>) -> Effect {
        if let Err(why) = cleared {
            self.note = Some(format!("Reset failed: {why}"));
            return Effect::None;
        }
        let range = Range::ThisWeek(self.week_start);
        let query_changed = self.range != range || !self.filter.is_empty();
        self.editor = None;
        self.range = range;
        self.filter.clear();
        self.spend_view = SpendView::default();
        self.projects_view = ProjectsView::default();
        self.sorts = PANELS.map(Panel::default_sort);
        self.selected = [0; 6];
        self.cursor = None;
        self.use_layout(super::layout::Layout::default(), 0);
        if query_changed {
            Effect::Requery
        } else {
            Effect::None
        }
    }

    /// Switches to `layout`, showing tab `tab` of it.
    fn use_layout(&mut self, layout: super::layout::Layout, tab: usize) {
        self.layout = layout;
        self.go_tab(tab as isize);
    }

    /// Keys while the help window is open: the arrows and j and k scroll it, Ctrl-C quits
    /// and any other key closes it. The data keeps refreshing underneath.
    fn help_key(&mut self, code: KeyCode, ctrl: bool) -> Effect {
        let at = self.help.unwrap_or(0);
        let page = self.help_page();
        let to = match code {
            KeyCode::Down | KeyCode::Char('j') => Some(at + 1),
            KeyCode::Up | KeyCode::Char('k') => Some(at.saturating_sub(1)),
            KeyCode::PageDown => Some(at + page),
            KeyCode::PageUp => Some(at.saturating_sub(page)),
            KeyCode::Home => Some(0),
            KeyCode::End => Some(usize::MAX),
            _ => None,
        };
        match to {
            Some(to) => self.help = Some(to.min(self.help_scroll_max())),
            None if ctrl && code == KeyCode::Char('c') => self.quit = true,
            None => self.help = None,
        }
        Effect::None
    }

    /// Help lines that fit in the window.
    pub fn help_page(&self) -> usize {
        usize::from(help_height(self.area.height).saturating_sub(2)).max(1)
    }

    fn help_scroll_max(&self) -> usize {
        keys::help_lines().len().saturating_sub(self.help_page())
    }

    /// Runs one command.
    fn run(&mut self, cmd: Cmd, code: KeyCode) -> Effect {
        let focus = self.focus;
        // A tab with no focusable panel (a tab of only the retired projection
        // word) has nothing for the row, sort and view keys to act on.
        let acts_on_focus = matches!(
            cmd,
            Cmd::CursorLeft
                | Cmd::CursorRight
                | Cmd::First
                | Cmd::Last
                | Cmd::Down
                | Cmd::Up
                | Cmd::PageDown
                | Cmd::PageUp
                | Cmd::NextView
                | Cmd::Sort
                | Cmd::SortFlip
                | Cmd::Pick
        );
        if acts_on_focus && !self.tab_panels().contains(&focus) {
            return Effect::None;
        }
        match cmd {
            Cmd::Quit => self.quit = true,
            Cmd::Theme if !self.no_color => self.theme = self.theme.next(),
            Cmd::Theme => {}
            Cmd::MaskAccount => {
                self.mask_account = !self.mask_account;
                return Effect::SaveMaskAccount(self.mask_account);
            }
            Cmd::Today => return self.set_range(Range::Today),
            Cmd::Week => return self.set_range(Range::ThisWeek(self.week_start)),
            Cmd::LastWeek => return self.set_range(Range::LastWeek(self.week_start)),
            Cmd::Month => return self.set_range(Range::CurrentMonth),
            Cmd::LastMonth => return self.set_range(Range::LastMonth),
            Cmd::All => return self.set_range(Range::All),
            Cmd::CycleRange => {
                let all = default_ranges(self.week_start);
                // From any other range, `r` goes to the first of the two.
                let next = match all.iter().position(|r| *r == self.range) {
                    Some(at) => all[(at + 1) % all.len()],
                    None => all[0],
                };
                return self.set_range(next);
            }
            Cmd::Filter => {
                self.editing = true;
                self.draft = self.filter.clone();
            }
            Cmd::ClearFilter if !self.filter.is_empty() => {
                self.filter.clear();
                return Effect::Requery;
            }
            Cmd::ClearFilter => {}
            Cmd::FocusPanel => {
                if let KeyCode::Char(c @ '1'..='6') = code {
                    let panel = PANELS[usize::from(c as u8 - b'1')];
                    if let Some(slot) = self
                        .tab_slots()
                        .into_iter()
                        .find(|id| id.panel() == Some(panel))
                    {
                        self.focus_on(slot);
                    }
                }
            }
            // With one tab there is nothing to switch to, so the key moves the
            // panel focus as it did before tabs.
            Cmd::NextTab if self.layout.tabs.len() < 2 => self.focus_next(1),
            Cmd::PrevTab if self.layout.tabs.len() < 2 => self.focus_next(-1),
            Cmd::NextTab => self.go_tab(self.tab as isize + 1),
            Cmd::PrevTab => self.go_tab(self.tab as isize - 1),
            Cmd::GoTab => {
                if let KeyCode::F(n @ 1..=12) = code {
                    if usize::from(n) <= self.layout.tabs.len() {
                        self.go_tab(isize::from(n) - 1);
                    }
                }
            }
            Cmd::NextPanel => self.focus_next(1),
            Cmd::PrevPanel => self.focus_next(-1),
            Cmd::CursorLeft => self.move_cursor(-1),
            Cmd::CursorRight => self.move_cursor(1),
            Cmd::First if focus.is_chart() && !self.is_table(focus) => {
                self.move_cursor(isize::MIN);
            }
            Cmd::Last if focus.is_chart() && !self.is_table(focus) => {
                self.move_cursor(isize::MAX);
            }
            Cmd::First => self.select(focus, 0),
            Cmd::Last => self.select(focus, usize::MAX),
            Cmd::Down => self.step(focus, 1),
            Cmd::Up => self.step(focus, -1),
            Cmd::PageDown => self.step(focus, 5),
            Cmd::PageUp => self.step(focus, -5),
            Cmd::NextView => match focus {
                Panel::Projects
                    if self
                        .focus_slot()
                        .is_some_and(|id| id.projects_view().is_some()) =>
                {
                    self.note = Some(PINNED_NOTE.to_string());
                }
                Panel::Projects => self.projects_view = self.projects_view.next(),
                Panel::Spend
                    if self
                        .focus_slot()
                        .is_some_and(|id| id.spend_view().is_some()) =>
                {
                    self.note = Some(PINNED_NOTE.to_string());
                }
                Panel::Spend => self.spend_view = self.spend_view.next(),
                _ => {}
            },
            Cmd::Sort if self.is_table(focus) => {
                self.sorts[focus.index()] = self.sort(focus).next(focus, self.compared());
            }
            Cmd::SortFlip if self.is_table(focus) => {
                self.sorts[focus.index()] = self.sort(focus).flip();
            }
            Cmd::Sort | Cmd::SortFlip => {}
            Cmd::Pick => {
                // Enter on the row already used as the filter lifts it.
                if let Some(key) = self.selected_key() {
                    self.filter = if self.filter == key {
                        String::new()
                    } else {
                        key
                    };
                    return Effect::Requery;
                }
            }
            Cmd::Sessions => return Effect::Sessions,
            Cmd::EditLayout => {
                self.editor = Some(super::editor::Editor::new(&self.layout, self.tab));
            }
            Cmd::Help => self.help = Some(0),
            Cmd::Budget => self.budget_popup = Some(super::budget_popup::BudgetPopup::default()),
            Cmd::BudgetDetail => self.budget_detail = !self.budget_detail,
            Cmd::Zone => {
                let to = match crate::tz::current() {
                    crate::tz::Zone::Utc => crate::tz::Zone::Local,
                    _ => crate::tz::Zone::Utc,
                };
                crate::tz::set(to);
                // The cursor is a day number in the old zone.
                self.cursor = None;
                self.note = Some(match to {
                    crate::tz::Zone::Utc => "Showing UTC".to_string(),
                    _ => "Showing local time".to_string(),
                });
                return Effect::Requery;
            }
            Cmd::Account => {
                let (shown, choices) = self.accounts_shown();
                let next = if code == KeyCode::Char('U') {
                    self.account.toggle_all()
                } else {
                    self.account.next(shown.as_deref(), &choices)
                };
                if next == self.account {
                    self.note = Some("Only one account has usage".to_string());
                } else {
                    self.account = next;
                    self.cursor = None;
                    return Effect::Requery;
                }
            }
            Cmd::Machine => {
                let (shown, choices) = self.machines_shown();
                let next = self.machine.next(shown.as_deref(), &choices);
                if next == self.machine {
                    self.note = Some("Only one machine has usage".to_string());
                } else {
                    self.machine = next;
                    self.cursor = None;
                    return Effect::Requery;
                }
            }
            Cmd::Refresh => return Effect::Requery,
            Cmd::ResetUsage => self.reset_popup = true,
        }
        Effect::None
    }

    /// The account on screen and every account to pick from, as the last
    /// refresh found them.
    fn accounts_shown(&self) -> (Option<String>, Vec<(String, String)>) {
        self.data.as_ref().map_or((None, Vec::new()), |d| {
            (d.accounts.key.clone(), d.accounts.choices.clone())
        })
    }

    /// The machine on screen and every machine to pick from, as the last
    /// refresh found them.
    fn machines_shown(&self) -> (Option<String>, Vec<String>) {
        self.data.as_ref().map_or((None, Vec::new()), |d| {
            (d.machines.key.clone(), d.machines.choices.clone())
        })
    }

    /// Keys while the filter is being typed. Enter applies, Esc drops it.
    fn edit(&mut self, code: KeyCode, ctrl: bool) -> Effect {
        match code {
            KeyCode::Enter => {
                self.editing = false;
                if self.draft != self.filter {
                    self.filter = std::mem::take(&mut self.draft);
                    return Effect::Requery;
                }
            }
            KeyCode::Esc => self.editing = false,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Backspace => {
                self.draft.pop();
            }
            KeyCode::Char(c) if !ctrl => self.draft.push(c),
            _ => {}
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn q_esc_and_ctrl_c_quit() {
        for key in [
            press(KeyCode::Char('q')),
            press(KeyCode::Esc),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            let mut app = App::new(Range::All);
            app.key(key);
            assert!(app.quit);
        }
    }

    #[test]
    fn r_steps_between_today_and_the_week_and_starts_over_from_any_other_range() {
        let mut app = App::new(Range::Today);
        assert_eq!(app.key(press(KeyCode::Char('r'))), Effect::Requery);
        assert_eq!(app.range, Range::ThisWeek(WeekStart::Monday));
        assert_eq!(app.key(press(KeyCode::Char('r'))), Effect::Requery);
        assert_eq!(app.range, Range::Today);
        for other in [Range::CurrentMonth, Range::LastDays(30), Range::All] {
            app.range = other;
            app.key(press(KeyCode::Char('r')));
            assert_eq!(app.range, Range::Today, "{other:?}");
        }
    }

    #[test]
    fn direct_keys_jump_to_a_range_and_requery_only_on_a_change() {
        let mut app = App::new(Range::LastDays(30));
        app.week_start = WeekStart::Sunday;
        for (key, want) in [
            ('d', Range::Today),
            ('w', Range::ThisWeek(WeekStart::Sunday)),
            ('W', Range::LastWeek(WeekStart::Sunday)),
            ('m', Range::CurrentMonth),
            ('M', Range::LastMonth),
            ('a', Range::All),
        ] {
            assert_eq!(app.key(press(KeyCode::Char(key))), Effect::Requery, "{key}");
            assert_eq!(app.range, want, "{key}");
            assert_eq!(app.key(press(KeyCode::Char(key))), Effect::None, "{key}");
        }
    }

    fn type_filter(app: &mut App, text: &str) {
        app.key(press(KeyCode::Char('/')));
        for c in text.chars() {
            app.key(press(KeyCode::Char(c)));
        }
    }

    #[test]
    fn slash_types_a_filter_and_enter_applies_it() {
        let mut app = App::new(Range::All);
        type_filter(&mut app, "opus");
        assert!(app.editing);
        assert_eq!(app.filter, "");
        assert_eq!(app.key(press(KeyCode::Enter)), Effect::Requery);
        assert_eq!((app.editing, app.filter.as_str()), (false, "opus"));
    }

    #[test]
    fn typing_q_or_r_while_editing_is_text_not_a_command() {
        let mut app = App::new(Range::All);
        type_filter(&mut app, "qr");
        assert!(!app.quit);
        assert_eq!(app.range, Range::All);
        assert_eq!(app.draft, "qr");
    }

    #[test]
    fn backspace_edits_and_esc_cancels_without_a_requery() {
        let mut app = App::new(Range::All);
        type_filter(&mut app, "abc");
        app.key(press(KeyCode::Backspace));
        assert_eq!(app.draft, "ab");
        assert_eq!(app.key(press(KeyCode::Esc)), Effect::None);
        assert!(!app.editing && !app.quit);
        assert_eq!(app.filter, "");
    }

    #[test]
    fn enter_on_an_unchanged_filter_does_not_requery() {
        let mut app = App::new(Range::All);
        app.key(press(KeyCode::Char('/')));
        assert_eq!(app.key(press(KeyCode::Enter)), Effect::None);
    }

    #[test]
    fn editing_an_existing_filter_starts_from_it() {
        let mut app = App::new(Range::All);
        app.filter = "sonnet".into();
        app.key(press(KeyCode::Char('/')));
        assert_eq!(app.draft, "sonnet");
    }

    #[test]
    fn c_clears_a_filter_and_requeries_only_when_one_is_set() {
        let mut app = App::new(Range::All);
        assert_eq!(app.key(press(KeyCode::Char('c'))), Effect::None);
        app.filter = "x".into();
        assert_eq!(app.key(press(KeyCode::Char('c'))), Effect::Requery);
        assert_eq!(app.filter, "");
    }

    #[test]
    fn t_cycles_the_theme() {
        let mut app = App::new(Range::All);
        assert_eq!(app.theme, Theme::Standard);
        app.key(press(KeyCode::Char('t')));
        assert_eq!(app.theme, Theme::Spectrum);
        app.key(press(KeyCode::Char('t')));
        assert_eq!(app.theme, Theme::Dark);
    }

    #[test]
    fn the_mask_key_flips_the_mask_and_asks_to_store_it() {
        let mut app = App::new(Range::All);
        assert!(!app.mask_account);
        assert_eq!(
            app.key(press(KeyCode::Char('o'))),
            Effect::SaveMaskAccount(true)
        );
        assert!(app.mask_account);
        assert_eq!(
            app.key(press(KeyCode::Char('o'))),
            Effect::SaveMaskAccount(false)
        );
        assert!(!app.mask_account);
    }

    #[test]
    fn no_color_keeps_the_theme_locked() {
        let mut app = App::new(Range::All);
        app.no_color = true;
        app.theme = Theme::Mono;
        app.key(press(KeyCode::Char('t')));
        assert_eq!(app.theme, Theme::Mono);
    }

    /// A snapshot of three models, one session each, with these costs.
    fn data_with(costs: [f64; 3]) -> Data {
        use crate::UsageEvent;
        let ev = |id: &str, model: &str, repo: &str, cost: f64| UsageEvent {
            event_id: id.into(),
            timestamp: 1_788_400_000 - 30,
            session_id: format!("sess-{id}"),
            model: model.into(),
            repo: repo.into(),
            cost_usd: cost,
            ..UsageEvent::default()
        };
        let events = [
            ev("a", "opus", "alpha", costs[0]),
            ev("b", "sonnet", "beta", costs[1]),
            ev("c", "haiku", "gamma", costs[2]),
        ];
        let live = crate::query::live_view(&events, &events, &events, 1_788_400_000);
        Data::build(&events, live, 1_788_400_000, Range::All, "")
    }

    fn loaded() -> App {
        let mut app = App::new(Range::All);
        app.data = Some(data_with([5.0, 1.0, 3.0]));
        app
    }

    fn with_accounts(keys: &[&str]) -> App {
        let mut app = loaded();
        let choices: Vec<(String, String)> = keys
            .iter()
            .map(|k| (k.to_string(), k.to_string()))
            .collect();
        if let Some(d) = app.data.as_mut() {
            d.accounts.key = AccountSel::Latest.resolve(&choices);
            d.accounts.choices = choices;
        }
        app
    }

    #[test]
    fn u_cycles_the_accounts_and_capital_u_shows_all_of_them() {
        use super::super::account::AccountSel as Sel;
        let mut app = with_accounts(&["work", "home"]);

        assert_eq!(app.key(press(KeyCode::Char('u'))), Effect::Requery);
        assert_eq!(app.account, Sel::Key("home".into()));
        if let Some(d) = app.data.as_mut() {
            d.accounts.key = Some("home".into());
        }
        assert_eq!(app.key(press(KeyCode::Char('u'))), Effect::Requery);
        assert_eq!(app.account, Sel::All);
        assert_eq!(app.key(press(KeyCode::Char('U'))), Effect::Requery);
        assert_eq!(app.account, Sel::Latest);
        assert_eq!(app.key(press(KeyCode::Char('U'))), Effect::Requery);
        assert_eq!(app.account, Sel::All);
    }

    fn with_machines(ids: &[&str]) -> App {
        let mut app = loaded();
        let choices: Vec<String> = ids.iter().map(|k| k.to_string()).collect();
        if let Some(d) = app.data.as_mut() {
            d.machines.key = app.machine.resolve(&choices[0], &choices);
            d.machines.choices = choices;
        }
        app
    }

    #[test]
    fn x_cycles_this_machine_each_other_machine_then_all() {
        let mut app = with_machines(&["me", "b"]);
        let mut seen = Vec::new();
        for _ in 0..3 {
            assert_eq!(app.key(press(KeyCode::Char('x'))), Effect::Requery);
            seen.push(app.machine.clone());
            let shown = app
                .machine
                .resolve("me", &["me".to_string(), "b".to_string()]);
            if let Some(d) = app.data.as_mut() {
                d.machines.key = shown;
            }
        }
        assert_eq!(
            seen,
            [
                MachineSel::Key("b".into()),
                MachineSel::All,
                MachineSel::This
            ]
        );
    }

    #[test]
    fn x_with_one_machine_changes_nothing_and_says_so() {
        let mut app = with_machines(&["me"]);

        assert_eq!(app.key(press(KeyCode::Char('x'))), Effect::None);
        assert_eq!(app.machine, MachineSel::This);
        assert_eq!(app.note.as_deref(), Some("Only one machine has usage"));
    }

    #[test]
    fn the_machine_pick_is_restored_from_the_saved_screen_setup() {
        let mut first = App::new(Range::All);
        first.machine = MachineSel::Key("b".into());
        let mut second = App::new(Range::All);

        second.apply_ui_state(&super::super::ui_state::UiState::parse(
            &first.ui_state().to_text(),
        ));

        assert_eq!(second.machine, MachineSel::Key("b".into()));
    }

    #[test]
    fn u_with_one_account_changes_nothing_and_says_so() {
        let mut app = with_accounts(&["work"]);

        assert_eq!(app.key(press(KeyCode::Char('u'))), Effect::None);
        assert_eq!(app.account, AccountSel::Latest);
        assert_eq!(app.note.as_deref(), Some("Only one account has usage"));
    }

    #[test]
    fn i_toggles_the_budget_detail_and_changes_nothing_else() {
        let mut app = loaded();
        assert!(!app.budget_detail);

        assert_eq!(app.key(press(KeyCode::Char('i'))), Effect::None);
        assert!(app.budget_detail);
        assert_eq!(app.key(press(KeyCode::Char('i'))), Effect::None);
        assert!(!app.budget_detail);
    }

    #[test]
    fn b_opens_the_budget_window_and_enter_asks_to_store_the_amount() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('b')));
        assert!(app.budget_popup.is_some());
        for c in "1,000".chars() {
            app.key(press(KeyCode::Char(c)));
        }
        let effect = app.key(press(KeyCode::Enter));
        assert_eq!(effect, Effect::SaveBudget(1000.0));
        assert!(app.budget_popup.is_none());
    }

    #[test]
    fn capital_x_asks_first_and_only_y_deletes_the_history() {
        // Arrange
        let mut app = loaded();

        // Act
        let opened = app.key(press(KeyCode::Char('X')));
        let confirmed = app.key(press(KeyCode::Char('y')));

        // Assert
        assert_eq!(opened, Effect::None);
        assert_eq!(confirmed, Effect::ResetUsage);
        assert!(!app.reset_popup);
        assert!(app.note.is_some());
    }

    #[test]
    fn any_other_key_cancels_the_reset_and_does_not_leak_to_the_view() {
        for cancel in [
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Char('n'),
            KeyCode::Char('q'),
        ] {
            let mut app = loaded();
            app.key(press(KeyCode::Char('X')));
            assert!(app.reset_popup);

            let effect = app.key(press(cancel));

            assert_eq!(effect, Effect::None, "{cancel:?}");
            assert!(!app.reset_popup && !app.quit, "{cancel:?}");
        }
    }

    #[test]
    fn caps_lock_does_not_make_the_answer_miss_and_ctrl_c_still_quits() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('X')));
        assert_eq!(app.key(press(KeyCode::Char('Y'))), Effect::ResetUsage);

        app.key(press(KeyCode::Char('X')));
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.key(ctrl_c), Effect::None);
        assert!(app.quit);
    }

    #[test]
    fn a_ctrl_chord_on_y_does_not_confirm_the_reset() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('X')));

        let effect = app.key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));

        assert_eq!(effect, Effect::None);
        assert!(!app.reset_popup);
    }

    #[test]
    fn escape_closes_the_budget_window_without_a_save_and_keys_do_not_leak() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('b')));
        app.key(press(KeyCode::Char('q')));
        assert!(!app.quit, "typing q must not quit");
        assert_eq!(app.key(press(KeyCode::Esc)), Effect::None);
        assert!(app.budget_popup.is_none());
    }

    #[test]
    fn a_ctrl_chord_does_not_type_into_the_budget_window() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('b')));
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.budget_popup.as_ref().map(|p| p.text.as_str()), Some(""));
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }

    #[test]
    fn capital_b_reads_the_budget_again_like_capital_r() {
        let mut app = loaded();
        assert_eq!(app.key(press(KeyCode::Char('B'))), Effect::Requery);
    }

    #[test]
    fn z_switches_between_local_time_and_utc_and_asks_for_a_new_read() {
        use crate::tz::{self, Zone};
        tz::set(Zone::Utc);
        let mut app = loaded();
        app.cursor = Some(NOW.div_euclid(86_400));

        let first = app.key(press(KeyCode::Char('z')));
        let after_first = tz::current();
        let note_first = app.note.clone();
        let second = app.key(press(KeyCode::Char('z')));

        assert_eq!((first, second), (Effect::Requery, Effect::Requery));
        assert_eq!(after_first, Zone::Local);
        assert_eq!(
            app.cursor, None,
            "a day number from the old zone is dropped"
        );
        assert_eq!(note_first.as_deref(), Some("Showing local time"));
        assert_eq!(tz::current(), Zone::Utc);
        assert_eq!(app.note.as_deref(), Some("Showing UTC"));
    }

    #[test]
    fn a_refresh_keeps_the_selected_row_when_the_rows_reorder() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('3')));
        app.key(press(KeyCode::Down));
        // Models by cost: opus 5, haiku 3, sonnet 1, so haiku is selected.
        assert_eq!(app.selected_key().as_deref(), Some("haiku"));
        app.cursor = Some(NOW.div_euclid(86_400));
        app.spend_view = SpendView::ByModel;
        app.error = Some("old error".into());

        // Haiku now costs the most, so it moves to the top of the table.
        app.refresh(data_with([5.0, 1.0, 9.0]));

        assert_eq!(app.selected_key().as_deref(), Some("haiku"));
        assert_eq!(app.selection(Panel::Models), 0);
        assert_eq!(app.focus, Panel::Models);
        assert_eq!(app.cursor, Some(NOW.div_euclid(86_400)));
        assert_eq!(app.spend_view, SpendView::ByModel);
        assert_eq!(app.sort(Panel::Models), Panel::Models.default_sort());
        assert_eq!(app.error, None);
    }

    #[test]
    fn a_refresh_keeps_the_selection_of_every_table_and_the_filter_draft() {
        let mut app = loaded();
        for panel in ['4', '5', '6'] {
            app.key(press(KeyCode::Char(panel)));
            app.key(press(KeyCode::Down));
        }
        let before: Vec<usize> = PANELS.iter().map(|p| app.selection(*p)).collect();
        app.key(press(KeyCode::Char('/')));
        app.key(press(KeyCode::Char('x')));

        app.refresh(data_with([5.0, 1.0, 3.0]));

        let after: Vec<usize> = PANELS.iter().map(|p| app.selection(*p)).collect();
        assert_eq!(before, after);
        assert!(app.editing);
        assert_eq!(app.draft, "x");
    }

    #[test]
    fn a_refresh_whose_selected_row_is_gone_keeps_the_index_inside_the_rows() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('3')));
        app.key(press(KeyCode::End));
        assert_eq!(app.selection(Panel::Models), 2);
        // A snapshot with no models at all.
        app.refresh(Data::build(
            &[],
            crate::query::LiveView::default(),
            1_788_400_000,
            Range::All,
            "",
        ));
        assert_eq!(app.selection(Panel::Models), 0);
        assert_eq!(app.focus, Panel::Models);
    }

    #[test]
    fn the_models_table_lists_the_models_of_both_periods_and_sorts_by_the_change() {
        use crate::query::LiveView;
        use crate::UsageEvent;
        let ev = |ts: i64, model: &str, cost: f64| UsageEvent {
            event_id: format!("{model}{ts}"),
            timestamp: ts,
            session_id: "s".into(),
            model: model.into(),
            cost_usd: cost,
            ..UsageEvent::default()
        };
        let events = [
            ev(NOW - 60, "opus", 5.0),
            ev(NOW - 86_400 - 60, "opus", 4.0),
            ev(NOW - 86_400 - 30, "haiku", 3.0),
        ];
        let mut app = App::new(Range::Today);
        app.data = Some(Data::build(
            &events,
            LiveView::default(),
            NOW,
            Range::Today,
            "",
        ));
        // Haiku had no cost now but still has a row, and the table is compared.
        assert!(app.compared());
        assert_eq!(app.rows(Panel::Models), 2);
        assert_eq!(app.row_ids(Panel::Models), ["opus", "haiku"]);
        // `s` reaches the previous cost and the change.
        app.key(press(KeyCode::Char('3')));
        app.key(press(KeyCode::Char('s')));
        assert_eq!(app.sort(Panel::Models).col, 4);
        app.key(press(KeyCode::Char('s')));
        assert_eq!(app.sort(Panel::Models).col, 5);
        // Biggest rise first: opus +1, haiku -3.
        assert_eq!(app.row_ids(Panel::Models), ["opus", "haiku"]);
        app.key(press(KeyCode::Char('S')));
        assert_eq!(app.row_ids(Panel::Models), ["haiku", "opus"]);
        // Another range has no comparison and only the first four columns.
        app.range = Range::All;
        app.data = Some(Data::build(
            &events,
            LiveView::default(),
            NOW,
            Range::All,
            "",
        ));
        assert!(!app.compared());
        app.sorts[Panel::Models.index()] = Sort::new(3, true);
        app.key(press(KeyCode::Char('s')));
        assert_eq!(app.sort(Panel::Models).col, 0);
    }

    #[test]
    fn number_keys_and_the_bracket_keys_move_the_focus() {
        let mut app = App::new(Range::All);
        app.key(press(KeyCode::Char('5')));
        assert_eq!(app.focus, Panel::Live);
        app.key(press(KeyCode::Char(']')));
        assert_eq!(app.focus, Panel::Sessions);
        app.key(press(KeyCode::Char(']')));
        assert_eq!(app.focus, Panel::Spend);
        app.key(press(KeyCode::Char('[')));
        assert_eq!(app.focus, Panel::Sessions);
        app.key(press(KeyCode::Char('7')));
        assert_eq!(app.focus, Panel::Sessions);
    }

    #[test]
    fn arrows_move_the_selection_and_stay_inside_the_rows() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('3')));
        app.key(press(KeyCode::Down));
        app.key(press(KeyCode::Char('j')));
        assert_eq!(app.selection(Panel::Models), 2);
        app.key(press(KeyCode::Down));
        assert_eq!(app.selection(Panel::Models), 2);
        app.key(press(KeyCode::Up));
        assert_eq!(app.selection(Panel::Models), 1);
        app.key(press(KeyCode::Home));
        assert_eq!(app.selection(Panel::Models), 0);
        app.key(press(KeyCode::End));
        assert_eq!(app.selection(Panel::Models), 2);
    }

    #[test]
    fn s_sorts_the_focused_table_and_capital_s_reverses_it() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('3')));
        assert_eq!(app.sort(Panel::Models), Sort::new(3, true));
        app.key(press(KeyCode::Char('s')));
        assert_eq!(app.sort(Panel::Models), Sort::new(0, false));
        app.key(press(KeyCode::Char('S')));
        assert_eq!(app.sort(Panel::Models), Sort::new(0, true));
        // Other panels keep their own sort.
        assert_eq!(app.sort(Panel::Projects), Sort::new(3, true));
        // A graph has nothing to sort.
        app.key(press(KeyCode::Char('1')));
        app.key(press(KeyCode::Char('S')));
        assert_eq!(app.sort(Panel::Spend), Panel::Spend.default_sort());
    }

    #[test]
    fn enter_filters_to_the_selected_row_and_again_lifts_it() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('3')));
        app.key(press(KeyCode::Down));
        // Models by cost: opus 5, haiku 3, sonnet 1.
        assert_eq!(app.key(press(KeyCode::Enter)), Effect::Requery);
        assert_eq!(app.filter, "haiku");
        assert_eq!(app.key(press(KeyCode::Enter)), Effect::Requery);
        assert_eq!(app.filter, "");
    }

    #[test]
    fn enter_on_a_session_filters_by_its_id() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('5')));
        assert_eq!(app.key(press(KeyCode::Enter)), Effect::Requery);
        assert!(app.filter.starts_with("sess-"), "{}", app.filter);
        // A graph has no row to pick.
        app.filter.clear();
        app.key(press(KeyCode::Char('1')));
        assert_eq!(app.key(press(KeyCode::Enter)), Effect::None);
    }

    const NOW: i64 = 1_788_400_000;
    const DAY: i64 = 86_400;

    /// An app with one event a day for the last `days` days, in a 100x30 screen.
    fn charted(range: Range, days: i64) -> App {
        use crate::query::LiveView;
        use crate::UsageEvent;
        let events: Vec<UsageEvent> = (0..days)
            .map(|d| UsageEvent {
                event_id: format!("e{d}"),
                timestamp: NOW - d * DAY,
                session_id: "s".into(),
                model: "opus".into(),
                cost_usd: 1.0 + d as f64,
                ..UsageEvent::default()
            })
            .collect();
        let mut app = App::new(range);
        app.data = Some(Data::build(&events, LiveView::default(), NOW, range, ""));
        app
    }

    fn today() -> i64 {
        NOW.div_euclid(DAY)
    }

    fn two_spend_slots() -> App {
        let mut app = charted(Range::LastDays(30), 30);
        app.layout = crate::tui::layout::Layout::parse("A=spend.total,spend.model,tokens").unwrap();
        app.go_tab(0);
        app.focus = Panel::Spend;
        app
    }

    #[test]
    fn tab_walks_each_slot_when_two_pinned_views_share_a_panel() {
        // Arrange
        let mut app = two_spend_slots();
        let mut seen = vec![app.focus_slot()];

        // Act
        for _ in 0..3 {
            app.key(press(KeyCode::Tab));
            seen.push(app.focus_slot());
        }

        // Assert
        use crate::tui::layout::PanelId::{SpendModel, SpendTotal, Tokens};
        assert_eq!(seen, [SpendTotal, SpendModel, Tokens, SpendTotal].map(Some));
        assert_eq!(app.focus, Panel::Spend);
    }

    #[test]
    fn v_on_a_pinned_spend_view_keeps_the_view_and_says_why() {
        // Arrange
        let mut app = two_spend_slots();
        app.key(press(KeyCode::Tab));
        let before = app.spend_view;

        // Act
        app.key(press(KeyCode::Char('v')));

        // Assert
        assert_eq!(app.spend_view, before);
        assert!(app.note.as_deref().is_some_and(|n| n.contains("one view")));
    }

    #[test]
    fn the_daily_table_owns_the_row_and_cursor_keys_only_while_its_slot_has_the_focus() {
        // Arrange
        let mut app = charted(Range::LastDays(30), 30);
        app.layout = crate::tui::layout::Layout::parse("A=spend.daily,spend.total").unwrap();
        app.go_tab(0);
        app.focus = Panel::Spend;
        let on_table = app.is_table(Panel::Spend);
        app.key(press(KeyCode::Left));
        let cursor_on_table = app.cursor;

        // Act
        app.key(press(KeyCode::Tab));
        app.key(press(KeyCode::Left));

        // Assert
        assert!(on_table);
        assert_eq!(cursor_on_table, None);
        assert!(!app.is_table(Panel::Spend));
        assert_eq!(app.cursor, Some(today() - 1));
    }

    #[test]
    fn row_counts_and_the_v_key_follow_the_focused_pinned_breakdown_view() {
        // Arrange
        let mut app = charted(Range::LastDays(30), 30);
        app.layout =
            crate::tui::layout::Layout::parse("A=breakdown.projects,breakdown.health").unwrap();
        app.go_tab(0);
        app.focus = Panel::Projects;
        let projects_rows = app.rows(Panel::Projects);

        // Act
        app.key(press(KeyCode::Tab));
        let health_rows = app.rows(Panel::Projects);
        app.key(press(KeyCode::Char('v')));

        // Assert
        assert_eq!(health_rows, panels::HEALTH_ROWS);
        assert_ne!(projects_rows, health_rows);
        assert_eq!(app.projects_view, ProjectsView::default());
        assert!(app.note.as_deref().is_some_and(|n| n.contains("one view")));
    }

    #[test]
    fn the_number_key_focuses_the_first_slot_of_its_panel() {
        // Arrange
        let mut app = two_spend_slots();
        app.key(press(KeyCode::Tab));

        // Act
        app.key(press(KeyCode::Char('1')));

        // Assert
        assert_eq!(
            app.focus_slot(),
            Some(crate::tui::layout::PanelId::SpendTotal)
        );
    }

    #[test]
    fn the_chart_cursor_moves_on_a_tab_that_draws_spend_only_as_a_pinned_word() {
        let mut app = charted(Range::LastDays(30), 30);
        app.layout = crate::tui::layout::Layout::parse("A=spend.total,tokens").unwrap();
        app.go_tab(0);
        app.focus = Panel::Spend;

        app.key(press(KeyCode::Left));

        assert_eq!(app.cursor, Some(today() - 1));
    }

    #[test]
    fn left_and_right_move_the_chart_cursor_one_day_and_stop_at_the_ends() {
        let mut app = charted(Range::LastDays(30), 30);
        assert_eq!(app.cursor, None);
        // From no cursor, the newest day is where it starts, so left goes back.
        app.key(press(KeyCode::Left));
        assert_eq!(app.cursor, Some(today() - 1));
        app.key(press(KeyCode::Left));
        assert_eq!(app.cursor, Some(today() - 2));
        app.key(press(KeyCode::Right));
        assert_eq!(app.cursor, Some(today() - 1));
        app.key(press(KeyCode::Right));
        app.key(press(KeyCode::Right));
        assert_eq!(app.cursor, Some(today()), "stops on the newest day");
        for _ in 0..40 {
            app.key(press(KeyCode::Left));
        }
        assert_eq!(app.cursor, Some(today() - 29), "stops on the oldest day");
    }

    #[test]
    fn h_and_l_move_the_chart_cursor_like_left_and_right() {
        let mut app = charted(Range::LastDays(30), 30);
        app.key(press(KeyCode::Char('h')));
        assert_eq!(app.cursor, Some(today() - 1));
        app.key(press(KeyCode::Char('h')));
        assert_eq!(app.cursor, Some(today() - 2));
        app.key(press(KeyCode::Char('l')));
        assert_eq!(app.cursor, Some(today() - 1));
        assert!(app.help.is_none(), "h no longer opens the help");
    }

    // NOW is Thursday 2026-09-03, so the Monday week has bars from today - 3
    // to today + 3 and the cursor starts on today, not on the last bar.
    #[test]
    fn the_cursor_starts_on_today_when_the_week_has_days_to_come() {
        let mut app = charted(Range::ThisWeek(WeekStart::Monday), 7);
        app.key(press(KeyCode::Left));
        assert_eq!(app.cursor, Some(today() - 1));
        let mut app = charted(Range::ThisWeek(WeekStart::Monday), 7);
        app.key(press(KeyCode::Right));
        assert_eq!(app.cursor, Some(today() + 1));
    }

    #[test]
    fn home_and_end_jump_the_cursor_when_a_chart_has_the_focus() {
        let mut app = charted(Range::LastDays(30), 30);
        app.key(press(KeyCode::Char('1')));
        app.key(press(KeyCode::Home));
        assert_eq!(app.cursor, Some(today() - 29));
        app.key(press(KeyCode::End));
        assert_eq!(app.cursor, Some(today()));
        // On a table they still move the row, not the cursor.
        app.cursor = None;
        app.key(press(KeyCode::Char('3')));
        app.key(press(KeyCode::End));
        assert_eq!(app.cursor, None);
    }

    #[test]
    fn a_wide_range_moves_the_cursor_one_bar_which_is_several_days() {
        let mut app = charted(Range::LastDays(90), 90);
        let plot = app.plot(Panel::Spend).expect("a chart");
        let per = plot.geo.per as i64;
        assert!(per > 1, "90 days do not fit one column each at 100 wide");
        app.key(press(KeyCode::Left));
        let first = app.cursor.unwrap();
        app.key(press(KeyCode::Left));
        assert_eq!(first - app.cursor.unwrap(), per);
    }

    #[test]
    fn the_cursor_resets_when_the_range_changes_and_survives_a_filter() {
        let mut app = charted(Range::LastDays(30), 30);
        app.key(press(KeyCode::Left));
        assert!(app.cursor.is_some());
        app.filter = "opus".into();
        app.key(press(KeyCode::Char('c')));
        assert!(app.cursor.is_some(), "a filter keeps the day");
        app.key(press(KeyCode::Char('w')));
        assert_eq!(app.cursor, None);
    }

    #[test]
    fn the_cursor_keys_do_nothing_without_data_or_on_a_tiny_screen() {
        let mut app = App::new(Range::All);
        app.key(press(KeyCode::Left));
        assert_eq!(app.cursor, None);
        let mut small = charted(Range::LastDays(30), 30);
        small.area = Rect::new(0, 0, 50, 10);
        small.key(press(KeyCode::Left));
        assert_eq!(small.cursor, None);
    }

    #[test]
    fn v_steps_the_spend_panel_through_total_by_model_and_daily() {
        let mut app = App::new(Range::All);
        app.key(press(KeyCode::Char('1')));
        assert_eq!(app.spend_view, SpendView::Total);
        let mut seen = Vec::new();
        for _ in 0..3 {
            assert_eq!(app.key(press(KeyCode::Char('v'))), Effect::None);
            seen.push(app.spend_view);
        }
        assert_eq!(
            seen,
            [SpendView::ByModel, SpendView::Daily, SpendView::Total]
        );
    }

    fn daily() -> App {
        let mut app = charted(Range::LastDays(30), 30);
        app.spend_view = SpendView::Daily;
        app.key(press(KeyCode::Char('1')));
        app
    }

    #[test]
    fn the_daily_table_has_a_row_for_each_day_and_moves_its_selection() {
        let mut app = daily();
        assert_eq!(app.rows(Panel::Spend), 30);
        assert!(app.is_table(Panel::Spend) && !app.is_table(Panel::Tokens));
        app.key(press(KeyCode::Down));
        app.key(press(KeyCode::Down));
        assert_eq!(app.selection(Panel::Spend), 2);
        app.key(press(KeyCode::End));
        assert_eq!(app.selection(Panel::Spend), 29);
        // Only the chart views are drawn as charts.
        app.spend_view = SpendView::Total;
        assert_eq!(app.rows(Panel::Spend), 0);
    }

    #[test]
    fn s_sorts_the_daily_table_but_not_the_chart_views() {
        let mut app = daily();
        assert_eq!(app.sort(Panel::Spend), Sort::new(0, true));
        app.key(press(KeyCode::Char('s')));
        assert_eq!(app.sort(Panel::Spend), Sort::new(1, true));
        app.key(press(KeyCode::Char('S')));
        assert_eq!(app.sort(Panel::Spend), Sort::new(1, false));
        app.spend_view = SpendView::ByModel;
        app.key(press(KeyCode::Char('s')));
        app.key(press(KeyCode::Char('S')));
        assert_eq!(app.sort(Panel::Spend), Sort::new(1, false), "unchanged");
    }

    #[test]
    fn the_cursor_keys_leave_the_daily_table_alone() {
        let mut app = daily();
        app.key(press(KeyCode::Left));
        assert_eq!(app.cursor, None);
    }

    #[test]
    fn enter_on_a_day_row_applies_no_filter() {
        let mut app = daily();
        app.key(press(KeyCode::Char('1')));
        for _ in 0..3 {
            app.key(press(KeyCode::Down));
        }
        assert_eq!((app.focus, app.selection(Panel::Spend)), (Panel::Spend, 3));
        assert_eq!(app.key(press(KeyCode::Enter)), Effect::None);
    }

    #[test]
    fn an_unbound_key_does_nothing() {
        let mut app = App::new(Range::All);
        assert_eq!(app.key(press(KeyCode::Char('y'))), Effect::None);
        assert!(!app.quit);
    }

    #[test]
    fn v_changes_only_the_view_of_the_focused_panel() {
        let mut app = loaded();
        // Models, Tokens, Live and Sessions have no view: v does nothing.
        for key in ['2', '3', '5', '6'] {
            app.key(press(KeyCode::Char(key)));
            app.key(press(KeyCode::Char('v')));
            assert_eq!(app.spend_view, SpendView::Total, "{key}");
            assert_eq!(app.projects_view, ProjectsView::Projects, "{key}");
        }
        app.key(press(KeyCode::Char('4')));
        app.key(press(KeyCode::Char('v')));
        assert_eq!(app.projects_view, ProjectsView::Agents);
        assert_eq!(app.spend_view, SpendView::Total);
        app.key(press(KeyCode::Char('1')));
        app.key(press(KeyCode::Char('v')));
        assert_eq!(app.spend_view, SpendView::ByModel);
        assert_eq!(app.projects_view, ProjectsView::Agents);
    }

    #[test]
    fn question_mark_opens_the_help_and_any_other_key_closes_it() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('?')));
        assert_eq!(app.help, Some(0));
        // A key that would change the range only closes the window.
        assert_eq!(app.key(press(KeyCode::Char('a'))), Effect::None);
        assert_eq!((app.help, app.range), (None, Range::All));
        for close in [KeyCode::Esc, KeyCode::Char('h'), KeyCode::Char('x')] {
            let mut app = loaded();
            app.key(press(KeyCode::Char('?')));
            app.key(press(close));
            assert_eq!(app.help, None, "{close:?}");
            assert!(!app.quit, "{close:?}");
        }
    }

    #[test]
    fn the_help_scrolls_with_the_arrows_and_stops_at_its_ends() {
        let mut app = loaded();
        app.area = Rect::new(0, 0, 80, 24);
        app.key(press(KeyCode::Char('?')));
        app.key(press(KeyCode::Up));
        assert_eq!(app.help, Some(0));
        app.key(press(KeyCode::Down));
        assert_eq!(app.help, Some(1));
        app.key(press(KeyCode::End));
        let last = keys::help_lines().len() - app.help_page();
        assert_eq!(app.help, Some(last));
        app.key(press(KeyCode::Down));
        assert_eq!(app.help, Some(last));
        app.key(press(KeyCode::Home));
        assert_eq!(app.help, Some(0));
    }

    #[test]
    fn the_help_scrolls_with_j_and_k() {
        let mut app = loaded();
        app.area = Rect::new(0, 0, 80, 24);
        app.key(press(KeyCode::Char('?')));
        app.key(press(KeyCode::Char('j')));
        assert_eq!(app.help, Some(1));
        app.key(press(KeyCode::Char('k')));
        assert_eq!(app.help, Some(0));
    }

    #[test]
    fn a_refresh_while_the_help_is_open_keeps_it_and_the_selection() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('3')));
        app.key(press(KeyCode::Down));
        app.key(press(KeyCode::Char('?')));
        app.key(press(KeyCode::Down));
        app.refresh(data_with([5.0, 1.0, 3.0]));
        assert_eq!(app.help, Some(1));
        assert_eq!(app.selection(Panel::Models), 1);
        assert_eq!(app.focus, Panel::Models);
    }

    #[test]
    fn ctrl_c_quits_while_the_help_is_open() {
        let mut app = loaded();
        app.key(press(KeyCode::Char('?')));
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }

    #[test]
    fn r_capital_asks_for_a_fresh_read_and_p_opens_sessions() {
        let mut app = loaded();
        assert_eq!(app.key(press(KeyCode::Char('R'))), Effect::Requery);
        assert_eq!(app.key(press(KeyCode::Char('p'))), Effect::Sessions);
    }

    /// Every key the handler reacts to is in the key table, so the help and
    /// the footer list it. The probe presses every key a keyboard offers on an
    /// app with data, a filter and each focus, and a key that changes anything
    /// must have a binding.
    #[test]
    fn every_key_that_does_something_is_in_the_help_table() {
        let mut keys_to_try: Vec<KeyCode> =
            (0x20u8..0x7f).map(|b| KeyCode::Char(b as char)).collect();
        keys_to_try.extend([
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Backspace,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Delete,
            KeyCode::Insert,
        ]);
        for code in keys_to_try {
            for focus in PANELS {
                let mut app = charted(Range::LastDays(30), 30);
                app.filter = "opus".into();
                app.focus = focus;
                let before = format!("{app:?}");
                let effect = app.key(press(code));
                let changed = effect != Effect::None || format!("{app:?}") != before;
                if changed {
                    assert!(
                        keys::lookup(code, false).is_some(),
                        "{code:?} does something on {focus:?} but is not in the key table"
                    );
                }
            }
        }
    }

    #[test]
    fn every_command_has_a_binding_and_every_binding_is_in_the_help() {
        use keys::Cmd::*;
        for cmd in [
            Quit,
            NextTab,
            PrevTab,
            GoTab,
            Theme,
            MaskAccount,
            Today,
            Week,
            LastWeek,
            Month,
            LastMonth,
            All,
            CycleRange,
            Filter,
            ClearFilter,
            FocusPanel,
            NextPanel,
            PrevPanel,
            CursorLeft,
            CursorRight,
            First,
            Last,
            Down,
            Up,
            PageDown,
            PageUp,
            NextView,
            Sort,
            SortFlip,
            Pick,
            Sessions,
            EditLayout,
            Help,
            Refresh,
            Zone,
            ResetUsage,
        ] {
            assert!(keys::BINDINGS.iter().any(|b| b.cmd == cmd), "{cmd:?}");
        }
        let lines = keys::help_lines();
        for b in keys::BINDINGS {
            assert!(
                lines.iter().any(|l| matches!(l,
                    keys::HelpLine::Key { shown, .. } if *shown == b.shown)),
                "{} missing from the help",
                b.shown
            );
            for code in b.keys {
                assert_eq!(
                    keys::lookup(*code, b.ctrl).map(|x| x.cmd),
                    Some(b.cmd),
                    "{code:?}"
                );
            }
        }
    }

    #[test]
    fn the_screen_setup_saved_from_one_app_is_restored_into_a_fresh_one() {
        let mut first = App::new(Range::All);
        first.layout = super::super::layout::Layout::default();
        first.tab = 1;
        first.spend_view = SpendView::Daily;
        first.projects_view = ProjectsView::Agents;
        first.sorts[Panel::Models.index()] = Sort::new(5, false);
        first.filter = "alpha".into();
        let text = first.ui_state().to_text();

        let mut second = App::new(Range::All);
        second.layout = super::super::layout::Layout::default();
        second.apply_ui_state(&super::super::ui_state::UiState::parse(&text));

        assert_eq!(second.tab, 1);
        assert_eq!(second.spend_view, SpendView::Daily);
        assert_eq!(second.projects_view, ProjectsView::Agents);
        assert_eq!(second.sorts[Panel::Models.index()], Sort::new(5, false));
        assert_eq!(second.filter, "alpha");
    }

    #[test]
    fn a_saved_tab_that_no_longer_exists_leaves_the_first_tab_selected() {
        let mut app = App::new(Range::All);
        let state = super::super::ui_state::UiState {
            tab: Some("Gone".into()),
            ..Default::default()
        };

        app.apply_ui_state(&state);

        assert_eq!(app.tab, 0);
    }

    #[test]
    fn a_saved_sort_on_a_column_the_panel_lacks_keeps_the_default_sort() {
        let mut app = App::new(Range::All);
        let mut sorts = vec![None; 6];
        sorts[Panel::Live.index()] = Some(Sort::new(0, true));
        let state = super::super::ui_state::UiState {
            sorts,
            ..Default::default()
        };

        app.apply_ui_state(&state);

        assert_eq!(app.sort(Panel::Live), Panel::Live.default_sort());
    }
}
