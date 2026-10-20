//! Interactive terminal dashboard.
//!
//! The dashboard is a pure view over an already-built [`ScanResult`] and
//! [`Report`]: it never scans, never writes and never reads the clock. Both
//! inputs are borrowed for the lifetime of [`App`], so the event loop only moves
//! an index around.
//!
//! Two things are deliberate:
//!
//! - The file list is built from `ScanResult.files`, **not** from
//!   `Report.top`. The report keeps 20 rows for its table; browsing beyond the
//!   top 20 is the whole point of the dashboard, so the list reuses the same
//!   measurement (function-level CCN, generated/vendored files excluded) and
//!   recomputes each file's risk with the same size-weighted penalty the report
//!   uses.
//! - Rendering is deterministic for a given [`App`] state. There are no
//!   timers, no colours used as the only signal, and no state hidden in the
//!   renderer other than the scroll offset, which is derived from the selection
//!   and the current height.

use std::io::{self, Stdout};
use std::panic::AssertUnwindSafe;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::tty::IsTty;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};

use crate::ccn::types::{CcnSummary, CcnTier, FunctionCcn};
use crate::metrics::{penalty, Report};
use crate::model::ScanResult;

/// Below this the dashboard shows a single "too small" line instead of trying
/// to lay out panels it cannot fit.
const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 8;

/// Run the dashboard. Returns `Ok(())` when the user quits.
///
/// Fails when stdout is not a TTY so the caller can fall back to the plain
/// terminal block instead of painting escape codes into a pipe.
pub fn run(result: &ScanResult, report: &Report) -> std::io::Result<()> {
    if !is_interactive() {
        return Err(io::Error::other(
            "ccn: the interactive dashboard needs a terminal (stdout is not a TTY)",
        ));
    }

    let mut app = App::new(result, report);
    // The guard restores the terminal on every path: normal return, I/O error
    // and panic. `drop(guard)` happens *before* the panic is resumed, so a
    // crashing dashboard never leaves the user's shell in raw mode.
    let outcome = {
        let mut guard = TerminalGuard::enter()?;
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            event_loop(&mut guard.terminal, &mut app)
        }));
        drop(guard);
        outcome
    };

    match outcome {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Whether a dashboard can be shown on this terminal.
pub fn is_interactive() -> bool {
    std::io::stdout().is_tty()
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App<'_>,
) -> io::Result<()> {
    loop {
        terminal.draw(|frame| render(frame, app))?;
        if app.quit {
            return Ok(());
        }
        if let Event::Key(key) = event::read()? {
            app.handle_key(key);
        }
    }
}

/// Owns raw mode and the alternate screen.
///
/// `Drop` is the single restoration point, which is why `run` only has to make
/// sure the guard is dropped before propagating anything.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;

        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }

        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
                let _ = disable_raw_mode();
                Err(error)
            }
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best effort: each step is independent so one failure cannot skip the
        // rest of the restoration.
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen, Show);
        let _ = self.terminal.show_cursor();
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Which chart the left panel shows. `Tab` cycles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeftTab {
    Ccn,
    Loc,
}

impl LeftTab {
    fn next(self) -> Self {
        match self {
            LeftTab::Ccn => LeftTab::Loc,
            LeftTab::Loc => LeftTab::Ccn,
        }
    }

    fn title(self) -> &'static str {
        match self {
            LeftTab::Ccn => "CCN per function",
            LeftTab::Loc => "LOC per file",
        }
    }

    fn number(self) -> usize {
        match self {
            LeftTab::Ccn => 1,
            LeftTab::Loc => 2,
        }
    }
}

/// Ordering keys for the file list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Risk,
    Loc,
    MaxCcn,
    Over10,
    Path,
}

impl SortKey {
    fn next(self) -> Self {
        match self {
            SortKey::Risk => SortKey::Loc,
            SortKey::Loc => SortKey::MaxCcn,
            SortKey::MaxCcn => SortKey::Over10,
            SortKey::Over10 => SortKey::Path,
            SortKey::Path => SortKey::Risk,
        }
    }

