# tuitree

A small terminal dashboard for your git worktrees and GitHub pull requests, in the spirit of
LazyGit. Built for Omarchy (Arch Linux), works on any Linux with `git`.

For each project you add, tuitree shows:

- **Worktrees**: every local worktree with its branch, commits ahead/behind `origin/<default>`,
  and the files and lines changed since the merge-base with `origin/<default>`. Open one to see
  the per-file `+added -deleted` list.
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
| `a`            | Add a project (type a path, `~` works)         |
| `d`            | Remove the selected project (asks first)       |
| `r`            | Refresh: fetch, worktree stats and PRs         |
| `?`            | Help                                           |
| `q`            | Quit                                           |

Removing a project only removes it from tuitree. Nothing on disk is touched.

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
untracked and binary files) and a fake `gh`. They check the rendered screens. You need `tmux`
installed.

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

It fetches both repos and never changes them otherwise. Its screens go to
`tests/artifacts/<date-time>/live_github_smoke/` and are copied to `smoke/`.
