//! The job queue, batch actions and forced deletes, against a fixture repo.

use std::time::Duration;

use crate::fixture::{Fixture, fake_gh_with_prs, git, path_with, plain_clone, queue_repo};
use crate::harness::{
    Artifacts, Tmux, branch_exists, branch_of, check_row, has_ahead_behind, has_run,
    has_worktree_row, is_selected, locate, select_row, settled, tokens,
};

const SIZE: (u16, u16) = (200, 64);
const LOAD: Duration = Duration::from_secs(60);
const SHORT: Duration = Duration::from_secs(5);
/// The projects pane is 30 columns wide; the main pane starts right of it.
const MAIN_PANE: usize = 30;

/// Screen row of the worktree table row for `branch`.
fn worktree_row(screen: &str, branch: &str) -> Option<u16> {
    screen
        .lines()
        .position(|l| has_ahead_behind(l) && branch_of(l) == Some(branch))
        .map(|row| row as u16)
}

/// A queue panel line (`<state> <verb> <label>`) is on screen.
fn queue_shows(screen: &str, state: &str, verb: &str, label: &str) -> bool {
    screen.lines().any(|l| has_run(l, &[state, verb, label]))
}

fn marked(screen: &str, branch: &str) -> bool {
    screen.lines().any(|l| {
        let t = tokens(l);
        t.windows(2).any(|w| w == ["●", branch]) || t.windows(3).any(|w| w == ["▶", "●", branch])
    })
}

