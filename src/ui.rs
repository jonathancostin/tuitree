//! Rendering. Pure function of the app state (plus selection state for scrolling tables).

use std::path::Path;
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Cell, Clear, HighlightSpacing, List, ListItem, Paragraph, Row, Table, TableState, Tabs,
    Wrap,
};

use crate::app::{App, Focus, Mode, PathInput, PrFilesState, Project, Tab};
use crate::config::display_path;
use crate::gh::PullRequest;
use crate::git::WorktreeInfo;
use crate::model::{FileStat, Totals};

const ACCENT: Color = Color::Cyan;
const ADDED: Color = Color::Green;
const DELETED: Color = Color::Red;
const MUTED: Color = Color::DarkGray;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [body, footer] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
    let [side, main] =
        Layout::horizontal([Constraint::Length(30), Constraint::Fill(1)]).areas(body);
    draw_projects(frame, app, side);
    draw_main(frame, app, main);
    draw_footer(frame, app, footer);
    match &app.mode {
        Mode::Normal => {}
        Mode::Help => draw_help(frame),
        Mode::AddProject(input) => draw_add_project(frame, input),
        Mode::ConfirmRemove => draw_confirm_remove(frame, app),
    }
}

fn pane(title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
    let border = if focused { ACCENT } else { MUTED };
    Block::bordered()
        .border_style(Style::new().fg(border))
        .title(title.into().bold())
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
            let busy = if project.is_loading() { spinner } else { "" };
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
    let list = List::new(items)
        .block(block)
        .highlight_symbol("▶ ")
        .highlight_spacing(HighlightSpacing::Always)
        .highlight_style(Style::new().fg(ACCENT));
    frame.render_stateful_widget(list, area, &mut app.project_list);
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
    let [info, tabs, content] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Fill(1),
    ])
    .areas(inner);

    frame.render_widget(info_line(project, spinner), info);
    frame.render_widget(tab_bar(project, tab, spinner), tabs);

    let (list_area, detail_area) = if focus == Focus::Detail {
        let [list, detail] =
            Layout::vertical([Constraint::Percentage(40), Constraint::Fill(1)]).areas(content);
        (list, Some(detail))
    } else {
        (content, None)
    };
    let list_focused = focus == Focus::List;
    match tab {
        Tab::Worktrees => draw_worktrees(
            frame,
            project,
            list_area,
            list_focused,
            home.as_deref(),
            spinner,
        ),
        Tab::Prs => draw_prs(frame, project, list_area, list_focused, spinner),
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
            draw_detail(frame, detail, detail_area, &mut app.detail_table, spinner);
        }
    }
}

fn info_line(project: &Project, spinner: &str) -> Line<'static> {
    let mut spans = vec![Span::raw("GitHub ").fg(MUTED)];
    spans.push(match (&project.github, &project.default_branch) {
        (Some(repo), _) => Span::raw(repo.clone()).bold(),
        (None, Some(_)) => Span::raw("none (origin is not on GitHub)"),
        (None, None) => Span::raw("…").fg(MUTED),
    });
    spans.push(Span::raw("  base ").fg(MUTED));
    spans.push(Span::raw(project.base_ref()).bold());
    spans.push(Span::raw("  "));
    if project.fetching {
        spans.push(Span::raw(format!("{spinner} fetching origin…")).fg(ACCENT));
    } else if project.git_loading {
        spans.push(Span::raw(format!("{spinner} updating worktrees…")).fg(ACCENT));
    } else if let Some(fetch) = &project.last_fetch {
        spans.push(match &fetch.result {
            Ok(()) => Span::raw(format!("fetched {}", age(fetch.at.elapsed()))).fg(MUTED),
            Err(err) => {
                let reason = err.lines().last().unwrap_or("unknown error");
                Span::raw(format!("fetch failed: {reason}")).fg(DELETED)
            }
        });
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
        _ if loading => format!("{name} {spinner}"),
        Some(Ok(items)) => format!("{name} ({})", items.len()),
        _ => name.to_string(),
    }
}

fn tab_bar(project: &Project, tab: Tab, spinner: &str) -> Tabs<'static> {
    let worktrees_loading = project.git_loading && project.worktrees.is_none();
    let titles = [
        tab_title("Worktrees", &project.worktrees, worktrees_loading, spinner),
        tab_title("Pull requests", &project.prs, project.prs_loading, spinner),
    ];
    let selected = match tab {
        Tab::Worktrees => 0,
        Tab::Prs => 1,
    };
    Tabs::new(titles)
        .select(selected)
        .style(Style::new().fg(MUTED))
        .highlight_style(Style::new().fg(Color::Black).bg(ACCENT).bold())
        .divider(" ")
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

