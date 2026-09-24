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
}

/// Position of a worktree relative to `origin/<default>`.
#[derive(Debug, Clone)]
pub struct WorktreeStats {
    pub ahead: u64,
    pub behind: u64,
    /// Changes vs the merge-base, including staged, unstaged and untracked files.
    pub files: Vec<FileStat>,
}

#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub worktree: Worktree,
    pub stats: Result<WorktreeStats, String>,
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    cmd::run("git", dir, args).map_err(|err| match err {
        CmdError::Missing => "git is not installed".to_string(),
        CmdError::Failed(msg) => msg,
    })
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

/// Lists the repository's worktrees and computes their stats against `base` (e.g. `origin/main`).
pub fn load_worktrees(repo: &Path, base: &str) -> Result<Vec<WorktreeInfo>, String> {
    let listing = git_text(repo, &["worktree", "list", "--porcelain"])?;
    let worktrees: Vec<Worktree> = parse_worktree_porcelain(&listing)
        .into_iter()
        .filter(|wt| !wt.bare)
        .collect();
    let mut infos = Vec::with_capacity(worktrees.len());
    for chunk in worktrees.chunks(PARALLEL_WORKTREES) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|wt| scope.spawn(move || stats_for(wt, base)))
                .collect();
            for (wt, handle) in chunk.iter().zip(handles) {
                let stats = handle
                    .join()
                    .unwrap_or_else(|_| Err("failed to read worktree".to_string()));
                infos.push(WorktreeInfo {
                    worktree: wt.clone(),
                    stats,
                });
            }
        });
    }
    Ok(infos)
}

fn stats_for(wt: &Worktree, base: &str) -> Result<WorktreeStats, String> {
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
        untracked
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| untracked_stat(dir, String::from_utf8_lossy(p).into_owned())),
    );
    Ok(WorktreeStats {
        ahead,
        behind,
        files,
    })
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
