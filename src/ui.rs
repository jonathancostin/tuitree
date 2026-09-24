//! Rendering. Draws the app state and records clickable areas in `app.hits` for the mouse.

use std::path::Path;
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Cell, Clear, HighlightSpacing, List, ListItem, Paragraph, Row, Table, TableState, Wrap,
};

use crate::app::{
    App, Button, Confirm, Focus, Hits, JobEntry, JobState, Mode, PathInput, PrFilesState, Project,
    Rows, Tab, Tone, branch_label,
};
use crate::config::display_path;
use crate::gh::PullRequest;
use crate::git::{Merged, SyncStatus, WorktreeInfo};
use crate::model::{FileStat, Totals};

const ACCENT: Color = Color::Cyan;
const ADDED: Color = Color::Green;
const DELETED: Color = Color::Red;
const WARN: Color = Color::Yellow;
const MUTED: Color = Color::DarkGray;
/// Most conflicting files listed above the file table of a worktree.
const MAX_CONFLICTS_SHOWN: usize = 8;
/// Tallest the queue panel gets, borders included.
const MAX_QUEUE_HEIGHT: u16 = 14;

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.hits = Hits::default();
    let queue_lines = if app.show_queue {
        queue_lines(&app.jobs, &app.projects, app.spinner())
    } else {
        Vec::new()
    };
    let queue_height = if app.show_queue {
        (queue_lines.len() as u16 + 2).min(MAX_QUEUE_HEIGHT)
    } else {
        0
    };
    let [body, queue, footer] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(queue_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    if app.show_queue {
        draw_queue(frame, &app.jobs, queue_lines, queue);
    }
    let [side, main] =
        Layout::horizontal([Constraint::Length(30), Constraint::Fill(1)]).areas(body);
    draw_projects(frame, app, side);
    draw_main(frame, app, main);
    draw_footer(frame, app, footer);
    let mut buttons = Vec::new();
    match &app.mode {
        Mode::Normal => {}
        Mode::Help => draw_help(frame),
        Mode::AddProject(input) => buttons = draw_add_project(frame, input),
        Mode::Confirm(confirm) => buttons = draw_confirm(frame, confirm),
    }
    app.hits.buttons = buttons;
}

fn pane(title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
    let border = if focused { ACCENT } else { MUTED };
    Block::bordered()
        .border_style(Style::new().fg(border))
        .title(title.into().bold())
}

/// Where the data rows of a table with a one-line header were drawn.
fn table_rows(area: Rect, state: &TableState) -> Rows {
    Rows {
        area: Rect {
            y: area.y + 1,
            height: area.height.saturating_sub(1),
            ..area
        },
        offset: state.offset(),
        row_height: 1,
    }
}

