//! Reading repository state by shelling out to `git`, plus pure parsers for its output.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use crate::cmd::{self, CmdError};
use crate::model::FileStat;

/// Run at most this many worktrees' git commands at the same time.
const PARALLEL_WORKTREES: usize = 8;
/// git treats a file as binary when its first 8000 bytes contain a NUL byte.
const BINARY_SNIFF_LEN: usize = 8000;

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: Option<String>,
    /// Short branch name; `None` when HEAD is detached.
    pub branch: Option<String>,
    pub bare: bool,
    /// The worktree directory no longer exists.
    pub prunable: bool,
    /// `git worktree lock` was used on it; removing needs `--force --force`.
    pub locked: bool,
    /// The repository's main worktree (always listed first by git).
    pub main: bool,
}

/// Position of a worktree relative to `origin/<default>`.
#[derive(Debug, Clone)]
pub struct WorktreeStats {
    pub ahead: u64,
    pub behind: u64,
    /// Tracked files with staged or unstaged changes.
    pub uncommitted: usize,
    /// Commits on HEAD that no `origin/*` branch contains.
    pub unpushed: u64,
    /// Changes vs the merge-base, including staged, unstaged and untracked files.
    pub files: Vec<FileStat>,
    /// `None` until the (slower) merge check has run.
    pub sync: Option<SyncStatus>,
    /// The branch's work is already in `origin/<default>` (e.g. squash or rebase merged), even
    /// though its own commits still count as ahead. Set by the merge check.
    pub merged: Option<Merged>,
}

/// How a branch was found to be merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merged {
    /// A merged GitHub PR has this branch as head, and HEAD has nothing newer than it.
    Pr(u64),
    /// Merging the branch into `origin/<default>` would change nothing.
    Content,
}

/// A pull request of this repository (fork PRs are left out), as far as local branches go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoPr {
    pub number: u64,
    pub head_ref_name: String,
    pub head_ref_oid: String,
    pub state: PrState,
    /// ISO 8601 timestamp, so it sorts chronologically as text.
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PrState {
    pub fn name(self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Draft => "draft",
            PrState::Merged => "merged",
            PrState::Closed => "closed",
        }
    }
}

/// The PR a worktree's branch belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrLink {
    /// No PR data: gh is missing, failed, or origin is not on GitHub.
    Unknown,
    /// gh knows no PR with this branch as head (or HEAD is detached).
    None,
    /// The newest PR with this branch as head.
    Pr { number: u64, state: PrState },
}

/// Whether `git merge origin/<default>` would bring the worktree up to date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStatus {
    UpToDate,
    /// Merges cleanly; `ff` when it is a fast-forward.
    Ready {
        ff: bool,
    },
    /// Files that would conflict.
    Conflicts(Vec<String>),
    /// Uncommitted changes to tracked files block the merge.
    Dirty,
    /// The check could not run.
    Unknown(String),
}

#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub worktree: Worktree,
    pub stats: Result<WorktreeStats, String>,
    pub pr: PrLink,
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    cmd::run("git", dir, args).map_err(describe)
}

