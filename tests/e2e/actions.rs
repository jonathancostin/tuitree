//! Sync status, sync, worktree cleanup and mouse input, against a fixture repo.

use std::time::Duration;

use crate::fixture::{Fixture, fake_gh_with_prs, git, path_with, plain_clone, sync_repo};
use crate::harness::{
    Artifacts, Tmux, check_row, git_ok, has_ahead_behind, has_worktree_row, is_selected,
    line_with_token, locate, row_below, select_row, settled, tokens,
};

const SIZE: (u16, u16) = (180, 50);
const LOAD: Duration = Duration::from_secs(30);
const SHORT: Duration = Duration::from_secs(5);
/// The projects pane is 30 columns wide; the main pane starts right of it.
const MAIN_PANE: usize = 30;

#[test]
fn worktree_cleanup_sync_and_mouse() {
    let fx = Fixture::new("actions");
    let project = sync_repo(&fx);
    let plain = plain_clone(&fx);
    fx.write_config(&[&project, &plain]);
    let gh_bin = fake_gh_with_prs(&fx);
    let mut art = Artifacts::new("worktree_cleanup_sync_and_mouse");
    let app = Tmux::launch("actions", SIZE, &fx.root, &fx.app_env(path_with(&gh_bin)));

    // Sync column.
    let (ok, screen) = app.wait_for(LOAD, settled);
    art.screen("sync-status", &screen);
    art.check("worktrees and sync statuses load", ok, "");
    check_row(
        &mut art,
        &screen,
        "trunk",
        &["trunk", "↑0", "↓0", "up", "to", "date"],
    );
    check_row(
        &mut art,
        &screen,
        "main",
        &["main", "↑0", "↓1", "ready", "(ff)"],
    );
    check_row(
        &mut art,
        &screen,
        "ff-only",
        &["ff-only", "↑0", "↓1", "ready", "(ff)"],
    );
    check_row(
        &mut art,
        &screen,
        "ready-merge",
        &["ready-merge", "↑1", "↓1", "ready", "-", "1", "+1", "-1"],
    );
    check_row(
        &mut art,
        &screen,
        "conflict",
        &["conflict", "↑1", "↓1", "conflicts", "1", "-", "2"],
    );
    check_row(
        &mut art,
        &screen,
        "dirty",
        &["dirty", "↑0", "↓1", "dirty", "-", "1", "+1", "-0"],
    );

    // Mouse: click the other project, then back.
    let target = locate(&screen, "plain", 0);
    art.check(
        "project `plain` located",
        target.is_some(),
        format!("{target:?}"),
    );
    app.click(target.unwrap_or_default());
    let (ok, screen) = app.wait_for(LOAD, |s| s.contains("┌ ~/plain "));
    art.screen("mouse-click-project", &screen);
    art.check("clicking a project selects it", ok, "");
    app.click(locate(&screen, "project", 0).unwrap_or_default());
    let (ok, _) = app.wait_for(SHORT, |s| s.contains("┌ ~/project "));
    art.check("clicking the first project goes back", ok, "");

    // Mouse: tabs.
    let screen = app.capture();
    app.click(locate(&screen, "Pull requests", MAIN_PANE).unwrap_or_default());
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("#42"));
    art.screen("mouse-click-prs-tab", &screen);
    art.check("clicking the Pull requests tab switches to it", ok, "");
    app.click(locate(&screen, "Worktrees", MAIN_PANE).unwrap_or_default());
    let (ok, screen) = app.wait_for(SHORT, |s| s.lines().any(|l| tokens(l).contains(&"Sync")));
    art.check("clicking the Worktrees tab switches back", ok, "");

    // Mouse: click a row to select it, click it again to open its detail.
    app.click(locate(&screen, "conflict ", MAIN_PANE).unwrap_or_default());
    let screen = app.capture();
    art.screen("mouse-select-row", &screen);
    art.check(
        "clicking a row selects it",
        is_selected(&screen, "conflict"),
        "",
    );
    art.check(
        "the first click does not open it",
        !screen.contains("vs merge-base"),
        "",
    );
    app.click(locate(&screen, "conflict ", MAIN_PANE).unwrap_or_default());
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("vs merge-base"));
    art.screen("mouse-open-row-conflicts", &screen);
    art.check("clicking the selected row opens it", ok, "");
    art.check(
        "detail lists the conflicting files",
        screen.contains("Merging origin/main would conflict in 1 file(s):")
            && line_with_token(&screen, "shared.txt").is_some_and(|l| l.contains("✗ shared.txt")),
        "",
    );
    let detail_row = screen
        .lines()
        .enumerate()
        .filter(|(_, l)| tokens(l).contains(&"shared.txt") && l.contains("+1"))
        .map(|(row, l)| {
            (
                l.find("shared.txt").map_or(0, |b| l[..b].chars().count()),
                row,
            )
        })
        .next();
    app.click(detail_row.map_or((0, 0), |(col, row)| (col as u16, row as u16)));
    let screen = app.capture();
    art.screen("mouse-select-file", &screen);
    art.check(
        "clicking a file row selects it",
        screen.lines().any(|l| {
            let t = tokens(l);
            t.windows(4).any(|w| w == ["▶", "+1", "-1", "shared.txt"])
        }),
        "",
    );

    // Mouse wheel over the worktree list moves its selection.
    app.keys(&["Escape"]);
    let screen = app.capture();
    let before = row_below(&screen, "Branch", |l| tokens(l).contains(&"▶"));
    let over_list = locate(&screen, "conflict ", MAIN_PANE).unwrap_or_default();
    app.scroll(over_list, true);
    let screen = app.capture();
    let after = row_below(&screen, "Branch", |l| tokens(l).contains(&"▶"));
    art.screen("mouse-wheel", &screen);
    art.check(
        "wheel down moves the selection one row",
        before.is_some() && after == before.map(|b| b + 1),
        format!("{before:?} -> {after:?}"),
    );

    // Sync refusals explain why.
    let refusals = [
        ("conflict", "would conflict in 1 file. Merge by hand."),
        (
            "dirty",
            "dirty has uncommitted changes. Commit or stash them, then sync.",
        ),
        ("trunk", "trunk is already up to date with origin/main."),
    ];
    for (branch, why) in refusals {
        select_row(&app, "Branch", branch);
        app.keys(&["s"]);
        let screen = app.capture();
        art.screen(&format!("sync-refused-{branch}"), &screen);
        art.check(
            format!("sync of {branch} is refused"),
            screen.contains(why),
            "",
        );
    }

    // Sync a branch that merges cleanly, confirming with a mouse click on the dialog button.
    art.check(
        "select ready-merge",
        select_row(&app, "Branch", "ready-merge"),
        "",
    );
    app.keys(&["s"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("Merge origin/main into ready-merge?"));
    art.screen("sync-confirm", &screen);
    art.check(
        "sync asks first",
        ok && screen.contains("Creates a merge commit"),
        "",
    );
    app.click(locate(&screen, "[ y Sync ]", 0).unwrap_or_default());
    let (ok, screen) = app.wait_for(LOAD, |s| {
        settled(s) && line_with_token(s, "ready-merge").is_some_and(|l| l.contains("↓0"))
    });
    art.screen("synced-ready-merge", &screen);
    art.check("clicking the Sync button merges", ok, "");
    check_row(
        &mut art,
        &screen,
        "ready-merge",
        &["ready-merge", "↑2", "↓0", "up", "to", "date"],
    );
    let merged = fx.path("wt-ready-merge");
    art.check(
        "git: ready-merge contains origin/main",
        git(&merged, &["rev-list", "--count", "HEAD..origin/main"]).trim() == "0",
        "",
    );

    // Sync a fast-forward with the keyboard.
    select_row(&app, "Branch", "ff-only");
    app.keys(&["s"]);
    let (ok, screen) = app.wait_for(SHORT, |s| {
        s.contains("Fast-forward: no merge commit needed.")
    });
    art.screen("sync-ff-confirm", &screen);
    art.check("ff sync asks first", ok, "");
    app.keys(&["y"]);
    let (ok, screen) = app.wait_for(LOAD, |s| {
        settled(s) && line_with_token(s, "ff-only").is_some_and(|l| l.contains("↓0"))
    });
    art.screen("synced-ff-only", &screen);
    art.check("ff sync leaves it up to date", ok, "");
    check_row(
        &mut art,
        &screen,
        "ff-only",
        &["ff-only", "↑0", "↓0", "up", "to", "date"],
    );

    // Protected worktrees.
    select_row(&app, "Branch", "trunk");
    app.keys(&["x"]);
    let screen = app.capture();
    art.screen("delete-main-worktree-refused", &screen);
    art.check(
        "main worktree is protected",
        screen.contains("The main worktree can't be deleted.")
            && !screen.contains("Delete worktree"),
        "",
    );
    select_row(&app, "Branch", "main");
    app.keys(&["x"]);
    let screen = app.capture();
    art.screen("delete-default-branch-refused", &screen);
    art.check(
        "default branch is protected",
        screen.contains("main is the default branch; tuitree won't delete it."),
        "",
    );

    // Clean delete: worktree and local branch go, the remote branch stays.
    select_row(&app, "Branch", "done");
    app.keys(&["x"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("Delete worktree"));
    art.screen("delete-done-confirm", &screen);
    art.check(
        "delete dialog shows path, branch and state",
        ok && screen.contains("• ~/wt-done  worktree and local branch done")
            && screen.contains("clean, pushed and merged"),
        "",
    );
    app.keys(&["y"]);
    let (ok, screen) = app.wait_for(LOAD, |s| settled(s) && !has_worktree_row(s, "done"));
    art.screen("deleted-done", &screen);
    art.check("clean worktree deleted and list refreshed", ok, "");
    art.check(
        "git: wt-done directory is gone",
        !fx.path("wt-done").exists(),
        "",
    );
    art.check(
        "git: local branch done is gone",
        !git_ok(
            &project,
            &["rev-parse", "--verify", "-q", "refs/heads/done"],
        ),
        "",
    );
    art.check(
        "git: remote branch done is untouched",
        git_ok(
            &fx.path("origin.git"),
            &["rev-parse", "--verify", "-q", "refs/heads/done"],
        ),
        "",
    );

    // Unmerged branch: one informed confirm, no second prompt.
    select_row(&app, "Branch", "conflict");
    app.keys(&["x"]);
    let (_, screen) = app.wait_for(SHORT, |s| s.contains("Delete worktree"));
    art.screen("delete-unmerged-confirm", &screen);
    art.check(
        "delete dialog warns about unpushed and unmerged commits",
        screen.contains("⚠ 1 commit not on origin (unpushed)")
            && screen
                .contains("⚠ branch not merged into origin/main: 1 commit only on this branch"),
        "",
    );
    let screen = app.capture();
    app.click(locate(&screen, "[ y Delete ]", 0).unwrap_or_default());
    let (ok, screen) = app.wait_for(LOAD, |s| settled(s) && !has_worktree_row(s, "conflict"));
    art.screen("deleted-unmerged", &screen);
    art.check(
        "clicking Delete deletes the unmerged branch without a second prompt",
        ok && !screen.contains("Force delete?"),
        "",
    );
    art.check(
        "git: wt-conflict and branch conflict are gone",
        !fx.path("wt-conflict").exists()
            && !git_ok(
                &project,
                &["rev-parse", "--verify", "-q", "refs/heads/conflict"],
            ),
        "",
    );

    // Detached worktree: only the worktree is removed; cancel first with the mouse.
    let screen = app.capture();
    let index = row_below(&screen, "Branch", |l| l.contains("(detached"));
    let current = row_below(&screen, "Branch", |l| tokens(l).contains(&"▶"));
    if let (Some(index), Some(current)) = (index, current) {
        let key = if index > current { "j" } else { "k" };
        app.keys(&vec![key; index.abs_diff(current)]);
    }
    app.keys(&["x"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("worktree only (detached HEAD)"));
    art.screen("delete-detached-confirm", &screen);
    art.check("detached delete dialog", ok, "");
    app.click(locate(&screen, "[ n Cancel ]", 0).unwrap_or_default());
    let screen = app.capture();
    art.check(
        "clicking Cancel closes the dialog and keeps the worktree",
        !screen.contains("Delete worktree") && fx.path("wt-detached").exists(),
        "",
    );
    app.keys(&["x", "y"]);
    let detached_row = |s: &str| {
        s.lines()
            .any(|l| has_ahead_behind(l) && l.contains("(detached"))
    };
    let (ok, screen) = app.wait_for(LOAD, |s| settled(s) && !detached_row(s));
    art.screen("deleted-detached", &screen);
    art.check(
        "detached worktree removed",
        ok && screen.contains("Deleted worktree ~/wt-detached.")
            && !fx.path("wt-detached").exists(),
        "",
    );

    app.keys(&["q"]);
    art.check("q quits", app.wait_exit(SHORT), "");
    art.finish();
}