    /// Numeric columns are most useful worst-first; paths read A-Z.
    fn default_desc(self) -> bool {
        self != SortKey::Path
    }

    fn label(self) -> &'static str {
        match self {
            SortKey::Risk => "risk",
            SortKey::Loc => "loc",
            SortKey::MaxCcn => "max ccn",
            SortKey::Over10 => "over 10",
            SortKey::Path => "path",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlay {
    None,
    Detail,
    Help,
}

/// One measured file. `index` points back into `ScanResult.files` so the detail
/// overlay can read the function list without cloning it.
#[derive(Debug, Clone)]
struct FileRow {
    index: usize,
    path: String,
    lang: String,
    loc: u32,
    loc_total: u32,
    functions: usize,
    ccn_sum: u32,
    ccn_max: u32,
    p90: u32,
    over_10: usize,
    over_20: usize,
    over_50: usize,
    risk: f64,
    tier: CcnTier,
}

/// Dashboard state. Every visual transition goes through this struct, which is
/// what lets the tests drive keys without a real terminal.
pub struct App<'a> {
    result: &'a ScanResult,
    report: &'a Report,
    /// All files with function-level CCN, before filtering and sorting.
    files: Vec<FileRow>,
    /// Indices into `files`, filtered and ordered for display.
    view: Vec<usize>,
    selected: usize,
    /// Scroll offset, clamped by the renderer against the visible height.
    offset: usize,
    sort: SortKey,
    sort_desc: bool,
    filter: Option<String>,
    /// `Some` while the user is typing a filter; the text is live-applied.
    filter_input: Option<String>,
    left_tab: LeftTab,
    overlay: Overlay,
    quit: bool,
}

impl<'a> App<'a> {
    pub fn new(result: &'a ScanResult, report: &'a Report) -> Self {
        let files = result
            .files
            .iter()
            .enumerate()
            .filter_map(|(index, file)| {
                let summary = file.ccn.as_ref()?;
                if summary.tier == CcnTier::Heuristic || file.is_generated || file.is_vendored {
                    return None;
                }
                Some(FileRow {
                    index,
                    path: file.path.clone(),
                    lang: file.lang.clone(),
                    loc: file.loc as u32,
                    loc_total: file.loc_total as u32,
                    functions: summary.functions,
                    ccn_sum: summary.sum,
                    ccn_max: summary.max,
                    p90: summary.p90,
                    over_10: summary.over_10,
                    over_20: summary.over_20,
                    over_50: summary.over_50,
                    risk: file_risk(summary),
                    tier: summary.tier,
                })
            })
            .collect();

        let mut app = Self {
            result,
            report,
            files,
            view: Vec::new(),
            selected: 0,
            offset: 0,
            sort: SortKey::Risk,
            sort_desc: true,
            filter: None,
            filter_input: None,
            left_tab: LeftTab::Ccn,
            overlay: Overlay::None,
            quit: false,
        };
        app.rebuild();
        app
    }

    /// Filter that is currently narrowing the list, if any.
    fn active_filter(&self) -> Option<&str> {
        match &self.filter_input {
            Some(text) if !text.is_empty() => Some(text.as_str()),
            Some(_) => None,
            None => self.filter.as_deref(),
        }
    }

    fn selected_row(&self) -> Option<&FileRow> {
        self.view
            .get(self.selected)
            .map(|&index| &self.files[index])
    }

    /// Recompute the visible order. Cheap enough to run on every keystroke:
    /// a repository is a few thousand files, and this is a single sort.
    fn rebuild(&mut self) {
        let needle = self.active_filter().map(str::to_lowercase);
        let mut view: Vec<usize> = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, row)| match &needle {
                Some(needle) => row.path.to_lowercase().contains(needle),
                None => true,
            })
            .map(|(index, _)| index)
            .collect();