fn describe(err: CmdError) -> String {
    match err {
        CmdError::Missing => "git is not installed".to_string(),
        CmdError::Failed(msg) => msg,
    }
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    git(dir, args).map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// The root of the working tree containing `dir`; fails when `dir` is not in a git repo.
pub fn repo_toplevel(dir: &Path) -> Result<PathBuf, String> {
    let out = git_text(dir, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(out.trim()))
}

/// `owner/repo` when `origin` points at GitHub.
///
/// Reads the configured URL rather than `git remote get-url`, so `insteadOf` rewrites do not
/// hide which GitHub repository the remote stands for.
pub fn github_repo(dir: &Path) -> Option<String> {
    let url = git_text(dir, &["config", "--get", "remote.origin.url"]).ok()?;
    parse_github_remote(&url)
}

/// The remote's default branch from `refs/remotes/origin/HEAD`, falling back to `main`.
pub fn default_branch(dir: &Path) -> String {
    git_text(dir, &["symbolic-ref", "refs/remotes/origin/HEAD"])
        .ok()
        .and_then(|out| parse_default_branch(&out))
        .unwrap_or_else(|| "main".to_string())
}

pub fn fetch(dir: &Path) -> Result<(), String> {
    git(dir, &["fetch", "origin", "--prune"]).map(|_| ())
}

/// `git merge-tree --write-tree` (used to predict conflicts) needs git 2.38 or newer.
fn supports_merge_tree(dir: &Path) -> bool {
    git_text(dir, &["--version"])
        .ok()
        .and_then(|out| parse_git_version(&out))
        .is_some_and(|version| version >= (2, 38))
}

/// Lists the repository's worktrees and computes their stats against `base` (e.g. `origin/main`).
///
/// `prs` (from gh, `None` when unavailable) links each branch to its newest PR. With `full`,
/// also predicts how `git merge <base>` would go in each worktree and finds branches whose work
/// is already merged: by a merged PR in `prs`, or because merging them into `base` would
/// change nothing (squash and rebase merges).
pub fn load_worktrees(
    repo: &Path,
    base: &str,
    full: bool,
    prs: Option<&[RepoPr]>,
) -> Result<Vec<WorktreeInfo>, String> {
    let listing = git_text(repo, &["worktree", "list", "--porcelain"])?;
    let worktrees: Vec<Worktree> = parse_worktree_porcelain(&listing)
        .into_iter()
        .filter(|wt| !wt.bare)
        .collect();
    let check = full.then(|| MergeCheck {
        base,
        base_tree: git_text(repo, &["rev-parse", &format!("{base}^{{tree}}")])
            .map(|out| out.trim().to_string())
            .unwrap_or_default(),
        merge_tree: supports_merge_tree(repo),
        prs: prs.unwrap_or_default(),
    });
    let mut infos = Vec::with_capacity(worktrees.len());
    for chunk in worktrees.chunks(PARALLEL_WORKTREES) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|wt| {
                    let check = check.as_ref();
                    scope.spawn(move || stats_for(wt, base, check))
                })
                .collect();
            for (wt, handle) in chunk.iter().zip(handles) {
                let stats = handle
                    .join()
                    .unwrap_or_else(|_| Err("failed to read worktree".to_string()));
                infos.push(WorktreeInfo {
                    worktree: wt.clone(),
                    stats,
                    pr: pr_for_branch(wt.branch.as_deref(), prs),
                });
            }
        });
    }
    Ok(infos)
}

/// Inputs of the merge check, shared by all worktrees of one load.
struct MergeCheck<'a> {
    base: &'a str,
    /// Tree of `base`; a branch whose merge result has this tree is already merged.
    base_tree: String,
    /// git supports `merge-tree --write-tree`.
    merge_tree: bool,
    prs: &'a [RepoPr],
}

/// The newest PR whose head is `branch`.
fn pr_for_branch(branch: Option<&str>, prs: Option<&[RepoPr]>) -> PrLink {
    let Some(prs) = prs else {
        return PrLink::Unknown;
    };
    branch
        .and_then(|branch| {
            prs.iter()
                .filter(|pr| pr.head_ref_name == branch)
                .max_by(|a, b| (&a.created_at, a.number).cmp(&(&b.created_at, b.number)))
        })
        .map_or(PrLink::None, |pr| PrLink::Pr {
            number: pr.number,
            state: pr.state,
        })
}

