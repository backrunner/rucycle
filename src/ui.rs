mod tree;

use std::{
    collections::HashSet,
    io::Stdout,
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, Clear, Gauge, Paragraph, Row, Table, Wrap},
    Frame, Terminal,
};

use crate::{
    cleaner::{clean_projects, CleanEvent, CleanPhase, CleanPlan, CleanSummary},
    format::{format_bytes, format_system_time, truncate_middle},
    scan::{discover_projects_with_progress, ScanControl, ScanPhase, ScanReport, SortKey},
};
use tree::{ProjectTree, VisibleRow};

#[derive(Clone, Debug)]
enum ProjectStatus {
    Idle,
    Queued,
    Cleaning,
    Cleaned,
    Skipped(String),
    Failed(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Scanning,
    Browsing,
    ConfirmClean,
    Cleaning,
}

struct CleanProgress {
    started: Instant,
    total: usize,
    completed: usize,
    current: usize,
    path: PathBuf,
    phase: CleanPhase,
}

struct App {
    report: ScanReport,
    selected: Vec<bool>,
    statuses: Vec<ProjectStatus>,
    tree: ProjectTree,
    expanded: HashSet<PathBuf>,
    visible: Vec<VisibleRow>,
    cursor: usize,
    offset: usize,
    sort: SortKey,
    mode: Mode,
    message: String,
    scan_control: ScanControl,
    scan_started: Instant,
    clean_progress: Option<CleanProgress>,
}

impl App {
    fn new(scan_root: PathBuf, sort: SortKey) -> Self {
        Self {
            report: ScanReport {
                scan_root,
                projects: Vec::new(),
                warnings: Vec::new(),
            },
            selected: Vec::new(),
            statuses: Vec::new(),
            tree: ProjectTree::default(),
            expanded: HashSet::new(),
            visible: Vec::new(),
            cursor: 0,
            offset: 0,
            sort,
            mode: Mode::Scanning,
            message: String::new(),
            scan_control: ScanControl::default(),
            scan_started: Instant::now(),
            clean_progress: None,
        }
    }

    fn set_report(&mut self, report: ScanReport) {
        self.selected = vec![true; report.projects.len()];
        self.statuses = vec![ProjectStatus::Idle; report.projects.len()];
        self.report = report;
        self.mode = Mode::Browsing;
        self.rebuild_tree();
        self.message = if self.report.projects.is_empty() {
            "No Rust projects found. Press q to quit.".into()
        } else {
            "Nested projects are grouped. Space selects a group; Right expands it.".into()
        };
    }

    fn current_index(&self) -> Option<usize> {
        self.visible.get(self.cursor).map(|row| row.index)
    }

    fn rebuild_tree(&mut self) {
        let current = self.current_index();
        self.tree = ProjectTree::new(&self.report.projects, self.sort);
        self.refresh_visible(current);
    }

    fn refresh_visible(&mut self, current: Option<usize>) {
        self.visible = self.tree.visible(&self.report.projects, &self.expanded);
        if let Some(position) = self
            .visible
            .iter()
            .position(|row| Some(row.index) == current)
        {
            self.cursor = position;
        }
        self.cursor = self.cursor.min(self.visible.len().saturating_sub(1));
    }

    fn selected_count(&self) -> usize {
        self.selected.iter().filter(|&&selected| selected).count()
    }

    fn hidden_selected_count(&self) -> usize {
        self.selected_count()
            - self
                .visible
                .iter()
                .filter(|row| self.selected[row.index])
                .count()
    }

    fn total_reclaimable(&self) -> u64 {
        self.report
            .projects
            .iter()
            .zip(&self.selected)
            .filter_map(|(project, &selected)| selected.then_some(project.target_size))
            .fold(0, u64::saturating_add)
    }

    fn page_size(area: Rect) -> usize {
        area.height.saturating_sub(3).max(1) as usize
    }

    fn ensure_cursor_visible(&mut self, page_size: usize) {
        self.offset = self
            .offset
            .min(self.visible.len().saturating_sub(page_size));
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        if self.cursor >= self.offset + page_size {
            self.offset = self.cursor + 1 - page_size;
        }
    }

    fn move_cursor(&mut self, delta: isize, page_size: usize) {
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(self.visible.len().saturating_sub(1));
        self.ensure_cursor_visible(page_size);
    }

