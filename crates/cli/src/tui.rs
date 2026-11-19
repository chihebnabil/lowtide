//! Interactive terminal UI: two tabs (global caches, project artifacts),
//! a details panel explaining each item, and background scanning/cleaning.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use rayon::prelude::*;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Cell, Clear, Padding, Paragraph, Row, Table, TableState, Wrap},
};

use lowtide_core::fsutil::{self, CleanStats, human};
use lowtide_core::projects::{self, Artifact};
use lowtide_core::targets::{self, Measure, Risk, Target};

// Lowtide palette: sea-glass, wet sand, coral on a night sea.
const ACCENT: Color = Color::Rgb(47, 214, 176);
const SAND: Color = Color::Rgb(241, 211, 154);
const GREEN: Color = Color::Rgb(47, 214, 176);
const YELLOW: Color = Color::Rgb(241, 194, 122);
const RED: Color = Color::Rgb(255, 125, 110);
const DIM: Color = Color::Rgb(110, 140, 140);
const SURFACE: Color = Color::Rgb(22, 40, 45);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const STALE_DAYS: u64 = 30;

enum Msg {
    Measured(String, Measure),
    Cleaned(String, CleanStats, Measure),
    Found(Artifact),
    ProjectsDone,
    ProjectCleaned(PathBuf, CleanStats),
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Caches,
    Projects,
}

#[derive(PartialEq)]
enum Mode {
    Browse,
    Confirm,
    Help,
}

struct CacheRow {
    t: Target,
    m: Option<Measure>,
    selected: bool,
    cleaning: bool,
    freed: Option<CleanStats>,
}

impl CacheRow {
    fn bytes(&self) -> u64 {
        match self.m {
            Some(Measure::Ready(u)) => u.bytes,
            _ => 0,
        }
    }
    fn selectable(&self) -> bool {
        self.bytes() > 0 && !self.cleaning
    }
    fn visible(&self) -> bool {
        self.m.is_none() || self.bytes() > 0 || matches!(self.m, Some(Measure::NeedsAdmin)) || self.freed.is_some()
    }
}

struct ProjRow {
    a: Artifact,
    selected: bool,
    cleaning: bool,
    freed: Option<CleanStats>,
}

impl ProjRow {
    fn bytes(&self) -> u64 {
        if self.freed.is_some() { 0 } else { self.a.usage.bytes }
    }
    fn selectable(&self) -> bool {
        self.freed.is_none() && !self.cleaning
    }
}

struct App {
    tab: Tab,
    mode: Mode,
    root: PathBuf,
    caches: Vec<CacheRow>,
    cache_state: TableState,
    projects: Vec<ProjRow>,
    proj_state: TableState,
    proj_scanning: bool,
    pending: usize,
    freed_total: u64,
    skipped_total: u64,
    status: Option<String>,
    tick: usize,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    quit: bool,
    /// Move project folders to the Recycle Bin instead of deleting them.
    recycle: bool,
    /// Whether the user has moved the cursor (so re-sorting should follow their row).
    navigated: bool,
}

pub fn run(root: PathBuf, recycle: bool) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App::new(root);
    app.recycle = recycle;
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}

impl App {
    fn new(root: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        App {
            tab: Tab::Caches,
            mode: Mode::Browse,
            root,
            caches: Vec::new(),
            cache_state: TableState::default().with_selected(0),
            projects: Vec::new(),
            proj_state: TableState::default().with_selected(0),
            proj_scanning: false,
            pending: 0,
            freed_total: 0,
            skipped_total: 0,
            status: None,
            tick: 0,
            tx,
            rx,
            quit: false,
            recycle: false,
            navigated: false,
        }
    }

