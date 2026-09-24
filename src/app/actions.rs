//! Planning worktree actions: which rows a delete or sync applies to (the marked rows, or the
//! selected one), which of them are skipped and why, and the one confirmation dialog.

use std::path::{Path, PathBuf};

use super::queue::{Planned, Skipped, Verb};
use super::{App, Focus, Mode, Project, Tab};
use crate::config::display_path;
use crate::git::{Merged, SyncStatus, WorktreeInfo};
use crate::worker::Job;

/// A yes/no dialog and what "yes" does.
pub struct Confirm {
    pub title: String,
    pub lines: Vec<(String, Tone)>,
    /// Label of the "yes" button.
    pub yes: String,
    /// Destructive: drawn in red.
    pub danger: bool,
    pub action: Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Muted,
    Warn,
}

pub enum Action {
    RemoveProject,
    Quit,
    /// Queue jobs for a project and list the skipped rows in the queue.
    Enqueue {
        project: PathBuf,
        planned: Vec<Planned>,
        skipped: Vec<Skipped>,
    },
}

fn plural(count: impl Into<u64>, one: &str, many: &str) -> String {
    let count = count.into();
    format!("{count} {}", if count == 1 { one } else { many })
}

pub fn branch_label(info: &WorktreeInfo) -> String {
    match (&info.worktree.branch, &info.worktree.head) {
        (Some(branch), _) => branch.clone(),
        (None, Some(head)) => format!("(detached {})", &head[..head.len().min(7)]),
        (None, None) => "(unknown)".to_string(),
    }
}

/// Why a row cannot be synced.
enum NotReady {
    Detached,
    Unreadable(String),
    Queued,
    Checking,
    UpToDate,
    Dirty,
    Conflicts(usize),
    Unknown(String),
    Merged,
}

impl NotReady {
    /// Short reason for batch dialogs and the queue.
    fn short(&self) -> String {
        match self {
            NotReady::Detached => "detached HEAD".into(),
            NotReady::Unreadable(err) => format!("can't read it: {err}"),
            NotReady::Queued => "already has a queued job".into(),
            NotReady::Checking => "still checking".into(),
            NotReady::UpToDate => "already up to date".into(),
            NotReady::Dirty => "uncommitted changes".into(),
            NotReady::Conflicts(n) => {
                format!("would conflict in {}", plural(*n as u64, "file", "files"))
            }
            NotReady::Unknown(why) => why.clone(),
            NotReady::Merged => "already merged".into(),
        }
    }

    /// Full sentence for a single row.
    fn sentence(&self, branch: &str, base: &str) -> String {
        match self {
            NotReady::Detached => "Detached HEAD: check out a branch in it to sync.".into(),
            NotReady::Unreadable(err) => format!("Can't sync {branch}: {err}"),
            NotReady::Queued => format!("{branch} already has a queued job."),
            NotReady::Checking => {
                "Still checking whether it can sync, try again in a moment.".into()
            }
            NotReady::UpToDate => format!("{branch} is already up to date with {base}."),
            NotReady::Dirty => {
                format!("{branch} has uncommitted changes. Commit or stash them, then sync.")
            }
            NotReady::Conflicts(n) => format!(
                "Merging {base} into {branch} would conflict in {}. Merge by hand.",
                plural(*n as u64, "file", "files")
            ),
            NotReady::Unknown(why) => format!("Can't check {branch}: {why}"),
            NotReady::Merged => {
                format!("{branch} is already merged into {base}; delete it (x) instead of syncing.")
            }
        }
    }
}