        let key = self.sort;
        let desc = self.sort_desc;
        view.sort_by(|&a, &b| {
            let ordering = compare_rows(&self.files[a], &self.files[b], key);
            if desc {
                ordering.reverse()
            } else {
                ordering
            }
        });

        self.view = view;
        self.offset = 0;
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        if self.view.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.view.len() {
            self.selected = self.view.len() - 1;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.view.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.view.len() - 1;
        let next = self.selected.saturating_add_signed(delta).min(last);
        self.selected = next;
    }

    fn select_first(&mut self) {
        self.selected = 0;
    }

    fn select_last(&mut self) {
        self.selected = self.view.len().saturating_sub(1);
    }

    /// `s`: advance the sort key. Repeating `s` on the last key (`path`) toggles
    /// its direction before wrapping around.
    fn cycle_sort(&mut self) {
        if self.sort == SortKey::Path && !self.sort_desc {
            self.sort_desc = true;
        } else {
            self.sort = self.sort.next();
            self.sort_desc = self.sort.default_desc();
        }
        self.rebuild();
    }

    fn toggle_direction(&mut self) {
        self.sort_desc = !self.sort_desc;
        self.rebuild();
    }

    fn begin_filter(&mut self) {
        self.filter_input = Some(self.filter.clone().unwrap_or_default());
    }

    fn apply_filter(&mut self) {
        let typed = self.filter_input.take().unwrap_or_default();
        let typed = typed.trim().to_string();
        self.filter = if typed.is_empty() { None } else { Some(typed) };
        self.selected = 0;
        self.rebuild();
    }

    fn cancel_filter(&mut self) {
        self.filter_input = None;
        self.selected = 0;
        self.rebuild();
    }

    /// Apply one key press. Test entry point; the event loop calls exactly this.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }

        match self.overlay {
            Overlay::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?')
                ) {
                    self.overlay = Overlay::None;
                }
                return;
            }
            Overlay::Detail => {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('d') => {
                        self.overlay = Overlay::None
                    }
                    KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
                    KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
                    _ => {}
                }
                return;
            }
            Overlay::None => {}
        }

        if self.filter_input.is_some() {
            match key.code {
                KeyCode::Esc => self.cancel_filter(),
                KeyCode::Enter => self.apply_filter(),
                KeyCode::Backspace => {
                    if let Some(buffer) = &mut self.filter_input {
                        buffer.pop();
                    }
                    self.selected = 0;
                    self.rebuild();
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(buffer) = &mut self.filter_input {
                        buffer.push(ch);
                    }
                    self.selected = 0;
                    self.rebuild();
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.select_last(),
            KeyCode::Tab => self.left_tab = self.left_tab.next(),
            KeyCode::Char('1') => self.left_tab = LeftTab::Ccn,
            KeyCode::Char('2') => self.left_tab = LeftTab::Loc,
            KeyCode::Char('s') => self.cycle_sort(),
            KeyCode::Char('S') => self.toggle_direction(),
            KeyCode::Char('/') => self.begin_filter(),
            KeyCode::Enter | KeyCode::Char('d') => {
                if self.selected_row().is_some() {
                    self.overlay = Overlay::Detail;
                }
            }
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            _ => {}
        }
    }
}

/// Same size-weighted penalty as `metrics::file_risk`, computed here because the
/// report only exposes it for its top 20 rows.
fn file_risk(summary: &CcnSummary) -> f64 {
    let mut mass = 0.0;
    let mut lines = 0u64;
    for function in &summary.detail {
        let weight = f64::from(function.nloc.max(1) as u32);
        mass += weight * penalty(function.ccn);
        lines += function.nloc.max(1) as u64;
    }
    if lines == 0 {
        0.0
    } else {
        mass / lines as f64
    }
}