fn draw_projects(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = pane(" Projects ", app.focus == Focus::Projects);
    if app.projects.is_empty() {
        let hint = Paragraph::new("No projects yet.\nPress a to add a git repository.")
            .fg(MUTED)
            .wrap(Wrap { trim: true })
            .block(block);
        frame.render_widget(hint, area);
        return;
    }
    let spinner = app.spinner();
    let items: Vec<ListItem> = app
        .projects
        .iter()
        .map(|project| {
            let name = project.path.file_name().map_or_else(
                || project.path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            let busy = if project.is_loading() || app.has_active_jobs(&project.path) {
                spinner
            } else {
                ""
            };
            let remote = match (&project.github, &project.default_branch) {
                (Some(repo), _) => Span::styled(repo.clone(), Style::new().fg(MUTED)),
                (None, Some(_)) => Span::styled("no GitHub remote", Style::new().fg(MUTED)),
                (None, None) => Span::raw(""),
            };
            ListItem::new(Text::from(vec![
                Line::from(vec![
                    Span::raw(name).bold(),
                    Span::raw(" "),
                    busy.fg(ACCENT),
                ]),
                Line::from(vec![Span::raw("  "), remote]),
            ]))
        })
        .collect();
    let inner = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_symbol("▶ ")
        .highlight_spacing(HighlightSpacing::Always)
        .highlight_style(Style::new().fg(ACCENT));
    frame.render_stateful_widget(list, area, &mut app.project_list);
    app.hits.projects = Some(Rows {
        area: inner,
        offset: app.project_list.offset(),
        row_height: 2,
    });
}

fn draw_main(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus != Focus::Projects;
    let spinner = app.spinner();
    let home = app.home.clone();
    let (focus, tab) = (app.focus, app.tab);
    let Some(index) = app.project_list.selected() else {
        let hint = Paragraph::new("No project selected. Press a to add a git repository.")
            .fg(MUTED)
            .block(pane(" tuitree ", focused));
        frame.render_widget(hint, area);
        return;
    };
    let project = &mut app.projects[index];

    let block = pane(
        format!(" {} ", display_path(&project.path, home.as_deref())),
        focused,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [info, tabs, _, content] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(inner);

    frame.render_widget(info_line(project, &app.jobs, spinner), info);
    app.hits.tabs = draw_tabs(frame, project, tab, spinner, tabs);

    let (list_area, detail_area) = if focus == Focus::Detail {
        let [list, detail] =
            Layout::vertical([Constraint::Percentage(40), Constraint::Fill(1)]).areas(content);
        (list, Some(detail))
    } else {
        (content, None)
    };
    let list_focused = focus == Focus::List;
    match tab {
        Tab::Worktrees => {
            let drawn = draw_worktrees(
                frame,
                project,
                &app.jobs,
                list_area,
                list_focused,
                home.as_deref(),
                spinner,
            );
            app.hits.list = drawn.map(|(rows, _)| rows);
            app.hits.marks = drawn.map(|(_, marks)| marks);
        }
        Tab::Prs => app.hits.list = draw_prs(frame, project, list_area, list_focused, spinner),
    }
    if let Some(detail_area) = detail_area {
        let project = &app.projects[index];
        let detail = match tab {
            Tab::Worktrees => project
                .selected_worktree()
                .map(|wt| worktree_detail(wt, project)),
            Tab::Prs => project
                .selected_pr()
                .map(|pr| pr_detail(pr, project.pr_files.get(&pr.number))),
        };
        if let Some(detail) = detail {
            app.hits.detail =
                draw_detail(frame, detail, detail_area, &mut app.detail_table, spinner);
        }
    }
}

fn info_line(project: &Project, jobs: &[JobEntry], spinner: &str) -> Line<'static> {
    let mut spans = vec![Span::raw("GitHub ").fg(MUTED)];
    spans.push(match (&project.github, &project.default_branch) {
        (Some(repo), _) => Span::raw(repo.clone()).bold(),
        (None, Some(_)) => Span::raw("none (origin is not on GitHub)"),
        (None, None) => Span::raw("…").fg(MUTED),
    });
    spans.push(Span::raw("  base ").fg(MUTED));
    spans.push(Span::raw(project.base_ref()).bold());
    spans.push(Span::raw("  "));
    let mine = || jobs.iter().filter(|job| job.project == project.path);
    let running = mine().filter(|job| job.state == JobState::Running).count();
    let pending = mine().filter(|job| job.state == JobState::Pending).count();
    if let Some(activity) = project.activity {
        spans.push(Span::raw(format!("{spinner} {activity}")).fg(ACCENT));
    } else if let Some(fetch) = &project.last_fetch {
        spans.push(match &fetch.result {
            Ok(()) => Span::raw(format!("fetched {}", age(fetch.at.elapsed()))).fg(MUTED),
            Err(err) => {
                let reason = err.lines().last().unwrap_or("unknown error");
                Span::raw(format!("fetch failed: {reason}")).fg(DELETED)
            }
        });
    }
    if running + pending > 0 {
        spans.push(Span::raw(format!("  jobs: {running} running, {pending} queued (Q)")).fg(WARN));
    }
    Line::from(spans)
}

/// `Name (count)`, a spinner while loading, or just the name when loading failed.
fn tab_title<T>(
    name: &str,
    state: &Option<Result<Vec<T>, String>>,
    loading: bool,
    spinner: &str,
) -> String {
    match state {
        _ if loading => format!(" {name} {spinner} "),
        Some(Ok(items)) => format!(" {name} ({}) ", items.len()),
        _ => format!(" {name} "),
    }
}

/// Draws the tab titles side by side and returns where each one landed.
fn draw_tabs(
    frame: &mut Frame,
    project: &Project,
    selected: Tab,
    spinner: &str,
    area: Rect,
) -> Vec<(Rect, Tab)> {
    let worktrees_loading = project.refreshing && project.worktrees.is_none();
    let tabs = [
        (
            Tab::Worktrees,
            tab_title("Worktrees", &project.worktrees, worktrees_loading, spinner),
        ),
        (
            Tab::Prs,
            tab_title("Pull requests", &project.prs, project.prs_loading, spinner),
        ),
    ];
    let widths = tabs
        .iter()
        .map(|(_, title)| Constraint::Length(Line::from(title.as_str()).width() as u16));
    let areas = Layout::horizontal(widths).spacing(1).split(area);
    tabs.into_iter()
        .zip(areas.iter())
        .map(|((tab, title), &rect)| {
            let style = if tab == selected {
                Style::new().fg(Color::Black).bg(ACCENT).bold()
            } else {
                Style::new().fg(MUTED)
            };
            frame.render_widget(Paragraph::new(title).style(style), rect);
            (rect, tab)
        })
        .collect()
}

fn row_highlight(focused: bool) -> Style {
    if focused {
        Style::new()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    }
}

fn header(cells: &[&'static str]) -> Row<'static> {
    Row::new(cells.iter().copied()).style(Style::new().fg(MUTED).add_modifier(Modifier::UNDERLINED))
}

fn sync_cell(sync: Option<&SyncStatus>, spinner: &str) -> Cell<'static> {
    match sync {
        None => Cell::from(spinner.to_string()).fg(MUTED),
        Some(SyncStatus::UpToDate) => Cell::from("up to date").fg(MUTED),
        Some(SyncStatus::Ready { ff: true }) => Cell::from("ready (ff)").fg(ADDED),
        Some(SyncStatus::Ready { ff: false }) => Cell::from("ready").fg(ADDED),
        Some(SyncStatus::Conflicts(files)) => {
            Cell::from(format!("conflicts {}", files.len())).fg(DELETED)
        }
        Some(SyncStatus::Dirty) => Cell::from("dirty").fg(WARN),
        Some(SyncStatus::Unknown(_)) => Cell::from("?").fg(MUTED),
    }
}

/// Already merged (squash, rebase or a merged PR) replaces the sync status.
fn merged_cell(merged: Merged) -> Cell<'static> {
    match merged {
        Merged::Pr(number) => Cell::from(format!("merged #{number}")).fg(ADDED),
        Merged::Content => Cell::from("merged").fg(ADDED),
    }
}

/// A queued or running job replaces the row's sync status.
fn job_cell(job: &JobEntry, spinner: &str) -> Cell<'static> {
    match job.state {
        JobState::Running => Cell::from(format!("{spinner} {}", job.verb.doing())).fg(ACCENT),
        _ => Cell::from(format!("queued {}", job.verb.name())).fg(WARN),
    }
}