    fn toggle_current(&mut self) {
        if let Some(index) = self.current_index() {
            let members = self.tree.members(index);
            let all_selected = members.iter().all(|&index| self.selected[index]);
            for index in members {
                self.selected[index] = !all_selected;
            }
        }
    }

    fn selection_marker(&self, index: usize) -> &'static str {
        let members = self.tree.members(index);
        let selected = members
            .iter()
            .filter(|&&index| self.selected[index])
            .count();
        if selected == members.len() {
            "[x]"
        } else if selected == 0 {
            "[ ]"
        } else {
            "[-]"
        }
    }

    fn expand_current(&mut self, toggle: bool) {
        if let Some(index) = self.current_index() {
            if !self.tree.nodes[index].children.is_empty() {
                let root = self.report.projects[index].root.clone();
                if toggle && self.expanded.contains(&root) {
                    self.expanded.remove(&root);
                } else {
                    self.expanded.insert(root);
                }
                self.refresh_visible(Some(index));
            }
        }
    }

    fn collapse_current(&mut self) {
        if let Some(index) = self.current_index() {
            if self.expanded.remove(&self.report.projects[index].root) {
                self.refresh_visible(Some(index));
            } else if let Some(parent) = self.tree.nodes[index].parent {
                self.refresh_visible(Some(parent));
            }
        }
    }

    fn prepare_clean(&mut self) -> CleanPlan {
        self.mode = Mode::Cleaning;
        self.clean_progress = Some(CleanProgress {
            started: Instant::now(),
            total: self.selected_count(),
            completed: 0,
            current: 0,
            path: PathBuf::new(),
            phase: CleanPhase::CheckingProcesses,
        });
        for (status, &selected) in self.statuses.iter_mut().zip(&self.selected) {
            if selected {
                *status = ProjectStatus::Queued;
            }
        }
        CleanPlan::selected(&self.report.projects, &self.selected)
    }

    fn clean_event(&mut self, event: CleanEvent) {
        let project = match &event {
            CleanEvent::Started { project, .. }
            | CleanEvent::Progress { project, .. }
            | CleanEvent::Cleaned { project, .. }
            | CleanEvent::Skipped { project, .. }
            | CleanEvent::Failed { project, .. } => project,
        };
        let Some(index) = self
            .report
            .projects
            .iter()
            .position(|candidate| candidate.root == project.root)
        else {
            return;
        };
        let progress = self.clean_progress.as_mut().unwrap();
        match event {
            CleanEvent::Started {
                index: current,
                total,
                project,
            } => {
                progress.current = current;
                progress.total = total;
                progress.path = project.root;
                progress.phase = CleanPhase::CheckingProcesses;
                self.statuses[index] = ProjectStatus::Cleaning;
            }
            CleanEvent::Progress { phase, .. } => {
                progress.phase = phase;
            }
            CleanEvent::Cleaned { reclaimed, .. } => {
                progress.completed += 1;
                self.statuses[index] = ProjectStatus::Cleaned;
                self.report.projects[index].target_size = 0;
                let mut ancestor = Some(index);
                while let Some(index) = ancestor {
                    self.report.projects[index].total_size = self.report.projects[index]
                        .total_size
                        .saturating_sub(reclaimed);
                    ancestor = self.tree.nodes[index].parent;
                }
                self.rebuild_tree();
            }
            CleanEvent::Skipped { message, .. } => {
                progress.completed += 1;
                self.statuses[index] = ProjectStatus::Skipped(message);
            }
            CleanEvent::Failed { message, .. } => {
                progress.completed += 1;
                self.statuses[index] = ProjectStatus::Failed(message);
            }
        }
    }

    fn finish_clean(&mut self, summary: CleanSummary) {
        self.mode = Mode::Browsing;
        let elapsed = self.clean_progress.take().unwrap().started.elapsed();
        self.message = format!(
            "Clean complete in {}: {} cleaned, {} failed, {} skipped",
            elapsed_text(elapsed),
            summary.cleaned,
            summary.failed,
            summary.skipped
        );
        self.rebuild_tree();
    }

