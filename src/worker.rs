//! Background loading. Each job runs on its own thread and reports back over a channel, so the
//! UI thread never waits on git, the network or gh.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;

use crate::gh::{self, PullRequest};
use crate::git::{self, WorktreeInfo};
use crate::model::FileStat;

/// A result for one project. `generation` ties it to the refresh that produced it so late
/// results of an older refresh are ignored.
pub struct Update {
    pub project: PathBuf,
    pub generation: u64,
    pub kind: UpdateKind,
}

pub enum UpdateKind {
    RepoInfo {
        github: Option<String>,
        default_branch: String,
    },
    Worktrees(Result<Vec<WorktreeInfo>, String>),
    Fetched(Result<(), String>),
    /// The git side of a refresh has finished.
    GitDone,
    Prs(Result<Vec<PullRequest>, String>),
    PrFiles {
        number: u64,
        files: Result<Vec<FileStat>, String>,
    },
}

/// Refreshes a project: worktree stats from local refs first (fast), then `git fetch`, then the
/// stats again against the fetched refs. Open PRs load in parallel.
pub fn spawn_refresh(tx: &Sender<Update>, project: &Path, generation: u64) {
    let sender = Reporter::new(tx, project, generation);
    let path = project.to_path_buf();
    thread::spawn(move || {
        let github = git::github_repo(&path);
        let default_branch = git::default_branch(&path);
        let base = format!("origin/{default_branch}");
        sender.send(UpdateKind::RepoInfo {
            github,
            default_branch,
        });
        sender.send(UpdateKind::Worktrees(git::load_worktrees(&path, &base)));
        let fetched = git::fetch(&path);
        let fetch_ok = fetched.is_ok();
        sender.send(UpdateKind::Fetched(fetched));
        if fetch_ok {
            sender.send(UpdateKind::Worktrees(git::load_worktrees(&path, &base)));
        }
        sender.send(UpdateKind::GitDone);
    });

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
