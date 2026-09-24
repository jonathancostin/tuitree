//! Throwaway git repositories for the e2e tests, plus fake `gh` programs.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Git settings that keep fixture commands independent of the user's own git config.
const ISOLATED_GIT: [(&str, &str); 6] = [
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_AUTHOR_NAME", "e2e"),
    ("GIT_AUTHOR_EMAIL", "e2e@example.com"),
    ("GIT_COMMITTER_NAME", "e2e"),
    ("GIT_COMMITTER_EMAIL", "e2e@example.com"),
];

/// A temporary directory that is deleted when the test ends. It doubles as `$HOME` for tuitree.
pub struct Fixture {
    pub root: PathBuf,
}

impl Fixture {
    pub fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("tuitree-e2e-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create fixture dir");
        Self { root }
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    pub fn config_file(&self) -> PathBuf {
        self.path("config/tuitree/config.toml")
    }

    /// Environment for a tuitree run: this fixture as `$HOME`, its own config dir, isolated git.
    pub fn app_env(&self, path: String) -> Vec<(&'static str, String)> {
        let mut env = vec![
            ("HOME", self.root.display().to_string()),
            ("XDG_CONFIG_HOME", self.path("config").display().to_string()),
            ("PATH", path),
        ];
        env.extend(ISOLATED_GIT.map(|(key, value)| (key, value.to_string())));
        env
    }