impl App {
    /// The rows `x`/`s` act on: the marked ones in list order, else the selected one.
    fn action_targets(&self) -> Result<(usize, Vec<&WorktreeInfo>, bool), &'static str> {
        if self.tab != Tab::Worktrees || self.focus == Focus::Projects {
            return Err("Select a worktree row first (l to open the list).");
        }
        let index = self.project_list.selected().ok_or("No project selected.")?;
        let project = &self.projects[index];
        let marked: Vec<&WorktreeInfo> = project
            .worktree_list()
            .iter()
            .filter(|wt| project.marked.contains(&wt.worktree.path))
            .collect();
        if !marked.is_empty() {
            return Ok((index, marked, true));
        }
        let selected = project
            .selected_worktree()
            .ok_or("Select a worktree row first.")?;
        Ok((index, vec![selected], false))
    }

    pub(super) fn run_action(&mut self, action: Action) {
        match action {
            Action::RemoveProject => self.remove_selected(),
            Action::Quit => self.should_quit = true,
            Action::Enqueue {
                project,
                planned,
                skipped,
            } => self.enqueue(&project, planned, skipped),
        }
    }

    pub(super) fn ask_delete_worktrees(&mut self) {
        let (index, targets, batch) = match self.action_targets() {
            Ok(targets) => targets,
            Err(why) => return self.set_status(why),
        };
        let project = &self.projects[index];
        let home = self.home.as_deref();
        let base = project.base_ref();
        let mut planned = Vec::new();
        let mut skipped = Vec::new();
        let mut lines = vec![(
            "Deletes with --force: uncommitted and untracked files in them are lost.".to_string(),
            Tone::Warn,
        )];
        for info in targets {
            let wt = &info.worktree;
            let label = branch_label(info);
            let refusal = if wt.main || wt.path == project.path {
                Some("The main worktree can't be deleted.".to_string())
            } else if wt.branch.as_deref() == Some(project.default_branch()) {
                Some(format!(
                    "{label} is the default branch; tuitree won't delete it."
                ))
            } else if self.active_job(&wt.path).is_some() {
                Some(format!("{label} already has a queued job."))
            } else {
                None
            };
            if let Some(reason) = refusal {
                lines.push((format!("• {label} skipped: {reason}"), Tone::Muted));
                skipped.push(Skipped {
                    worktree: wt.path.clone(),
                    label,
                    verb: Verb::Delete,
                    reason,
                });
                continue;
            }
            let what = match &wt.branch {
                Some(branch) => format!("worktree and local branch {branch}"),
                None => "worktree only (detached HEAD)".to_string(),
            };
            lines.push((
                format!("• {}  {what}", display_path(&wt.path, home)),
                Tone::Normal,
            ));
            lines.extend(
                delete_warnings(info, &base)
                    .into_iter()
                    .map(|warning| (format!("    {warning}"), Tone::Warn)),
            );
            planned.push(Planned {
                worktree: wt.path.clone(),
                label,
                verb: Verb::Delete,
                job: Job::Delete {
                    path: wt.path.clone(),
                    branch: wt.branch.clone(),
                },
            });
        }
        if planned.is_empty() {
            let reasons: Vec<String> = skipped.into_iter().map(|s| s.reason).collect();
            return self.set_status(if batch {
                format!("Nothing to delete: {}", reasons.join(" "))
            } else {
                reasons.join(" ")
            });
        }
        lines.push(("Remote branches are not touched.".into(), Tone::Muted));
        let count = planned.len();
        self.mode = Mode::Confirm(Confirm {
            title: if count == 1 {
                " Delete worktree ".into()
            } else {
                format!(" Delete {count} worktrees ")
            },
            lines,
            yes: if count == 1 {
                "Delete".into()
            } else {
                format!("Delete {count}")
            },
            danger: true,
            action: Action::Enqueue {
                project: project.path.clone(),
                planned,
                skipped,
            },
        });
    }

    fn not_ready(&self, info: &WorktreeInfo) -> Result<bool, NotReady> {
        if info.worktree.branch.is_none() {
            return Err(NotReady::Detached);
        }
        if self.active_job(&info.worktree.path).is_some() {
            return Err(NotReady::Queued);
        }
        let stats = info
            .stats
            .as_ref()
            .map_err(|err| NotReady::Unreadable(err.clone()))?;
        if stats.merged.is_some() {
            return Err(NotReady::Merged);
        }
        match &stats.sync {
            None => Err(NotReady::Checking),
            Some(SyncStatus::UpToDate) => Err(NotReady::UpToDate),
            Some(SyncStatus::Dirty) => Err(NotReady::Dirty),
            Some(SyncStatus::Conflicts(files)) => Err(NotReady::Conflicts(files.len())),
            Some(SyncStatus::Unknown(why)) => Err(NotReady::Unknown(why.clone())),
            Some(SyncStatus::Ready { ff }) => Ok(*ff),
        }
    }

    pub(super) fn ask_sync(&mut self) {
        let (index, targets, batch) = match self.action_targets() {
            Ok(targets) => targets,
            Err(why) => return self.set_status(why),
        };
        let project = &self.projects[index];
        let base = project.base_ref();
        let mut planned = Vec::new();
        let mut skipped = Vec::new();
        let mut lines = Vec::new();
        for info in &targets {
            let label = branch_label(info);
            match self.not_ready(info) {
                Err(why) if !batch => return self.set_status(why.sentence(&label, &base)),
                Err(why) => {
                    let reason = why.short();
                    lines.push((format!("• {label} skipped: {reason}"), Tone::Muted));
                    skipped.push(Skipped {
                        worktree: info.worktree.path.clone(),
                        label,
                        verb: Verb::Sync,
                        reason,
                    });
                }
                Ok(ff) => {
                    lines.extend(sync_lines(
                        info,
                        project,
                        &label,
                        ff,
                        batch,
                        self.home.as_deref(),
                    ));
                    planned.push(Planned {
                        worktree: info.worktree.path.clone(),
                        label,
                        verb: Verb::Sync,
                        job: Job::Sync {
                            worktree: info.worktree.path.clone(),
                            base: base.clone(),
                        },
                    });
                }
            }
        }
        if planned.is_empty() {
            let reasons: Vec<String> = skipped
                .iter()
                .map(|s| format!("{}: {}", s.label, s.reason))
                .collect();
            return self.set_status(format!("Nothing to sync. {}", reasons.join("; ")));
        }
        if batch {
            lines.insert(
                0,
                (
                    format!("Merge {base} into each ready worktree (git merge, no history is rewritten):"),
                    Tone::Normal,
                ),
            );
        }
        let count = planned.len();
        self.mode = Mode::Confirm(Confirm {
            title: if batch {
                format!(" Sync {count} with origin ")
            } else {
                " Sync with origin ".into()
            },
            lines,
            yes: if count == 1 {
                "Sync".into()
            } else {
                format!("Sync {count}")
            },
            danger: false,
            action: Action::Enqueue {
                project: project.path.clone(),
                planned,
                skipped,
            },
        });
    }
}

