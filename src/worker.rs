//! Background work. The UI thread never waits on git, the network or gh.
//!
//! Everything that touches a repository runs on that project's *lane*: one thread that takes
//! tasks (refreshes and queued jobs) one at a time, because git locks the repository. Lanes of
//! different projects run in parallel. PR loading only talks to GitHub and runs on its own
//! threads.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;

use crate::gh::{self, PullRequest};
use crate::git::{self, RepoPr, WorktreeInfo};
use crate::model::FileStat;

/// A result for one project.
pub struct Update {
    pub project: PathBuf,
    /// Ties PR results to the refresh that asked for them; late ones are ignored.
    pub generation: u64,
    pub kind: UpdateKind,
}

pub enum UpdateKind {
    RepoInfo {
        github: Option<String>,
        default_branch: String,
    },
    /// What the lane is doing right now; `None` when it is idle.
    Activity(Option<&'static str>),
    Worktrees(Result<Vec<WorktreeInfo>, String>),
    Fetched(Result<(), String>),
    /// A refresh has finished.
    RefreshDone,
    Prs(Result<Vec<PullRequest>, String>),
    PrFiles {
        number: u64,
        files: Result<Vec<FileStat>, String>,
    },
    JobStarted(u64),
    JobFinished {
        id: u64,
        result: Result<String, String>,
    },
}

/// Work for a project's lane.
pub enum Task {
    /// `git fetch` and reload the worktrees.
    Refresh,
    /// Run a queued job, then reload the worktrees.
    Job { id: u64, job: Job },
}

#[derive(Debug, Clone)]
pub enum Job {
    /// Remove the worktree (forcefully) and delete its local branch.
    Delete {
        path: PathBuf,
        branch: Option<String>,
    },
    /// Merge `base` into the branch checked out in `worktree`.
    Sync { worktree: PathBuf, base: String },
}

/// Handle to a project's lane; the thread ends when the handle is dropped.
pub struct Lane {
    tasks: Sender<Task>,
}

impl Lane {
    pub fn spawn(updates: &Sender<Update>, project: &Path) -> Self {
        let (tasks, inbox) = mpsc::channel();
        let sender = Reporter::new(updates, project, 0);
        let repo = project.to_path_buf();
        thread::spawn(move || {
            // The repo's recent PRs from gh, refreshed with every fetch and reused by job
            // reloads; `None` while gh is unavailable.
            let mut prs = None;
            for task in inbox {
                match task {
                    Task::Refresh => refresh(&sender, &repo, &mut prs),
                    Task::Job { id, job } => {
                        sender.send(UpdateKind::JobStarted(id));
                        let result = run_job(&repo, &job);
                        sender.send(UpdateKind::JobFinished { id, result });
                        reload(&sender, &repo, prs.as_deref());
                    }
                }
                sender.send(UpdateKind::Activity(None));
            }
        });
        Self { tasks }
    }

    pub fn send(&self, task: Task) {
        // The lane thread only stops when this handle is dropped, so sending cannot fail.
        let _ = self.tasks.send(task);
    }
}

fn base_ref(repo: &Path) -> String {
    format!("origin/{}", git::default_branch(repo))
}

/// Quick numbers from local refs first, then `git fetch` and the repo's PRs (one `gh` call;
/// kept from last time when gh fails), then the full numbers including the merge check against
/// the fetched refs.
fn refresh(sender: &Reporter, repo: &Path, prs: &mut Option<Vec<RepoPr>>) {
    let github = git::github_repo(repo);
    let default_branch = git::default_branch(repo);
    let base = format!("origin/{default_branch}");
    sender.send(UpdateKind::RepoInfo {
        github: github.clone(),
        default_branch,
    });
    sender.send(UpdateKind::Activity(Some("updating worktrees…")));
    let quick = git::load_worktrees(repo, &base, false, prs.as_deref());
    sender.send(UpdateKind::Worktrees(quick));
    sender.send(UpdateKind::Activity(Some("fetching origin…")));
    sender.send(UpdateKind::Fetched(git::fetch(repo)));
    if let Some(github) = github {
        sender.send(UpdateKind::Activity(Some("checking PRs…")));
        if let Ok(list) = gh::list_repo_prs(repo, &github) {
            *prs = Some(list);
        }
    }
    sender.send(UpdateKind::Activity(Some("updating worktrees…")));
    let worktrees = git::load_worktrees(repo, &base, true, prs.as_deref());
    sender.send(UpdateKind::Worktrees(worktrees));
    sender.send(UpdateKind::RefreshDone);
}

/// Reloads the worktrees without fetching (after a job changed them).
fn reload(sender: &Reporter, repo: &Path, prs: Option<&[RepoPr]>) {
    sender.send(UpdateKind::Activity(Some("updating worktrees…")));
    let base = base_ref(repo);
    let worktrees = git::load_worktrees(repo, &base, true, prs);
    sender.send(UpdateKind::Worktrees(worktrees));
}

fn run_job(repo: &Path, job: &Job) -> Result<String, String> {
    match job {
        Job::Delete { path, branch } => {
            git::delete_worktree(repo, path, branch.as_deref())?;
            Ok(match branch {
                Some(branch) => format!("worktree and branch {branch} deleted"),
                None => "worktree deleted".to_string(),
            })
        }
        Job::Sync { worktree, base } => git::merge(worktree, base),
    }
}

/// Loads the open PRs of a project.
pub fn spawn_prs(tx: &Sender<Update>, project: &Path, generation: u64) {
    let sender = Reporter::new(tx, project, generation);
    let path = project.to_path_buf();
    thread::spawn(move || {
        let prs = match git::github_repo(&path) {
            Some(repo) => gh::list_prs(&path, &repo),
            None => {
                Err("origin is not a GitHub remote, so there are no pull requests to show.".into())
            }
        };
        sender.send(UpdateKind::Prs(prs));
    });
}

/// Loads the per-file changes of one PR.
pub fn spawn_pr_files(
    tx: &Sender<Update>,
    project: &Path,
    generation: u64,
    repo: String,
    number: u64,
) {
    let sender = Reporter::new(tx, project, generation);
    let path = project.to_path_buf();
    thread::spawn(move || {
        let files = gh::pr_files(&path, &repo, number);
        sender.send(UpdateKind::PrFiles { number, files });
    });
}

struct Reporter {
    tx: Sender<Update>,
    project: PathBuf,
    generation: u64,
}

impl Reporter {
    fn new(tx: &Sender<Update>, project: &Path, generation: u64) -> Self {
        Self {
            tx: tx.clone(),
            project: project.to_path_buf(),
            generation,
        }
    }

    fn send(&self, kind: UpdateKind) {
        // The receiver only disappears when the app is quitting; nothing to report then.
        let _ = self.tx.send(Update {
            project: self.project.clone(),
            generation: self.generation,
            kind,
        });
    }
}
