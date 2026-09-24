//! Mouse input. The UI records where it drew each clickable thing (`Hits`) and clicks are
//! matched against those rectangles.

use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use super::{App, Focus, Mode, Tab};
use crate::model::FileStat;

/// Clickable areas of the last drawn frame.
#[derive(Debug, Default)]
pub struct Hits {
    pub projects: Option<Rows>,
    pub tabs: Vec<(Rect, Tab)>,
    pub list: Option<Rows>,
    /// The mark column of the worktree table (click toggles a mark).
    pub marks: Option<Rect>,
    /// The PR column of the worktree table (click jumps to the PR).
    pub pr: Option<Rect>,
    pub detail: Option<Rows>,
    pub buttons: Vec<(Rect, Button)>,
}

/// Rows of a rendered list or table: where they are and which item the top row shows.
#[derive(Debug, Clone, Copy)]
pub struct Rows {
    pub area: Rect,
    pub offset: usize,
    pub row_height: u16,
}

impl Rows {
    fn index_at(&self, pos: Position) -> Option<usize> {
        self.area
            .contains(pos)
            .then(|| self.offset + usize::from((pos.y - self.area.y) / self.row_height))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Yes,
    No,
}

impl App {
    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        let pos = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.on_click(pos),
            MouseEventKind::ScrollDown => self.on_scroll(pos, 1),
            MouseEventKind::ScrollUp => self.on_scroll(pos, -1),
            _ => {}
        }
    }

    fn on_click(&mut self, pos: Position) {
        match self.mode {
            Mode::Normal => {}
            Mode::Help => {
                self.mode = Mode::Normal;
                return;
            }
            Mode::AddProject(_) | Mode::Confirm(_) => {
                let button = self
                    .hits
                    .buttons
                    .iter()
                    .find(|(area, _)| area.contains(pos));
                if let Some(&(_, button)) = button {
                    self.press(button);
                }
                return;
            }
        }
        if let Some(&(_, tab)) = self.hits.tabs.iter().find(|(area, _)| area.contains(pos)) {
            self.set_tab(tab);
        } else if let Some(index) = self.hits.projects.and_then(|rows| rows.index_at(pos)) {
            if index < self.projects.len() {
                self.focus = Focus::Projects;
                self.select_project(Some(index));
            }
        } else if let Some(index) = self
            .hits
            .marks
            .filter(|area| area.contains(pos))
            .and(self.hits.list)
            .and_then(|rows| rows.index_at(pos))
        {
            self.toggle_mark(index);
        } else if let Some(index) = self
            .hits
            .pr
            .filter(|area| area.contains(pos))
            .and(self.hits.list)
            .and_then(|rows| rows.index_at(pos))
        {
            self.jump_to_pr(index);
        } else if let Some(index) = self.hits.list.and_then(|rows| rows.index_at(pos)) {
            self.click_list_row(index);
        } else if let Some(index) = self.hits.detail.and_then(|rows| rows.index_at(pos))
            && index < self.detail_files().map_or(0, <[FileStat]>::len)
        {
            self.detail_table.select(Some(index));
            self.focus = Focus::Detail;
        }
    }

    /// Selects a row; clicking the already selected row opens its detail.
    fn click_list_row(&mut self, index: usize) {
        let (tab, focus) = (self.tab, self.focus);
        let Some(project) = self.selected_project_mut() else {
            return;
        };
        let (len, table) = match tab {
            Tab::Worktrees => (project.worktree_list().len(), &mut project.worktree_table),
            Tab::Prs => (project.pr_list().len(), &mut project.pr_table),
        };
        if index >= len {
            return;
        }
        let already_selected = table.selected() == Some(index);
        table.select(Some(index));
        match focus {
            Focus::List if already_selected => self.open(),
            Focus::Detail if already_selected => {}
            _ => self.focus = Focus::List,
        }
    }

    /// The wheel moves the selection of whichever list is under the pointer.
    fn on_scroll(&mut self, pos: Position, delta: isize) {
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        let over = |rows: Option<Rows>| rows.is_some_and(|rows| rows.area.contains(pos));
        if over(self.hits.projects) {
            self.move_in(Focus::Projects, delta);
        } else if over(self.hits.list) {
            self.move_in(Focus::List, delta);
        } else if over(self.hits.detail) {
            self.move_in(Focus::Detail, delta);
        }
    }
}