    fn row_status(&self, index: usize) -> (&'static str, Color) {
        let members = self.tree.members(index);
        let has = |predicate: fn(&ProjectStatus) -> bool| {
            members
                .iter()
                .any(|&index| predicate(&self.statuses[index]))
        };
        if has(|s| matches!(s, ProjectStatus::Cleaning)) {
            ("working", Color::Cyan)
        } else if has(|s| matches!(s, ProjectStatus::Queued)) {
            ("queued", Color::Cyan)
        } else if has(|s| matches!(s, ProjectStatus::Failed(_))) {
            ("failed", Color::Red)
        } else if has(|s| matches!(s, ProjectStatus::Skipped(_))) {
            ("skipped", Color::Yellow)
        } else if members
            .iter()
            .all(|&index| matches!(self.statuses[index], ProjectStatus::Cleaned))
        {
            ("cleaned", Color::Green)
        } else if has(|s| matches!(s, ProjectStatus::Cleaned)) {
            ("partial", Color::Green)
        } else {
            ("", Color::Reset)
        }
    }
}

enum WorkerEvent {
    Scanned(Result<ScanReport>),
    Clean(CleanEvent),
    Finished(Result<CleanSummary>),
}

struct Worker {
    receiver: Receiver<WorkerEvent>,
    handle: Option<JoinHandle<()>>,
    scan_control: Option<ScanControl>,
}

impl Worker {
    fn scan(root: PathBuf, sort: SortKey, control: ScanControl) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(64);
        let worker_control = control.clone();
        let handle = thread::Builder::new()
            .name("rucycle-scan".into())
            .spawn(move || {
                let report = discover_projects_with_progress(&root, sort, &worker_control);
                let _ = sender.send(WorkerEvent::Scanned(report));
            })?;
        Ok(Self {
            receiver,
            handle: Some(handle),
            scan_control: Some(control),
        })
    }

    fn clean(plan: CleanPlan) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(64);
        let handle = thread::Builder::new()
            .name("rucycle-clean".into())
            .spawn(move || {
                let result = clean_projects(plan, |event| {
                    let _ = sender.send(WorkerEvent::Clean(event));
                });
                let _ = sender.send(WorkerEvent::Finished(result));
            })?;
        Ok(Self {
            receiver,
            handle: Some(handle),
            scan_control: None,
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(control) = &self.scan_control {
            control.cancel();
        } else if let Some(handle) = self.handle.take() {
            // Drain events on terminal errors so cleanup cannot block on a full
            // channel. Let Cargo finish instead of abandoning a staged target.
            while self.receiver.recv().is_ok() {}
            let _ = handle.join();
        }
    }
}