fn stats_for(
    wt: &Worktree,
    base: &str,
    check: Option<&MergeCheck>,
) -> Result<WorktreeStats, String> {
    if wt.prunable || !wt.path.is_dir() {
        return Err("worktree directory is missing".to_string());
    }
    let dir = wt.path.as_path();
    let range = format!("{base}...HEAD");
    let counts = git_text(dir, &["rev-list", "--left-right", "--count", &range])?;
    let (behind, ahead) = parse_left_right_count(&counts)
        .ok_or_else(|| format!("unexpected rev-list output: {}", counts.trim()))?;
    let merge_base = git_text(dir, &["merge-base", "HEAD", base])?;
    let merge_base = merge_base.trim();
    // Diffing the merge-base against the working tree covers commits, staged and unstaged edits.
    let numstat = git(
        dir,
        &["diff", "--numstat", "-z", "--no-ext-diff", merge_base, "--"],
    )?;
    let mut files = parse_numstat_z(&numstat);
    let untracked = git(dir, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    files.extend(
        split_z(&untracked).map(|p| untracked_stat(dir, String::from_utf8_lossy(p).into_owned())),
    );
    let uncommitted = split_z(&git(dir, &["diff", "--name-only", "-z", "HEAD", "--"])?).count();
    let unpushed = git_text(
        dir,
        &["rev-list", "--count", "HEAD", "--not", "--remotes=origin"],
    )?
    .trim()
    .parse()
    .unwrap_or(0);
    let (sync, merged) = match check {
        None => (None, None),
        Some(check) => {
            // Only branches with commits of their own need a merge simulation.
            let merge = (ahead > 0 && check.merge_tree).then(|| merge_tree(dir, check.base));
            let merged = if ahead > 0 {
                merged_by_pr(dir, wt, check.prs).or_else(|| {
                    let same_tree = matches!(&merge, Some(Ok(m)) if m.conflicts.is_empty()
                        && !check.base_tree.is_empty()
                        && m.tree == check.base_tree);
                    same_tree.then_some(Merged::Content)
                })
            } else {
                None
            };
            let sync = sync_status(ahead, behind, uncommitted, merge.as_ref());
            (Some(sync), merged)
        }
    };
    Ok(WorktreeStats {
        ahead,
        behind,
        uncommitted,
        unpushed,
        files,
        sync,
        merged,
    })
}

/// A merged PR whose head branch is this worktree's branch, as long as HEAD has nothing newer
/// than the PR's head (a reused branch name with new work is not merged).
fn merged_by_pr(dir: &Path, wt: &Worktree, prs: &[RepoPr]) -> Option<Merged> {
    let branch = wt.branch.as_deref()?;
    prs.iter()
        .filter(|pr| pr.state == PrState::Merged && pr.head_ref_name == branch)
        .find(|pr| {
            wt.head.as_deref() == Some(pr.head_ref_oid.as_str())
                || git(
                    dir,
                    &["merge-base", "--is-ancestor", "HEAD", &pr.head_ref_oid],
                )
                .is_ok()
        })
        .map(|pr| Merged::Pr(pr.number))
}

fn split_z(raw: &[u8]) -> impl Iterator<Item = &[u8]> {
    raw.split(|&b| b == 0).filter(|p| !p.is_empty())
}

/// Result of `git merge-tree --write-tree HEAD <base>`: the merged tree and conflicted files.
struct MergeTree {
    tree: String,
    conflicts: Vec<String>,
}

/// Simulates `git merge <base>` without touching the worktree: `git merge-tree` only writes
/// objects, never refs, the index or files.
fn merge_tree(dir: &Path, base: &str) -> Result<MergeTree, String> {
    let args = [
        "merge-tree",
        "--write-tree",
        "--name-only",
        "-z",
        "HEAD",
        base,
    ];
    let out = cmd::run_status("git", dir, &args).map_err(describe)?;
    let tree = || {
        out.stdout
            .split(|&b| b == 0)
            .next()
            .map(|t| String::from_utf8_lossy(t).trim().to_string())
            .unwrap_or_default()
    };
    match out.status.code() {
        Some(0) => Ok(MergeTree {
            tree: tree(),
            conflicts: Vec::new(),
        }),
        Some(1) => Ok(MergeTree {
            tree: tree(),
            conflicts: parse_merge_tree_conflicts(&out.stdout),
        }),
        _ => Err(String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or("git merge-tree failed")
            .to_string()),
    }
}

/// Predicts `git merge <base>` from the counts and the merge simulation (run when ahead > 0).
fn sync_status(
    ahead: u64,
    behind: u64,
    uncommitted: usize,
    merge: Option<&Result<MergeTree, String>>,
) -> SyncStatus {
    if behind == 0 {
        return SyncStatus::UpToDate;
    }
    if uncommitted > 0 {
        return SyncStatus::Dirty;
    }
    if ahead == 0 {
        return SyncStatus::Ready { ff: true };
    }
    match merge {
        None => SyncStatus::Unknown("conflict check needs git 2.38 or newer".to_string()),
        Some(Err(err)) => SyncStatus::Unknown(err.clone()),
        Some(Ok(m)) if m.conflicts.is_empty() => SyncStatus::Ready { ff: false },
        Some(Ok(m)) => SyncStatus::Conflicts(m.conflicts.clone()),
    }
}

/// Runs `git merge --no-edit <base>` in the worktree. A merge that stops with conflicts is
/// aborted so the worktree is left as it was. Returns a short summary.
pub fn merge(dir: &Path, base: &str) -> Result<String, String> {
    match git_text(dir, &["merge", "--no-edit", base]) {
        Ok(out) if out.contains("Fast-forward") => Ok("fast-forwarded".to_string()),
        Ok(_) => Ok("merge commit created".to_string()),
        Err(err) => {
            let in_progress = git(dir, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).is_ok();
            if !in_progress {
                return Err(err);
            }
            match git(dir, &["merge", "--abort"]) {
                Ok(_) => Err(
                    "the merge hit conflicts, so it was aborted (git merge --abort); nothing changed."
                        .to_string(),
                ),
                Err(abort) => Err(format!(
                    "the merge hit conflicts and `git merge --abort` failed: {abort}"
                )),
            }
        }
    }
}

/// Deletes a worktree and its local branch in one go, without asking git twice:
///
/// - directory already gone: `git worktree prune`, and `git worktree remove --force --force`
///   if a lock kept the entry around;
/// - otherwise `git worktree remove --force` (`--force --force` when locked), which also
///   handles untracked and ignored files and submodules;
/// - then `git branch -D <branch>`.
///
/// The worktree state is re-read here, so it is current even when the job waited in a queue.
/// Remote branches are never touched. Errors carry git's full stderr.
pub fn delete_worktree(repo: &Path, path: &Path, branch: Option<&str>) -> Result<(), String> {
    let entry = find_worktree(repo, path)?;
    if entry.as_ref().is_some_and(|wt| wt.main) {
        return Err("refusing to delete the main worktree".to_string());
    }
    let shown = path.to_string_lossy();
    if let Some(wt) = entry {
        if wt.path.is_dir() {
            let mut args = vec!["worktree", "remove", "--force"];
            if wt.locked {
                args.push("--force");
            }
            args.push(&shown);
            git(repo, &args).map_err(|err| format!("git {} failed:\n{err}", args.join(" ")))?;
        } else {
            git(repo, &["worktree", "prune"])
                .map_err(|err| format!("git worktree prune failed:\n{err}"))?;
            if find_worktree(repo, path)?.is_some() {
                let args = ["worktree", "remove", "--force", "--force", &shown];
                git(repo, &args).map_err(|err| format!("git {} failed:\n{err}", args.join(" ")))?;
            }
        }
    }
    if let Some(branch) = branch {
        git(repo, &["branch", "-D", branch]).map_err(|err| {
            format!("The worktree is gone, but git branch -D {branch} failed:\n{err}")
        })?;
    }
    Ok(())
}

/// The registered worktree at `path`, if any.
fn find_worktree(repo: &Path, path: &Path) -> Result<Option<Worktree>, String> {
    let listing = git_text(repo, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktree_porcelain(&listing)
        .into_iter()
        .find(|wt| wt.path == path))
}

/// Untracked files count as new files: every line is an addition, binary files get no counts.
fn untracked_stat(root: &Path, path: String) -> FileStat {
    let full = root.join(&path);
    let added = match fs::symlink_metadata(&full) {
        // git stores a symlink as a one-line blob holding the target path.
        Ok(meta) if meta.file_type().is_symlink() => Some(1),
        Ok(meta) if meta.is_file() => File::open(&full).and_then(count_text_lines).ok().flatten(),
        _ => None,
    };
    FileStat {
        path,
        added,
        deleted: added.map(|_| 0),
        untracked: true,
    }
}

/// Counts lines the way `git diff --numstat` does for a new file (a final line without a
/// trailing newline still counts). Returns `None` for binary content.
fn count_text_lines(reader: impl Read) -> io::Result<Option<u64>> {
    let mut reader = BufReader::new(reader);
    let mut lines = 0u64;
    let mut sniffed = 0usize;
    let mut last = None;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        if sniffed < BINARY_SNIFF_LEN {
            let window = buf.len().min(BINARY_SNIFF_LEN - sniffed);
            if buf[..window].contains(&0) {
                return Ok(None);
            }
            sniffed += window;
        }
        lines += buf.iter().filter(|&&b| b == b'\n').count() as u64;
        last = buf.last().copied();
        let len = buf.len();
        reader.consume(len);
    }
    if last.is_some_and(|b| b != b'\n') {
        lines += 1;
    }
    Ok(Some(lines))
}