#[test]
fn queue_batch_and_forced_delete() {
    let fx = Fixture::new("queue");
    let project = queue_repo(&fx);
    let plain = plain_clone(&fx);
    fx.write_config(&[&project, &plain]);
    let gh_bin = fake_gh_with_prs(&fx);
    let mut art = Artifacts::new("queue_batch_and_forced_delete");
    let app = Tmux::launch("queue", SIZE, &fx.root, &fx.app_env(path_with(&gh_bin)));

    let (ok, screen) = app.wait_for(LOAD, settled);
    art.screen("start", &screen);
    art.check("worktrees load", ok, "");
    art.check(
        "missing worktree row shows the missing directory",
        screen
            .lines()
            .any(|l| tokens(l).contains(&"missing") && l.contains("worktree directory is missing")),
        "",
    );
    check_row(
        &mut art,
        &screen,
        "conflicted",
        &["conflicted", "↑1", "↓1", "conflicts", "1"],
    );
    check_row(
        &mut art,
        &screen,
        "dirtysync",
        &["dirtysync", "↑0", "↓1", "dirty"],
    );
    check_row(
        &mut art,
        &screen,
        "ready-ff",
        &["ready-ff", "↑0", "↓1", "ready", "(ff)"],
    );
    check_row(
        &mut art,
        &screen,
        "ready-merge",
        &["ready-merge", "↑1", "↓1", "ready", "-", "1"],
    );
    app.keys(&["l"]);

    // 1. A dirty worktree goes with one confirm; no second "force" prompt.
    select_row(&app, "Branch", "dirty");
    app.keys(&["x"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("Delete worktree"));
    art.screen("delete-dirty-confirm", &screen);
    art.check(
        "single confirm explains the force and the uncommitted change",
        ok && screen
            .contains("Deletes with --force: uncommitted and untracked files in them are lost.")
            && screen.contains("⚠ uncommitted changes: 1 changed file, 0 untracked files"),
        "",
    );
    app.keys(&["y"]);
    let (ok, screen) = app.wait_for(LOAD, |s| settled(s) && !has_worktree_row(s, "dirty"));
    art.screen("deleted-dirty", &screen);
    art.check(
        "dirty worktree deleted after one confirm",
        ok && !screen.contains("Force delete") && queue_shows(&screen, "done", "delete", "dirty"),
        "",
    );
    art.check(
        "git: wt-dirty and branch dirty are gone",
        !fx.path("wt-dirty").exists() && !branch_exists(&project, "dirty"),
        "",
    );

    // 2. Batch: untracked-heavy, submodule, locked, missing, locked+missing; marked with space.
    let hard = ["untracked", "withsub", "locked", "missing", "lockgone"];
    for branch in hard {
        select_row(&app, "Branch", branch);
        app.keys(&["Space"]);
    }
    let screen = app.capture();
    art.screen("marked-hard-cases", &screen);
    for branch in hard {
        art.check(format!("{branch} is marked"), marked(&screen, branch), "");
    }
    app.keys(&["x"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("Delete 5 worktrees"));
    art.screen("delete-hard-cases-confirm", &screen);
    art.check(
        "one confirm lists all five",
        ok && hard
            .iter()
            .all(|b| screen.contains(&format!("• ~/wt-{b}  worktree and local branch {b}")))
            && screen.contains("301 untracked files")
            && screen.contains("⚠ locked (git worktree lock); it is unlocked and removed")
            && screen
                .contains("⚠ could not read the worktree state: worktree directory is missing")
            && screen.contains("[ y Delete 5 ]"),
        "",
    );
    app.keys(&["y"]);
    let (ok, screen) = app.wait_for(LOAD, |s| {
        settled(s) && hard.iter().all(|b| !has_worktree_row(s, b))
    });
    art.screen("deleted-hard-cases", &screen);
    art.check("all five deleted", ok, "");
    let listing = git(&project, &["worktree", "list", "--porcelain"]);
    for branch in hard {
        art.check(
            format!("git: {branch} worktree and branch are gone"),
            !fx.path(&format!("wt-{branch}")).exists()
                && !listing.contains(&format!("wt-{branch}"))
                && !branch_exists(&project, branch),
            "",
        );
    }

    // 3. Batch delete of 3 rows marked with the mouse; the cursor stays on its worktree.
    select_row(&app, "Branch", "ready-merge");
    let screen = app.capture();
    let mark_x = locate(&screen, "Branch", MAIN_PANE).map(|(x, _)| x - 2);
    for branch in ["b1", "b2", "b3"] {
        if let (Some(x), Some(y)) = (mark_x, worktree_row(&screen, branch)) {
            app.click((x, y));
        }
    }
    let screen = app.capture();
    art.screen("marked-with-mouse", &screen);
    art.check(
        "clicking the mark column marks rows without moving the cursor",
        ["b1", "b2", "b3"].iter().all(|b| marked(&screen, b))
            && !marked(&screen, "ready-merge")
            && is_selected(&screen, "ready-merge"),
        "",
    );
    app.keys(&["x"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("Delete 3 worktrees"));
    art.screen("delete-three-confirm", &screen);
    art.check(
        "one confirm lists the three marked rows",
        ok && ["b1", "b2", "b3"]
            .iter()
            .all(|b| screen.contains(&format!("• ~/wt-{b}  worktree and local branch {b}"))),
        "",
    );
    app.click(locate(&screen, "[ y Delete 3 ]", 0).unwrap_or_default());
    let (ok, screen) = app.wait_for(LOAD, |s| {
        settled(s) && ["b1", "b2", "b3"].iter().all(|b| !has_worktree_row(s, b))
    });
    art.screen("deleted-three", &screen);
    art.check("three worktrees deleted", ok, "");
    art.check(
        "cursor stays on ready-merge across the reloads",
        is_selected(&screen, "ready-merge"),
        "",
    );
    art.check(
        "git: b1, b2, b3 are gone",
        ["b1", "b2", "b3"]
            .iter()
            .all(|b| !fx.path(&format!("wt-{b}")).exists() && !branch_exists(&project, b)),
        "",
    );

    // 4. Marks survive a refresh; batch sync runs only the ready rows.
    let mixed = ["ready-ff", "ready-merge", "conflicted", "dirtysync"];
    for branch in mixed {
        select_row(&app, "Branch", branch);
        app.keys(&["Space"]);
    }
    app.keys(&["r"]);
    let (ok, screen) = app.wait_for(LOAD, settled);
    art.screen("marks-after-refresh", &screen);
    art.check(
        "marks survive a refresh",
        ok && mixed.iter().all(|b| marked(&screen, b)),
        "",
    );
    app.keys(&["s"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("Sync 2 with origin"));
    art.screen("sync-mixed-confirm", &screen);
    art.check(
        "one confirm: two to sync, two skipped with reasons",
        ok && screen.contains("• ready-ff: fast-forward")
            && screen.contains("• ready-merge: merge commit (1 behind, 1 ahead)")
            && screen.contains("• conflicted skipped: would conflict in 1 file")
            && screen.contains("• dirtysync skipped: uncommitted changes"),
        "",
    );
    app.keys(&["y"]);
    let (ok, screen) = app.wait_for(LOAD, |s| {
        settled(s)
            && ["ready-ff", "ready-merge"]
                .iter()
                .all(|b| crate::harness::line_with_token(s, b).is_some_and(|l| l.contains("↓0")))
    });
    art.screen("synced-mixed", &screen);
    art.check("the ready rows synced", ok, "");
    check_row(
        &mut art,
        &screen,
        "ready-ff",
        &["ready-ff", "↑0", "↓0", "up", "to", "date"],
    );
    check_row(
        &mut art,
        &screen,
        "ready-merge",
        &["ready-merge", "↑2", "↓0", "up", "to", "date"],
    );
    check_row(
        &mut art,
        &screen,
        "conflicted",
        &["conflicted", "↑1", "↓1", "conflicts", "1"],
    );
    check_row(
        &mut art,
        &screen,
        "dirtysync",
        &["dirtysync", "↑0", "↓1", "dirty"],
    );
    art.check(
        "queue reports the skipped rows",
        queue_shows(&screen, "skipped", "sync", "conflicted")
            && queue_shows(&screen, "skipped", "sync", "dirtysync")
            && screen.contains("would conflict in 1 file")
            && queue_shows(&screen, "done", "sync", "ready-ff")
            && queue_shows(&screen, "done", "sync", "ready-merge"),
        "",
    );
    art.check(
        "git: only the ready worktrees contain origin/main",
        git(
            &fx.path("wt-ready-merge"),
            &["rev-list", "--count", "HEAD..origin/main"],
        )
        .trim()
            == "0"
            && git(
                &fx.path("wt-conflicted"),
                &["rev-list", "--count", "HEAD..origin/main"],
            )
            .trim()
                == "1",
        "",
    );

    // 5. A failing job, a slow one, and a job queued while the slow one runs.
    select_row(&app, "Branch", "~/wt-twin-a");
    app.keys(&["x", "y"]);
    select_row(&app, "Branch", "slow");
    app.keys(&["x", "y"]);
    let (ok, _) = app.wait_for(LOAD, |s| queue_shows(s, "running", "delete", "slow"));
    art.check("slow job starts after the failed one", ok, "");
    select_row(&app, "Branch", "after");
    app.keys(&["x", "y"]);
    let screen = app.capture();
    art.screen("queued-while-running", &screen);
    art.check(
        "failed job with git's full error, next job still running, third one pending",
        queue_shows(&screen, "failed", "delete", "twin")
            && screen.contains("The worktree is gone, but git branch -D twin failed:")
            && screen.contains("cannot delete branch 'twin' used by worktree at")
            && queue_shows(&screen, "running", "delete", "slow")
            && queue_shows(&screen, "pending", "delete", "after"),
        "",
    );
    art.check(
        "rows show their queued and running jobs",
        crate::harness::line_with_token(&screen, "slow").is_some_and(|l| l.contains("deleting"))
            && crate::harness::line_with_token(&screen, "after")
                .is_some_and(|l| l.contains("queued delete")),
        "",
    );

    // The UI stays usable while the slow job runs: move, switch projects, come back.
    let moved = select_row(&app, "Branch", "conflicted");
    let screen = app.capture();
    app.click(locate(&screen, "plain", 0).unwrap_or_default());
    let (switched, _) = app.wait_for(SHORT, |s| s.contains("┌ ~/plain "));
    let screen = app.capture();
    art.screen("responsive-other-project", &screen);
    app.click(locate(&screen, "project", 0).unwrap_or_default());
    let (back, screen) = app.wait_for(SHORT, |s| s.contains("┌ ~/project "));
    art.screen("responsive-back", &screen);
    art.check(
        "navigation and project switching work while the slow job runs",
        moved && switched && back && queue_shows(&screen, "running", "delete", "slow"),
        "",
    );

    let (ok, screen) = app.wait_for(LOAD, |s| {
        settled(s) && !has_worktree_row(s, "slow") && !has_worktree_row(s, "after")
    });
    art.screen("queue-finished", &screen);
    art.check(
        "the jobs after the failure finished",
        ok && queue_shows(&screen, "done", "delete", "slow")
            && queue_shows(&screen, "done", "delete", "after"),
        "",
    );
    art.check(
        "git: twin-a worktree removed, branch twin kept for twin-b; slow and after gone",
        !fx.path("wt-twin-a").exists()
            && branch_exists(&project, "twin")
            && !fx.path("wt-slow").exists()
            && !branch_exists(&project, "slow")
            && !fx.path("wt-after").exists(),
        "",
    );

    // 6. Quitting with a job still running asks first.
    app.keys(&["l"]);
    select_row(&app, "Branch", "slow2");
    app.keys(&["x", "y", "q"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("Quit?"));
    art.screen("quit-with-running-job", &screen);
    art.check(
        "q asks while a job runs",
        ok && screen.contains("1 job is still queued or running."),
        "",
    );
    app.keys(&["n"]);
    let still_running = app.is_running();
    let (done, _) = app.wait_for(LOAD, |s| settled(s) && !has_worktree_row(s, "slow2"));
    app.keys(&["q"]);
    art.check(
        "cancel keeps the app; once idle, q quits right away",
        still_running && done && app.wait_exit(SHORT),
        "",
    );
    art.finish();
}