pub fn run(scan_root: PathBuf, sort: SortKey) -> Result<()> {
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(err) => {
            ratatui::restore();
            return Err(err.into());
        }
    };
    let mut app = App::new(scan_root, sort);
    let result = run_app(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    terminal.draw(|frame| draw(frame, app))?;
    let mut worker = Some(Worker::scan(
        app.report.scan_root.clone(),
        app.sort,
        app.scan_control.clone(),
    )?);
    loop {
        if let Some(active) = &worker {
            let mut finished = false;
            for _ in 0..256 {
                match active.receiver.try_recv() {
                    Ok(WorkerEvent::Scanned(report)) => {
                        app.set_report(report?);
                        finished = true;
                        break;
                    }
                    Ok(WorkerEvent::Clean(event)) => app.clean_event(event),
                    Ok(WorkerEvent::Finished(summary)) => {
                        app.finish_clean(summary?);
                        finished = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        return Err(anyhow!("background worker stopped unexpectedly"))
                    }
                }
            }
            if finished {
                worker = None;
            }
        }
        let size = terminal.size()?;
        let page_size = App::page_size(table_area(
            Rect::new(0, 0, size.width, size.height),
            app.mode,
        ));
        app.ensure_cursor_visible(page_size);
        terminal.draw(|frame| draw(frame, app))?;
        if !event::poll(Duration::from_millis(80))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let quit = matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
            || (key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c' | 'd')));
        match app.mode {
            Mode::Scanning => {
                if quit {
                    app.scan_control.cancel();
                    return Ok(());
                }
            }
            Mode::ConfirmClean => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let plan = app.prepare_clean();
                    terminal.draw(|frame| draw(frame, app))?;
                    worker = Some(Worker::clean(plan)?);
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {
                    app.mode = Mode::Browsing;
                }
                _ => {}
            },
            Mode::Browsing | Mode::Cleaning => {
                if quit && app.mode == Mode::Browsing {
                    return Ok(());
                }
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') => app.move_cursor(1, page_size),
                    KeyCode::Up | KeyCode::Char('k') => app.move_cursor(-1, page_size),
                    KeyCode::PageDown => app.move_cursor(page_size as isize, page_size),
                    KeyCode::PageUp => app.move_cursor(-(page_size as isize), page_size),
                    KeyCode::Home => {
                        app.cursor = 0;
                    }
                    KeyCode::End => {
                        app.cursor = app.visible.len().saturating_sub(1);
                    }
                    KeyCode::Right => app.expand_current(false),
                    KeyCode::Enter => app.expand_current(true),
                    KeyCode::Left => app.collapse_current(),
                    KeyCode::Char('s') => {
                        app.sort = match app.sort {
                            SortKey::Size => SortKey::Modified,
                            SortKey::Modified => SortKey::Size,
                        };
                        app.rebuild_tree();
                    }
                    KeyCode::Char(' ') if app.mode == Mode::Browsing => app.toggle_current(),
                    KeyCode::Char('a') if app.mode == Mode::Browsing => {
                        let all = app.selected.iter().all(|&selected| selected);
                        app.selected.fill(!all);
                    }
                    KeyCode::Char('c') if app.mode == Mode::Browsing => {
                        if app.selected_count() > 0 {
                            app.mode = Mode::ConfirmClean;
                        } else {
                            app.message = "Select at least one project before cleaning".into();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn main_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(5),
        ])
        .split(area)
}

fn table_area(area: Rect, mode: Mode) -> Rect {
    let body = main_layout(area)[1];
    if mode == Mode::Cleaning {
        Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).split(body)[1]
    } else {
        body
    }
}

fn draw(frame: &mut Frame<'_>, app: &App) {
    if app.mode == Mode::Scanning {
        draw_scan(frame, app);
        return;
    }
    let chunks = main_layout(frame.area());
    draw_header(frame, app, chunks[0]);
    if app.mode == Mode::Cleaning {
        let area =
            Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).split(chunks[1])[0];
        draw_clean_progress(frame, app, area);
    }
    draw_table(frame, app, table_area(frame.area(), app.mode));
    draw_footer(frame, app, chunks[2]);
    if app.mode == Mode::ConfirmClean {
        draw_confirm(frame, app);
    }
}

fn draw_scan(frame: &mut Frame<'_>, app: &App) {
    let progress = app.scan_control.snapshot();
    let elapsed = app.scan_started.elapsed();
    let outer = Block::bordered().title(" rucycle / Scanning ");
    let inner = outer.inner(frame.area());
    frame.render_widget(outer, frame.area());
    let area = centered_rect(90, 13, inner);
    let width = area.width as usize;
    let discovering = progress.phase == ScanPhase::Discovering;
    let phase = if discovering {
        "Finding Rust projects"
    } else {
        "Measuring project sizes"
    };
    let detail = if discovering {
        format!(
            "{} entries visited   {} projects found",
            progress.entries, progress.found
        )
    } else {
        format!(
            "{} / {} projects measured   {} inspected",
            progress.completed,
            progress.found,
            format_bytes(progress.bytes)
        )
    };
    let text = vec![
        Line::styled(
            format!("{}  {phase}", spinner(elapsed)),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(format!("Elapsed {}", elapsed_text(elapsed))),
        Line::from(""),
        Line::from(detail),
        Line::from(if discovering {
            "Discovering files; the total is not known yet."
        } else {
            "Progress counts completed projects; large projects may take longer."
        }),
        Line::from(""),
        Line::from(format!(
            "Root: {}",
            truncate_middle(
                &app.report.scan_root.display().to_string(),
                width.saturating_sub(6)
            )
        )),
        Line::from(format!(
            "Now:  {}",
            truncate_middle(
                &progress.current_path.display().to_string(),
                width.saturating_sub(6)
            )
        )),
        Line::from(""),
        Line::from("q / Esc  Cancel scan"),
    ];
    frame.render_widget(Paragraph::new(text), area);
    if !discovering && area.height >= 12 {
        draw_gauge(
            frame,
            Rect::new(area.x, area.y + 11, area.width, 1),
            progress.completed,
            progress.found,
        );
    }
}

fn draw_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let sort = match app.sort {
        SortKey::Size => "target size",
        SortKey::Modified => "modified",
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(
                "rucycle",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  {} groups / {} projects   selected: {}   reclaimable: {}",
                app.tree.roots.len(),
                app.report.projects.len(),
                app.selected_count(),
                format_bytes(app.total_reclaimable())
            )),
        ]),
        Line::from(format!(
            "Sort: {sort}   {} selected inside collapsed groups   warnings: {}",
            app.hidden_selected_count(),
            app.report.warnings.len()
        )),
    ];
    frame.render_widget(Paragraph::new(lines).block(Block::bordered()), area);
}