    fn run(mut self, terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
        self.scan_caches();
        self.scan_projects();
        while !self.quit {
            while let Ok(msg) = self.rx.try_recv() {
                self.on_msg(msg);
            }
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(80))?
                && let Event::Key(k) = event::read()?
                    && k.kind == KeyEventKind::Press {
                        self.on_key(k.code, k.modifiers);
                    }
            self.tick = self.tick.wrapping_add(1);
        }
        Ok(())
    }

    // ── Background work ──────────────────────────────────────────────────

    fn scan_caches(&mut self) {
        self.caches = targets::catalog()
            .into_iter()
            .map(|t| CacheRow { t, m: None, selected: false, cleaning: false, freed: None })
            .collect();
        let list: Vec<Target> = self.caches.iter().map(|r| r.t.clone()).collect();
        let tx = self.tx.clone();
        thread::spawn(move || {
            list.into_par_iter().for_each(|t| {
                let m = t.measure();
                let _ = tx.send(Msg::Measured(t.id, m));
            });
        });
    }

    fn scan_projects(&mut self) {
        self.projects.clear();
        self.proj_scanning = true;
        let (tx, root) = (self.tx.clone(), self.root.clone());
        thread::spawn(move || {
            projects::scan(&root, |a| {
                let _ = tx.send(Msg::Found(a));
            });
            let _ = tx.send(Msg::ProjectsDone);
        });
    }

    fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Measured(id, m) => {
                if let Some(r) = self.caches.iter_mut().find(|r| r.t.id == id) {
                    r.m = Some(m);
                    // Pre-select what's risk-free; everything else is opt-in.
                    r.selected = r.t.risk == Risk::Safe && r.bytes() > 0;
                }
                self.sort_caches();
            }
            Msg::Cleaned(id, stats, m) => {
                if let Some(r) = self.caches.iter_mut().find(|r| r.t.id == id) {
                    r.m = Some(m);
                    r.cleaning = false;
                    r.selected = false;
                    r.freed = Some(stats);
                }
                self.finish_one(stats);
                self.sort_caches();
            }
            Msg::Found(a) => {
                self.projects.push(ProjRow { a, selected: false, cleaning: false, freed: None });
                self.sort_projects();
            }
            Msg::ProjectsDone => self.proj_scanning = false,
            Msg::ProjectCleaned(path, stats) => {
                if let Some(r) = self.projects.iter_mut().find(|r| r.a.path == path) {
                    r.cleaning = false;
                    r.selected = false;
                    r.freed = Some(stats);
                }
                self.finish_one(stats);
            }
        }
    }

    fn finish_one(&mut self, stats: CleanStats) {
        self.pending -= 1;
        self.freed_total += stats.freed + stats.recycled;
        self.skipped_total += stats.skipped;
        if self.pending == 0 {
            let mut msg = format!("Tide's out. {} back on your drive this session.", human(self.freed_total));
            if self.skipped_total > 0 {
                msg += &format!(" {} item(s) were in use and skipped: close those apps and clean again.", self.skipped_total);
            }
            self.status = Some(msg);
        }
    }

    fn start_clean(&mut self) {
        self.mode = Mode::Browse;
        self.status = None;
        match self.tab {
            Tab::Caches => {
                for r in self.caches.iter_mut().filter(|r| r.selected && r.selectable()) {
                    r.cleaning = true;
                    self.pending += 1;
                    let (t, tx) = (r.t.clone(), self.tx.clone());
                    rayon::spawn(move || {
                        let stats = t.clean();
                        let _ = tx.send(Msg::Cleaned(t.id.clone(), stats, t.measure()));
                    });
                }
            }
            Tab::Projects => {
                for r in self.projects.iter_mut().filter(|r| r.selected && r.selectable()) {
                    r.cleaning = true;
                    self.pending += 1;
                    let (a, tx, recycle) = (r.a.clone(), self.tx.clone(), self.recycle);
                    rayon::spawn(move || {
                        let stats = projects::remove(&a, recycle);
                        let _ = tx.send(Msg::ProjectCleaned(a.path, stats));
                    });
                }
            }
        }
    }

    // ── Ordering & selection ─────────────────────────────────────────────

    fn sort_caches(&mut self) {
        // Until the user navigates, keep the cursor on the top (biggest) row.
        let current = self.cursor_cache().filter(|_| self.navigated).map(|i| self.caches[i].t.id.clone());
        self.caches.sort_by(|a, b| {
            b.bytes().cmp(&a.bytes()).then_with(|| a.m.is_none().cmp(&b.m.is_none())).then_with(|| a.t.name.cmp(&b.t.name))
        });
        if let Some(id) = current
            && let Some(pos) = self.visible_caches().iter().position(|&i| self.caches[i].t.id == id) {
                self.cache_state.select(Some(pos));
            }
    }

    fn sort_projects(&mut self) {
        let current = self.cursor_proj().filter(|_| self.navigated).map(|i| self.projects[i].a.path.clone());
        self.projects.sort_by_key(|r| std::cmp::Reverse(r.a.usage.bytes));
        if let Some(p) = current
            && let Some(pos) = self.projects.iter().position(|r| r.a.path == p) {
                self.proj_state.select(Some(pos));
            }
    }

    fn visible_caches(&self) -> Vec<usize> {
        (0..self.caches.len()).filter(|&i| self.caches[i].visible()).collect()
    }

    fn cursor_cache(&self) -> Option<usize> {
        let vis = self.visible_caches();
        self.cache_state.selected().and_then(|s| vis.get(s).copied())
    }

    fn cursor_proj(&self) -> Option<usize> {
        self.proj_state.selected().filter(|&s| s < self.projects.len())
    }

    fn selected_bytes(&self) -> (u64, usize) {
        match self.tab {
            Tab::Caches => self.caches.iter().filter(|r| r.selected && r.selectable()).fold((0, 0), |(b, n), r| (b + r.bytes(), n + 1)),
            Tab::Projects => self.projects.iter().filter(|r| r.selected && r.selectable()).fold((0, 0), |(b, n), r| (b + r.bytes(), n + 1)),
        }
    }

    fn row_count(&self) -> usize {
        match self.tab {
            Tab::Caches => self.visible_caches().len(),
            Tab::Projects => self.projects.len(),
        }
    }

    fn state(&mut self) -> &mut TableState {
        match self.tab {
            Tab::Caches => &mut self.cache_state,
            Tab::Projects => &mut self.proj_state,
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        self.navigated = true;
        let n = self.row_count();
        if n == 0 {
            return;
        }
        let cur = self.state().selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, n as isize - 1) as usize;
        self.state().select(Some(next));
    }

    // ── Input ────────────────────────────────────────────────────────────

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        match self.mode {
            Mode::Help => self.mode = Mode::Browse,
            Mode::Confirm => match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.start_clean(),
                _ => self.mode = Mode::Browse,
            },
            Mode::Browse => self.on_browse_key(code),
        }
    }

    fn on_browse_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                self.tab = if self.tab == Tab::Caches { Tab::Projects } else { Tab::Caches };
            }
            KeyCode::Char('1') => self.tab = Tab::Caches,
            KeyCode::Char('2') => self.tab = Tab::Projects,
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::PageUp => self.move_cursor(-10),
            KeyCode::PageDown => self.move_cursor(10),
            KeyCode::Home | KeyCode::Char('g') => self.move_cursor(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_cursor(isize::MAX / 2),
            KeyCode::Char(' ') => {
                match self.tab {
                    Tab::Caches => {
                        if let Some(i) = self.cursor_cache().filter(|&i| self.caches[i].selectable()) {
                            self.caches[i].selected ^= true;
                        }
                    }
                    Tab::Projects => {
                        if let Some(i) = self.cursor_proj().filter(|&i| self.projects[i].selectable()) {
                            self.projects[i].selected ^= true;
                        }
                    }
                }
                self.move_cursor(1);
            }
            KeyCode::Char('a') => match self.tab {
                Tab::Caches => self.caches.iter_mut().for_each(|r| r.selected = r.selectable()),
                Tab::Projects => self.projects.iter_mut().for_each(|r| r.selected = r.selectable()),
            },
            KeyCode::Char('s') if self.tab == Tab::Caches => {
                self.caches.iter_mut().for_each(|r| r.selected = r.selectable() && r.t.risk == Risk::Safe)
            }
            KeyCode::Char('o') if self.tab == Tab::Projects => {
                self.projects.iter_mut().for_each(|r| r.selected = r.selectable() && r.a.age_days() >= STALE_DAYS)
            }
            KeyCode::Char('t') if self.tab == Tab::Projects => {
                self.recycle ^= true;
                let mut settings = lowtide_core::settings::Settings::load();
                settings.recycle_projects = self.recycle;
                let _ = settings.save();
            }
            KeyCode::Char('n') => {
                self.caches.iter_mut().for_each(|r| r.selected = false);
                self.projects.iter_mut().for_each(|r| r.selected = false);
            }
            KeyCode::Char('r') if self.pending == 0 => {
                self.status = None;
                match self.tab {
                    Tab::Caches => self.scan_caches(),
                    Tab::Projects if !self.proj_scanning => self.scan_projects(),
                    Tab::Projects => {}
                }
            }
            KeyCode::Enter | KeyCode::Char('c') | KeyCode::Delete
                if self.selected_bytes().1 > 0 => {
                    self.mode = Mode::Confirm;
                }
            _ => {}
        }
    }

    // ── Drawing ──────────────────────────────────────────────────────────

    fn draw(&mut self, f: &mut Frame) {
        let [header, tabs, body, status, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(f.area());

        self.draw_header(f, header);
        self.draw_tabs(f, tabs);
        let [list, details] = Layout::horizontal([Constraint::Min(60), Constraint::Length(46)]).areas(body);
        match self.tab {
            Tab::Caches => {
                self.draw_caches(f, list);
                self.draw_cache_details(f, details);
            }
            Tab::Projects => {
                self.draw_projects(f, list);
                self.draw_project_details(f, details);
            }
        }
        self.draw_status(f, status);
        self.draw_footer(f, footer);

        match self.mode {
            Mode::Confirm => self.draw_confirm(f),
            Mode::Help => draw_help(f),
            Mode::Browse => {}
        }
    }

    fn spinner(&self) -> &'static str {
        SPINNER[self.tick % SPINNER.len()]
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let reclaimable: u64 = self.caches.iter().map(|r| r.bytes()).sum::<u64>() + self.projects.iter().map(|r| r.bytes()).sum::<u64>();
        let (sel, _) = self.selected_bytes();
        let title = Line::from(vec![
            Span::styled(" ● ", Style::new().fg(SAND).bg(SURFACE)),
            Span::styled("〰 Lowtide ", Style::new().fg(ACCENT).bg(SURFACE).bold()),
            Span::styled("  see what your dev tools left behind", Style::new().fg(DIM)),
        ]);
        let mut stats = vec![
            Span::styled("reclaimable ", Style::new().fg(DIM)),
            Span::styled(human(reclaimable), Style::new().bold()),
            Span::styled("   selected ", Style::new().fg(DIM)),
            Span::styled(human(sel), Style::new().fg(ACCENT).bold()),
            Span::styled("   freed ", Style::new().fg(DIM)),
            Span::styled(human(self.freed_total), Style::new().fg(SAND).bold()),
            Span::raw(" "),
        ];
        if self.pending > 0 {
            stats.insert(0, Span::styled(format!("{} cleaning {}   ", self.spinner(), self.pending), Style::new().fg(YELLOW)));
        }
        f.render_widget(Paragraph::new(title), area);
        f.render_widget(Paragraph::new(Line::from(stats)).right_aligned(), area);
    }

    fn draw_tabs(&self, f: &mut Frame, area: Rect) {
        let tab = |label: String, active: bool| {
            if active {
                Span::styled(format!(" {label} "), Style::new().fg(ACCENT).bg(SURFACE).bold())
            } else {
                Span::styled(format!(" {label} "), Style::new().fg(DIM))
            }
        };
        let cache_scanning = self.caches.iter().any(|r| r.m.is_none());
        let caches_label = if cache_scanning { format!("1 Caches {}", self.spinner()) } else { "1 Caches".into() };
        let proj_label = if self.proj_scanning {
            format!("2 Projects {}", self.spinner())
        } else {
            format!("2 Projects ({})", self.projects.len())
        };
        let line = Line::from(vec![
            Span::raw(" "),
            tab(caches_label, self.tab == Tab::Caches),
            Span::raw(" "),
            tab(proj_label, self.tab == Tab::Projects),
        ]);
        f.render_widget(Paragraph::new(line), area);
    }

    fn panel(title: impl Into<Line<'static>>) -> Block<'static> {
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(SURFACE))
            .title(title.into())
            .title_style(Style::new().fg(ACCENT).bold())
    }

    fn draw_caches(&mut self, f: &mut Frame, area: Rect) {
        let vis = self.visible_caches();
        let max = vis.iter().map(|&i| self.caches[i].bytes()).max().unwrap_or(0);
        let rows: Vec<Row> = vis
            .iter()
            .map(|&i| {
                let r = &self.caches[i];
                let size = match (&r.m, r.cleaning) {
                    (_, true) => Span::styled(format!("{} cleaning", self.spinner()), Style::new().fg(YELLOW)),
                    (None, _) => Span::styled(self.spinner(), Style::new().fg(DIM)),
                    (Some(Measure::NeedsAdmin), _) => Span::styled("needs admin", Style::new().fg(DIM)),
                    (Some(Measure::Ready(u)), _) if u.bytes == 0 => Span::styled("✓ clean", Style::new().fg(GREEN)),
                    (Some(Measure::Ready(u)), _) => Span::styled(human(u.bytes), Style::new().bold()),
                };
                let dim = !r.selectable() && !r.cleaning;
                Row::new(vec![
                    Cell::from(checkbox(r.selected && r.selectable(), r.selectable())),
                    Cell::from(Span::styled(r.t.name.clone(), if dim { Style::new().fg(DIM) } else { Style::new() })),
                    Cell::from(Span::styled(r.t.category, Style::new().fg(DIM))),
                    Cell::from(risk_span(r.t.risk)),
                    Cell::from(Span::styled(bar(r.bytes(), max, 10), Style::new().fg(ACCENT))),
                    Cell::from(Line::from(size).right_aligned()),
                ])
            })
            .collect();
        let title = format!(" Caches · {} locations ", vis.len());
        let table = Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Fill(1),
                Constraint::Length(11),
                Constraint::Length(12),
                Constraint::Length(10),
                Constraint::Length(12),
            ],
        )
        .header(header_row(&["", "Name", "Category", "Risk", "", "Size"]))
        .row_highlight_style(Style::new().bg(SURFACE))
        .block(Self::panel(title));
        f.render_stateful_widget(table, area, &mut self.cache_state);
    }

    fn draw_cache_details(&self, f: &mut Frame, area: Rect) {
        let block = Self::panel(" Details ").padding(Padding::horizontal(1));
        let Some(r) = self.cursor_cache().map(|i| &self.caches[i]) else {
            f.render_widget(Paragraph::new("Scanning...").fg(DIM).block(block), area);
            return;
        };
        let mut lines = vec![
            Line::from(Span::styled(r.t.name.clone(), Style::new().bold())),
            Line::from(vec![Span::styled(r.t.category, Style::new().fg(DIM)), Span::raw("  "), risk_span(r.t.risk)]),
            Line::raw(""),
            section("What it is"),
            Line::raw(r.t.what),
            Line::raw(""),
            section("If you delete it"),
            Line::raw(r.t.after),
            Line::raw(""),
        ];
        match r.m {
            Some(Measure::Ready(u)) => lines.push(Line::from(vec![
                Span::styled(human(u.bytes), Style::new().bold()),
                Span::styled(format!(" in {} files", u.files), Style::new().fg(DIM)),
            ])),
            Some(Measure::NeedsAdmin) => lines.push(Line::styled("Run lowtide from an admin terminal to clean this.", Style::new().fg(YELLOW))),
            None => {}
        }
        if let Some(s) = r.freed {
            let mut l = vec![Span::styled(format!("Freed {}", human(s.freed)), Style::new().fg(GREEN))];
            if s.skipped > 0 {
                l.push(Span::styled(format!(" · {} in use, skipped", s.skipped), Style::new().fg(DIM)));
            }
            lines.push(Line::from(l));
        }
        let paths = r.t.paths();
        if !paths.is_empty() {
            lines.push(Line::raw(""));
            lines.push(section("Location"));
            for p in paths.iter().take(6) {
                lines.push(Line::styled(pretty_path(p), Style::new().fg(DIM)));
            }
            if paths.len() > 6 {
                lines.push(Line::styled(format!("+{} more", paths.len() - 6), Style::new().fg(DIM)));
            }
        }
        f.render_widget(Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }).block(block), area);
    }

    fn draw_projects(&mut self, f: &mut Frame, area: Rect) {
        let max = self.projects.iter().map(|r| r.a.usage.bytes).max().unwrap_or(0);
        let rows: Vec<Row> = self
            .projects
            .iter()
            .map(|r| {
                let rel = r.a.project().strip_prefix(&self.root).unwrap_or(r.a.project());
                let rel = if rel.as_os_str().is_empty() { ".".into() } else { rel.display().to_string() };
                let age = r.a.modified.map(fsutil::age).unwrap_or_default();
                let stale = r.a.age_days() >= STALE_DAYS;
                let size = if r.cleaning {
                    Span::styled(format!("{} cleaning", self.spinner()), Style::new().fg(YELLOW))
                } else if let Some(s) = r.freed {
                    Span::styled(format!("✓ {}", human(s.freed + s.recycled)), Style::new().fg(GREEN))
                } else {
                    Span::styled(human(r.a.usage.bytes), Style::new().bold())
                };
                let dim = r.freed.is_some();
                Row::new(vec![
                    Cell::from(checkbox(r.selected && r.selectable(), r.selectable())),
                    Cell::from(Span::styled(rel, if dim { Style::new().fg(DIM) } else { Style::new() })),
                    Cell::from(Span::styled(r.a.kind, Style::new().fg(DIM))),
                    Cell::from(Span::styled(age, Style::new().fg(if stale { YELLOW } else { DIM }))),
                    Cell::from(Span::styled(bar(r.a.usage.bytes, max, 10), Style::new().fg(ACCENT))),
                    Cell::from(Line::from(size).right_aligned()),
                ])
            })
            .collect();
        let title = format!(" Projects in {} ", pretty_path(&self.root));
        let empty = self.projects.is_empty();
        let table = Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Fill(1),
                Constraint::Length(18),
                Constraint::Length(14),
                Constraint::Length(10),
                Constraint::Length(10),
            ],
        )
        .header(header_row(&["", "Project", "Kind", "Last built", "", "Size"]))
        .row_highlight_style(Style::new().bg(SURFACE))
        .block(Self::panel(title));
        f.render_stateful_widget(table, area, &mut self.proj_state);
        if empty {
            let msg = if self.proj_scanning { "Looking for node_modules, target, .next, venvs..." } else { "No build artifacts found. Try `lowtide --root D:\\code`." };
            let inner = area.inner(ratatui::layout::Margin::new(2, 3));
            f.render_widget(Paragraph::new(msg).fg(DIM), inner);
        }
    }

    fn draw_project_details(&self, f: &mut Frame, area: Rect) {
        let block = Self::panel(" Details ").padding(Padding::horizontal(1));
        let Some(r) = self.cursor_proj().map(|i| &self.projects[i]) else {
            f.render_widget(
                Paragraph::new("Build folders are safe to delete: they are regenerated from your source code. Projects you haven't touched in a while are the best candidates.")
                    .wrap(Wrap { trim: false })
                    .fg(DIM)
                    .block(block),
                area,
            );
            return;
        };
        let name = r.a.project().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut lines = vec![
            Line::from(Span::styled(name, Style::new().bold())),
            Line::from(vec![Span::styled(r.a.kind, Style::new().fg(DIM)), Span::raw("  "), risk_span(Risk::Redownload)]),
            Line::raw(""),
            Line::from(vec![
                Span::styled(human(r.a.usage.bytes), Style::new().bold()),
                Span::styled(format!(" in {} files", r.a.usage.files), Style::new().fg(DIM)),
            ]),
        ];
        if let Some(m) = r.a.modified {
            let stale = r.a.age_days() >= STALE_DAYS;
            lines.push(Line::from(vec![
                Span::styled("Last built ", Style::new().fg(DIM)),
                Span::styled(fsutil::age(m), Style::new().fg(if stale { YELLOW } else { Color::Reset })),
            ]));
        }
        lines.extend([
            Line::raw(""),
            section("If you delete it"),
            Line::raw("Your source code is untouched. Rebuild with:"),
            Line::styled(format!("  {}", r.a.restore), Style::new().fg(ACCENT)),
            Line::raw(""),
            section("Location"),
            Line::styled(pretty_path(&r.a.path), Style::new().fg(DIM)),
        ]);
        if let Some(s) = r.freed {
            lines.push(Line::raw(""));
            lines.push(Line::styled(format!("Freed {}", human(s.freed)), Style::new().fg(GREEN)));
        }
        f.render_widget(Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }).block(block), area);
    }

    fn draw_status(&self, f: &mut Frame, area: Rect) {
        let admin = self.caches.iter().filter(|r| matches!(r.m, Some(Measure::NeedsAdmin))).count();
        let line = if let Some(s) = &self.status {
            Line::styled(format!(" {s}"), Style::new().fg(GREEN))
        } else if admin > 0 && self.tab == Tab::Caches {
            Line::styled(format!(" {admin} system location(s) need admin: restart lowtide in an elevated terminal to include them."), Style::new().fg(DIM))
        } else {
            Line::raw("")
        };
        f.render_widget(Paragraph::new(line), area);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let mut keys = vec![("↑↓", "move"), ("space", "select"), ("a", "all"), ("n", "none")];
        match self.tab {
            Tab::Caches => keys.push(("s", "safe only")),
            Tab::Projects => {
                keys.push(("o", "stale 30d+"));
                keys.push(("t", if self.recycle { "recycle bin: on" } else { "recycle bin: off" }));
            }
        }
        keys.extend([("enter", "clean"), ("tab", "switch"), ("r", "rescan"), ("?", "help"), ("q", "quit")]);
        let spans: Vec<Span> = keys
            .into_iter()
            .flat_map(|(k, d)| [Span::styled(format!(" {k} "), Style::new().fg(Color::Black).bg(DIM)), Span::styled(format!(" {d}  "), Style::new().fg(DIM))])
            .collect();
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_confirm(&self, f: &mut Frame) {
        let (bytes, count) = self.selected_bytes();
        let redownload = match self.tab {
            Tab::Caches => self.caches.iter().filter(|r| r.selected && r.selectable() && r.t.risk != Risk::Safe).count(),
            Tab::Projects => count,
        };
        let area = centered(f.area(), 58, 8);
        f.render_widget(Clear, area);
        let mut lines = vec![
            Line::raw(""),
            Line::from(vec![
                Span::raw(if self.tab == Tab::Projects && self.recycle { "Move to Recycle Bin " } else { "Delete " }),
                Span::styled(human(bytes), Style::new().fg(ACCENT).bold()),
                Span::raw(format!(" from {count} location(s)?")),
            ]),
        ];
        lines.push(if redownload > 0 {
            Line::styled(format!("{redownload} of them will need a re-download or rebuild later."), Style::new().fg(YELLOW))
        } else {
            Line::styled("All of it regenerates on its own.", Style::new().fg(GREEN))
        });
        lines.extend([
            Line::raw(""),
            Line::from(vec![
                Span::styled(" y ", Style::new().fg(Color::Black).bg(GREEN).bold()),
                Span::raw(" clean     "),
                Span::styled(" any key ", Style::new().fg(Color::Black).bg(DIM)),
                Span::raw(" cancel"),
            ]),
        ]);
        let block = Self::panel(" Confirm ").border_style(Style::new().fg(ACCENT));
        f.render_widget(Paragraph::new(Text::from(lines)).centered().block(block), area);
    }
}

