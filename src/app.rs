//! Application state and key handling. Rendering lives in `ui.rs`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::{ListState, TableState};

use crate::config::{self, Config, ProjectEntry};
use crate::gh::PullRequest;
use crate::git::{self, WorktreeInfo};
use crate::model::FileStat;
use crate::worker::{self, Update, UpdateKind};

const STATUS_TTL: Duration = Duration::from_secs(6);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Projects,
    List,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Worktrees,
    Prs,
}

pub enum Mode {
    Normal,
    Help,
    AddProject(PathInput),
    ConfirmRemove,
}

#[derive(Default)]
pub struct PathInput {
    pub text: String,
    pub error: Option<String>,
}

pub struct FetchResult {
    pub at: Instant,
    pub result: Result<(), String>,
}

/// Files of one PR; `None` while they are loading.
pub type PrFilesState = Option<Result<Vec<FileStat>, String>>;

pub struct Project {
    pub path: PathBuf,
    pub github: Option<String>,
    pub default_branch: Option<String>,
    /// Bumped on every refresh; 0 means never loaded.
    generation: u64,
    pub git_loading: bool,
    pub fetching: bool,
    pub prs_loading: bool,
    pub last_fetch: Option<FetchResult>,
    pub worktrees: Option<Result<Vec<WorktreeInfo>, String>>,
    pub prs: Option<Result<Vec<PullRequest>, String>>,
    pub pr_files: HashMap<u64, PrFilesState>,
    pub worktree_table: TableState,
    pub pr_table: TableState,
}

impl Project {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            github: None,
            default_branch: None,
            generation: 0,
            git_loading: false,
            fetching: false,
            prs_loading: false,
            last_fetch: None,
            worktrees: None,
            prs: None,
            pr_files: HashMap::new(),
            worktree_table: TableState::default(),
            pr_table: TableState::default(),
        }
    }

    pub fn is_loading(&self) -> bool {
        self.git_loading || self.prs_loading
    }

    pub fn worktree_list(&self) -> &[WorktreeInfo] {
        match &self.worktrees {
            Some(Ok(list)) => list,
            _ => &[],
        }
    }

    pub fn pr_list(&self) -> &[PullRequest] {
        match &self.prs {
            Some(Ok(list)) => list,
            _ => &[],
        }
    }

    pub fn selected_worktree(&self) -> Option<&WorktreeInfo> {
        self.worktree_list().get(self.worktree_table.selected()?)
    }

    pub fn selected_pr(&self) -> Option<&PullRequest> {
        self.pr_list().get(self.pr_table.selected()?)
    }

    pub fn base_ref(&self) -> String {
        format!(
            "origin/{}",
            self.default_branch.as_deref().unwrap_or("main")
        )
    }
}

pub struct App {
    pub projects: Vec<Project>,
    pub project_list: ListState,
    pub focus: Focus,
    pub tab: Tab,
    pub mode: Mode,
    pub detail_table: TableState,
    pub home: Option<PathBuf>,
    pub should_quit: bool,
    status: Option<(String, Instant)>,
    started: Instant,
    config_path: PathBuf,
    tx: Sender<Update>,
}

impl App {
    pub fn new(config_path: PathBuf, config: Config, tx: Sender<Update>) -> Self {
        let projects: Vec<Project> = config
            .projects
            .into_iter()
            .map(|entry| Project::new(entry.path))
            .collect();
        let mut app = Self {
            project_list: ListState::default().with_selected((!projects.is_empty()).then_some(0)),
            projects,
            focus: Focus::Projects,
            tab: Tab::Worktrees,
            mode: Mode::Normal,
            detail_table: TableState::default(),
            home: config::home_dir(),
            should_quit: false,
            status: None,
            started: Instant::now(),
            config_path,
            tx,
        };
        app.load_selected_if_new();
        app
    }