/// Width of the table's selection column (`▶ `); the mark column starts right after it.
const HIGHLIGHT_WIDTH: u16 = 2;

/// Draws the worktree table; returns its rows and its mark column for the mouse.
fn draw_worktrees(
    frame: &mut Frame,
    project: &mut Project,
    jobs: &[JobEntry],
    area: Rect,
    focused: bool,
    home: Option<&Path>,
    spinner: &str,
) -> Option<(Rows, Rect)> {
    let list = match &project.worktrees {
        None => {
            message(frame, area, format!("{spinner} Reading worktrees…"), ACCENT);
            return None;
        }
        Some(Err(err)) => {
            message(
                frame,
                area,
                format!("Could not list worktrees: {err}"),
                DELETED,
            );
            return None;
        }
        Some(Ok(list)) => list,
    };
    let labels: Vec<String> = list.iter().map(branch_label).collect();
    let branch_width = labels
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(6, 40);
    let rows: Vec<Row> = list
        .iter()
        .zip(labels)
        .map(|(info, label)| {
            let path = display_path(&info.worktree.path, home);
            let mark = if project.marked.contains(&info.worktree.path) {
                Cell::from("●").fg(ACCENT)
            } else {
                Cell::from(" ")
            };
            let job = jobs
                .iter()
                .find(|job| job.is_active() && job.worktree == info.worktree.path);
            match &info.stats {
                Ok(stats) => {
                    let totals = Totals::of(&stats.files);
                    let status = match (job, stats.merged) {
                        (Some(job), _) => job_cell(job, spinner),
                        (None, Some(merged)) => merged_cell(merged),
                        (None, None) => sync_cell(stats.sync.as_ref(), spinner),
                    };
                    let row = Row::new(vec![
                        mark,
                        Cell::from(label),
                        count_cell("↑", stats.ahead, ACCENT),
                        count_cell("↓", stats.behind, WARN),
                        status,
                        Cell::from(totals.files.to_string()),
                        count_cell("+", totals.added, ADDED),
                        count_cell("-", totals.deleted, DELETED),
                        Cell::from(path),
                    ]);
                    // Merged rows are done work: dimmed, safe to clean up.
                    if stats.merged.is_some() {
                        row.style(Style::new().add_modifier(Modifier::DIM))
                    } else {
                        row
                    }
                }
                Err(err) => Row::new(vec![
                    mark,
                    Cell::from(label),
                    Cell::from(""),
                    Cell::from(""),
                    job.map_or_else(|| Cell::from(""), |job| job_cell(job, spinner)),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(format!("{path}  ⚠ {err}")).fg(DELETED),
                ]),
            }
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(branch_width as u16),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(14),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Fill(1),
        ],
    )
    .header(header(&[
        "", "Branch", "Ahead", "Behind", "Sync", "Files", "Added", "Deleted", "Path",
    ]))
    .row_highlight_style(row_highlight(focused))
    .highlight_symbol("▶ ")
    .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, area, &mut project.worktree_table);
    let rows = table_rows(area, &project.worktree_table);
    let marks = Rect {
        x: rows.area.x + HIGHLIGHT_WIDTH,
        width: 2.min(rows.area.width.saturating_sub(HIGHLIGHT_WIDTH)),
        ..rows.area
    };
    Some((rows, marks))
}