    pub fn write_config(&self, projects: &[&Path]) {
        let entries: String = projects
            .iter()
            .map(|p| format!("[[projects]]\npath = \"{}\"\n\n", p.display()))
            .collect();
        let file = self.config_file();
        fs::create_dir_all(file.parent().unwrap()).expect("create config dir");
        fs::write(file, entries).expect("write config");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .envs(ISOLATED_GIT)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(path: PathBuf, content: impl AsRef<[u8]>) {
    fs::write(path, content).expect("write fixture file");
}

fn lines(prefix: &str, count: usize) -> String {
    (1..=count).map(|i| format!("{prefix} {i}\n")).collect()
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

/// Builds this layout inside the fixture:
///
/// - `origin.git`: bare "remote"; `project` reaches it as `https://github.com/e2e/fixture.git`
///   through `url.<path>.insteadOf`, so tuitree sees a GitHub repo but fetches locally.
/// - `project` (main worktree, `main`): clean; origin gains 2 commits after the clone, so it is
///   1 fetch away from being 2 behind.
/// - `wt-feature-a` (`feature-a`): 3 commits (base.txt +4 -2, new.rs +7, logo.bin binary),
///   unstaged app.rs +2, untracked notes.md (3 lines, no final newline) and blob.dat (binary),
///   and an ignored debug.log.
/// - `wt-feature-b` (`feature-b`): 1 commit (base.txt -3) and staged staged.txt +2.
/// - `wt-detached`: detached HEAD at the initial commit.
/// - `wt-gone`: worktree whose directory was deleted.
///
/// Returns the path of `project`.
pub fn standard_repo(fx: &Fixture) -> PathBuf {
    let origin = fx.path("origin.git");
    git(
        &fx.root,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );

    let upstream = fx.path("upstream");
    git(&fx.root, &["clone", "-q", "origin.git", "upstream"]);
    write(upstream.join("base.txt"), lines("line", 10));
    write(upstream.join("app.rs"), lines("// app", 5));
    write(upstream.join("logo.bin"), [0x89, b'P', 0, 1, 2, 3, 0, 255]);
    commit_all(&upstream, "initial");
    git(&upstream, &["push", "-q", "origin", "main"]);

    let project = fx.path("project");
    git(&fx.root, &["clone", "-q", "origin.git", "project"]);
    let github_url = "https://github.com/e2e/fixture.git";
    git(&project, &["remote", "set-url", "origin", github_url]);
    let rewrite = format!("url.{}.insteadOf", origin.display());
    git(&project, &["config", &rewrite, github_url]);
    let exclude = project.join(".git/info/exclude");
    let mut excluded = fs::read_to_string(&exclude).unwrap_or_default();
    excluded.push_str("*.log\n");
    write(exclude, excluded);

    git(
        &project,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature-a",
            "../wt-feature-a",
        ],
    );
    git(
        &project,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature-b",
            "../wt-feature-b",
        ],
    );
    git(
        &project,
        &["worktree", "add", "-q", "--detach", "../wt-detached"],
    );
    git(
        &project,
        &["worktree", "add", "-q", "-b", "gone", "../wt-gone"],
    );
    fs::remove_dir_all(fx.path("wt-gone")).expect("delete wt-gone");

    let a = fx.path("wt-feature-a");
    let base: String = lines("line", 10)
        .lines()
        .enumerate()
        .flat_map(|(i, line)| match i {
            1 => vec!["alpha 1", "alpha 2", "alpha 3", "alpha 4"],
            2 => vec![],
            _ => vec![line],
        })
        .map(|line| format!("{line}\n"))
        .collect();
    write(a.join("base.txt"), base);
    commit_all(&a, "rework base");
    write(a.join("new.rs"), lines("// new", 7));
    commit_all(&a, "add new.rs");
    write(a.join("logo.bin"), [0x89, b'P', 0, 9, 9, 9, 0, 255, 7]);
    commit_all(&a, "new logo");
    let mut app = fs::read_to_string(a.join("app.rs")).unwrap();
    app.push_str("// wip 1\n// wip 2\n");
    write(a.join("app.rs"), app);
    write(a.join("notes.md"), "one\ntwo\nthree");
    write(a.join("blob.dat"), [0u8, 1, 2, 3, 0, 0]);
    write(a.join("debug.log"), lines("log", 4));

    let b = fx.path("wt-feature-b");
    write(b.join("base.txt"), lines("line", 7));
    commit_all(&b, "trim base");
    write(b.join("staged.txt"), lines("staged", 2));
    git(&b, &["add", "staged.txt"]);

    write(upstream.join("upstream.txt"), lines("upstream", 5));
    commit_all(&upstream, "upstream work");
    write(upstream.join("upstream.txt"), lines("upstream", 6));
    commit_all(&upstream, "more upstream work");
    git(&upstream, &["push", "-q", "origin", "main"]);

    project
}

/// A local "origin" whose `main` is one commit ("upstream": edits shared.txt line 1, adds
/// up.txt) past the initial commit (base.txt, shared.txt), and a clone `project` that sees it
/// as `https://github.com/e2e/fixture.git`. Returns the path of `project`.
fn github_clone(fx: &Fixture) -> PathBuf {
    let origin = fx.path("origin.git");
    git(
        &fx.root,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );

    let upstream = fx.path("upstream");
    git(&fx.root, &["clone", "-q", "origin.git", "upstream"]);
    write(upstream.join("base.txt"), lines("line", 10));
    write(upstream.join("shared.txt"), lines("shared", 5));
    commit_all(&upstream, "initial");
    let shared = lines("shared", 5).replacen("shared 1", "upstream edit", 1);
    write(upstream.join("shared.txt"), shared);
    write(upstream.join("up.txt"), lines("up", 3));
    commit_all(&upstream, "upstream");
    git(&upstream, &["push", "-q", "origin", "main"]);

    let project = fx.path("project");
    git(&fx.root, &["clone", "-q", "origin.git", "project"]);
    let github_url = "https://github.com/e2e/fixture.git";
    git(&project, &["remote", "set-url", "origin", github_url]);
    let rewrite = format!("url.{}.insteadOf", origin.display());
    git(&project, &["config", &rewrite, github_url]);
    project
}

/// The initial commit, one behind `origin/main` in [`github_clone`].
const INITIAL: &str = "HEAD~1";

/// Commits in `dir` that make merging `origin/main` conflict in shared.txt.
fn commit_conflicting_edit(dir: &Path) {
    let shared = lines("shared", 5).replacen("shared 1", "my own edit", 1);
    write(dir.join("shared.txt"), shared);
    write(dir.join("notes.txt"), lines("note", 2));
    commit_all(dir, "edit shared");
}

/// Commits in `dir` that merge cleanly with `origin/main`.
fn commit_clean_edit(dir: &Path) {
    let base = lines("line", 10).replacen("line 5", "line five", 1);
    write(dir.join("base.txt"), base);
    commit_all(dir, "edit base");
}

/// Builds a repo for the sync and cleanup tests. `origin/main` is one commit ("upstream")
/// past the initial commit; every linked worktree starts from the initial commit:
///
/// - `project`: main worktree, on branch `trunk` at `origin/main` (up to date). Keeping it
///   off `main` lets `wt-main` check out the default branch.
/// - `wt-main` (`main`): the default branch, reset to the initial commit (ready, ff).
/// - `wt-ff-only` (`ff-only`): no commits of its own (ready, ff).
/// - `wt-ready-merge` (`ready-merge`): 1 commit editing base.txt (ready: clean merge).
/// - `wt-conflict` (`conflict`): 1 commit editing shared.txt line 1 like upstream did, plus
///   notes.txt (conflicts in shared.txt; also unmerged and unpushed).
/// - `wt-dirty` (`dirty`): unstaged edit to base.txt (dirty).
/// - `wt-done` (`done`): no commits, also pushed to origin (clean delete; the remote branch
///   must survive).
/// - `wt-detached`: detached HEAD at the initial commit.
///
/// Returns the path of `project`.
pub fn sync_repo(fx: &Fixture) -> PathBuf {
    let project = github_clone(fx);
    git(&project, &["checkout", "-q", "-b", "trunk"]);
    git(&project, &["branch", "-q", "-f", "main", INITIAL]);

    let initial = INITIAL;
    git(&project, &["worktree", "add", "-q", "../wt-main", "main"]);
    for branch in ["ff-only", "ready-merge", "conflict", "dirty", "done"] {
        let dir = format!("../wt-{branch}");
        git(
            &project,
            &["worktree", "add", "-q", "-b", branch, &dir, initial],
        );
    }
    git(
        &project,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            "../wt-detached",
            initial,
        ],
    );