    pub fn spinner(&self) -> &'static str {
        let frame = self.started.elapsed().as_millis() / 100;
        SPINNER[frame as usize % SPINNER.len()]
    }

    pub fn status(&self) -> Option<&str> {
        self.status
            .as_ref()
            .filter(|(_, at)| at.elapsed() < STATUS_TTL)
            .map(|(msg, _)| msg.as_str())
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.projects.get(self.project_list.selected()?)
    }

    fn selected_project_mut(&mut self) -> Option<&mut Project> {
        self.projects.get_mut(self.project_list.selected()?)
    }

    /// Files shown in the detail pane for the current selection, when available.
    pub fn detail_files(&self) -> Option<&[FileStat]> {
        let project = self.selected_project()?;
        match self.tab {
            Tab::Worktrees => project
                .selected_worktree()?
                .stats
                .as_ref()
                .ok()
                .map(|s| &s.files[..]),
            Tab::Prs => {
                let number = project.selected_pr()?.number;
                match project.pr_files.get(&number)? {
                    Some(Ok(files)) => Some(files),
                    _ => None,
                }
            }
        }
    }

    fn set_status(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now()));
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        match &mut self.mode {
            Mode::Normal => self.on_normal_key(key.code),
            Mode::Help => self.mode = Mode::Normal,
            Mode::ConfirmRemove => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    self.mode = Mode::Normal;
                    self.remove_selected();
                }
                KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => self.mode = Mode::Normal,
                _ => {}
            },
            Mode::AddProject(input) => match key.code {
                KeyCode::Esc => self.mode = Mode::Normal,
                KeyCode::Enter => {
                    let text = input.text.clone();
                    match self.add_project(&text) {
                        Ok(()) => self.mode = Mode::Normal,
                        Err(err) => {
                            if let Mode::AddProject(input) = &mut self.mode {
                                input.error = Some(err);
                            }
                        }
                    }
                }
                KeyCode::Backspace => {
                    input.text.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.text.clear();
                }
                KeyCode::Char(c) => input.text.push(c),
                _ => {}
            },
        }
    }

    fn on_normal_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('a') => self.mode = Mode::AddProject(PathInput::default()),
            KeyCode::Char('d') if self.selected_project().is_some() => {
                self.mode = Mode::ConfirmRemove
            }
            KeyCode::Char('r') => self.refresh_selected(),
            KeyCode::Tab | KeyCode::BackTab => self.switch_tab(),
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.open(),
            KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left => self.back(),
            _ => {}
        }
    }

    fn switch_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Worktrees => Tab::Prs,
            Tab::Prs => Tab::Worktrees,
        };
        if self.focus == Focus::Detail {
            self.focus = Focus::List;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::Projects => {
                let next = stepped(self.project_list.selected(), self.projects.len(), delta);
                if next != self.project_list.selected() {
                    self.project_list.select(next);
                    self.load_selected_if_new();
                }
            }
            Focus::List => {
                let tab = self.tab;
                let Some(project) = self.selected_project_mut() else {
                    return;
                };
                let (len, table) = match tab {
                    Tab::Worktrees => (project.worktree_list().len(), &mut project.worktree_table),
                    Tab::Prs => (project.pr_list().len(), &mut project.pr_table),
                };
                table.select(stepped(table.selected(), len, delta));
            }
            Focus::Detail => {
                let len = self.detail_files().map_or(0, <[FileStat]>::len);
                self.detail_table
                    .select(stepped(self.detail_table.selected(), len, delta));
            }
        }
    }

    fn open(&mut self) {
        match self.focus {
            Focus::Projects if self.selected_project().is_some() => self.focus = Focus::List,
            Focus::List => {
                let Some(project) = self.selected_project() else {
                    return;
                };
                match self.tab {
                    Tab::Worktrees if project.selected_worktree().is_none() => return,
                    Tab::Worktrees => {}
                    Tab::Prs => {
                        let Some(number) = project.selected_pr().map(|pr| pr.number) else {
                            return;
                        };
                        self.request_pr_files(number);
                    }
                }
                self.detail_table = TableState::default().with_selected(Some(0));
                self.focus = Focus::Detail;
            }
            _ => {}
        }
    }

    fn back(&mut self) {
        self.focus = match self.focus {
            Focus::Detail => Focus::List,
            Focus::List | Focus::Projects => Focus::Projects,
        };
    }

    fn load_selected_if_new(&mut self) {
        if self.selected_project().is_some_and(|p| p.generation == 0) {
            self.refresh_selected();
        }
    }

    fn refresh_selected(&mut self) {
        let tx = self.tx.clone();
        let Some(project) = self.selected_project_mut() else {
            return;
        };
        if project.is_loading() {
            self.set_status("Already refreshing, please wait.");
            return;
        }
        project.generation += 1;
        project.git_loading = true;
        project.fetching = true;
        project.prs_loading = true;
        project.pr_files.clear();
        worker::spawn_refresh(&tx, &project.path, project.generation);
        if self.focus == Focus::Detail
            && self.tab == Tab::Prs
            && let Some(number) = self
                .selected_project()
                .and_then(|p| p.selected_pr())
                .map(|pr| pr.number)
        {
            self.request_pr_files(number);
        }
    }

    fn request_pr_files(&mut self, number: u64) {
        let tx = self.tx.clone();
        let Some(project) = self.selected_project_mut() else {
            return;
        };
        if matches!(project.pr_files.get(&number), Some(None | Some(Ok(_)))) {
            return; // already loading or loaded
        }
        let Some(repo) = project.github.clone() else {
            return;
        };
        project.pr_files.insert(number, None);
        worker::spawn_pr_files(&tx, &project.path, project.generation, repo, number);
    }

    /// Validates and adds a project. Errors are shown in the input dialog.
    fn add_project(&mut self, text: &str) -> Result<(), String> {
        let raw = text.trim();
        if raw.is_empty() {
            return Err("Type the path of a git repository.".into());
        }
        let mut path = config::expand_tilde(raw, self.home.as_deref());
        if path.is_relative()
            && let Ok(cwd) = std::env::current_dir()
        {
            path = cwd.join(path);
        }
        if !path.is_dir() {
            return Err(format!("Not a directory: {}", path.display()));
        }
        let root = git::repo_toplevel(&path)
            .map_err(|_| format!("Not a git repository: {}", path.display()))?;
        let shown = config::display_path(&root, self.home.as_deref());
        if let Some(index) = self.projects.iter().position(|p| p.path == root) {
            self.project_list.select(Some(index));
            self.set_status(format!("{shown} is already in the list."));
            return Ok(());
        }
        let mut project = Project::new(root);
        project.github = git::github_repo(&project.path);
        let remote = match &project.github {
            Some(repo) => format!("GitHub: {repo}"),
            None => "origin is not on GitHub".to_string(),
        };
        self.projects.push(project);
        self.project_list.select(Some(self.projects.len() - 1));
        self.focus = Focus::Projects;
        self.save_config();
        self.set_status(format!("Added {shown} ({remote})."));
        self.refresh_selected();
        Ok(())
    }

    fn remove_selected(&mut self) {
        let Some(index) = self.project_list.selected() else {
            return;
        };
        let removed = self.projects.remove(index);
        let next = (!self.projects.is_empty()).then(|| index.min(self.projects.len() - 1));
        self.project_list.select(next);
        self.focus = Focus::Projects;
        self.save_config();
        let shown = config::display_path(&removed.path, self.home.as_deref());
        self.set_status(format!(
            "Removed {shown} from tuitree. Files on disk are untouched."
        ));
        self.load_selected_if_new();
    }

    fn save_config(&mut self) {
        let config = Config {
            projects: self
                .projects
                .iter()
                .map(|p| ProjectEntry {
                    path: p.path.clone(),
                })
                .collect(),
        };
        if let Err(err) = config.save(&self.config_path) {
            self.set_status(format!("Could not save config: {err:#}"));
        }
    }

    pub fn on_update(&mut self, update: Update) {
        let Some(project) = self.projects.iter_mut().find(|p| p.path == update.project) else {
            return; // project was removed meanwhile
        };
        if update.generation != project.generation {
            return; // result of an older refresh
        }
        match update.kind {
            UpdateKind::RepoInfo {
                github,
                default_branch,
            } => {
                project.github = github;
                project.default_branch = Some(default_branch);
            }
            UpdateKind::Worktrees(result) => {
                project.worktrees = Some(result);
                let len = project.worktree_list().len();
                project
                    .worktree_table
                    .select(clamped(project.worktree_table.selected(), len));
            }
            UpdateKind::Fetched(result) => {
                project.fetching = false;
                project.last_fetch = Some(FetchResult {
                    at: Instant::now(),
                    result,
                });
            }
            UpdateKind::GitDone => {
                project.git_loading = false;
                project.fetching = false;
            }
            UpdateKind::Prs(result) => {
                project.prs_loading = false;
                project.prs = Some(result);
                let len = project.pr_list().len();
                project
                    .pr_table
                    .select(clamped(project.pr_table.selected(), len));
            }
            UpdateKind::PrFiles { number, files } => {
                project.pr_files.insert(number, Some(files));
            }
        }
        let detail_len = self.detail_files().map_or(0, <[FileStat]>::len);
        self.detail_table
            .select(clamped(self.detail_table.selected(), detail_len));
    }
}

/// Moves a selection by `delta` within `0..len`, without wrapping.
fn stepped(current: Option<usize>, len: usize, delta: isize) -> Option<usize> {
    match (len, current) {
        (0, _) => None,
        (_, None) => Some(0),
        (_, Some(index)) => Some(index.saturating_add_signed(delta).min(len - 1)),
    }
}

/// Keeps a selection valid after the list changed; selects the first row of a new list.
fn clamped(current: Option<usize>, len: usize) -> Option<usize> {
    (len > 0).then(|| current.unwrap_or(0).min(len - 1))
}