fn draw_prs(
    frame: &mut Frame,
    project: &mut Project,
    area: Rect,
    focused: bool,
    spinner: &str,
) -> Option<Rows> {
    let prs = match &project.prs {
        None => {
            message(
                frame,
                area,
                format!("{spinner} Loading pull requests…"),
                ACCENT,
            );
            return None;
        }
        Some(Err(err)) => {
            message(frame, area, err.clone(), WARN);
            return None;
        }
        Some(Ok(prs)) if prs.is_empty() => {
            message(frame, area, "No open pull requests.".to_string(), MUTED);
            return None;
        }
        Some(Ok(prs)) => prs,
    };
    let widest = |f: fn(&PullRequest) -> usize, cap: usize| {
        prs.iter().map(f).max().unwrap_or(0).clamp(6, cap) as u16
    };
    let branch_width = widest(|pr| pr.head_ref_name.chars().count(), 32);
    let author_width = widest(
        |pr| pr.author.as_ref().map_or(0, |a| a.login.chars().count()),
        20,
    );
    let rows: Vec<Row> = prs
        .iter()
        .map(|pr| {
            Row::new(vec![
                Cell::from(format!("#{}", pr.number)).fg(ACCENT),
                Cell::from(pr.title.clone()),
                Cell::from(pr.head_ref_name.clone()),
                Cell::from(
                    pr.author
                        .as_ref()
                        .map_or_else(String::new, |a| a.login.clone()),
                ),
                if pr.is_draft {
                    Cell::from("draft").fg(WARN)
                } else {
                    Cell::from("")
                },
                Cell::from(pr.changed_files.to_string()),
                count_cell("+", pr.additions, ADDED),
                count_cell("-", pr.deletions, DELETED),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Fill(1),
            Constraint::Length(branch_width),
            Constraint::Length(author_width),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Length(8),
            Constraint::Length(8),
        ],
    )
    .header(header(&[
        "PR", "Title", "Branch", "Author", "Draft", "Files", "Added", "Deleted",
    ]))
    .row_highlight_style(row_highlight(focused))
    .highlight_symbol("▶ ")
    .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, area, &mut project.pr_table);
    Some(table_rows(area, &project.pr_table))
}

/// What the detail pane shows: a title, optional notes above the files, and either the files,
/// a loading state or an error.
struct Detail<'a> {
    title: String,
    notes: Vec<Line<'static>>,
    files: Result<Option<&'a [FileStat]>, String>,
}

