//! Application state and input handling. Rendering lives in `ui.rs`. Submodules: `actions`
//! (planning delete/sync), `queue` (background jobs), `mouse` (hit-testing).

mod actions;
mod mouse;
mod queue;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::{ListState, TableState};

pub use self::actions::{Action, Confirm, Tone, branch_label};
pub use self::mouse::{Button, Hits, Rows};
pub use self::queue::{JobEntry, JobState};
use crate::config::{self, Config, ProjectEntry};
use crate::gh::PullRequest;
use crate::git::{self, WorktreeInfo};
use crate::model::FileStat;
use crate::worker::{self, Lane, Task, Update, UpdateKind};

const STATUS_TTL: Duration = Duration::from_secs(8);
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
    Confirm(Confirm),
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
    /// Runs this project's git work one task at a time; started on first use.
    lane: Option<Lane>,
    /// Refreshed at least once.
    loaded: bool,
    /// A refresh is queued or running on the lane.
    pub refreshing: bool,
    /// What the lane is doing right now.
    pub activity: Option<&'static str>,
    /// Bumped on every PR reload; late PR results of older reloads are ignored.
    pr_generation: u64,
    pub prs_loading: bool,
    pub last_fetch: Option<FetchResult>,
    pub worktrees: Option<Result<Vec<WorktreeInfo>, String>>,
    /// Worktrees marked with space, by path so marks survive reloads.
    pub marked: HashSet<PathBuf>,
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
            lane: None,
            loaded: false,
            refreshing: false,
            activity: None,
            pr_generation: 0,
            prs_loading: false,
            last_fetch: None,
            worktrees: None,
            marked: HashSet::new(),
            prs: None,
            pr_files: HashMap::new(),
            worktree_table: TableState::default(),
            pr_table: TableState::default(),
        }
    }

    pub fn is_loading(&self) -> bool {
        self.refreshing || self.activity.is_some() || self.prs_loading
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

    pub fn default_branch(&self) -> &str {
        self.default_branch.as_deref().unwrap_or("main")
    }

    pub fn base_ref(&self) -> String {
        format!("origin/{}", self.default_branch())
    }

    /// Replaces the worktree list, keeping the cursor on the same worktree (by path) and
    /// dropping marks of worktrees that are gone.
    fn set_worktrees(&mut self, result: Result<Vec<WorktreeInfo>, String>) {
        let selected = self.selected_worktree().map(|wt| wt.worktree.path.clone());
        let old_index = self.worktree_table.selected();
        self.worktrees = Some(result);
        let list = self.worktree_list();
        let index = selected
            .and_then(|path| list.iter().position(|wt| wt.worktree.path == path))
            .or(old_index);
        let len = list.len();
        if let Some(Ok(list)) = &self.worktrees {
            self.marked
                .retain(|path| list.iter().any(|wt| &wt.worktree.path == path));
        }
        self.worktree_table.select(clamped(index, len));
    }

    fn lane(&mut self, updates: &Sender<Update>) -> &Lane {
        self.lane
            .get_or_insert_with(|| Lane::spawn(updates, &self.path))
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
    /// Every delete/sync job of this session, oldest first.
    pub jobs: Vec<JobEntry>,
    /// The queue panel is visible (`Q`).
    pub show_queue: bool,
    /// Screen areas of the last draw, for mouse hit-testing.
    pub hits: Hits,
    next_job: u64,
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
            jobs: Vec::new(),
            show_queue: false,
            hits: Hits::default(),
            next_job: 1,
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
            return self.ask_quit();
        }
        match &mut self.mode {
            Mode::Normal => self.on_normal_key(key.code),
            Mode::Help => self.mode = Mode::Normal,
            Mode::Confirm(_) => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.press(Button::Yes),
                KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => self.press(Button::No),
                _ => {}
            },
            Mode::AddProject(input) => match key.code {
                KeyCode::Esc => self.press(Button::No),
                KeyCode::Enter => self.press(Button::Yes),
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

    /// Accepts (`Yes`) or dismisses (`No`) the open dialog.
    fn press(&mut self, button: Button) {
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::AddProject(input) if button == Button::Yes => {
                if let Err(err) = self.add_project(&input.text) {
                    self.mode = Mode::AddProject(PathInput {
                        error: Some(err),
                        ..input
                    });
                }
            }
            Mode::Confirm(confirm) if button == Button::Yes => self.run_action(confirm.action),
            _ => {}
        }
    }

    fn on_normal_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') => self.ask_quit(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('a') => self.mode = Mode::AddProject(PathInput::default()),
            KeyCode::Char('d') => self.ask_remove_project(),
            KeyCode::Char('x') => self.ask_delete_worktrees(),
            KeyCode::Char('s') => self.ask_sync(),
            KeyCode::Char(' ') => self.toggle_mark_selected(),
            KeyCode::Char('M') => self.mark_merged(),
            KeyCode::Char('Q') => self.show_queue = !self.show_queue,
            KeyCode::Char('r') => self.refresh_selected(),
            KeyCode::Tab | KeyCode::BackTab => self.set_tab(match self.tab {
                Tab::Worktrees => Tab::Prs,
                Tab::Prs => Tab::Worktrees,
            }),
            KeyCode::Char('j') | KeyCode::Down => self.move_in(self.focus, 1),
            KeyCode::Char('k') | KeyCode::Up => self.move_in(self.focus, -1),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.open(),
            KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left => self.back(),
            _ => {}
        }
    }

    fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        if self.focus == Focus::Detail {
            self.focus = Focus::List;
        }
    }

    fn select_project(&mut self, index: Option<usize>) {
        if index != self.project_list.selected() {
            self.project_list.select(index);
            self.load_selected_if_new();
        }
    }

    /// Marks or unmarks worktree row `index` of the selected project.
    fn toggle_mark(&mut self, index: usize) {
        let Some(project) = self.selected_project_mut() else {
            return;
        };
        let Some(path) = project
            .worktree_list()
            .get(index)
            .map(|wt| wt.worktree.path.clone())
        else {
            return;
        };
        if !project.marked.remove(&path) {
            project.marked.insert(path);
        }
    }

    fn toggle_mark_selected(&mut self) {
        if self.tab != Tab::Worktrees || self.focus == Focus::Projects {
            return self.set_status("Marks are for worktree rows (l to open the list).");
        }
        if let Some(index) = self
            .selected_project()
            .and_then(|p| p.worktree_table.selected())
        {
            self.toggle_mark(index);
        }
    }

    /// Marks every merged worktree of the selected project, or unmarks them when all already
    /// are.
    fn mark_merged(&mut self) {
        if self.tab != Tab::Worktrees {
            return self.set_status("Merged rows are on the Worktrees tab.");
        }
        let Some(project) = self.selected_project_mut() else {
            return;
        };
        let merged: Vec<PathBuf> = project
            .worktree_list()
            .iter()
            .filter(|wt| wt.stats.as_ref().is_ok_and(|s| s.merged.is_some()))
            .map(|wt| wt.worktree.path.clone())
            .collect();
        let status = if merged.is_empty() {
            "No merged worktrees (yet: the check runs after each fetch).".to_string()
        } else if merged.iter().all(|path| project.marked.contains(path)) {
            for path in &merged {
                project.marked.remove(path);
            }
            format!("Unmarked {} merged worktrees.", merged.len())
        } else {
            let count = merged.len();
            project.marked.extend(merged);
            format!("Marked {count} merged worktrees. x deletes them.")
        };
        if self.focus == Focus::Projects {
            self.focus = Focus::List;
        }
        self.set_status(status);
    }

    /// Moves the selection of one pane by `delta` rows.
    fn move_in(&mut self, pane: Focus, delta: isize) {
        match pane {
            Focus::Projects => {
                let next = stepped(self.project_list.selected(), self.projects.len(), delta);
                self.select_project(next);
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
        if self.selected_project().is_some_and(|p| !p.loaded) {
            self.refresh_selected();
        }
    }

    fn refresh_selected(&mut self) {
        if let Some(index) = self.project_list.selected() {
            self.refresh(index);
        }
    }

    /// Queues a fetch + reload on the project's lane (after any running jobs) and reloads PRs.
    fn refresh(&mut self, index: usize) {
        let tx = self.tx.clone();
        let Some(project) = self.projects.get_mut(index) else {
            return;
        };
        if project.refreshing {
            return self.set_status("Already refreshing, please wait.");
        }
        project.loaded = true;
        project.refreshing = true;
        project.lane(&tx).send(Task::Refresh);
        if !project.prs_loading {
            project.pr_generation += 1;
            project.prs_loading = true;
            project.pr_files.clear();
            worker::spawn_prs(&tx, &project.path, project.pr_generation);
        }
        if self.project_list.selected() == Some(index)
            && self.focus == Focus::Detail
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
        worker::spawn_pr_files(&tx, &project.path, project.pr_generation, repo, number);
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

    fn ask_remove_project(&mut self) {
        let Some(project) = self.selected_project() else {
            return;
        };
        if self.has_active_jobs(&project.path) {
            return self.set_status("This project still has queued jobs; wait for them to finish.");
        }
        let shown = config::display_path(&project.path, self.home.as_deref());
        self.mode = Mode::Confirm(Confirm {
            title: " Remove project ".into(),
            lines: vec![
                (format!("Remove {shown} from tuitree?"), Tone::Normal),
                ("The repository on disk is not touched.".into(), Tone::Muted),
            ],
            yes: "Remove".into(),
            danger: false,
            action: Action::RemoveProject,
        });
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
        let Some(index) = self.projects.iter().position(|p| p.path == update.project) else {
            return; // project was removed meanwhile
        };
        let project = &mut self.projects[index];
        match update.kind {
            UpdateKind::RepoInfo {
                github,
                default_branch,
            } => {
                project.github = github;
                project.default_branch = Some(default_branch);
            }
            UpdateKind::Activity(activity) => project.activity = activity,
            UpdateKind::Worktrees(result) => project.set_worktrees(result),
            UpdateKind::Fetched(result) => {
                project.last_fetch = Some(FetchResult {
                    at: Instant::now(),
                    result,
                });
            }
            UpdateKind::RefreshDone => project.refreshing = false,
            UpdateKind::Prs(result) if update.generation == project.pr_generation => {
                project.prs_loading = false;
                project.prs = Some(result);
                let len = project.pr_list().len();
                project
                    .pr_table
                    .select(clamped(project.pr_table.selected(), len));
            }
            UpdateKind::PrFiles { number, files } if update.generation == project.pr_generation => {
                project.pr_files.insert(number, Some(files));
            }
            UpdateKind::Prs(_) | UpdateKind::PrFiles { .. } => {} // from an older reload
            UpdateKind::JobStarted(id) => self.on_job_started(id),
            UpdateKind::JobFinished { id, result } => self.on_job_finished(id, result),
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