fn compare_rows(a: &FileRow, b: &FileRow, key: SortKey) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let primary = match key {
        SortKey::Risk => a
            .risk
            .partial_cmp(&b.risk)
            .unwrap_or(Ordering::Equal)
            // A file with no functions has risk 0 by construction; it must not
            // outrank a real but simple file just because of its path.
            .then_with(|| (a.functions > 0).cmp(&(b.functions > 0)))
            .then_with(|| a.ccn_max.cmp(&b.ccn_max)),
        SortKey::Loc => a.loc.cmp(&b.loc).then_with(|| a.ccn_max.cmp(&b.ccn_max)),
        SortKey::MaxCcn => a
            .ccn_max
            .cmp(&b.ccn_max)
            .then_with(|| a.ccn_sum.cmp(&b.ccn_sum)),
        SortKey::Over10 => a
            .over_10
            .cmp(&b.over_10)
            .then_with(|| a.ccn_max.cmp(&b.ccn_max)),
        SortKey::Path => Ordering::Equal,
    };
    primary.then_with(|| a.path.cmp(&b.path))
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Draw the whole dashboard. Kept free of terminal setup so tests can point a
/// `TestBackend` at it.
pub fn render(frame: &mut Frame, app: &mut App<'_>) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new("terminal too small (need at least 40x8)"),
            area,
        );
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    render_header(frame, app, rows[0]);

    let body =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).split(rows[1]);
    render_left(frame, app, body[0]);
    render_right(frame, app, body[1]);

    frame.render_widget(Paragraph::new(footer_text(area.width as usize)), rows[2]);

    match app.overlay {
        Overlay::Detail => render_detail(frame, app, area),
        Overlay::Help => render_help(frame, area),
        Overlay::None => {}
    }
}

fn footer_text(width: usize) -> String {
    let hint = "j/k move · Tab panel · s sort · / filter · Enter detail · ? help · q quit";
    truncate_right(hint, width)
}

fn render_header(frame: &mut Frame, app: &App<'_>, area: Rect) {
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let mut left = format!("ccn · {}", app.report.root.replace('\\', "/"));
    if let Some(filter) = app.active_filter() {
        left.push_str("  [filter: ");
        left.push_str(filter);
        left.push(']');
    }

    let mut right = match &app.report.health {
        Some(health) => format!("Health {:.1} / 100  grade {}", health.score, health.grade),
        None => "Health n/a".to_string(),
    };
    let counts = format!(
        " · {} files · {} fns",
        thousands(app.report.files_measured),
        thousands(app.report.stats.functions_total)
    );
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    if left_len + right_len + counts.chars().count() + 4 <= width {
        right.push_str(&counts);
    }

    let left_len = left.chars().count();
    let right_len = right.chars().count();
    let line = if left_len + right_len + 2 <= width {
        format!("{left}{}{right}", " ".repeat(width - left_len - right_len))
    } else if right_len + 1 < width {
        let allowed = width - right_len - 1;
        format!("{} {}", truncate_left(&left, allowed), right)
    } else {
        truncate_left(&right, width)
    };

    frame.render_widget(Paragraph::new(line), inner);
}