/// Parses `git worktree list --porcelain`: blank-line separated records of `key value` lines.
pub fn parse_worktree_porcelain(text: &str) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let mut current: Option<Worktree> = None;
    for line in text.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key == "worktree" {
            worktrees.extend(current.take());
            current = Some(Worktree {
                path: PathBuf::from(value),
                head: None,
                branch: None,
                bare: false,
                prunable: false,
                locked: false,
                main: worktrees.is_empty(),
            });
            continue;
        }
        let Some(wt) = current.as_mut() else { continue };
        match key {
            "HEAD" => wt.head = Some(value.to_string()),
            "branch" => {
                wt.branch = Some(
                    value
                        .strip_prefix("refs/heads/")
                        .unwrap_or(value)
                        .to_string(),
                )
            }
            "bare" => wt.bare = true,
            "prunable" => wt.prunable = true,
            "locked" => wt.locked = true,
            _ => {}
        }
    }
    worktrees.extend(current);
    worktrees
}

/// Parses `git diff --numstat -z`. Binary files report `-` counts, which become `None`.
/// Renames arrive as `added\tdeleted\t\0old\0new\0` and are shown as `old → new`.
pub fn parse_numstat_z(raw: &[u8]) -> Vec<FileStat> {
    let mut fields = raw.split(|&b| b == 0).map(String::from_utf8_lossy);
    let mut files = Vec::new();
    while let Some(record) = fields.next() {
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let path = if path.is_empty() {
            let from = fields.next().unwrap_or_default();
            let to = fields.next().unwrap_or_default();
            format!("{from} → {to}")
        } else {
            path.to_string()
        };
        files.push(FileStat {
            path,
            added: added.parse().ok(),
            deleted: deleted.parse().ok(),
            untracked: false,
        });
    }
    files
}

