//! End-to-end tests: the real `tuitree` binary, driven through tmux, against real git repos.
//!
//! Run with `cargo test --test e2e`. Screens and a pass/fail summary of every test land in
//! `tests/artifacts/<run>/`.
//!
//! The live smoke test talks to GitHub for real and is ignored by default:
//! `TUITREE_LIVE_WORKTREES=~/some-repo TUITREE_LIVE_PRS=~/repo-with-open-prs \
//!  cargo test --test e2e -- --ignored`
//! It also mirrors its captures into `smoke/`.

mod actions;
mod fixture;
mod harness;
mod merged;
mod pr_column;
mod queue;

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use fixture::{
    Fixture, fake_gh_logged_out, fake_gh_with_prs, git_only_bin, path_with, plain_clone,
    standard_repo,
};
use harness::{
    Artifacts, Tmux, check_row, file_rows, has_ahead_behind, has_run, line_with_token, pr_rows,
    row_below,
};

const SIZE: (u16, u16) = (180, 45);
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
const SHORT: Duration = Duration::from_secs(5);

/// Loaded after `git fetch`: stats are final and nothing is spinning.
fn fetched(screen: &str) -> bool {
    screen.contains("fetched") && !screen.contains("fetching") && !screen.contains("updating")
}

#[test]
fn worktrees_prs_and_project_management() {
    let fx = Fixture::new("main");
    let project = standard_repo(&fx);
    fs::create_dir_all(fx.path("not-a-repo")).unwrap();
    let gh_bin = fake_gh_with_prs(&fx);
    let mut art = Artifacts::new("worktrees_prs_and_project_management");
    let app = Tmux::launch("main", SIZE, &fx.root, &fx.app_env(path_with(&gh_bin)));

    // Empty config.
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("No projects yet"));
    art.screen("empty", &screen);
    art.check("starts with no projects", ok, "");

    // Adding a directory that is not a git repo is rejected inline.
    app.keys(&["a"]);
    app.type_text("~/not-a-repo");
    app.keys(&["Enter"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("Not a git repository"));
    art.screen("add-rejected", &screen);
    art.check("non-repo path is rejected", ok, "");

    // `~` is expanded and the repo is added and persisted.
    app.keys(&["C-u"]);
    app.type_text("~/project");
    art.screen("add-dialog", &app.capture());
    app.keys(&["Enter"]);
    let (ok, screen) = app.wait_for(SHORT, |s| {
        s.contains("Added ~/project (GitHub: e2e/fixture)")
    });
    art.screen("added", &screen);
    art.check("add confirms path and detected GitHub repo", ok, "");
    let config = fs::read_to_string(fx.config_file()).unwrap_or_default();
    art.check(
        "config.toml lists the project",
        config.contains(&format!("path = \"{}\"", project.display())),
        format!("{}: {config:?}", fx.config_file().display()),
    );

    // Worktrees with stats against the fetched origin/main.
    let (ok, screen) = app.wait_for(LOAD_TIMEOUT, |s| {
        fetched(s) && line_with_token(s, "feature-a").is_some_and(|l| l.contains("↓2"))
    });
    art.screen("worktrees", &screen);
    art.check("worktrees load and origin is fetched", ok, "");
    art.check(
        "header shows GitHub repo and base",
        screen.contains("GitHub e2e/fixture") && screen.contains("base origin/main"),
        "",
    );
    art.check(
        "worktrees tab counts 5 worktrees",
        screen.contains("Worktrees (5)"),
        "",
    );
    check_row(
        &mut art,
        &screen,
        "main",
        &[
            "main",
            "↑0",
            "↓2",
            "ready",
            "(ff)",
            "-",
            "0",
            "+0",
            "-0",
            "~/project",
        ],
    );
    check_row(
        &mut art,
        &screen,
        "feature-a",
        &[
            "feature-a",
            "↑3",
            "↓2",
            "dirty",
            "-",
            "6",
            "+16",
            "-2",
            "~/wt-feature-a",
        ],
    );
    check_row(
        &mut art,
        &screen,
        "feature-b",
        &[
            "feature-b",
            "↑1",
            "↓2",
            "dirty",
            "-",
            "2",
            "+2",
            "-3",
            "~/wt-feature-b",
        ],
    );
    let detached = screen.lines().find(|l| l.contains("(detached"));
    art.check(
        "detached worktree row",
        detached.is_some_and(|l| {
            has_run(
                l,
                &[
                    "↑0",
                    "↓2",
                    "ready",
                    "(ff)",
                    "-",
                    "0",
                    "+0",
                    "-0",
                    "~/wt-detached",
                ],
            )
        }),
        format!("found: {}", detached.map_or("<none>", str::trim)),
    );
    check_row(
        &mut art,
        &screen,
        "gone",
        &["~/wt-gone", "⚠", "worktree", "directory", "is", "missing"],
    );

    // Per-file changes of feature-a, including uncommitted and untracked files.
    let index = row_below(&screen, "Branch", |line| {
        harness::tokens(line).contains(&"feature-a")
    });
    art.check(
        "feature-a row located",
        index.is_some(),
        format!("{index:?}"),
    );
    app.keys(&["l"]);
    app.keys(&vec!["j"; index.unwrap_or(0)]);
    app.keys(&["Enter"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("vs merge-base"));
    art.screen("feature-a-files", &screen);
    art.check("feature-a detail opens", ok, "");
    art.check(
        "detail title has totals",
        screen.contains("feature-a: 6 files +16 -2 vs merge-base with origin/main (incl. uncommitted and untracked)"),
        "",
    );
    check_row(&mut art, &screen, "app.rs", &["+2", "-0", "app.rs"]);
    check_row(&mut art, &screen, "base.txt", &["+4", "-2", "base.txt"]);
    check_row(&mut art, &screen, "logo.bin", &["bin", "logo.bin"]);
    check_row(&mut art, &screen, "new.rs", &["+7", "-0", "new.rs"]);
    check_row(
        &mut art,
        &screen,
        "notes.md",
        &["+3", "-0", "notes.md", "(untracked)"],
    );
    check_row(
        &mut art,
        &screen,
        "blob.dat",
        &["bin", "blob.dat", "(untracked)"],
    );
    art.check(
        "ignored debug.log is not counted",
        !screen.contains("debug.log"),
        "",
    );
    art.check(
        "upstream-only upstream.txt is not counted",
        !screen.contains("upstream.txt"),
        "",
    );

    app.keys(&["j"]);
    let screen = app.capture();
    art.screen("feature-a-files-moved", &screen);
    check_row(
        &mut art,
        &screen,
        "base.txt",
        &["▶", "+4", "-2", "base.txt"],
    );

    // Open PRs from gh.
    app.keys(&["Escape", "Tab"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("#42"));
    art.screen("prs", &screen);
    art.check("PR list renders", ok, "");
    art.check(
        "PR tab counts 2 PRs",
        screen.contains("Pull requests (2)"),
        "",
    );
    check_row(
        &mut art,
        &screen,
        "#42",
        &[
            "#42",
            "Add",
            "worktree",
            "dashboard",
            "feature-a",
            "octocat",
            "3",
            "+25",
            "-4",
        ],
    );
    check_row(
        &mut art,
        &screen,
        "#7",
        &[
            "#7",
            "Try",
            "new",
            "colors",
            "feature-b",
            "hubot",
            "draft",
            "1",
            "+2",
            "-3",
        ],
    );

    app.keys(&["Enter"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("src/ui.rs"));
    art.screen("pr-42-files", &screen);
    art.check("PR files render", ok, "");
    art.check(
        "PR detail title",
        screen.contains("#42 Add worktree dashboard: 3 files +25 -4"),
        "",
    );
    check_row(&mut art, &screen, "src/ui.rs", &["+20", "-4", "src/ui.rs"]);
    check_row(&mut art, &screen, "README.md", &["+5", "-0", "README.md"]);
    check_row(
        &mut art,
        &screen,
        "assets/logo.png",
        &["+0", "-0", "assets/logo.png"],
    );
    let calls = fs::read_to_string(fx.path("gh-calls.log")).unwrap_or_default();
    art.check(
        "gh queried the detected repo",
        calls
            .lines()
            .any(|l| l.starts_with("pr list -R e2e/fixture --state open"))
            && calls
                .lines()
                .any(|l| l == "pr view 42 -R e2e/fixture --json files"),
        format!("{calls:?}"),
    );

    // Help.
    app.keys(&["?"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("press any key to close"));
    art.screen("help", &screen);
    art.check("help opens", ok && screen.contains("merge-base"), "");
    app.keys(&["x"]);
    art.check(
        "help closes",
        !app.capture().contains("press any key to close"),
        "",
    );

    // Remove with confirmation.
    app.keys(&["h", "h", "d"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("Remove ~/project from tuitree?"));
    art.screen("confirm-remove", &screen);
    art.check("remove asks first", ok, "");
    app.keys(&["y"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("No projects yet"));
    art.screen("removed", &screen);
    art.check("project removed", ok, "");
    let config = fs::read_to_string(fx.config_file()).unwrap_or_default();
    art.check(
        "config.toml no longer lists it",
        !config.contains("project\""),
        format!("{config:?}"),
    );

    app.keys(&["q"]);
    art.check("q quits", app.wait_exit(SHORT), "");
    art.finish();
}

#[test]
fn pull_requests_explain_missing_gh() {
    let fx = Fixture::new("gh");
    let project = standard_repo(&fx);
    let plain = plain_clone(&fx);
    fx.write_config(&[&project, &plain]);
    let mut art = Artifacts::new("pull_requests_explain_missing_gh");

    // gh installed but logged out.
    let logged_out = fake_gh_logged_out(&fx);
    let app = Tmux::launch(
        "gh-logged-out",
        SIZE,
        &fx.root,
        &fx.app_env(path_with(&logged_out)),
    );
    let (ok, _) = app.wait_for(LOAD_TIMEOUT, fetched);
    art.check("project from existing config loads", ok, "");
    app.keys(&["Tab"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("gh is not logged in"));
    art.screen("gh-logged-out", &screen);
    art.check(
        "logged-out gh explains how to log in",
        ok && screen.contains("Run `gh auth login`, then press r."),
        "",
    );
    app.keys(&["q"]);
    art.check("q quits", app.wait_exit(SHORT), "");

    // gh not installed at all.
    let app = Tmux::launch(
        "gh-missing",
        SIZE,
        &fx.root,
        &fx.app_env(git_only_bin(&fx).display().to_string()),
    );
    let (ok, _) = app.wait_for(LOAD_TIMEOUT, fetched);
    art.check("project loads with only git on PATH", ok, "");
    app.keys(&["Tab"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("GitHub CLI (gh) is not installed"));
    art.screen("gh-missing", &screen);
    art.check(
        "missing gh explains how to install it",
        ok && screen.contains("sudo pacman -S github-cli"),
        "",
    );

    // A repo whose origin is not on GitHub.
    app.keys(&["j"]);
    let (ok, screen) = app.wait_for(LOAD_TIMEOUT, |s| {
        s.contains("origin is not a GitHub remote")
    });
    art.screen("not-github", &screen);
    art.check("non-GitHub origin is explained", ok, "");
    art.check(
        "header says so too",
        screen.contains("GitHub none (origin is not on GitHub)"),
        "",
    );
    app.keys(&["q"]);
    art.check("q quits", app.wait_exit(SHORT), "");
    art.finish();
}

/// Where gh keeps its login, so a temporary `XDG_CONFIG_HOME` does not log it out.
fn gh_config_dir() -> String {
    std::env::var("GH_CONFIG_DIR").unwrap_or_else(|_| {
        let base = std::env::var("XDG_CONFIG_HOME")
            .unwrap_or_else(|_| format!("{}/.config", std::env::var("HOME").unwrap()));
        format!("{base}/gh")
    })
}

fn live_repo(var: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| {
        panic!("set {var} to a local clone with a GitHub origin (see tests/e2e/main.rs)")
    })
}

#[test]
#[ignore = "needs network, gh auth and local GitHub clones; see module docs"]
fn live_github_smoke() {
    let worktree_repo = live_repo("TUITREE_LIVE_WORKTREES");
    let pr_repo = live_repo("TUITREE_LIVE_PRS");
    let config = Fixture::new("live");
    let mirror = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("smoke");
    let mut art = Artifacts::new("live_github_smoke").mirrored_to(mirror);
    let env = vec![
        (
            "XDG_CONFIG_HOME",
            config.path("config").display().to_string(),
        ),
        ("GH_CONFIG_DIR", gh_config_dir()),
    ];
    let app = Tmux::launch("live", (200, 50), &config.root, &env);
    let live_timeout = Duration::from_secs(120);

    // Worktrees of a real repo, fetched from GitHub.
    // Type paths only into the open Add dialog, never into the main screen.
    let add_dialog = |app: &Tmux| app.wait_for(SHORT, |s| s.contains("Add project")).0;
    app.keys(&["a"]);
    assert!(add_dialog(&app), "the Add project dialog did not open");
    app.type_text(&worktree_repo);
    app.keys(&["Enter"]);
    let screen = app.capture();
    art.screen("worktrees-loading", &screen);
    art.check(
        "loading indicator while fetching",
        screen.contains("fetching origin…") || screen.contains("updating worktrees…"),
        "",
    );
    let (ok, screen) = app.wait_for(live_timeout, fetched);
    art.screen("worktrees", &screen);
    art.check("worktrees load and fetch succeeds", ok, "");
    art.check(
        "header shows the GitHub repo",
        screen.contains("GitHub ") && !screen.contains("GitHub none"),
        "",
    );
    let rows = screen.lines().filter(|l| has_ahead_behind(l)).count();
    art.check(
        "worktree rows show ahead/behind",
        rows > 0,
        format!("{rows} rows"),
    );

    // Every worktree row gets a sync status (computed read-only with git merge-tree).
    let sync_labels = ["up to date", "ready", "conflicts", "dirty", "merged", " ? "];
    let without_sync = screen
        .lines()
        .filter(|l| has_ahead_behind(l) && !sync_labels.iter().any(|label| l.contains(label)))
        .count();
    art.check(
        "Sync column shows a status for every worktree",
        screen.lines().any(|l| harness::tokens(l).contains(&"Sync")) && without_sync == 0,
        format!("{without_sync} rows without a status"),
    );

    // The PR column links branches to their PRs (one gh call per refresh).
    let linked = screen
        .lines()
        .filter(|l| {
            has_ahead_behind(l)
                && ["open", "draft", "merged", "closed"].iter().any(|state| {
                    harness::tokens(l)
                        .windows(2)
                        .any(|w| w[0].starts_with('#') && w[1] == *state)
                })
        })
        .count();
    art.check(
        "PR column shows linked PRs",
        screen.lines().any(|l| harness::tokens(l).contains(&"PR")) && linked > 0,
        format!("{linked} rows with a PR"),
    );

    // Open a worktree with conflicts if there is one, else one with changes, preferably with
    // its own commits. Nothing is ever deleted or synced here.
    let ahead_and_files = |line: &str| {
        let tokens = harness::tokens(line);
        let at = tokens.iter().position(|t| t.starts_with('↑'))?;
        let ahead = tokens[at].trim_start_matches('↑').parse::<u64>().ok()?;
        let added = at + tokens[at..].iter().position(|t| t.starts_with('+'))?;
        let files = tokens.get(added - 1)?.parse::<u64>().ok()?;
        Some((ahead, files))
    };
    let changed = row_below(&screen, "Branch", |line| line.contains("conflicts"))
        .or_else(|| {
            row_below(&screen, "Branch", |line| {
                ahead_and_files(line).is_some_and(|(ahead, files)| ahead > 0 && files > 0)
            })
        })
        .or_else(|| {
            row_below(&screen, "Branch", |line| {
                ahead_and_files(line).is_some_and(|(_, files)| files > 0)
            })
        });
    art.check(
        "a worktree with changes exists",
        changed.is_some(),
        format!("row {changed:?}"),
    );
    app.keys(&["l"]);
    app.keys(&vec!["j"; changed.unwrap_or(0)]);
    app.keys(&["Enter"]);
    let (ok, screen) = app.wait_for(Duration::from_secs(10), |s| s.contains("vs merge-base"));
    art.screen("worktree-files", &screen);
    let files = file_rows(&screen);
    art.check(
        "worktree per-file list renders",
        ok && files > 0,
        format!("{files} file rows"),
    );

    // Open PRs of a second real repo.
    app.keys(&["Escape", "h", "a"]);
    assert!(add_dialog(&app), "the Add project dialog did not open");
    app.type_text(&pr_repo);
    app.keys(&["Enter", "Tab"]);
    let (ok, screen) = app.wait_for(live_timeout, |s| pr_rows(s) > 0);
    art.screen("prs", &screen);
    let prs = pr_rows(&screen);
    art.check("PR list renders", ok, format!("{prs} PR rows"));
    let counted = screen
        .split_once("Pull requests (")
        .and_then(|(_, rest)| rest.split(')').next()?.parse::<usize>().ok());
    art.check(
        "PR tab count covers the visible rows",
        counted.is_some_and(|n| n >= prs && n > 0),
        format!("tab count {counted:?}"),
    );

    app.keys(&["l", "Enter"]);
    let (ok, screen) = app.wait_for(Duration::from_secs(30), |s| file_rows(s) > 0);
    art.screen("pr-files", &screen);
    let files = file_rows(&screen);
    art.check("PR per-file list renders", ok, format!("{files} file rows"));

    app.keys(&["q"]);
    art.check("q quits", app.wait_exit(SHORT), "");
    art.finish();
}