fn draw_help(f: &mut Frame) {
    let area = centered(f.area(), 70, 18);
    f.render_widget(Clear, area);
    let lines = vec![
        section("Risk levels"),
        Line::from(vec![risk_span(Risk::Safe), Span::raw("  regenerates by itself, nothing to download")]),
        Line::from(vec![risk_span(Risk::Redownload), Span::raw("  comes back, but must be downloaded/rebuilt")]),
        Line::from(vec![risk_span(Risk::Review), Span::raw("  permanent, look before you delete")]),
        Line::raw(""),
        section("Guarantees"),
        Line::raw("• Only caches are touched: no settings, logins, cookies or source code."),
        Line::raw("• Symlinks and junctions are never followed (pnpm, OneDrive safe)."),
        Line::raw("• Files in use are skipped; temp files newer than 24h are kept."),
        Line::raw("• Safe items are pre-selected; everything else is opt-in."),
        Line::raw(""),
        section("Projects tab"),
        Line::raw("Finds node_modules, target, .next, venvs, .gradle, bin/obj... only"),
        Line::raw("next to their project file (package.json, Cargo.toml, ...)."),
        Line::raw("Press o to select everything not rebuilt in 30+ days."),
    ];
    let block = App::panel(" Help ").padding(Padding::horizontal(2));
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let [r] = Layout::horizontal([Constraint::Length(w.min(area.width))]).flex(Flex::Center).areas(area);
    let [r] = Layout::vertical([Constraint::Length(h.min(area.height))]).flex(Flex::Center).areas(r);
    r
}