fn draw_clean_progress(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let progress = app.clean_progress.as_ref().unwrap();
    let elapsed = progress.started.elapsed();
    let text = vec![
        Line::styled(
            format!(
                "{} {}   {}/{} completed   {} elapsed",
                spinner(elapsed),
                progress.phase,
                progress.completed,
                progress.total,
                elapsed_text(elapsed)
            ),
            Style::default().fg(Color::Cyan),
        ),
        Line::from(if progress.current == 0 {
            "Preparing selected projects...".into()
        } else {
            format!(
                "Project {}/{}: {}",
                progress.current,
                progress.total,
                truncate_middle(
                    &progress.path.display().to_string(),
                    area.width.saturating_sub(25) as usize
                )
            )
        }),
    ];
    let block = Block::bordered().title(" Cleaning ");
    let inner = block.inner(area);
    frame.render_widget(Paragraph::new(text).block(block), area);
    if inner.height >= 3 {
        draw_gauge(
            frame,
            Rect::new(inner.x, inner.y + 2, inner.width, 1),
            progress.completed,
            progress.total,
        );
    }
}

fn draw_gauge(frame: &mut Frame<'_>, area: Rect, completed: usize, total: usize) {
    let ratio = if total == 0 {
        0.0
    } else {
        (completed as f64 / total as f64).clamp(0.0, 1.0)
    };
    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(Color::Cyan))
            .ratio(ratio)
            .label(format!("{completed}/{total}")),
        area,
    );
}

