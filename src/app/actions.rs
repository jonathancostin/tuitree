//! Worktree actions: delete a worktree with its branch, and sync a branch with origin.
//! Both ask for confirmation first and run in the background.

use super::{App, Focus, Mode, Tab};
use crate::config::display_path;
use crate::git::{DeleteOutcome, SyncStatus, WorktreeInfo};
use crate::worker::{self, DeleteJob, SyncJob};

/// A yes/no dialog and what "yes" does.
pub struct Confirm {
    pub title: &'static str,
    pub lines: Vec<(String, Tone)>,
    /// Label of the "yes" button.
    pub yes: &'static str,
    /// Destructive: drawn in red.
    pub danger: bool,
    pub action: Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Muted,
    Warn,
    Danger,
}

pub enum Action {
    RemoveProject,
    DeleteWorktree(DeleteJob),
    Sync(SyncJob),
}

fn plural(count: impl Into<u64>, one: &str, many: &str) -> String {
    let count = count.into();
    format!("{count} {}", if count == 1 { one } else { many })
}

impl App {
    /// The worktree row that `x` / `s` act on, with its project.
    fn action_target(&self) -> Result<(usize, &WorktreeInfo), &'static str> {
        if self.tab != Tab::Worktrees || self.focus == Focus::Projects {
            return Err("Select a worktree row first (l to open the list).");
        }
        let index = self.project_list.selected().ok_or("No project selected.")?;
        let project = &self.projects[index];
        if project.is_loading() {
            return Err("Wait until the current refresh or action has finished.");
        }
        let info = project
            .selected_worktree()
            .ok_or("Select a worktree row first.")?;
        Ok((index, info))
    }

    pub(super) fn run_action(&mut self, action: Action) {
        let Some(index) = self.project_list.selected() else {
            return;
        };
        let tx = self.tx.clone();
        let project = &mut self.projects[index];
        match action {
            Action::RemoveProject => self.remove_selected(),
            Action::DeleteWorktree(job) => {
                let name = job.branch.clone().unwrap_or_else(|| "worktree".into());
                project.busy = Some(format!("deleting {name}…"));
                worker::spawn_delete(&tx, &project.path, project.generation, job);
            }
            Action::Sync(job) => {
                project.busy = Some(format!("syncing {}…", job.branch));
                worker::spawn_sync(&tx, &project.path, project.generation, job);
            }
        }
    }

    pub(super) fn ask_delete_worktree(&mut self) {
        let (index, info) = match self.action_target() {
            Ok(target) => target,
            Err(why) => return self.set_status(why),
        };
        let project = &self.projects[index];
        let wt = &info.worktree;
        let home = self.home.as_deref();
        if wt.main || wt.path == project.path {
            return self.set_status("The main worktree can't be deleted.");
        }
        if wt.branch.as_deref() == Some(project.default_branch()) {
            let branch = project.default_branch();
            return self.set_status(format!(
                "{branch} is the default branch; tuitree won't delete it."
            ));
        }
        let base = project.base_ref();
        let mut lines = vec![(
            format!("Path:   {}", display_path(&wt.path, home)),
            Tone::Normal,
        )];
        lines.push(match &wt.branch {
            Some(branch) => (
                format!("Branch: {branch} (local branch is deleted too)"),
                Tone::Normal,
            ),
            None => (
                "Detached HEAD: only the worktree is removed.".into(),
                Tone::Normal,
            ),
        });
        let mut warnings = Vec::new();
        match &info.stats {
            Ok(stats) => {
                let untracked = stats.files.iter().filter(|f| f.untracked).count();
                if stats.uncommitted > 0 || untracked > 0 {
                    warnings.push(format!(
                        "⚠ uncommitted changes: {}, {}",
                        plural(stats.uncommitted as u64, "changed file", "changed files"),
                        plural(untracked as u64, "untracked file", "untracked files")
                    ));
                }
                if stats.unpushed > 0 {
                    warnings.push(format!(
                        "⚠ {} not on origin (unpushed)",
                        plural(stats.unpushed, "commit", "commits")
                    ));
                }
                if wt.branch.is_some() && stats.ahead > 0 {
                    warnings.push(format!(
                        "⚠ branch not merged into {base}: {} only on this branch",
                        plural(stats.ahead, "commit", "commits")
                    ));
                }
            }
            Err(err) => warnings.push(format!("⚠ could not read the worktree state: {err}")),
        }
        if warnings.is_empty() {
            lines.push(("Clean, pushed and merged.".into(), Tone::Muted));
        }
        lines.extend(warnings.into_iter().map(|w| (w, Tone::Warn)));
        lines.push(("Remote branches are not touched.".into(), Tone::Muted));
        let job = DeleteJob {
            path: wt.path.clone(),
            branch: wt.branch.clone(),
            force: false,
            worktree_removed: false,
        };
        self.mode = Mode::Confirm(Confirm {
            title: " Delete worktree ",
            lines,
            yes: "Delete",
            danger: false,
            action: Action::DeleteWorktree(job),
        });
    }

    pub(super) fn on_deleted(&mut self, index: usize, job: DeleteJob, outcome: DeleteOutcome) {
        let shown = display_path(&job.path, self.home.as_deref());
        match outcome {
            DeleteOutcome::Done => self.set_status(match &job.branch {
                Some(branch) => format!("Deleted worktree {shown} and branch {branch}."),
                None => format!("Deleted worktree {shown}."),
            }),
            DeleteOutcome::NeedsForce {
                reason,
                worktree_removed,
            } => {
                let branch = job.branch.clone().unwrap_or_default();
                let mut lines = vec![
                    ("The normal delete was refused:".to_string(), Tone::Danger),
                    (
                        reason.lines().next().unwrap_or("").to_string(),
                        Tone::Danger,
                    ),
                ];
                let (what, commands) = if worktree_removed {
                    lines.push((
                        format!(
                            "The worktree {shown} is already removed; branch {branch} is left."
                        ),
                        Tone::Normal,
                    ));
                    (
                        format!("commits that exist only on branch {branch}"),
                        format!("git branch -D {branch}"),
                    )
                } else if job.branch.is_some() {
                    (
                        format!("uncommitted changes in {shown} and commits only on {branch}"),
                        format!("git worktree remove --force, then git branch -D {branch}"),
                    )
                } else {
                    (
                        format!("uncommitted changes in {shown}"),
                        "git worktree remove --force".to_string(),
                    )
                };
                lines.push((
                    format!("Force delete permanently throws away {what}. This cannot be undone."),
                    Tone::Warn,
                ));
                lines.push((format!("Runs: {commands}"), Tone::Muted));
                self.mode = Mode::Confirm(Confirm {
                    title: " Force delete? ",
                    lines,
                    yes: "Force delete",
                    danger: true,
                    action: Action::DeleteWorktree(DeleteJob {
                        force: true,
                        worktree_removed,
                        ..job
                    }),
                });
            }
            DeleteOutcome::Failed(err) => self.set_status(format!("Delete failed: {err}")),
        }
        self.refresh(index);
    }

    pub(super) fn ask_sync(&mut self) {
        let (index, info) = match self.action_target() {
            Ok(target) => target,
            Err(why) => return self.set_status(why),
        };
        let project = &self.projects[index];
        let base = project.base_ref();
        let Some(branch) = info.worktree.branch.clone() else {
            return self.set_status("Detached HEAD: check out a branch in it to sync.");
        };
        let stats = match &info.stats {
            Ok(stats) => stats,
            Err(err) => return self.set_status(format!("Can't sync {branch}: {err}")),
        };
        let ff = match &stats.sync {
            None => {
                return self
                    .set_status("Still checking whether it can sync, try again in a moment.");
            }
            Some(SyncStatus::UpToDate) => {
                return self.set_status(format!("{branch} is already up to date with {base}."));
            }
            Some(SyncStatus::Dirty) => {
                return self.set_status(format!(
                    "{branch} has uncommitted changes. Commit or stash them, then sync."
                ));
            }
            Some(SyncStatus::Conflicts(files)) => {
                return self.set_status(format!(
                    "Merging {base} into {branch} would conflict in {}. Merge by hand.",
                    plural(files.len() as u64, "file", "files")
                ));
            }
            Some(SyncStatus::Unknown(why)) => {
                return self.set_status(format!("Can't check {branch}: {why}"));
            }
            Some(SyncStatus::Ready { ff }) => *ff,
        };
        let shown = display_path(&info.worktree.path, self.home.as_deref());
        let lines = vec![
            (format!("Merge {base} into {branch}?"), Tone::Normal),
            (
                format!(
                    "{branch} is {} behind and {} ahead.",
                    plural(stats.behind, "commit", "commits"),
                    plural(stats.ahead, "commit", "commits")
                ),
                Tone::Normal,
            ),
            if ff {
                ("Fast-forward: no merge commit needed.".into(), Tone::Muted)
            } else {
                (
                    "Creates a merge commit. No history is rewritten, so it is safe for pushed branches."
                        .into(),
                    Tone::Muted,
                )
            },
            (format!("Runs: git merge {base}  (in {shown})"), Tone::Muted),
        ];
        let job = SyncJob {
            worktree: info.worktree.path.clone(),
            branch,
            base,
        };
        self.mode = Mode::Confirm(Confirm {
            title: " Sync with origin ",
            lines,
            yes: "Sync",
            danger: false,
            action: Action::Sync(job),
        });
    }

    pub(super) fn on_synced(&mut self, index: usize, job: SyncJob, result: Result<String, String>) {
        self.set_status(match result {
            Ok(summary) => format!("Synced {} with {}: {summary}.", job.branch, job.base),
            Err(err) => format!("Sync of {} failed: {err}", job.branch),
        });
        self.refresh(index);
    }
}
