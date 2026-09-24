//! The PR column of the worktree table, with a fake gh.

use std::time::Duration;

use crate::fixture::{
    FakePr, Fixture, fake_gh_with_prs, git_only_bin, path_with, pr_repo, set_open_prs, set_repo_prs,
};
use crate::harness::{
    Artifacts, Tmux, check_row, has_run, locate, select_row, settled, style_before,
};

const SIZE: (u16, u16) = (190, 50);
const LOAD: Duration = Duration::from_secs(30);
const SHORT: Duration = Duration::from_secs(5);
/// The projects pane is 30 columns wide; the main pane starts right of it.
const MAIN_PANE: usize = 30;

fn pr<'a>(number: u64, branch: &'a str, state: &'a str, created: &'a str) -> FakePr<'a> {
    FakePr {
        number,
        branch,
        head: "0000000000000000000000000000000000000000",
        state,
        draft: false,
        fork: false,
        created,
    }
}

fn pr_selected(screen: &str, number: &str) -> bool {
    screen.lines().any(|l| has_run(l, &["▶", number]))
}

#[test]
fn pr_column() {
    let fx = Fixture::new("prcol");
    let project = pr_repo(&fx);
    fx.write_config(&[&project]);
    let gh_bin = fake_gh_with_prs(&fx);
    set_repo_prs(
        &fx,
        &[
            pr(42, "feat-open", "OPEN", "2026-09-10T00:00:00Z"),
            FakePr {
                draft: true,
                ..pr(43, "feat-draft", "OPEN", "2026-09-11T00:00:00Z")
            },
            pr(44, "feat-closed", "CLOSED", "2026-09-12T00:00:00Z"),
            pr(45, "feat-merged", "MERGED", "2026-09-13T00:00:00Z"),
            FakePr {
                fork: true,
                ..pr(50, "feat-fork", "OPEN", "2026-09-14T00:00:00Z")
            },
            pr(20, "feat-multi", "CLOSED", "2026-09-01T00:00:00Z"),
            pr(21, "feat-multi", "OPEN", "2026-09-20T00:00:00Z"),
        ],
    );
    set_open_prs(
        &fx,
        r#"[
  {"number":21,"title":"Second try","headRefName":"feat-multi","author":{"login":"octocat"},"isDraft":false,"changedFiles":1,"additions":1,"deletions":0},
  {"number":42,"title":"Open feature","headRefName":"feat-open","author":{"login":"octocat"},"isDraft":false,"changedFiles":1,"additions":2,"deletions":0},
  {"number":43,"title":"Draft feature","headRefName":"feat-draft","author":{"login":"hubot"},"isDraft":true,"changedFiles":1,"additions":3,"deletions":1}
]"#,
    );
    let mut art = Artifacts::new("pr_column");
    let app = Tmux::launch("prcol", SIZE, &fx.root, &fx.app_env(path_with(&gh_bin)));

    let (ok, screen) = app.wait_for(LOAD, settled);
    art.screen("pr-column", &screen);
    art.check("worktrees load with gh", ok, "");
    let expected: [(&str, &[&str]); 8] = [
        ("main", &["up", "to", "date", "-", "0"]),
        ("feat-open", &["(ff)", "#42", "open", "0"]),
        ("feat-draft", &["(ff)", "#43", "draft", "0"]),
        ("feat-closed", &["(ff)", "#44", "closed", "0"]),
        ("feat-merged", &["(ff)", "#45", "merged", "0"]),
        ("feat-fork", &["(ff)", "-", "0"]),
        ("feat-multi", &["(ff)", "#21", "open", "0"]),
        ("feat-none", &["(ff)", "-", "0"]),
    ];
    for (branch, run) in expected {
        check_row(&mut art, &screen, branch, run);
    }
    let styled = app.capture_styled();
    art.screen("pr-column-styled", &styled);
    // green open, grey draft, red closed, purple (magenta) merged
    let colors = [
        ("#42 open", "[38;5;2m"),
        ("#43 draft", "[38;5;8m"),
        ("#44 closed", "[38;5;1m"),
        ("#45 merged", "[38;5;5m"),
    ];
    for (text, color) in colors {
        let style = style_before(&styled, text);
        art.check(
            format!("{text} is colored ({color})"),
            style.as_deref().is_some_and(|s| s.ends_with(color)),
            format!("{style:?}"),
        );
    }

    // p on a row jumps to its open PR.
    app.keys(&["l"]);
    select_row(&app, "Branch", "feat-multi");
    app.keys(&["p"]);
    let (ok, screen) = app.wait_for(SHORT, |s| pr_selected(s, "#21"));
    art.screen("p-jumps-to-pr", &screen);
    art.check(
        "p opens the Pull requests tab on the newest PR (#21)",
        ok && screen.contains("Second try"),
        "",
    );

    // Clicking the PR cell does the same.
    app.keys(&["Tab"]);
    let (_, screen) = app.wait_for(SHORT, |s| s.contains("#42 open"));
    app.click(locate(&screen, "#42 open", MAIN_PANE).unwrap_or_default());
    let (ok, screen) = app.wait_for(SHORT, |s| pr_selected(s, "#42"));
    art.screen("click-jumps-to-pr", &screen);
    art.check("clicking a PR cell jumps to that PR", ok, "");

    // Merged and missing PRs explain themselves.
    app.keys(&["Tab"]);
    select_row(&app, "Branch", "feat-merged");
    app.keys(&["p"]);
    let screen = app.capture();
    art.screen("p-on-merged", &screen);
    art.check(
        "p on a merged PR says so",
        screen.contains("PR #45 is merged; the Pull requests tab lists open PRs."),
        "",
    );
    select_row(&app, "Branch", "feat-none");
    app.keys(&["p"]);
    art.check(
        "p without a PR says so",
        app.capture().contains("feat-none has no PR."),
        "",
    );
    let calls = std::fs::read_to_string(fx.path("gh-calls.log")).unwrap_or_default();
    let all_calls = calls.lines().filter(|l| l.contains("--state all")).count();
    art.check(
        "one gh call for all worktrees",
        all_calls == 1,
        format!("{all_calls} calls"),
    );
    app.keys(&["q"]);
    art.check("q quits", app.wait_exit(SHORT), "");

    // Without gh the column stays empty.
    let app = Tmux::launch(
        "prcol-no-gh",
        SIZE,
        &fx.root,
        &fx.app_env(git_only_bin(&fx).display().to_string()),
    );
    let (ok, screen) = app.wait_for(LOAD, settled);
    art.screen("without-gh", &screen);
    art.check("loads without gh", ok, "");
    check_row(
        &mut art,
        &screen,
        "feat-open",
        &["feat-open", "↑0", "↓1", "ready", "(ff)", "0"],
    );
    check_row(
        &mut art,
        &screen,
        "main",
        &["main", "↑0", "↓0", "up", "to", "date", "0"],
    );
    app.keys(&["q"]);
    art.check("q quits (no gh)", app.wait_exit(SHORT), "");
    art.finish();
}
