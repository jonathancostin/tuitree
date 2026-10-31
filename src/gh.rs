//! Pull requests via the GitHub CLI (`gh`), plus pure parsers for its JSON.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

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

/// What opening a PR shows: its per-file changes and its conversation, oldest first.
#[derive(Debug, Clone)]
pub struct PrDetail {
    pub files: Vec<FileStat>,
    pub comments: Vec<PrComment>,
}

/// A conversation comment or a review summary (inline code comments are not included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrComment {
    pub author: String,
    pub kind: CommentKind,
    pub at: Option<SystemTime>,
    /// Empty for reviews without a summary (e.g. a bare approval).
    pub body: String,
    /// Hidden (minimized) on GitHub; the body is not shown.
    pub hidden: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentKind {
    Comment,
    Approved,
    ChangesRequested,
    Reviewed,
    Dismissed,
}

#[derive(Deserialize)]
struct PrDetailJson {
    files: Vec<PrFile>,
    #[serde(default)]
    comments: Vec<CommentJson>,
    #[serde(default)]
    reviews: Vec<ReviewJson>,
}

#[derive(Deserialize)]
struct PrFile {
    path: String,
    additions: u64,
    deletions: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentJson {
    #[serde(default)]
    author: Option<Author>,
    #[serde(default)]
    body: String,
    created_at: String,
    #[serde(default)]
    is_minimized: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewJson {
    #[serde(default)]
    author: Option<Author>,
    #[serde(default)]
    body: String,
    state: String,
    #[serde(default)]
    submitted_at: Option<String>,
}

/// Open PRs of `repo` (`owner/name`).
pub fn list_prs(dir: &Path, repo: &str) -> Result<Vec<PullRequest>, String> {
    let args = [
        "pr", "list", "-R", repo, "--state", "open", "--limit", "100", "--json", PR_FIELDS,
    ];
    let out = cmd::run("gh", dir, &args).map_err(describe_error)?;
    parse_pr_list(&String::from_utf8_lossy(&out))
}

/// Branches looked up per `gh api graphql` request (one aliased field each).
const BRANCHES_PER_QUERY: usize = 100;
/// PRs fetched per branch, newest first; a branch name is rarely reused more often.
const PRS_PER_BRANCH: u32 = 20;

#[derive(Deserialize)]
struct GraphQlResponse {
    data: GraphQlData,
}

#[derive(Deserialize)]
struct GraphQlData {
    /// `null` when the repository is missing or not visible to the gh login.
    repository: Option<HashMap<String, PrConnection>>,
}

#[derive(Deserialize)]
struct PrConnection {
    nodes: Vec<RepoPrJson>,
}

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

/// The PRs (any state, up to `PRS_PER_BRANCH` newest per branch) whose head is one of
/// `branches`, without PRs from forks. One GraphQL request per refresh, asking only about these
/// branches, links every worktree's branch to its PR and recognizes squash or rebase merged
/// branches.
pub fn list_repo_prs(dir: &Path, repo: &str, branches: &[String]) -> Result<Vec<RepoPr>, String> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| format!("not an owner/name GitHub repository: {repo}"))?;
    let mut prs = Vec::new();
    for chunk in branches.chunks(BRANCHES_PER_QUERY) {
        let query = repo_prs_query(chunk);
        let args = [
            "api",
            "graphql",
            "-f",
            &query,
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("name={name}"),
        ];
        let out = cmd::run("gh", dir, &args).map_err(describe_error)?;
        prs.extend(parse_repo_prs(&out)?);
    }
    Ok(prs)
}

/// `query=…` for `gh api graphql`: one aliased `pullRequests(headRefName: …)` per branch.
fn repo_prs_query(branches: &[String]) -> String {
    let fields = "number headRefName headRefOid state isDraft isCrossRepository createdAt";
    let lookups: Vec<String> = branches
        .iter()
        .enumerate()
        .map(|(i, branch)| {
            // A JSON string literal is a valid GraphQL string literal.
            let branch = serde_json::Value::from(branch.as_str());
            format!(
                "b{i}: pullRequests(headRefName: {branch}, first: {PRS_PER_BRANCH}, \
                 orderBy: {{field: CREATED_AT, direction: DESC}}) {{ nodes {{ {fields} }} }}"
            )
        })
        .collect();
    format!(
        "query=query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ {} }} }}",
        lookups.join(" ")
    )
}