fn draw_table(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if app.visible.is_empty() {
        frame.render_widget(
            Paragraph::new("No Rust projects found under this directory.")
                .block(Block::bordered().title(" Projects ")),
            area,
        );
        return;
    }
    let compact = area.width < 100;
    let path_width = area
        .width
        .saturating_sub(if compact { 29 } else { 58 })
        .max(8) as usize;
    let rows = app
        .visible
        .iter()
        .enumerate()
        .skip(app.offset)
        .take(App::page_size(area))
        .map(|(position, row)| {
            let index = row.index;
            let project = &app.report.projects[index];
            let node = &app.tree.nodes[index];
            let group = !node.children.is_empty();
            let fold = if !group {
                " "
            } else if app.expanded.contains(&project.root) {
                "v"
            } else {
                ">"
            };
            let relative = node
                .parent
                .map(|parent| {
                    project
                        .root
                        .strip_prefix(&app.report.projects[parent].root)
                        .unwrap_or(&project.root)
                })
                .unwrap_or_else(|| {
                    project
                        .root
                        .strip_prefix(&app.report.scan_root)
                        .unwrap_or(&project.root)
                });
            let path = if relative.as_os_str().is_empty() {
                ".".into()
            } else {
                relative.display().to_string()
            };
            let suffix = if group {
                format!(" (+{})", node.count - 1)
            } else {
                String::new()
            };
            let prefix = format!("{}{} ", "  ".repeat(row.depth.min(4)), fold);
            let label = format!(
                "{}{}{}",
                prefix,
                truncate_middle(
                    &path,
                    path_width.saturating_sub(prefix.len() + suffix.len())
                ),
                suffix
            );
            let (status, color) = app.row_status(index);
            let mut cells = vec![
                Cell::from(if position == app.cursor { ">" } else { " " }),
                Cell::from(app.selection_marker(index)),
                Cell::from(label),
                Cell::from(format_bytes(node.target_size)),
            ];
            if !compact {
                cells.extend([
                    Cell::from(format_bytes(project.total_size)),
                    Cell::from(format_system_time(project.last_modified)),
                ]);
            }
            cells.push(Cell::from(status).style(Style::default().fg(color)));
            let row = Row::new(cells);
            if position == app.cursor {
                row.style(Style::default().bg(Color::DarkGray).fg(Color::White))
            } else {
                row
            }
        });
    let mut headings = vec!["", "sel", "project / group", "target"];
    let mut widths = vec![
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(10),
    ];
    if !compact {
        headings.extend(["total", "modified"]);
        widths.extend([Constraint::Length(10), Constraint::Length(17)]);
    }
    headings.push("status");
    widths.push(Constraint::Length(7));
    let table = Table::new(rows, widths)
        .header(
            Row::new(headings).style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .block(Block::bordered().title(" Projects / group sizes include nested projects "));
    frame.render_widget(table, area);
}

fn draw_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let detail = app
        .current_index()
        .map(|index| {
            let issue = app.tree.members(index).into_iter().find_map(|member| {
                match &app.statuses[member] {
                    ProjectStatus::Skipped(reason) | ProjectStatus::Failed(reason) => {
                        Some(format!("{}: {reason}", app.report.projects[member].name))
                    }
                    _ => None,
                }
            });
            issue.unwrap_or_else(|| {
                let project = &app.report.projects[index];
                format!(
                    "{}   local target: {}",
                    project.root.display(),
                    format_bytes(project.target_size)
                )
            })
        })
        .unwrap_or_default();
    let message = if app.mode == Mode::Cleaning {
        "Cleanup in progress. Browse freely; selection and quit unlock when done."
    } else {
        &app.message
    };
    let controls = if app.mode == Mode::Cleaning {
        "Up/Down move  PgUp/PgDn page  Left/Right fold  Enter expand  s sort"
    } else {
        "Up/Down move  Left/Right fold  Space select  a all  s sort  c clean  q quit"
    };
    let width = area.width.saturating_sub(2) as usize;
    let text = vec![
        Line::from(truncate_middle(&detail, width)),
        Line::from(message),
        Line::from(controls),
    ];
    frame.render_widget(Paragraph::new(text).block(Block::bordered()), area);
}

fn draw_confirm(frame: &mut Frame<'_>, app: &App) {
    let area = centered_rect(70, 10, frame.area());
    let text = vec![
        Line::styled(
            "Clean selected local target directories?",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::from(format!(
            "{} projects selected, about {} reclaimable.",
            app.selected_count(),
            format_bytes(app.total_reclaimable())
        )),
        Line::from(format!(
            "Includes {} selected projects inside collapsed groups.",
            app.hidden_selected_count()
        )),
        Line::from("Each target is checked before cleanup; unsafe targets are skipped."),
        Line::from(""),
        Line::from("y / Enter  Clean    n / Esc  Cancel"),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .block(Block::bordered().title(" Confirm cleanup ")),
        area,
    );
}

fn spinner(elapsed: Duration) -> &'static str {
    ["|", "/", "-", "\\"][(elapsed.as_millis() / 100 % 4) as usize]
}