fn render_left(frame: &mut Frame, app: &App<'_>, area: Rect) {
    let title = format!(" {} [{}/2] ", app.left_tab.title(), app.left_tab.number());
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let histogram = match app.left_tab {
        LeftTab::Ccn => &app.report.ccn,
        LeftTab::Loc => &app.report.loc,
    };
    if histogram.is_empty() {
        frame.render_widget(Paragraph::new("no data"), inner);
        return;
    }

    let width = inner.width as usize;
    let label_width = if app.left_tab == LeftTab::Loc { 11 } else { 6 };
    let max_share = histogram
        .bins
        .iter()
        .map(|bin| bin.share_count)
        .fold(0.0_f64, f64::max);

    let mut lines: Vec<Line> = Vec::new();
    for bin in histogram.bins.iter().take(inner.height as usize) {
        let mut line = String::new();
        line.push_str(&pad(&bin.label, label_width));
        line.push_str(&format!("{:>7}", thousands(bin.count)));
        line.push_str(&format!("{:>5.1}%", bin.share_count));

        let used = label_width + 7 + 7;
        if width > used + 1 {
            let bar_width = width - used - 1;
            let share = if max_share > 0.0 {
                bin.share_count / max_share
            } else {
                0.0
            };
            let mut cells = (share * bar_width as f64).round() as usize;
            if bin.share_count > 0.0 {
                cells = cells.max(1);
            }
            cells = cells.min(bar_width);
            line.push(' ');
            for _ in 0..cells {
                line.push('█');
            }
        }
        lines.push(Line::from(line));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_right(frame: &mut Frame, app: &mut App<'_>, area: Rect) {
    let arrow = if app.sort_desc { '↓' } else { '↑' };
    let title = format!(
        " Files ({})   sort: {} {} ",
        app.view.len(),
        app.sort.label(),
        arrow
    );
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if app.view.is_empty() {
        let message = match app.active_filter() {
            Some(filter) => format!("no file matches \"{filter}\""),
            None => "no measured files with function-level CCN".to_string(),
        };
        frame.render_widget(Paragraph::new(message), inner);
        return;
    }

    let width = inner.width as usize;
    let with_stats = width >= 40;
    let path_width = if with_stats {
        width.saturating_sub(31)
    } else {
        width.saturating_sub(4)
    };

    let header_height = 1;
    let rows_height = (inner.height as usize).saturating_sub(header_height);
    if rows_height == 0 {
        return;
    }

    if app.selected < app.offset {
        app.offset = app.selected;
    }
    if app.selected >= app.offset + rows_height {
        app.offset = app.selected + 1 - rows_height;
    }
    let max_offset = app.view.len().saturating_sub(rows_height);
    if app.offset > max_offset {
        app.offset = max_offset;
    }

    let mut lines: Vec<Line> = Vec::with_capacity(rows_height + 1);
    let header = if with_stats {
        format!(
            "    {:<path_width$}{:>7}{:>8}{:>6}{:>6}",
            "path",
            "loc",
            "max",
            ">10",
            "risk",
            path_width = path_width
        )
    } else {
        format!("    {:<path_width$}", "path", path_width = path_width)
    };
    lines.push(Line::from(Span::styled(
        truncate_right(&header, width),
        Style::default().add_modifier(Modifier::DIM),
    )));

    let end = (app.offset + rows_height).min(app.view.len());
    for position in app.offset..end {
        let Some(&index) = app.view.get(position) else {
            continue;
        };
        let row = &app.files[index];
        let marker = if position == app.selected { '>' } else { ' ' };
        let tag = tier_tag(row.tier);
        let path = truncate_left(&row.path, path_width);
        let mut text = format!("{} {} {}", marker, tag, pad(&path, path_width));
        if with_stats {
            text.push_str(&format!(
                "{:>7}{:>8}{:>6}{:>6}",
                thousands(row.loc as usize),
                row.ccn_max,
                row.over_10,
                (row.risk * 100.0).round() as u32
            ));
        }
        text = truncate_right(&text, width);

        let mut style = Style::default();
        if position == app.selected {
            style = style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
            text = pad(&text, width);
        }
        lines.push(Line::from(Span::styled(text, style)));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_detail(frame: &mut Frame, app: &App<'_>, area: Rect) {
    let Some(row) = app.selected_row() else {
        return;
    };
    let area = centered(area, 92, 82);
    if area.width < 4 || area.height < 4 {
        return;
    }
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" file detail · Esc closes ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        truncate_left(&row.path, width),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(
        "{} · {} loc ({} total) · {} functions · tier {}",
        row.lang,
        thousands(row.loc as usize),
        thousands(row.loc_total as usize),
        thousands(row.functions),
        row.tier.as_str()
    )));
    lines.push(Line::from(format!(
        "ccn sum {} · max {} · p90 {}",
        row.ccn_sum, row.ccn_max, row.p90
    )));
    lines.push(Line::from(format!(
        "over 10: {}   over 20: {}   over 50: {}   risk {}",
        row.over_10,
        row.over_20,
        row.over_50,
        (row.risk * 100.0).round() as u32
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "top functions by ccn",
        Style::default().add_modifier(Modifier::BOLD),
    )));

    let mut functions: Vec<&FunctionCcn> = app
        .result
        .files
        .get(row.index)
        .and_then(|file| file.ccn.as_ref())
        .map(|summary| summary.detail.iter().collect())
        .unwrap_or_default();
    functions.sort_by(|a, b| {
        b.ccn
            .cmp(&a.ccn)
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.name.cmp(&b.name))
    });

    if functions.is_empty() {
        lines.push(Line::from("no functions measured"));
    } else {
        lines.push(Line::from(Span::styled(
            format!("{:>4}  {:<30}{:>6}  {:>5}", "ccn", "name", "line", "nloc"),
            Style::default().add_modifier(Modifier::DIM),
        )));
        for function in functions.iter().take(10) {
            lines.push(Line::from(format!(
                "{:>4}  {:<30}{:>6}  {:>5}",
                function.ccn,
                truncate_right(&function.name, 30),
                function.line,
                function.nloc
            )));
        }
    }

    lines.truncate(inner.height as usize);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_help(frame: &mut Frame, area: Rect) {
    let area = centered(area, 76, 76);
    if area.width < 4 || area.height < 4 {
        return;
    }
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" keys · Esc closes ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let entries = [
        ("j / k", "move selection"),
        ("g / G", "first / last file"),
        ("Tab", "switch chart: CCN per function, LOC per file"),
        ("s", "cycle sort: risk, loc, max ccn, over 10, path"),
        ("S", "reverse the sort direction"),
        ("/", "filter paths (Enter applies, Esc cancels)"),
        ("Enter / d", "file detail"),
        ("?", "this help"),
        ("q / Esc", "quit"),
    ];

    let mut lines: Vec<Line> = Vec::with_capacity(entries.len());
    for (keys, description) in entries {
        lines.push(Line::from(format!(" {:<12}{}", keys, description)));
    }
    lines.truncate(inner.height as usize);
    frame.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Tier tag: only non-`Exact` files get a letter, so the common case is blank.
fn tier_tag(tier: CcnTier) -> char {
    match tier {
        CcnTier::Exact => ' ',
        CcnTier::BestEffort => 'B',
        CcnTier::Heuristic => '?',
    }
}

/// Thousands separator; a local copy keeps the dashboard independent of the
/// plain renderer's formatting choices.
fn thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Keep the tail of a string, which is the informative half of a path.
fn truncate_left(text: &str, max: usize) -> String {
    let length = text.chars().count();
    if length <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let skip = length - (max - 1);
    let tail: String = text.chars().skip(skip).collect();
    format!("…{tail}")
}

/// Keep the head of a string.
fn truncate_right(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let length = text.chars().count();
    if length <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max - 1).collect();
    format!("{head}…")
}

