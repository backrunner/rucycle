use std::{
    io::{self, Stdout},
    time::Duration,
};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap},
    Frame, Terminal,
};

use crate::{
    cleaner::{clean_projects, CleanEvent, CleanPlan},
    format::{format_bytes, format_system_time, truncate_middle},
    scan::{sort_projects, ProjectInfo, ScanReport, SortKey},
};

#[derive(Clone, Debug)]
enum ProjectStatus {
    Idle,
    Cleaning,
    Cleaned,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Browsing,
    ConfirmClean,
    Cleaning,
}

struct App {
    report: ScanReport,
    selected: Vec<bool>,
    statuses: Vec<ProjectStatus>,
    cursor: usize,
    offset: usize,
    sort: SortKey,
    mode: Mode,
    message: String,
}

impl App {
    fn new(report: ScanReport, sort: SortKey) -> Self {
        let count = report.projects.len();
        Self {
            report,
            selected: vec![true; count],
            statuses: vec![ProjectStatus::Idle; count],
            cursor: 0,
            offset: 0,
            sort,
            mode: Mode::Browsing,
            message: "Space toggles, c cleans, s sorts, a selects all, q quits".to_string(),
        }
    }

    fn selected_count(&self) -> usize {
        self.selected.iter().filter(|selected| **selected).count()
    }

    fn total_reclaimable(&self) -> u64 {
        self.report
            .projects
            .iter()
            .zip(self.selected.iter())
            .filter_map(|(project, selected)| selected.then_some(project.target_size))
            .sum()
    }

    fn page_size(area: Rect) -> usize {
        area.height.saturating_sub(3).max(1) as usize
    }

    fn ensure_cursor_visible(&mut self, page_size: usize) {
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        if self.cursor >= self.offset + page_size {
            self.offset = self.cursor + 1 - page_size;
        }
    }

    fn move_cursor(&mut self, delta: isize, page_size: usize) {
        let max = self.report.projects.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(max);
        self.ensure_cursor_visible(page_size);
    }

    fn jump_cursor(&mut self, index: usize, page_size: usize) {
        let max = self.report.projects.len().saturating_sub(1);
        self.cursor = index.min(max);
        self.ensure_cursor_visible(page_size);
    }

    fn toggle_current(&mut self) {
        if let Some(selected) = self.selected.get_mut(self.cursor) {
            *selected = !*selected;
        }
    }

    fn toggle_all(&mut self) {
        let all_selected = self.selected.iter().all(|selected| *selected);
        for selected in &mut self.selected {
            *selected = !all_selected;
        }
    }

    fn toggle_sort(&mut self) {
        self.sort = match self.sort {
            SortKey::Size => SortKey::Modified,
            SortKey::Modified => SortKey::Size,
        };
        self.sort_rows_preserving_cursor();
        self.message = match self.sort {
            SortKey::Size => "Sorted by reclaimable target size".to_string(),
            SortKey::Modified => "Sorted by latest project modification".to_string(),
        };
    }

    fn sort_rows_preserving_cursor(&mut self) {
        let current_root = self
            .report
            .projects
            .get(self.cursor)
            .map(|project| project.root.clone());

        let rows: Vec<_> = self
            .report
            .projects
            .drain(..)
            .zip(self.selected.drain(..))
            .zip(self.statuses.drain(..))
            .map(|((project, selected), status)| (project, selected, status))
            .collect();

        let mut projects: Vec<ProjectInfo> =
            rows.iter().map(|(project, _, _)| project.clone()).collect();
        sort_projects(&mut projects, self.sort);

        self.report.projects = projects;
        self.selected = self
            .report
            .projects
            .iter()
            .map(|project| {
                rows.iter()
                    .find(|(old_project, _, _)| old_project.root == project.root)
                    .map(|(_, selected, _)| *selected)
                    .unwrap_or(true)
            })
            .collect();
        self.statuses = self
            .report
            .projects
            .iter()
            .map(|project| {
                rows.iter()
                    .find(|(old_project, _, _)| old_project.root == project.root)
                    .map(|(_, _, status)| status.clone())
                    .unwrap_or(ProjectStatus::Idle)
            })
            .collect();

        if let Some(root) = current_root {
            if let Some(index) = self
                .report
                .projects
                .iter()
                .position(|project| project.root == root)
            {
                self.cursor = index;
            }
        }
    }
}

