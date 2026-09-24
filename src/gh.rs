//! Pull requests via the GitHub CLI (`gh`), plus pure parsers for its JSON.

use std::path::Path;

use serde::Deserialize;

use crate::cmd::{self, CmdError};
use crate::git::{PrState, RepoPr};
use crate::model::FileStat;

const PR_FIELDS: &str = "number,title,headRefName,author,isDraft,changedFiles,additions,deletions";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub head_ref_name: String,
    #[serde(default)]
    pub author: Option<Author>,
    pub is_draft: bool,
    pub changed_files: u64,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Author {
    pub login: String,
}

#[derive(Deserialize)]
struct PrFiles {
    files: Vec<PrFile>,
}

#[derive(Deserialize)]
struct PrFile {
    path: String,
    additions: u64,
    deletions: u64,
}

/// Open PRs of `repo` (`owner/name`).
pub fn list_prs(dir: &Path, repo: &str) -> Result<Vec<PullRequest>, String> {
    let args = [
        "pr", "list", "-R", repo, "--state", "open", "--limit", "100", "--json", PR_FIELDS,
    ];
    let out = cmd::run("gh", dir, &args).map_err(describe_error)?;
    parse_pr_list(&String::from_utf8_lossy(&out))
}

/// How many recent PRs (any state) to match against local branches.
const REPO_PR_LIMIT: &str = "300";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoPrJson {
    number: u64,
    head_ref_name: String,
    head_ref_oid: String,
    state: String,
    is_draft: bool,
    is_cross_repository: bool,
    created_at: String,
}

/// The most recent PRs of `repo` in any state, without PRs from forks. One call per refresh
/// links every worktree's branch to its PR and recognizes squash or rebase merged branches.
pub fn list_repo_prs(dir: &Path, repo: &str) -> Result<Vec<RepoPr>, String> {
    let args = [
        "pr",
        "list",
        "-R",
        repo,
        "--state",
        "all",
        "--limit",
        REPO_PR_LIMIT,
        "--json",
        "number,headRefName,headRefOid,state,isDraft,isCrossRepository,createdAt",
    ];
    let out = cmd::run("gh", dir, &args).map_err(describe_error)?;
    let prs: Vec<RepoPrJson> = serde_json::from_slice(&out)
        .map_err(|err| format!("unexpected gh pr list output: {err}"))?;
    Ok(prs
        .into_iter()
        .filter(|pr| !pr.is_cross_repository)
        .map(|pr| RepoPr {
            number: pr.number,
            state: match pr.state.as_str() {
                "MERGED" => PrState::Merged,
                "CLOSED" => PrState::Closed,
                _ if pr.is_draft => PrState::Draft,
                _ => PrState::Open,
            },
            head_ref_name: pr.head_ref_name,
            head_ref_oid: pr.head_ref_oid,
            created_at: pr.created_at,
        })
        .collect())
}

/// Per-file changes of PR `number` in `repo`.
pub fn pr_files(dir: &Path, repo: &str, number: u64) -> Result<Vec<FileStat>, String> {
    let number = number.to_string();
    let args = ["pr", "view", &number, "-R", repo, "--json", "files"];
    let out = cmd::run("gh", dir, &args).map_err(describe_error)?;
    parse_pr_files(&String::from_utf8_lossy(&out))
}

pub fn parse_pr_list(json: &str) -> Result<Vec<PullRequest>, String> {
    serde_json::from_str(json).map_err(|err| format!("unexpected gh pr list output: {err}"))
}

pub fn parse_pr_files(json: &str) -> Result<Vec<FileStat>, String> {
    let parsed: PrFiles =
        serde_json::from_str(json).map_err(|err| format!("unexpected gh pr view output: {err}"))?;
    Ok(parsed
        .files
        .into_iter()
        .map(|file| FileStat {
            path: file.path,
            added: Some(file.additions),
            deleted: Some(file.deletions),
            untracked: false,
        })
        .collect())
}

/// Turns a failed `gh` call into a message the user can act on.
fn describe_error(err: CmdError) -> String {
    match err {
        CmdError::Missing => {
            "GitHub CLI (gh) is not installed. Install it with `sudo pacman -S github-cli`, then press r."
                .to_string()
        }
        CmdError::Failed(stderr) if is_auth_error(&stderr) => {
            "gh is not logged in. Run `gh auth login`, then press r.".to_string()
        }
        CmdError::Failed(stderr) => {
            let first_line = stderr.lines().next().unwrap_or("gh failed");
            format!("gh failed: {first_line}")
        }
    }
}

fn is_auth_error(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    [
        "gh auth login",
        "not logged in",
        "authentication",
        "http 401",
        "bad credentials",
    ]
    .iter()
    .any(|needle| stderr.contains(needle))
}