fn elapsed_text(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::ProjectInfo;
    use ratatui::backend::TestBackend;
    use std::time::SystemTime;

    fn fixture() -> App {
        let projects = [
            ("repo", 10, 360),
            ("repo/crates/a", 20, 150),
            ("repo/crates/a/nested", 30, 80),
            ("other", 40, 90),
        ]
        .into_iter()
        .map(|(path, target_size, total_size)| ProjectInfo {
            root: PathBuf::from(path),
            manifest_path: PathBuf::from(path).join("Cargo.toml"),
            name: path.into(),
            target_size,
            total_size,
            last_modified: SystemTime::UNIX_EPOCH,
        })
        .collect();
        let mut app = App::new(PathBuf::from("."), SortKey::Size);
        app.set_report(ScanReport {
            scan_root: PathBuf::from("."),
            projects,
            warnings: Vec::new(),
        });
        app
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn collapsed_selection_includes_children_and_supports_partial_selection() {
        let mut app = fixture();
        assert_eq!(app.visible.len(), 2);
        assert_eq!(app.hidden_selected_count(), 2);
        assert_eq!(app.total_reclaimable(), 100);
        app.toggle_current();
        assert_eq!(app.selected, vec![false, false, false, true]);
        app.expand_current(false);
        app.move_cursor(1, 8);
        app.toggle_current();
        assert_eq!(app.selected, vec![false, true, true, true]);
        assert_eq!(app.selection_marker(0), "[-]");
        app.collapse_current();
        assert_eq!(app.current_index(), Some(0));
        app.collapse_current();
        assert_eq!(app.visible.len(), 2);
        assert_eq!(app.hidden_selected_count(), 2);
        assert_eq!(app.total_reclaimable(), 90);
    }

    #[test]
    fn clean_starts_visibly_and_updates_ancestor_sizes_without_losing_selection() {
        let mut app = fixture();
        app.prepare_clean();
        assert_eq!(app.mode, Mode::Cleaning);
        let pending = render(&app, 110, 24);
        assert!(pending.contains("Preparing selected projects"));
        assert!(pending.contains("0/4 completed"));
        assert!(pending.contains("queued"));
        let project = app.report.projects[2].clone();
        app.clean_event(CleanEvent::Started {
            index: 1,
            total: 4,
            project: project.clone(),
        });
        app.clean_event(CleanEvent::Progress {
            project: project.clone(),
            phase: CleanPhase::Validating,
        });
        assert!(render(&app, 110, 24).contains("Validating target safety"));
        app.clean_event(CleanEvent::Cleaned {
            project,
            reclaimed: 30,
        });
        assert_eq!(app.report.projects[0].total_size, 330);
        assert_eq!(app.report.projects[1].total_size, 120);
        assert_eq!(app.report.projects[2].total_size, 50);
        assert_eq!(app.total_reclaimable(), 70);
        assert_eq!(app.current_index(), Some(0));
        assert_eq!(app.selected, vec![true; 4]);
        app.clean_event(CleanEvent::Skipped {
            project: app.report.projects[1].clone(),
            message: "protected source".into(),
        });
        app.clean_event(CleanEvent::Failed {
            project: app.report.projects[0].clone(),
            message: "could not clean".into(),
        });
        assert_eq!(app.clean_progress.as_ref().unwrap().completed, 3);
        assert_eq!(app.row_status(0).0, "failed");
        app.clean_event(CleanEvent::Cleaned {
            project: app.report.projects[3].clone(),
            reclaimed: 40,
        });
        app.finish_clean(CleanSummary {
            cleaned: 2,
            failed: 1,
            skipped: 1,
        });
        assert_eq!(app.mode, Mode::Browsing);
        assert!(app.message.contains("2 cleaned, 1 failed, 1 skipped"));
    }

    #[test]
    fn all_states_render_at_small_terminal_sizes_and_empty_results() {
        let mut app = App::new(PathBuf::from("."), SortKey::Size);
        assert!(render(&app, 80, 24).contains("Finding Rust projects"));
        for (width, height) in [(1, 1), (40, 10), (80, 24), (120, 30)] {
            render(&app, width, height);
        }
        app.set_report(ScanReport {
            scan_root: PathBuf::from("."),
            projects: Vec::new(),
            warnings: Vec::new(),
        });
        assert!(render(&app, 80, 24).contains("No Rust projects found"));
        for mode in [Mode::Browsing, Mode::ConfirmClean, Mode::Cleaning] {
            let mut app = fixture();
            if mode == Mode::Cleaning {
                app.prepare_clean();
            }
            app.mode = mode;
            for (width, height) in [(1, 1), (40, 10), (80, 24), (120, 30)] {
                render(&app, width, height);
            }
        }
    }

    #[test]
    fn resizing_keeps_cursor_on_the_visible_cleaning_page() {
        let mut app = fixture();
        app.expand_current(false);
        app.move_cursor(1, 10);
        app.expand_current(false);
        app.move_cursor(2, 10);
        app.prepare_clean();
        let page = App::page_size(table_area(Rect::new(0, 0, 80, 16), Mode::Cleaning));
        app.ensure_cursor_visible(page);
        assert!(app.cursor >= app.offset && app.cursor < app.offset + page);
    }
}