fn draw_worktrees(
    frame: &mut Frame,
    project: &mut Project,
    area: Rect,
    focused: bool,
    home: Option<&Path>,
    spinner: &str,
) {
    let list = match &project.worktrees {
        None => return message(frame, area, format!("{spinner} Reading worktrees…"), ACCENT),
        Some(Err(err)) => {
            return message(
                frame,
                area,
                format!("Could not list worktrees: {err}"),
                DELETED,
            );
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
            match &info.stats {
                Ok(stats) => {
                    let totals = Totals::of(&stats.files);
                    Row::new(vec![
                        Cell::from(label),
                        count_cell("↑", stats.ahead, ACCENT),
                        count_cell("↓", stats.behind, Color::Yellow),
                        Cell::from(totals.files.to_string()),
                        count_cell("+", totals.added, ADDED),
                        count_cell("-", totals.deleted, DELETED),
                        Cell::from(path),
                    ])
                }
                Err(err) => Row::new(vec![
                    Cell::from(label),
                    Cell::from(""),
                    Cell::from(""),
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
            Constraint::Length(branch_width as u16),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Fill(1),
        ],
    )
    .header(header(&[
        "Branch", "Ahead", "Behind", "Files", "Added", "Deleted", "Path",
    ]))
    .row_highlight_style(row_highlight(focused))
    .highlight_symbol("▶ ")
    .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, area, &mut project.worktree_table);
}

fn draw_prs(frame: &mut Frame, project: &mut Project, area: Rect, focused: bool, spinner: &str) {
    let prs = match &project.prs {
        None => {
            return message(
                frame,
                area,
                format!("{spinner} Loading pull requests…"),
                ACCENT,
            );
        }
        Some(Err(err)) => return message(frame, area, err.clone(), Color::Yellow),
        Some(Ok(prs)) if prs.is_empty() => {
            return message(frame, area, "No open pull requests.".to_string(), MUTED);
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
                    Cell::from("draft").fg(Color::Yellow)
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
}

/// What the detail pane shows: a title and either files, a loading state or an error.
struct Detail<'a> {
    title: String,
    files: Result<Option<&'a [FileStat]>, String>,
}

fn worktree_detail<'a>(info: &'a WorktreeInfo, project: &Project) -> Detail<'a> {
    let label = branch_label(info);
    match &info.stats {
        Ok(stats) => {
            let t = Totals::of(&stats.files);
            Detail {
                title: format!(
                    " {label}: {} files +{} -{} vs merge-base with {} (incl. uncommitted and untracked) ",
                    t.files,
                    t.added,
                    t.deleted,
                    project.base_ref()
                ),
                files: Ok(Some(&stats.files)),
            }
        }
        Err(err) => Detail {
            title: format!(" {label} "),
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
        files,
    }
}

fn draw_detail(
    frame: &mut Frame,
    detail: Detail,
    area: Rect,
    state: &mut TableState,
    spinner: &str,
) {
    let block = pane(detail.title, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let files = match detail.files {
        Ok(None) => return message(frame, inner, format!("{spinner} Loading files…"), ACCENT),
        Err(err) => return message(frame, inner, err, DELETED),
        Ok(Some([])) => return message(frame, inner, "No changes.".to_string(), MUTED),
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
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let line = match app.status() {
        Some(status) => Line::from(status.to_string()).fg(Color::Yellow),
        None => {
            let hints = match app.focus {
                Focus::Projects => {
                    "j/k move · l/enter open · tab worktrees/PRs · a add · d remove · r refresh · ? help · q quit"
                }
                Focus::List => {
                    "j/k move · l/enter files · h/esc back · tab worktrees/PRs · r refresh · ? help · q quit"
                }
                Focus::Detail => {
                    "j/k move · h/esc back · tab worktrees/PRs · r refresh · ? help · q quit"
                }
            };
            Line::from(hints).fg(MUTED)
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn popup(frame: &mut Frame, title: &'static str, width: u16, height: u16, color: Color) -> Rect {
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

fn draw_add_project(frame: &mut Frame, input: &PathInput) {
    let inner = popup(frame, " Add project ", 72, 6, ACCENT);
    let lines = vec![
        Line::from("Path to a local git repository (~ is expanded):"),
        Line::from(vec![
            Span::raw("> ").fg(ACCENT),
            Span::raw(input.text.clone()),
        ]),
        Line::from(input.error.clone().unwrap_or_default()).fg(DELETED),
        Line::from("enter add · esc cancel · ctrl+u clear").fg(MUTED),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
    let cursor_x = inner.x + 2 + input.text.chars().count() as u16;
    frame.set_cursor_position((cursor_x.min(inner.right().saturating_sub(1)), inner.y + 1));
}

fn draw_confirm_remove(frame: &mut Frame, app: &App) {
    let Some(project) = app.selected_project() else {
        return;
    };
    let inner = popup(frame, " Remove project ", 64, 6, DELETED);
    let lines = vec![
        Line::from(format!(
            "Remove {} from tuitree?",
            display_path(&project.path, app.home.as_deref())
        )),
        Line::from("The repository on disk is not touched.").fg(MUTED),
        Line::from(""),
        Line::from("y remove · n cancel").fg(MUTED),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_help(frame: &mut Frame) {
    let keys = [
        ("j / k, ↓ / ↑", "move"),
        ("tab", "switch Worktrees / Pull requests"),
        ("enter / l", "open (project → list → files)"),
        ("esc / h", "back"),
        ("a", "add project"),
        ("d", "remove project (asks first)"),
        ("r", "refresh: git fetch, worktree stats, PRs"),
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
    let inner = popup(frame, " Help ", 64, 18, ACCENT);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
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

fn branch_label(info: &WorktreeInfo) -> String {
    match (&info.worktree.branch, &info.worktree.head) {
        (Some(branch), _) => branch.clone(),
        (None, Some(head)) => format!("(detached {})", &head[..head.len().min(7)]),
        (None, None) => "(unknown)".to_string(),
    }
}

fn age(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        0..=4 => "just now".to_string(),
        secs @ 5..=59 => format!("{secs}s ago"),
        secs @ 60..=3599 => format!("{}m ago", secs / 60),
        secs => format!("{}h ago", secs / 3600),
    }
}