    commit_clean_edit(&fx.path("wt-ready-merge"));
    commit_conflicting_edit(&fx.path("wt-conflict"));

    let dirty = fx.path("wt-dirty");
    write(dirty.join("base.txt"), lines("line", 11));

    git(&fx.path("wt-done"), &["push", "-q", "origin", "done"]);
    project
}

/// Builds a repo for the queue and forced-delete tests. Every linked worktree starts from the
/// initial commit, one behind `origin/main`:
///
/// - `dirty`: unstaged edit. `untracked`: 300 untracked files, an untracked nested repo and an
///   ignored `node_modules/`. `withsub`: a committed submodule. `locked`: `git worktree lock`.
///   `missing`: directory deleted. `lockgone`: locked, then its directory deleted.
/// - `b1`, `b2`, `b3`: clean, for batch delete.
/// - `ready-ff`, `ready-merge`, `conflicted`, `dirtysync`: one of each sync state.
/// - `twin-a` and `twin-b` both have branch `twin` checked out, so deleting `twin-a` removes
///   the worktree but `git branch -D twin` fails.
/// - `slow`, `slow2`: a `reference-transaction` hook sleeps 10 s whenever a `refs/heads/slow*`
///   ref changes, so deleting them takes a while. `after`: clean, deleted after `slow`.
///
/// Returns the path of `project`.
pub fn queue_repo(fx: &Fixture) -> PathBuf {
    let project = github_clone(fx);
    let exclude = project.join(".git/info/exclude");
    let mut excluded = fs::read_to_string(&exclude).unwrap_or_default();
    excluded.push_str("node_modules/\n");
    write(exclude, excluded);

    let branches = [
        "dirty",
        "untracked",
        "withsub",
        "locked",
        "missing",
        "lockgone",
        "b1",
        "b2",
        "b3",
        "ready-ff",
        "ready-merge",
        "conflicted",
        "dirtysync",
        "slow",
        "slow2",
        "after",
    ];
    for branch in branches {
        let dir = format!("../wt-{branch}");
        git(
            &project,
            &["worktree", "add", "-q", "-b", branch, &dir, INITIAL],
        );
    }
    git(
        &project,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "twin",
            "../wt-twin-a",
            INITIAL,
        ],
    );
    git(
        &project,
        &["worktree", "add", "-q", "--force", "../wt-twin-b", "twin"],
    );

    write(fx.path("wt-dirty/base.txt"), lines("line", 11));
    write(fx.path("wt-dirtysync/base.txt"), lines("line", 12));

    let untracked = fx.path("wt-untracked");
    fs::create_dir_all(untracked.join("files")).unwrap();
    for i in 0..300 {
        write(untracked.join(format!("files/f{i}.txt")), lines("x", 3));
    }
    fs::create_dir_all(untracked.join("node_modules/pkg/deep")).unwrap();
    write(untracked.join("node_modules/pkg/index.js"), lines("js", 20));
    write(untracked.join("node_modules/pkg/deep/x.js"), lines("js", 5));
    fs::create_dir_all(untracked.join("nested")).unwrap();
    git(&untracked.join("nested"), &["init", "-q"]);
    write(untracked.join("nested/readme.txt"), lines("nested", 2));

    let sub_source = fx.path("sub-source");
    git(&fx.root, &["init", "-q", "-b", "main", "sub-source"]);
    write(sub_source.join("lib.txt"), lines("lib", 3));
    commit_all(&sub_source, "lib");
    let withsub = fx.path("wt-withsub");
    git(
        &withsub,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub_source.to_str().unwrap(),
            "sub",
        ],
    );
    commit_all(&withsub, "add submodule");

    git(
        &project,
        &[
            "worktree",
            "lock",
            "--reason",
            "on a USB disk",
            "../wt-locked",
        ],
    );
    fs::remove_dir_all(fx.path("wt-missing")).unwrap();
    git(&project, &["worktree", "lock", "../wt-lockgone"]);
    fs::remove_dir_all(fx.path("wt-lockgone")).unwrap();

    commit_clean_edit(&fx.path("wt-ready-merge"));
    commit_conflicting_edit(&fx.path("wt-conflicted"));

    let hooks = fx.path("hooks");
    fs::create_dir_all(&hooks).unwrap();
    executable(
        hooks.join("reference-transaction"),
        r#"#!/bin/sh
[ "$1" = prepared ] || exit 0
slow=0
while read -r old new ref; do
  case "$ref" in refs/heads/slow*) slow=1 ;; esac