fn worktree_detail<'a>(info: &'a WorktreeInfo, project: &Project) -> Detail<'a> {
    let label = branch_label(info);
    let base = project.base_ref();
    match &info.stats {
        Ok(stats) => {
            let t = Totals::of(&stats.files);
            let mut notes = Vec::new();
            if let Some(merged) = stats.merged {
                let how = match merged {
                    Merged::Pr(number) => format!("PR #{number} was merged"),
                    Merged::Content => format!("merging it into {base} would change nothing"),
                };
                notes.push(
                    Line::from(format!(
                        "✓ Already merged: {how}. Safe to clean up (x, or M to mark all merged)."
                    ))
                    .fg(ADDED),
                );
            } else if let Some(SyncStatus::Conflicts(conflicts)) = &stats.sync {
                notes.push(
                    Line::from(format!(
                        "Merging {base} would conflict in {} file(s):",
                        conflicts.len()
                    ))
                    .fg(DELETED)
                    .bold(),
                );
                notes.extend(
                    conflicts
                        .iter()
                        .take(MAX_CONFLICTS_SHOWN)
                        .map(|file| Line::from(format!("  ✗ {file}")).fg(DELETED)),
                );
                if conflicts.len() > MAX_CONFLICTS_SHOWN {
                    let more = conflicts.len() - MAX_CONFLICTS_SHOWN;
                    notes.push(Line::from(format!("  … and {more} more")).fg(DELETED));
                }
            }
            Detail {
                title: format!(
                    " {label}: {} files +{} -{} vs merge-base with {base} (incl. uncommitted and untracked) ",
                    t.files, t.added, t.deleted,
                ),
                notes,
                files: Ok(Some(&stats.files)),
            }
        }
        Err(err) => Detail {
            title: format!(" {label} "),
            notes: Vec::new(),
            files: Err(err.clone()),
        },
    }
}

fn pr_detail<'a>(pr: &PullRequest, state: Option<&'a PrFilesState>) -> Detail<'a> {
    let files = match state {
        None | Some(None) => Ok(None),
        Some(Some(Ok(files))) => Ok(Some(&files[..])),
        Some(Some(Err(err))) => Err(err.clone()),
    };
    Detail {
        title: format!(
            " #{} {}: {} files +{} -{} ",
            pr.number, pr.title, pr.changed_files, pr.additions, pr.deletions
        ),
        notes: Vec::new(),
        files,
    }
}