/// Parses `git rev-list --left-right --count A...B` output: `<left>\t<right>`.
pub fn parse_left_right_count(text: &str) -> Option<(u64, u64)> {
    let mut numbers = text.split_whitespace().map(str::parse::<u64>);
    let left = numbers.next()?.ok()?;
    let right = numbers.next()?.ok()?;
    numbers.next().is_none().then_some((left, right))
}

/// `refs/remotes/origin/main` → `main`.
pub fn parse_default_branch(symbolic_ref: &str) -> Option<String> {
    symbolic_ref
        .trim()
        .strip_prefix("refs/remotes/origin/")
        .filter(|branch| !branch.is_empty())
        .map(str::to_string)
}

/// `git version 2.43.0` → `(2, 43)`.
pub fn parse_git_version(text: &str) -> Option<(u32, u32)> {
    let version = text.trim().strip_prefix("git version ")?;
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Parses `git merge-tree --write-tree --name-only -z` output of a conflicted merge: the tree
/// id, then the conflicted paths, then an empty field before the informational messages.
pub fn parse_merge_tree_conflicts(raw: &[u8]) -> Vec<String> {
    raw.split(|&b| b == 0)
        .skip(1)
        .take_while(|field| !field.is_empty())
        .map(|field| String::from_utf8_lossy(field).into_owned())
        .collect()
}

/// Extracts `owner/repo` from GitHub remote URLs in https, ssh:// and scp-like forms.
pub fn parse_github_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = match url.split_once("://") {
        Some((_, rest)) => {
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next()?;
            (host.split(':').next()?, path)
        }
        None => {
            let (user_host, path) = url.split_once(':')?;
            (user_host.rsplit('@').next()?, path)
        }
    };
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    let valid = |part: &str| !part.is_empty() && !part.contains('/');
    (valid(owner) && valid(repo)).then(|| format!("{owner}/{repo}"))
}