fn parse_repo_prs(json: &[u8]) -> Result<Vec<RepoPr>, String> {
    let response: GraphQlResponse = serde_json::from_slice(json)
        .map_err(|err| format!("unexpected gh api graphql output: {err}"))?;
    let lookups = response
        .data
        .repository
        .ok_or("GitHub did not find the repository (or gh cannot see it).")?;
    Ok(lookups
        .into_values()
        .flat_map(|connection| connection.nodes)
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

/// Per-file changes and conversation of PR `number` in `repo`, in one `gh` call.
pub fn pr_detail(dir: &Path, repo: &str, number: u64) -> Result<PrDetail, String> {
    let number = number.to_string();
    let args = [
        "pr",
        "view",
        &number,
        "-R",
        repo,
        "--json",
        "files,comments,reviews",
    ];
    let out = cmd::run("gh", dir, &args).map_err(describe_error)?;
    parse_pr_detail(&String::from_utf8_lossy(&out))
}

pub fn parse_pr_list(json: &str) -> Result<Vec<PullRequest>, String> {
    serde_json::from_str(json).map_err(|err| format!("unexpected gh pr list output: {err}"))
}

pub fn parse_pr_detail(json: &str) -> Result<PrDetail, String> {
    let parsed: PrDetailJson =
        serde_json::from_str(json).map_err(|err| format!("unexpected gh pr view output: {err}"))?;
    let files = parsed
        .files
        .into_iter()
        .map(|file| FileStat {
            path: file.path,
            added: Some(file.additions),
            deleted: Some(file.deletions),
            untracked: false,
        })
        .collect();
    let login = |author: Option<Author>| author.map_or_else(|| "ghost".to_string(), |a| a.login);
    // (sort key, comment): GitHub's ISO-8601 UTC timestamps sort chronologically as text.
    let mut timeline: Vec<(String, PrComment)> = parsed
        .comments
        .into_iter()
        .map(|c| {
            let comment = PrComment {
                author: login(c.author),
                kind: CommentKind::Comment,
                at: parse_timestamp(&c.created_at),
                body: clean_body(&c.body),
                hidden: c.is_minimized,
            };
            (c.created_at, comment)
        })
        .collect();
    for review in parsed.reviews {
        let kind = match review.state.as_str() {
            "APPROVED" => CommentKind::Approved,
            "CHANGES_REQUESTED" => CommentKind::ChangesRequested,
            "DISMISSED" => CommentKind::Dismissed,
            "COMMENTED" => CommentKind::Reviewed,
            _ => continue, // PENDING: a draft review only its author can see
        };
        let Some(submitted_at) = review.submitted_at else {
            continue;
        };
        let body = clean_body(&review.body);
        // A comment-only review without a summary just wraps inline code comments.
        if kind == CommentKind::Reviewed && body.is_empty() {
            continue;
        }
        let comment = PrComment {
            author: login(review.author),
            kind,
            at: parse_timestamp(&submitted_at),
            body,
            hidden: false,
        };
        timeline.push((submitted_at, comment));
    }
    timeline.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(PrDetail {
        files,
        comments: timeline.into_iter().map(|(_, comment)| comment).collect(),
    })
}

/// Drops HTML comments (bots hide markers in them) and `\r`, and trims the text.
fn clean_body(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find("-->") {
            Some(end) => &rest[start + end + 3..],
            None => "",
        };
    }
    out.push_str(rest);
    out.retain(|c| c != '\r');
    out.trim().to_string()
}

/// Parses GitHub's `YYYY-MM-DDTHH:MM:SSZ` (UTC) timestamps.
fn parse_timestamp(text: &str) -> Option<SystemTime> {
    let num = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).ok()?))
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