fn draw_detail(
    frame: &mut Frame,
    detail: Detail,
    area: Rect,
    state: &mut TableState,
    spinner: &str,
) -> Option<Rows> {
    let block = pane(detail.title, true);
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    if !detail.notes.is_empty() {
        let height = detail.notes.len() as u16 + 1;
        let [notes, rest] =
            Layout::vertical([Constraint::Length(height), Constraint::Fill(1)]).areas(inner);
        frame.render_widget(Paragraph::new(detail.notes), notes);
        inner = rest;
    }
    let files = match detail.files {
        Ok(None) => {
            message(frame, inner, format!("{spinner} Loading files…"), ACCENT);
            return None;
        }
        Err(err) => {
            message(frame, inner, err, DELETED);
            return None;
        }
        Ok(Some([])) => {
            message(frame, inner, "No changes.".to_string(), MUTED);
            return None;
        }
        Ok(Some(files)) => files,
    };
    let rows: Vec<Row> = files
        .iter()
        .map(|file| {
            let (added, deleted) = match (file.added, file.deleted) {
                (Some(added), Some(deleted)) => (
                    count_cell("+", added, ADDED),
                    count_cell("-", deleted, DELETED),
                ),
                _ => (Cell::from("bin").fg(MUTED), Cell::from("")),
            };
            let mut path = Line::from(file.path.clone());
            if file.untracked {
                path.push_span(Span::raw(" (untracked)").fg(MUTED));
            }
            Row::new(vec![added, deleted, Cell::from(path)])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Fill(1),
        ],
    )
    .header(header(&["Added", "Deleted", "File"]))
    .row_highlight_style(row_highlight(true))
    .highlight_symbol("▶ ")
    .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, inner, state);
    Some(table_rows(inner, state))
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let line = match app.status() {
        Some(status) => Line::from(status.to_string()).fg(WARN),
        None => {
            let hints = match (app.focus, app.tab) {
                (Focus::Projects, _) => {
                    "j/k move · l/enter open · tab worktrees/PRs · a add · d remove · r refresh · Q queue · ? help · q quit"
                }
                (Focus::List | Focus::Detail, Tab::Worktrees) => {
                    "j/k move · l/enter files · h/esc back · space mark · M mark merged · s sync · x delete · Q queue · tab PRs · r refresh · ? help · q quit"
                }
                (Focus::List, Tab::Prs) => {
                    "j/k move · l/enter files · h/esc back · tab worktrees · r refresh · Q queue · ? help · q quit"
                }
                (Focus::Detail, Tab::Prs) => {
                    "j/k move · h/esc back · tab worktrees · r refresh · Q queue · ? help · q quit"
                }
            };
            Line::from(hints).fg(MUTED)
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn popup(frame: &mut Frame, title: &str, width: u16, height: u16, color: Color) -> Rect {
    let area = frame
        .area()
        .centered(Constraint::Length(width), Constraint::Length(height));
    let block = Block::bordered()
        .border_style(Style::new().fg(color))
        .title(Line::from(title).bold());
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    inner
}

/// Draws `[ yes ]  [ no ]` on the row `area` and returns where they landed.
fn draw_buttons(
    frame: &mut Frame,
    area: Rect,
    yes: &str,
    no: &str,
    yes_color: Color,
) -> Vec<(Rect, Button)> {
    let buttons = [
        (
            Button::Yes,
            format!("[ {yes} ]"),
            Style::new().fg(Color::Black).bg(yes_color).bold(),
        ),
        (
            Button::No,
            format!("[ {no} ]"),
            Style::new().fg(Color::White).bg(MUTED),
        ),
    ];
    let widths = buttons
        .iter()
        .map(|(_, label, _)| Constraint::Length(Line::from(label.as_str()).width() as u16));
    let areas = Layout::horizontal(widths).spacing(2).split(area);
    buttons
        .into_iter()
        .zip(areas.iter())
        .map(|((button, label, style), &rect)| {
            frame.render_widget(Paragraph::new(label).style(style), rect);
            (rect, button)
        })
        .collect()
}

fn draw_add_project(frame: &mut Frame, input: &PathInput) -> Vec<(Rect, Button)> {
    let inner = popup(frame, " Add project ", 72, 7, ACCENT);
    let [prompt, field, error, _, buttons] =
        Layout::vertical([Constraint::Length(1); 5]).areas(inner);
    frame.render_widget(
        Paragraph::new("Path to a local git repository (~ is expanded, ctrl+u clears):"),
        prompt,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("> ").fg(ACCENT),
            Span::raw(input.text.clone()),
        ])),
        field,
    );
    frame.render_widget(
        Paragraph::new(input.error.clone().unwrap_or_default()).fg(DELETED),
        error,
    );
    let cursor_x = field.x + 2 + input.text.chars().count() as u16;
    frame.set_cursor_position((cursor_x.min(field.right().saturating_sub(1)), field.y));
    draw_buttons(frame, buttons, "enter Add", "esc Cancel", ACCENT)
}

fn draw_confirm(frame: &mut Frame, confirm: &Confirm) -> Vec<(Rect, Button)> {
    let width = frame.area().width.saturating_sub(4).min(84);
    let text_width = width.saturating_sub(2).max(1) as usize;
    let text_height: usize = confirm
        .lines
        .iter()
        .map(|(line, _)| {
            Line::from(line.as_str())
                .width()
                .div_ceil(text_width)
                .max(1)
        })
        .sum();
    let color = if confirm.danger { DELETED } else { ACCENT };
    let height = (text_height as u16 + 4).min(frame.area().height.saturating_sub(2));
    let inner = popup(frame, &confirm.title, width, height, color);
    let [text, _, buttons] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let lines: Vec<Line> = confirm
        .lines
        .iter()
        .map(|(line, tone)| {
            let color = match tone {
                Tone::Normal => Color::Reset,
                Tone::Muted => MUTED,
                Tone::Warn => WARN,
            };
            Line::from(line.clone()).fg(color)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), text);
    draw_buttons(
        frame,
        buttons,
        &format!("y {}", confirm.yes),
        "n Cancel",
        color,
    )
}