/// What the user loses or should know before deleting a worktree.
fn delete_warnings(info: &WorktreeInfo, base: &str) -> Vec<String> {
    let stats = match &info.stats {
        Ok(stats) => stats,
        Err(err) => return vec![format!("⚠ could not read the worktree state: {err}")],
    };
    let mut warnings = Vec::new();
    if let Some(merged) = stats.merged {
        warnings.push(match merged {
            Merged::Pr(number) => format!("✓ merged (PR #{number}); its work is in {base}"),
            Merged::Content => format!("✓ merged; its work is already in {base}"),
        });
    }
    let untracked = stats.files.iter().filter(|f| f.untracked).count();
    if stats.uncommitted > 0 || untracked > 0 {
        warnings.push(format!(
            "⚠ uncommitted changes: {}, {}",
            plural(stats.uncommitted as u64, "changed file", "changed files"),
            plural(untracked as u64, "untracked file", "untracked files")
        ));
    }
    // A merged branch's own commits are not lost even when no origin ref has them.
    if stats.unpushed > 0 && stats.merged.is_none() {
        warnings.push(format!(
            "⚠ {} not on origin (unpushed)",
            plural(stats.unpushed, "commit", "commits")
        ));
    }
    if info.worktree.branch.is_some() && stats.ahead > 0 && stats.merged.is_none() {
        warnings.push(format!(
            "⚠ branch not merged into {base}: {} only on this branch",
            plural(stats.ahead, "commit", "commits")
        ));
    }
    if info.worktree.locked {
        warnings.push("⚠ locked (git worktree lock); it is unlocked and removed".into());
    }
    if warnings.is_empty() {
        warnings.push("clean, pushed and merged".into());
    }
    warnings
}

fn sync_lines(
    info: &WorktreeInfo,
    project: &Project,
    label: &str,
    ff: bool,
    batch: bool,
    home: Option<&Path>,
) -> Vec<(String, Tone)> {
    let base = project.base_ref();
    let (ahead, behind) = info
        .stats
        .as_ref()
        .map_or((0, 0), |stats| (stats.ahead, stats.behind));
    if batch {
        let how = if ff {
            "fast-forward".to_string()
        } else {
            format!("merge commit ({behind} behind, {ahead} ahead)")
        };
        return vec![(format!("• {label}: {how}"), Tone::Normal)];
    }
    let shown = display_path(&info.worktree.path, home);
    vec![
        (format!("Merge {base} into {label}?"), Tone::Normal),
        (
            format!(
                "{label} is {} behind and {} ahead.",
                plural(behind, "commit", "commits"),
                plural(ahead, "commit", "commits")
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
    ]
}