done
[ "$slow" = 1 ] && sleep 10
exit 0
"#
        .to_string(),
    );
    git(
        &project,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    project
}

/// A clone whose `origin` is a plain local path (not GitHub).
pub fn plain_clone(fx: &Fixture) -> PathBuf {
    git(&fx.root, &["clone", "-q", "origin.git", "plain"]);
    fx.path("plain")
}

fn executable(path: PathBuf, script: String) {
    write(path.clone(), script);
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod fake gh");
}

/// A `gh` that serves canned PR JSON and logs its arguments to `gh-calls.log`. Merged PRs come
/// from `gh-data/merged.json` (empty unless [`set_merged_prs`] wrote it).
/// Returns the directory to put first on `PATH`.
pub fn fake_gh_with_prs(fx: &Fixture) -> PathBuf {
    let data = fx.path("gh-data");
    fs::create_dir_all(&data).unwrap();
    write(
        data.join("pr-list.json"),
        r#"[
  {"number":42,"title":"Add worktree dashboard","headRefName":"feature-a","author":{"login":"octocat","name":"Octo Cat","is_bot":false},"isDraft":false,"changedFiles":3,"additions":25,"deletions":4},
  {"number":7,"title":"Try new colors","headRefName":"feature-b","author":{"login":"hubot","is_bot":true},"isDraft":true,"changedFiles":1,"additions":2,"deletions":3}
]"#,
    );
    write(
        data.join("pr-42.json"),
        r#"{"files":[
  {"path":"src/ui.rs","additions":20,"deletions":4,"changeType":"MODIFIED"},
  {"path":"README.md","additions":5,"deletions":0,"changeType":"MODIFIED"},
  {"path":"assets/logo.png","additions":0,"deletions":0,"changeType":"ADDED"}
]}"#,
    );
    if !data.join("merged.json").exists() {
        write(data.join("merged.json"), "[]");
    }
    let bin = fx.path("bin-gh-prs");
    fs::create_dir_all(&bin).unwrap();
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
case "$*" in
  "pr list"*"--state merged"*) exec cat '{data}/merged.json' ;;
esac
case "$1 $2" in
  "pr list") exec cat '{data}/pr-list.json' ;;
  "pr view") exec cat "{data}/pr-$3.json" ;;
esac
echo "fake gh: unsupported arguments: $*" >&2
exit 1
"#,
        log = fx.path("gh-calls.log").display(),
        data = data.display()
    );
    executable(bin.join("gh"), script);
    bin
}

/// Makes the fake gh report these merged PRs: `(number, head branch, head commit)`.
pub fn set_merged_prs(fx: &Fixture, prs: &[(u64, &str, &str)]) {
    let data = fx.path("gh-data");
    fs::create_dir_all(&data).unwrap();
    let entries: Vec<String> = prs
        .iter()
        .map(|(number, branch, oid)| {
            format!(
                r#"{{"number":{number},"headRefName":"{branch}","headRefOid":"{oid}","mergedAt":"2026-09-24T05:00:00Z"}}"#
            )
        })
        .collect();
    write(data.join("merged.json"), format!("[{}]", entries.join(",")));
}

/// Commit ids a test needs to fake GitHub's merged PR list for [`merged_repo`].
pub struct MergedFixture {
    pub project: PathBuf,
    /// Head of `pr-merged` when its PR was merged.
    pub pr_merged_head: String,
    /// An older commit of `reused`; the branch has new work on top.
    pub reused_old_head: String,
}

