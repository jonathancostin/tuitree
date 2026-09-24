//! The job queue: deletes and syncs run in the background, one at a time per repository, while
//! the UI stays usable. Every job of the session is kept for the queue panel.

use std::path::{Path, PathBuf};

use super::{Action, App, Confirm, Mode, Tone};
use crate::config::display_path;
use crate::worker::{Job, Task};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Delete,
    Sync,
}

impl Verb {
    pub fn name(self) -> &'static str {
        match self {
            Verb::Delete => "delete",
            Verb::Sync => "sync",
        }
    }

    pub fn doing(self) -> &'static str {
        match self {
            Verb::Delete => "deleting",
            Verb::Sync => "syncing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Pending,
    Running,
    Done(String),
    /// Carries git's full error output.
    Failed(String),
    /// Never ran; carries the reason.
    Skipped(String),
}

pub struct JobEntry {
    pub id: u64,
    pub project: PathBuf,
    pub worktree: PathBuf,
    /// Branch name, or `(detached …)`.
    pub label: String,
    pub verb: Verb,
    pub state: JobState,
}

impl JobEntry {
    pub fn is_active(&self) -> bool {
        matches!(self.state, JobState::Pending | JobState::Running)
    }
}

/// A job ready to be queued.
pub struct Planned {
    pub worktree: PathBuf,
    pub label: String,
    pub verb: Verb,
    pub job: Job,
}

/// A marked row that will not get a job, and why.
pub struct Skipped {
    pub worktree: PathBuf,
    pub label: String,
    pub verb: Verb,
    pub reason: String,
}

impl App {
    pub fn has_active_jobs(&self, project: &Path) -> bool {
        self.jobs
            .iter()
            .any(|job| job.is_active() && job.project == project)
    }

    /// The pending or running job of a worktree, if any.
    pub fn active_job(&self, worktree: &Path) -> Option<&JobEntry> {
        self.jobs
            .iter()
            .find(|job| job.is_active() && job.worktree == worktree)
    }

    /// Queues `planned` on the project's lane and records `skipped` in the queue panel.
    pub(super) fn enqueue(&mut self, project: &Path, planned: Vec<Planned>, skipped: Vec<Skipped>) {
        let Some(index) = self.projects.iter().position(|p| p.path == project) else {
            return;
        };
        let tx = self.tx.clone();
        let queued = planned.len();
        for plan in planned {
            let id = self.next_job;
            self.next_job += 1;
            self.jobs.push(JobEntry {
                id,
                project: project.to_path_buf(),
                worktree: plan.worktree.clone(),
                label: plan.label,
                verb: plan.verb,
                state: JobState::Pending,
            });
            self.projects[index].marked.remove(&plan.worktree);
            self.projects[index]
                .lane(&tx)
                .send(Task::Job { id, job: plan.job });
        }
        let skipped_count = skipped.len();
        for skip in skipped {
            let id = self.next_job;
            self.next_job += 1;
            self.projects[index].marked.remove(&skip.worktree);
            self.jobs.push(JobEntry {
                id,
                project: project.to_path_buf(),
                worktree: skip.worktree,
                label: skip.label,
                verb: skip.verb,
                state: JobState::Skipped(skip.reason),
            });
        }
        self.show_queue = true;
        let mut status = format!("Queued {queued} job{}", if queued == 1 { "" } else { "s" });
        if skipped_count > 0 {
            status.push_str(&format!(", skipped {skipped_count}"));
        }
        status.push_str(". Q shows or hides the queue.");
        self.set_status(status);
    }

    pub(super) fn on_job_started(&mut self, id: u64) {
        if let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) {
            job.state = JobState::Running;
        }
    }

    pub(super) fn on_job_finished(&mut self, id: u64, result: Result<String, String>) {
        let home = self.home.clone();
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        let shown = display_path(&job.worktree, home.as_deref());
        let status = match &result {
            Ok(_) if job.verb == Verb::Delete && job.label.starts_with('(') => {
                format!("Deleted worktree {shown}.")
            }
            Ok(_) if job.verb == Verb::Delete => {
                format!("Deleted worktree {shown} and branch {}.", job.label)
            }
            Ok(summary) => format!("Synced {}: {summary}.", job.label),
            Err(_) => format!(
                "{} {} failed. The queue (Q) shows git's error.",
                job.verb.name(),
                job.label
            ),
        };
        let failed = result.is_err();
        job.state = match result {
            Ok(summary) => JobState::Done(summary),
            Err(err) => JobState::Failed(err),
        };
        if failed {
            self.show_queue = true;
        }
        self.set_status(status);
    }

    /// Quits, asking first when jobs are still queued or running.
    pub(super) fn ask_quit(&mut self) {
        let active = self.jobs.iter().filter(|job| job.is_active()).count();
        if active == 0 {
            self.should_quit = true;
            return;
        }
        self.mode = Mode::Confirm(Confirm {
            title: " Quit? ".into(),
            lines: vec![
                (
                    format!(
                        "{active} job{} still queued or running.",
                        if active == 1 { " is" } else { "s are" }
                    ),
                    Tone::Warn,
                ),
                (
                    "Quitting drops queued jobs; a git command that already started finishes on its own."
                        .into(),
                    Tone::Muted,
                ),
            ],
            yes: "Quit".into(),
            danger: true,
            action: Action::Quit,
        });
    }
}