fn header_row(cols: &[&'static str]) -> Row<'static> {
    let cells: Vec<Cell> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let l = Line::from(*c);
            Cell::from(if i == cols.len() - 1 { l.right_aligned() } else { l })
        })
        .collect();
    Row::new(cells).style(Style::new().fg(DIM).add_modifier(Modifier::BOLD)).bottom_margin(0)
}

fn section(title: &'static str) -> Line<'static> {
    Line::styled(title.to_uppercase(), Style::new().fg(DIM).add_modifier(Modifier::BOLD))
}

fn checkbox(on: bool, enabled: bool) -> Span<'static> {
    match (on, enabled) {
        (_, false) => Span::styled(" ·", Style::new().fg(DIM)),
        (true, _) => Span::styled(" ●", Style::new().fg(ACCENT)),
        (false, _) => Span::styled(" ○", Style::new().fg(DIM)),
    }
}

fn risk_span(r: Risk) -> Span<'static> {
    let color = match r {
        Risk::Safe => GREEN,
        Risk::Redownload => YELLOW,
        Risk::Review => RED,
    };
    Span::styled(r.label(), Style::new().fg(color))
}

fn bar(value: u64, max: u64, width: usize) -> String {
    const PARTIAL: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    if max == 0 || value == 0 {
        return String::new();
    }
    let eighths = ((value as f64 / max as f64) * (width * 8) as f64).round().max(1.0) as usize;
    let mut s = "█".repeat(eighths / 8);
    s.push_str(PARTIAL[eighths % 8]);
    s
}

