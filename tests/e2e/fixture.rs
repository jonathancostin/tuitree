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

/// A clone whose `origin` is a plain local path (not GitHub).
pub fn plain_clone(fx: &Fixture) -> PathBuf {
    git(&fx.root, &["clone", "-q", "origin.git", "plain"]);
    fx.path("plain")
}

fn executable(path: PathBuf, script: String) {
    write(path.clone(), script);
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod fake gh");
}

/// A `gh` that serves canned PR JSON and logs its arguments to `gh-calls.log`.
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
    let bin = fx.path("bin-gh-prs");
    fs::create_dir_all(&bin).unwrap();
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
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