fn pad(text: &str, width: usize) -> String {
    let length = text.chars().count();
    if length >= width {
        return text.to_string();
    }
    format!("{text}{}", " ".repeat(width - length))
}

fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    let horizontal = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1]);
    horizontal[1]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccn::types::{CcnSummary, FileCcn};
    use crate::metrics;
    use crate::model::{ExcludedFiles, FileLoc};
    use ratatui::backend::TestBackend;

    fn function(name: &str, line: usize, ccn: u32, nloc: usize) -> FunctionCcn {
        FunctionCcn {
            name: name.to_string(),
            line,
            end_line: line + nloc.saturating_sub(1),
            nloc,
            ccn,
            params: 0,
        }
    }

    fn file(path: &str, loc: usize, lang: &str, functions: Vec<FunctionCcn>) -> FileLoc {
        FileLoc {
            path: path.to_string(),
            lang: lang.to_string(),
            loc_total: loc + 10,
            loc,
            comments: 0,
            blanks: 10,
            ccn: Some(CcnSummary::from_file(FileCcn {
                tier: CcnTier::Exact,
                functions,
            })),
            is_generated: false,
            is_vendored: false,
        }
    }

    /// Four files chosen so every sort key produces a distinct first row:
    ///
    /// | path              | loc | max ccn | risk |
    /// |-------------------|-----|---------|------|
    /// | src/beta.rs       | 100 | 137     | 1.00 |
    /// | src/tui_app.py    | 300 | 20      | 0.30 |
    /// | src/alpha.rs      | 500 | 1       | 0.00 |
    /// | src/zeta.rs       | 900 | 2       | 0.00 |
    fn fixture() -> (ScanResult, Report) {
        let files = vec![
            file(
                "src/alpha.rs",
                500,
                "Rust",
                vec![function("alpha_main", 10, 1, 400)],
            ),
            file(
                "src/beta.rs",
                100,
                "Rust",
                vec![function("monster", 7, 137, 90)],
            ),
            file(
                "src/tui_app.py",
                300,
                "Python",
                vec![
                    function("draw", 3, 20, 80),
                    function("event_loop", 90, 3, 50),
                ],
            ),
            file(
                "src/zeta.rs",
                900,
                "Rust",
                vec![
                    function("helper_one", 1, 2, 300),
                    function("helper_two", 400, 1, 300),
                ],
            ),
        ];

        let loc: usize = files.iter().map(|file| file.loc).sum();
        let loc_total: usize = files.iter().map(|file| file.loc_total).sum();
        let functions_total: usize = files
            .iter()
            .filter_map(|file| file.ccn.as_ref())
            .map(|summary| summary.functions)
            .sum();
        let result = ScanResult {
            root: "C:/work/repo".to_string(),
            files_analyzed: files.len(),
            loc_total,
            loc,
            comments: 0,
            blanks: loc_total - loc,
            files_excluded: ExcludedFiles::default(),
            functions_total,
            ccn: None,
            languages: Vec::new(),
            files,
        };
        let report = metrics::build(&result);
        (result, report)
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn draw(app: &mut App<'_>, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, app))
            .expect("draw dashboard");
        buffer_text(terminal.backend().buffer())
    }

    fn press(app: &mut App<'_>, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn type_text(app: &mut App<'_>, text: &str) {
        for ch in text.chars() {
            press(app, KeyCode::Char(ch));
        }
    }

    fn first_path(app: &App<'_>) -> String {
        app.view
            .first()
            .map(|&index| app.files[index].path.clone())
            .unwrap_or_default()
    }

    #[test]
    fn renders_header_and_panels() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        let text = draw(&mut app, 100, 30);

        assert!(text.contains("repo"), "repo root missing:\n{text}");
        assert!(text.contains("Health"), "health missing:\n{text}");
        assert!(text.contains("sort:"), "sort label missing:\n{text}");
        assert!(text.contains("risk"), "sort key missing:\n{text}");
        assert!(text.contains("beta.rs"), "first file missing:\n{text}");
        assert!(text.contains("Files (4)"), "file count missing:\n{text}");
        assert!(text.contains("CCN per function"), "chart missing:\n{text}");
    }

    #[test]
    fn selection_moves_and_clamps() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        let last = app.view.len() - 1;

        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected, 0, "k at the top stays put");

        for _ in 0..20 {
            press(&mut app, KeyCode::Char('j'));
        }
        assert_eq!(app.selected, last, "j clamps at the bottom");
        assert!(app.selected < app.view.len());

        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected, last - 1);

        for _ in 0..20 {
            press(&mut app, KeyCode::Down);
        }
        assert_eq!(app.selected, last, "arrows behave like j/k");

        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.selected, 0);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.selected, last);

        // An empty list must not be able to produce an out-of-range index.
        app.view.clear();
        app.clamp_selection();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn sort_cycles() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);

        assert_eq!(app.sort, SortKey::Risk);
        assert!(app.sort_desc);
        assert_eq!(first_path(&app), "src/beta.rs");

        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Loc);
        assert_eq!(first_path(&app), "src/zeta.rs", "longest file first");

        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::MaxCcn);
        assert_eq!(first_path(&app), "src/beta.rs");

        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Over10);
        assert_eq!(app.sort.label(), "over 10");

        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Path);
        assert!(!app.sort_desc, "path sorts ascending");
        assert_eq!(
            first_path(&app),
            "src/alpha.rs",
            "path sort is lexicographic"
        );

        // Repeating `s` on the last key flips the direction ...
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Path);
        assert!(app.sort_desc);
        assert_eq!(first_path(&app), "src/zeta.rs");

        // ... and the next press wraps back to the default.
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Risk);
        assert!(app.sort_desc);
        assert_eq!(first_path(&app), "src/beta.rs");

        let text = draw(&mut app, 100, 30);
        assert!(text.contains("sort: risk"), "{text}");
    }

    #[test]
    fn filter_narrows_the_list() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        let before = app.view.len();
        assert_eq!(before, 4);

        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "tui");
        assert!(app.view.len() < before, "filter must narrow the list");
        assert_eq!(app.view.len(), 1);

        let text = draw(&mut app, 100, 30);
        assert!(
            text.contains("[filter: tui]"),
            "filter chip missing:\n{text}"
        );
        assert!(text.contains("tui_app.py"), "{text}");
        assert!(
            !text.contains("beta.rs"),
            "filtered row still visible:\n{text}"
        );

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.filter.as_deref(), Some("tui"));
        assert!(app.filter_input.is_none());

        // Esc while typing discards the edit but keeps the applied filter.
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "zzz");
        assert!(app.view.is_empty());
        let empty = draw(&mut app, 100, 30);
        assert!(
            empty.contains("no file matches"),
            "empty-state line missing:\n{empty}"
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.filter.as_deref(), Some("tui"));
        assert_eq!(app.view.len(), 1);

        // An applied filter can be cleared by submitting an empty box.
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Enter);
        assert!(app.filter.is_none());
        assert_eq!(app.view.len(), before);
    }

    #[test]
    fn detail_overlay_shows_functions() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);

        // Default sort puts the 137-CCN file first.
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.overlay, Overlay::Detail);

        let text = draw(&mut app, 100, 30);
        assert!(text.contains("beta.rs"), "detail path missing:\n{text}");
        assert!(text.contains("monster"), "function name missing:\n{text}");
        assert!(text.contains("137"), "function ccn missing:\n{text}");
        assert!(
            text.contains("top functions"),
            "detail header missing:\n{text}"
        );
        assert!(text.contains("ccn sum"), "aggregates missing:\n{text}");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        let text = draw(&mut app, 20, 6);
        assert!(text.contains("too small"), "fallback line missing:\n{text}");

        // Also safe below the layout minimums in each dimension separately.
        let _ = draw(&mut app, 100, 4);
        let _ = draw(&mut app, 10, 40);
    }

    #[test]
    fn help_overlay_lists_keys() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);

        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.overlay, Overlay::Help);

        let text = draw(&mut app, 100, 30);
        assert!(text.contains("keys"), "help title missing:\n{text}");
        assert!(text.contains("move selection"), "{text}");
        assert!(text.contains("filter paths"), "{text}");
        assert!(text.contains("file detail"), "{text}");
        assert!(text.contains("quit"), "{text}");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn q_quits_but_esc_only_closes_overlays() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);

        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::Esc);
        assert!(!app.quit);
        assert_eq!(app.overlay, Overlay::None);

        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }
}