/// Builds a repo whose branches were squash merged on "GitHub" (every linked worktree starts
/// from the initial commit; `origin/main` gets 4 commits):
///
/// - `squashed`: 2 commits, squash merged and its remote branch deleted. Merging it into
///   `origin/main` changes nothing, so the content check finds it without gh.
/// - `pr-merged`: 1 commit, squash merged, then `origin/main` edited the same line again, so the
///   content check conflicts; only the merged PR (#42 in the fake gh) identifies it.
/// - `reused`: a merged PR (#7) had its older head; the branch has a new commit since, so it is
///   not merged.
/// - `open-work`: 1 unmerged commit.
pub fn merged_repo(fx: &Fixture) -> MergedFixture {
    let project = github_clone(fx);
    for branch in ["squashed", "pr-merged", "reused", "open-work"] {
        let dir = format!("../wt-{branch}");
        git(
            &project,
            &["worktree", "add", "-q", "-b", branch, &dir, INITIAL],
        );
    }
    let squashed = fx.path("wt-squashed");
    write(squashed.join("feature.txt"), lines("feature", 4));
    commit_all(&squashed, "add feature");
    let base = lines("line", 10).replacen("line 3\n", "line three\n", 1);
    write(squashed.join("base.txt"), base);
    commit_all(&squashed, "tweak base");
    git(&squashed, &["push", "-q", "origin", "squashed"]);

    let pr_merged = fx.path("wt-pr-merged");
    let base = lines("line", 10).replacen("line 7\n", "line seven\n", 1);
    write(pr_merged.join("base.txt"), base);
    commit_all(&pr_merged, "edit line 7");
    git(&pr_merged, &["push", "-q", "origin", "pr-merged"]);
    let pr_merged_head = git(&pr_merged, &["rev-parse", "HEAD"]).trim().to_string();

    let reused = fx.path("wt-reused");
    write(reused.join("reuse.txt"), lines("old", 2));
    commit_all(&reused, "old work");
    let reused_old_head = git(&reused, &["rev-parse", "HEAD"]).trim().to_string();
    write(reused.join("reuse.txt"), lines("new", 3));
    commit_all(&reused, "new work");

    let open = fx.path("wt-open-work");
    write(open.join("work.txt"), lines("work", 2));
    commit_all(&open, "open work");

    // "GitHub" squash merges both PRs and deletes their branches; main moves on.
    let upstream = fx.path("upstream");
    git(&upstream, &["fetch", "-q", "origin"]);
    git(&upstream, &["merge", "-q", "--squash", "origin/squashed"]);
    git(&upstream, &["commit", "-q", "-m", "Add feature (#11)"]);
    git(&upstream, &["merge", "-q", "--squash", "origin/pr-merged"]);
    git(&upstream, &["commit", "-q", "-m", "Edit line 7 (#42)"]);
    let base = fs::read_to_string(upstream.join("base.txt"))
        .unwrap()
        .replacen("line seven\n", "line seven, edited later\n", 1);
    write(upstream.join("base.txt"), base);
    commit_all(&upstream, "Edit line 7 again");
    git(&upstream, &["push", "-q", "origin", "main"]);
    git(
        &upstream,
        &["push", "-q", "origin", "--delete", "squashed", "pr-merged"],
    );
    MergedFixture {
        project,
        pr_merged_head,
        reused_old_head,
    }
}

/// A `gh` that fails like a real one without a login.
pub fn fake_gh_logged_out(fx: &Fixture) -> PathBuf {
    let bin = fx.path("bin-gh-logged-out");
    fs::create_dir_all(&bin).unwrap();
    let script = r#"#!/bin/sh
echo "To get started with GitHub CLI, please run:  gh auth login" >&2
echo "Alternatively, populate the GH_TOKEN environment variable with a GitHub API authentication token." >&2
exit 4
"#;
    executable(bin.join("gh"), script.to_string());
    bin
}

/// A directory holding only a `git` symlink: a `PATH` on which `gh` does not exist.
pub fn git_only_bin(fx: &Fixture) -> PathBuf {
    let git_path = crate::harness::which("git").expect("git on PATH");
    let bin = fx.path("bin-git-only");
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(git_path, bin.join("git")).expect("symlink git");
    bin
}

/// `PATH` with `dir` in front of the current one.
pub fn path_with(dir: &Path) -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    format!("{}:{current}", dir.display())
}