pub fn run(report: ScanReport, sort: SortKey) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new(report, sort);

    let result = run_app(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app))?;

        if event::poll(Duration::from_millis(200))? {
            let Event::Key(key) = event::read()? else {
                continue;
            };

            if key.kind == KeyEventKind::Release {
                continue;
            }

            let size = terminal.size()?;
            let table_area = table_area(Rect::new(0, 0, size.width, size.height));
            let page_size = App::page_size(table_area);

            match app.mode {
                Mode::Browsing => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') => {
                        if app.selected_count() == 0 {
                            app.message = "Select at least one project before cleaning".to_string();
                        } else {
                            app.mode = Mode::ConfirmClean;
                        }
                    }
                    KeyCode::Char(' ') => app.toggle_current(),
                    KeyCode::Char('a') => app.toggle_all(),
                    KeyCode::Char('s') => {
                        app.toggle_sort();
                        app.ensure_cursor_visible(page_size);
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.move_cursor(1, page_size),
                    KeyCode::Up | KeyCode::Char('k') => app.move_cursor(-1, page_size),
                    KeyCode::PageDown => app.move_cursor(page_size as isize, page_size),
                    KeyCode::PageUp => app.move_cursor(-(page_size as isize), page_size),
                    KeyCode::Home => app.jump_cursor(0, page_size),
                    KeyCode::End => {
                        app.jump_cursor(app.report.projects.len().saturating_sub(1), page_size)
                    }
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(())
                    }
                    _ => {}
                },
                Mode::ConfirmClean => match key.code {
                    KeyCode::Char('y') | KeyCode::Enter => clean_selected(terminal, app)?,
                    KeyCode::Char('n') | KeyCode::Esc => {
                        app.mode = Mode::Browsing;
                        app.message = "Clean cancelled".to_string();
                    }
                    _ => {}
                },
                Mode::Cleaning => {}
            }
        }
    }
}

fn clean_selected(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    app.mode = Mode::Cleaning;
    app.message = "Cleaning selected projects...".to_string();
    let plan = CleanPlan::selected(&app.report.projects, &app.selected);

    clean_projects(plan, |event| {
        match event {
            CleanEvent::Started { project, .. } => {
                if let Some(index) = app
                    .report
                    .projects
                    .iter()
                    .position(|candidate| candidate.root == project.root)
                {
                    app.statuses[index] = ProjectStatus::Cleaning;
                    app.message = format!("Cleaning {}", project.root.display());
                }
            }
            CleanEvent::Cleaned { project, reclaimed } => {
                if let Some(index) = app
                    .report
                    .projects
                    .iter()
                    .position(|candidate| candidate.root == project.root)
                {
                    app.statuses[index] = ProjectStatus::Cleaned;
                    app.report.projects[index].target_size = 0;
                    app.report.projects[index].total_size = app.report.projects[index]
                        .total_size
                        .saturating_sub(reclaimed);
                }
            }
            CleanEvent::Failed { project, message } => {
                if let Some(index) = app
                    .report
                    .projects
                    .iter()
                    .position(|candidate| candidate.root == project.root)
                {
                    app.statuses[index] = ProjectStatus::Failed;
                    app.message = format!("Failed {}: {}", project.name, message);
                }
            }
        }

        let _ = terminal.draw(|frame| draw(frame, app));
    })?;

    let failed = app
        .statuses
        .iter()
        .filter(|status| matches!(status, ProjectStatus::Failed))
        .count();
    let cleaned = app
        .statuses
        .iter()
        .filter(|status| matches!(status, ProjectStatus::Cleaned))
        .count();

    app.mode = Mode::Browsing;
    app.message = format!("Clean complete: {cleaned} cleaned, {failed} failed");
    app.sort_rows_preserving_cursor();
    terminal.draw(|frame| draw(frame, app))?;
    Ok(())
}

