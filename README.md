# tuitree

A small terminal dashboard for your git worktrees and GitHub pull requests, in the spirit of
LazyGit. Built for Omarchy (Arch Linux), works on any Linux with `git`.

For each project you add, tuitree shows:

- **Worktrees**: every local worktree with its branch, commits ahead/behind `origin/<default>`,
  whether it can sync with `origin/<default>`, and the files and lines changed since the
  merge-base with `origin/<default>`. Open one to see the per-file `+added -deleted` list.
- **Pull requests**: open PRs from GitHub (number, title, branch, author, draft, files, lines).
  Open one to see its per-file list.

On start and on refresh it runs `git fetch origin --prune` in the background, so the numbers
match the remote. The UI never waits for git or the network.

## Install

You need the Rust toolchain (`sudo pacman -S rustup && rustup default stable`) and `git`.
For the PR view you also need the GitHub CLI, logged in:

```sh
sudo pacman -S github-cli
gh auth login
```

Then, from this directory:

```sh
cargo install --path .
```

This puts `tuitree` in `~/.cargo/bin`. Make sure that directory is on your `PATH`, then run:

```sh
tuitree
```

## Keys

| Key            | Action                                         |
| -------------- | ---------------------------------------------- |
| `j` / `k`, arrows | Move                                        |
| `Tab`          | Switch between Worktrees and Pull requests     |
| `Enter` / `l`  | Open (project, then list, then files)          |
| `Esc` / `h`    | Back                                           |
| `s`            | Sync the selected worktree with `origin/<default>` (asks first) |
| `x`            | Delete the selected worktree and its local branch (asks first) |
| `a`            | Add a project (type a path, `~` works)         |
| `d`            | Remove the selected project (asks first)       |
| `r`            | Refresh: fetch, worktree stats and PRs         |
| `?`            | Help                                           |
| `q`            | Quit                                           |

The mouse works too: click a project, a tab or a row to select it, click a selected row
again (or double click) to open it, click dialog buttons, and use the wheel to scroll the list
under the pointer.

Removing a project only removes it from tuitree. Nothing on disk is touched.

## Sync with origin

The Sync column shows, for each worktree, what `git merge origin/<default>` would do:

- `up to date`: nothing to merge.
- `ready`: merges cleanly. `ready (ff)` means a fast-forward, no merge commit.
- `conflicts N`: N files would conflict. Open the worktree to see which ones.
- `dirty`: uncommitted changes to tracked files, so tuitree won't merge.

The check runs in the background with `git merge-tree --write-tree` (git 2.38 or newer),
which never touches your branches, index or files. Press `s` on a `ready` worktree to run
`git merge origin/<default>` in it. It merges and never rebases, so no history is rewritten
and it is safe for pushed branches. If the merge hits conflicts anyway, tuitree runs
`git merge --abort` and tells you.

## Delete a worktree

Press `x` on a worktree to remove it with `git worktree remove` and delete its local branch
with `git branch -d`. The dialog warns about uncommitted changes, commits that are not on
origin, and a branch that is not merged into `origin/<default>`. If git refuses because of
those, tuitree asks a second time before forcing it (`git worktree remove --force`,
`git branch -D`). Detached worktrees only lose the worktree. The main worktree and the
default branch are never deleted, and remote branches are never touched.

## Config

Projects are stored in `$XDG_CONFIG_HOME/tuitree/config.toml`, or
`~/.config/tuitree/config.toml` when `XDG_CONFIG_HOME` is not set:

```toml
[[projects]]
path = "/home/you/code/my-app"
```

## How the numbers are counted

- **Ahead/behind**: `git rev-list --left-right --count origin/<default>...HEAD`. The default
  branch comes from `refs/remotes/origin/HEAD` and falls back to `main`.
- **Files and lines**: `git diff --numstat <merge-base>` in the worktree. This compares the
  merge-base with the files on disk, so it includes commits, staged and unstaged changes.
- **Untracked files** (not ignored) count as new files: every line is an addition. Binary
  files show `bin` and add no line counts.

## Tests

The tests start the real `tuitree` binary inside `tmux` against throwaway git repos (a local
"origin", a clone with several worktrees, commits ahead and behind, uncommitted edits,
untracked and binary files, branches that merge cleanly or conflict) and a fake `gh`. They
press keys and send mouse clicks, check the rendered screens, and check the repos with git
after syncing and deleting. You need `tmux` installed.

```sh
cargo test
```

Each run saves every captured screen and a pass/fail summary in
`tests/artifacts/<date-time>/`.

There is also a live smoke test against real GitHub repos. It needs network access and a
logged-in `gh`. Point it at a local clone with worktrees and at a local clone of a repo with
open PRs (they can be the same):

```sh
TUITREE_LIVE_WORKTREES=~/code/my-app TUITREE_LIVE_PRS=~/code/other-app \
  cargo test --test e2e -- --ignored
```

It fetches both repos and only reads them otherwise (it never syncs or deletes anything).
Its screens go to `tests/artifacts/<date-time>/live_github_smoke/` and are copied to `smoke/`.