fn draw_help(frame: &mut Frame) {
    let keys = [
        ("j / k, ↓ / ↑", "move"),
        ("tab", "switch Worktrees / Pull requests"),
        ("enter / l", "open (project → list → files)"),
        ("esc / h", "back"),
        ("space", "mark / unmark a worktree row"),
        ("M", "mark every merged worktree (again: unmark)"),
        ("s", "sync marked (or selected) worktrees with origin"),
        ("x", "delete marked (or selected) worktrees + branches"),
        ("Q", "show / hide the job queue"),
        ("a", "add project"),
        ("d", "remove project (asks first)"),
        ("r", "refresh: git fetch, worktree stats, PRs"),
        ("mouse", "click to select, click again to open, wheel"),
        ("?", "this help"),
        ("q", "quit"),
    ];
    let mut lines: Vec<Line> = keys
        .iter()
        .map(|(key, what)| Line::from(vec![format!("{key:<14}").fg(ACCENT), Span::raw(*what)]))
        .collect();
    lines.push(Line::from(""));
    lines.push(
        Line::from(
            "Worktree stats compare against the merge-base with origin/<default branch>, \
         including staged, unstaged and untracked files. Untracked text files count every \
         line as added; binary files show \"bin\".",
        )
        .fg(MUTED),
    );
    lines.push(Line::from(""));
    lines.push(Line::from("press any key to close").fg(MUTED));
    let inner = popup(frame, " Help ", 64, 25, ACCENT);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

/// The queue panel's text: one line per job, and git's full error under failed ones.
fn queue_lines(jobs: &[JobEntry], projects: &[Project], spinner: &str) -> Vec<Line<'static>> {
    if jobs.is_empty() {
        return vec![
            Line::from("No jobs yet. x deletes and s syncs worktrees in the background.").fg(MUTED),
        ];
    }
    let project_name = |path: &Path| {
        projects
            .iter()
            .find(|p| p.path == path)
            .and_then(|p| p.path.file_name())
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    };
    let mut lines = Vec::new();
    for job in jobs {
        let (state, color, note) = match &job.state {
            JobState::Pending => ("… pending ".to_string(), WARN, String::new()),
            JobState::Running => (format!("{spinner} running "), ACCENT, String::new()),
            JobState::Done(summary) => ("✓ done    ".to_string(), ADDED, summary.clone()),
            JobState::Failed(_) => ("✗ failed  ".to_string(), DELETED, String::new()),
            JobState::Skipped(reason) => ("– skipped ".to_string(), MUTED, reason.clone()),
        };
        lines.push(Line::from(vec![
            Span::raw(state).fg(color).bold(),
            Span::raw(format!(" {:<6} ", job.verb.name())),
            Span::raw(format!("{:<24}", job.label)).bold(),
            Span::raw(format!(" {:<14} ", project_name(&job.project))).fg(MUTED),
            Span::raw(note).fg(MUTED),
        ]));
        if let JobState::Failed(err) = &job.state {
            lines.extend(
                err.lines()
                    .map(|line| Line::from(format!("    {line}")).fg(DELETED)),
            );
        }
    }
    lines
}

/// Draws the queue panel, scrolled so the newest lines are visible.
fn draw_queue(frame: &mut Frame, jobs: &[JobEntry], lines: Vec<Line<'static>>, area: Rect) {
    let count =
        |wanted: fn(&JobState) -> bool| jobs.iter().filter(|job| wanted(&job.state)).count();
    let title = format!(
        " Queue: {} running, {} pending, {} failed  (Q hides) ",
        count(|s| *s == JobState::Running),
        count(|s| *s == JobState::Pending),
        count(|s| matches!(s, JobState::Failed(_))),
    );
    let block = pane(title, false);
    let visible = area.height.saturating_sub(2) as usize;
    let skip = lines.len().saturating_sub(visible);
    let shown: Vec<Line> = lines.into_iter().skip(skip).collect();
    frame.render_widget(Paragraph::new(shown).block(block), area);
}

fn message(frame: &mut Frame, area: Rect, text: String, color: Color) {
    frame.render_widget(
        Paragraph::new(text).fg(color).wrap(Wrap { trim: true }),
        area,
    );
}

fn count_cell(sign: &str, value: u64, color: Color) -> Cell<'static> {
    let style = if value == 0 {
        Style::new().fg(MUTED)
    } else {
        Style::new().fg(color)
    };
    Cell::from(format!("{sign}{value}")).style(style)
}

fn age(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        0..=4 => "just now".to_string(),
        secs @ 5..=59 => format!("{secs}s ago"),
        secs @ 60..=3599 => format!("{}m ago", secs / 60),
        secs => format!("{}h ago", secs / 3600),
    }
}