fn draw(frame: &mut Frame<'_>, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(frame.area());

    draw_header(frame, app, chunks[0]);
    draw_table(frame, app, chunks[1]);
    draw_footer(frame, app, chunks[2]);

    if app.mode == Mode::ConfirmClean {
        draw_confirm(frame, app);
    }
}

fn draw_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let sort = match app.sort {
        SortKey::Size => "target size",
        SortKey::Modified => "modified date",
    };
    let warning = if app.report.warnings.is_empty() {
        String::new()
    } else {
        format!("  warnings: {}", app.report.warnings.len())
    };
    let line = Line::from(vec![
        Span::styled(
            "rucycle",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  {} projects  selected: {}  reclaimable: {}  sort: {}{}",
            app.report.projects.len(),
            app.selected_count(),
            format_bytes(app.total_reclaimable()),
            sort,
            warning
        )),
    ]);
    let block = Block::default().borders(Borders::ALL);
    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn draw_table(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let page_size = App::page_size(area);
    let offset = app.offset.min(app.report.projects.len().saturating_sub(1));
    let end = (offset + page_size).min(app.report.projects.len());
    let scan_root = &app.report.scan_root;
    let path_width = area.width.saturating_sub(62).max(14) as usize;

    let rows = app.report.projects[offset..end]
        .iter()
        .enumerate()
        .map(|(row_index, project)| {
            let index = offset + row_index;
            let marker = if index == app.cursor { ">" } else { " " };
            let checked = if app.selected[index] { "[x]" } else { "[ ]" };
            let display_path = project.display_path(scan_root).display().to_string();
            let status = match &app.statuses[index] {
                ProjectStatus::Idle => "",
                ProjectStatus::Cleaning => "cleaning",
                ProjectStatus::Cleaned => "cleaned",
                ProjectStatus::Failed => "failed",
            };
            let mut row = Row::new(vec![
                Cell::from(marker),
                Cell::from(checked),
                Cell::from(truncate_middle(&display_path, path_width)),
                Cell::from(format_bytes(project.target_size)),
                Cell::from(format_bytes(project.total_size)),
                Cell::from(format_system_time(project.last_modified)),
                Cell::from(status),
            ]);

            if index == app.cursor {
                row = row.style(Style::default().bg(Color::DarkGray).fg(Color::White));
            } else if matches!(app.statuses[index], ProjectStatus::Cleaned) {
                row = row.style(Style::default().fg(Color::Green));
            } else if matches!(app.statuses[index], ProjectStatus::Failed) {
                row = row.style(Style::default().fg(Color::Red));
            }

            row
        });

    let header = Row::new(vec![
        Cell::from(""),
        Cell::from("sel"),
        Cell::from("project"),
        Cell::from("target"),
        Cell::from("total"),
        Cell::from("modified"),
        Cell::from("status"),
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(14),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(17),
            Constraint::Length(9),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Projects"));

    frame.render_widget(table, area);
}

fn draw_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let page = if app.report.projects.is_empty() {
        "0/0".to_string()
    } else {
        format!("{}/{}", app.cursor + 1, app.report.projects.len())
    };
    let text = vec![
        Line::from(app.message.as_str()),
        Line::from(format!(
            "{page}   arrows/jk move   PgUp/PgDn page   Space toggle   a all/none   s sort   c clean   q quit"
        )),
    ];
    let block = Block::default().borders(Borders::ALL);
    frame.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_confirm(frame: &mut Frame<'_>, app: &App) {
    let area = centered_rect(62, 8, frame.area());
    let text = vec![
        Line::from(Span::styled(
            "Run cargo clean?",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "{} selected, about {} reclaimable from target directories.",
            app.selected_count(),
            format_bytes(app.total_reclaimable())
        )),
        Line::from("Press y or Enter to continue, n or Esc to cancel."),
    ];
    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title("Confirm"))
        .wrap(Wrap { trim: true });
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn table_area(area: Rect) -> Rect {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area)[1]
}