/// Replace the home folder with `~` to keep paths readable.
fn pretty_path(p: &std::path::Path) -> String {
    if let Some(home) = fsutil::env_path("USERPROFILE")
        && let Ok(rest) = p.strip_prefix(&home) {
            return format!("~\\{}", rest.display());
        }
    p.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lowtide_core::fsutil::Usage;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn renders_both_tabs() {
        let mut app = App::new(PathBuf::from(r"C:\code"));
        app.caches = targets::catalog()
            .into_iter()
            .map(|t| CacheRow { t, m: None, selected: false, cleaning: false, freed: None })
            .collect();
        let ids: Vec<String> = app.caches.iter().map(|r| r.t.id.clone()).collect();
        for (n, id) in ids.into_iter().enumerate() {
            let m = if id == "windows-temp" { Measure::NeedsAdmin } else { Measure::Ready(Usage { bytes: (n as u64 + 1) * 70_000_000, files: 42 }) };
            app.on_msg(Msg::Measured(id, m));
        }
        let mut term = Terminal::new(TestBackend::new(140, 32)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let screen = format!("{}", term.backend());
        println!("{screen}");
        assert!(screen.contains("Lowtide"));
        assert!(screen.contains("What it is") || screen.contains("WHAT IT IS"));

        app.tab = Tab::Projects;
        app.on_msg(Msg::Found(Artifact {
            path: PathBuf::from(r"C:\code\shop\node_modules"),
            kind: "node_modules",
            restore: "npm install",
            usage: Usage { bytes: 900_000_000, files: 12000 },
            modified: Some(std::time::SystemTime::now() - Duration::from_secs(86_400 * 90)),
        }));
        app.on_msg(Msg::ProjectsDone);
        app.mode = Mode::Confirm;
        app.projects[0].selected = true;
        term.draw(|f| app.draw(f)).unwrap();
        let screen = format!("{}", term.backend());
        println!("{screen}");
        assert!(screen.contains("shop"));
        assert!(screen.contains("858.3 MB"));
    }
}
