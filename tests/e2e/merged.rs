//! Branches that were squash merged: found by content (no gh needed) and by merged PRs.

use std::time::Duration;

use crate::fixture::{
    FakePr, Fixture, fake_gh_with_prs, git_only_bin, merged_repo, path_with, set_repo_prs,
};
use crate::harness::{
    Artifacts, Tmux, branch_exists, check_row, has_worktree_row, main_tokens, row_line, select_row,
    settled,
};

const SIZE: (u16, u16) = (190, 50);
const LOAD: Duration = Duration::from_secs(30);
const SHORT: Duration = Duration::from_secs(5);

fn marked(screen: &str, branch: &str) -> bool {
    row_line(screen, branch).is_some_and(|l| main_tokens(l).contains(&"●"))
}

#[test]
fn squash_merged_branches() {
    let fx = Fixture::new("merged");
    let repo = merged_repo(&fx);
    fx.write_config(&[&repo.project]);
    set_repo_prs(
        &fx,
        &[
            FakePr::merged(42, "pr-merged", &repo.pr_merged_head),
            FakePr::merged(7, "reused", &repo.reused_old_head),
        ],
    );
    let gh_bin = fake_gh_with_prs(&fx);
    let mut art = Artifacts::new("squash_merged_branches");

    // Without gh: the content check alone finds the squash merge that main left alone.
    let app = Tmux::launch(
        "merged-no-gh",
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
        "squashed",
        &["squashed", "↑2", "↓4", "merged", "2"],
    );
    check_row(
        &mut art,
        &screen,
        "pr-merged",
        &["pr-merged", "↑1", "↓4", "conflicts", "1"],
    );
    app.keys(&["q"]);
    art.check("q quits", app.wait_exit(SHORT), "");

    // With gh: merged PRs identify the rest, but not a reused branch with new work.
    let app = Tmux::launch("merged", SIZE, &fx.root, &fx.app_env(path_with(&gh_bin)));
    let (ok, screen) = app.wait_for(LOAD, settled);
    art.screen("with-gh", &screen);
    art.check("loads with gh", ok, "");
    check_row(
        &mut art,
        &screen,
        "squashed",
        &["squashed", "↑2", "↓4", "merged", "-", "2"],
    );
    check_row(
        &mut art,
        &screen,
        "pr-merged",
        &[
            "pr-merged",
            "↑1",
            "↓4",
            "merged",
            "#42",
            "#42",
            "merged",
            "1",
        ],
    );
    check_row(
        &mut art,
        &screen,
        "reused",
        &["reused", "↑2", "↓4", "ready", "#7", "merged", "1"],
    );
    check_row(
        &mut art,
        &screen,
        "open-work",
        &["open-work", "↑1", "↓4", "ready", "-", "1"],
    );
    let calls = std::fs::read_to_string(fx.path("gh-calls.log")).unwrap_or_default();
    let merged_calls = calls.lines().filter(|l| l.contains("--state all")).count();
    art.check(
        "one PR query per refresh",
        merged_calls == 1 && calls.contains("pr list -R e2e/fixture --state all --limit 300"),
        format!("{merged_calls} calls: {calls:?}"),
    );

    // Detail and sync explain it; the delete dialog does not call it unmerged.
    app.keys(&["l"]);
    select_row(&app, "Branch", "squashed");
    app.keys(&["Enter"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("✓ Already merged"));
    art.screen("squashed-detail", &screen);
    art.check(
        "detail says it is merged",
        ok && screen.contains("merging it into origin/main would change nothing"),
        "",
    );
    app.keys(&["Escape", "s"]);
    let screen = app.capture();
    art.check(
        "sync refuses a merged branch",
        screen.contains("squashed is already merged into origin/main; delete it (x) instead"),
        "",
    );
    app.keys(&["x"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("Delete worktree"));
    art.screen("squashed-delete-confirm", &screen);
    art.check(
        "delete dialog: merged, no unmerged or unpushed warning",
        ok && screen.contains("✓ merged; its work is already in origin/main")
            && !screen.contains("not merged")
            && !screen.contains("unpushed"),
        "",
    );
    app.keys(&["n"]);

    // M marks every merged row, and one confirm cleans them up.
    app.keys(&["M"]);
    let screen = app.capture();
    art.screen("marked-merged", &screen);
    art.check(
        "M marks exactly the merged rows",
        marked(&screen, "squashed")
            && marked(&screen, "pr-merged")
            && !marked(&screen, "reused")
            && !marked(&screen, "open-work"),
        "",
    );
    app.keys(&["x"]);
    let (ok, screen) = app.wait_for(SHORT, |s| s.contains("Delete 2 worktrees"));
    art.screen("delete-merged-confirm", &screen);
    art.check(
        "batch delete of merged rows has no unmerged warnings",
        ok && screen.contains("✓ merged (PR #42); its work is in origin/main")
            && !screen.contains("not merged"),
        "",
    );
    app.keys(&["y"]);
    let (ok, screen) = app.wait_for(LOAD, |s| {
        settled(s) && !has_worktree_row(s, "squashed") && !has_worktree_row(s, "pr-merged")
    });
    art.screen("deleted-merged", &screen);
    art.check(
        "merged worktrees cleaned up",
        ok && !fx.path("wt-squashed").exists()
            && !branch_exists(&repo.project, "squashed")
            && !branch_exists(&repo.project, "pr-merged")
            && has_worktree_row(&screen, "reused")
            && has_worktree_row(&screen, "open-work"),
        "",
    );
    app.keys(&["q"]);
    art.check("q quits", app.wait_exit(SHORT), "");
    art.finish();
}
