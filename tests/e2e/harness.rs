//! Drives the real `tuitree` binary inside a private tmux server and records what it rendered.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::thread::sleep;
use std::time::{Duration, Instant};

const SESSION: &str = "e2e";
/// Time for the app to process keys and redraw (it polls input every 100 ms).
const SETTLE: Duration = Duration::from_millis(300);

pub fn tuitree_bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_tuitree"))
}

/// One id per `cargo test` process, so all tests of a run share an artifacts directory.
static RUN_ID: LazyLock<String> = LazyLock::new(|| {
    let out = Command::new("date")
        .arg("+%Y%m%d-%H%M%S")
        .output()
        .expect("run `date`");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
});

pub fn run_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/artifacts")
        .join(&*RUN_ID)
}

/// Captured screens and pass/fail checks of one test, written under `tests/artifacts/<run>/`.
pub struct Artifacts {
    test: String,
    dir: PathBuf,
    mirror: Option<PathBuf>,
    screens: usize,
    checks: Vec<(bool, String, String)>,
}

impl Artifacts {
    pub fn new(test: &str) -> Self {
        let dir = run_dir().join(test);
        fs::create_dir_all(&dir).expect("create artifacts dir");
        Self {
            test: test.to_string(),
            dir,
            mirror: None,
            screens: 0,
            checks: Vec::new(),
        }
    }

    /// Also writes every artifact into `dir`, replacing the `.txt` files already there.
    pub fn mirrored_to(mut self, dir: PathBuf) -> Self {
        fs::create_dir_all(&dir).expect("create mirror dir");
        for entry in fs::read_dir(&dir).expect("read mirror dir").flatten() {
            if entry.path().extension().is_some_and(|ext| ext == "txt") {
                fs::remove_file(entry.path()).expect("clear old capture");
            }
        }
        self.mirror = Some(dir);
        self
    }

    fn write(&self, name: &str, content: &str) {
        for dir in std::iter::once(&self.dir).chain(&self.mirror) {
            fs::write(dir.join(name), content).expect("write artifact");
        }
    }

    pub fn screen(&mut self, label: &str, screen: &str) {
        self.screens += 1;
        self.write(&format!("{:02}-{label}.txt", self.screens), screen);
    }

    /// Records a check; returns whether it passed so callers can branch on it.
    pub fn check(
        &mut self,
        name: impl Into<String>,
        passed: bool,
        detail: impl Into<String>,
    ) -> bool {
        self.checks.push((passed, name.into(), detail.into()));
        passed
    }

    /// Writes the summary and fails the test if any check failed.
    pub fn finish(self) {
        let failed = self.checks.iter().filter(|(passed, ..)| !passed).count();
        let verdict = if failed == 0 { "PASS" } else { "FAIL" };
        let mut summary = format!(
            "{verdict} {}: {} checks, {failed} failed, {} screens\n\n",
            self.test,
            self.checks.len(),
            self.screens
        );
        for (passed, name, detail) in &self.checks {
            let mark = if *passed { "PASS" } else { "FAIL" };
            summary.push_str(&format!("[{mark}] {name}"));
            if !detail.is_empty() {
                summary.push_str(&format!(": {detail}"));
            }
            summary.push('\n');
        }
        self.write("summary.txt", &summary);
        let mut run_summary = OpenOptions::new()
            .create(true)
            .append(true)
            .open(run_dir().join("SUMMARY.txt"))
            .expect("open run summary");
        writeln!(
            run_summary,
            "{verdict} {} ({} checks, {failed} failed) {}",
            self.test,
            self.checks.len(),
            self.dir.display()
        )
        .expect("write run summary");
        assert_eq!(
            failed,
            0,
            "{failed} checks failed, see {}",
            self.dir.join("summary.txt").display()
        );
    }
}

/// Absolute path of `program` on the current `PATH`.
pub fn which(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// A tmux server of its own running one tuitree session.
pub struct Tmux {
    tmux: PathBuf,
    socket: String,
}

impl Tmux {
    /// Starts tuitree in a fresh tmux server whose environment is ours plus `env`.
    pub fn launch(label: &str, size: (u16, u16), cwd: &Path, env: &[(&str, String)]) -> Self {
        let tmux = which("tmux").expect("the e2e tests need tmux on PATH (sudo pacman -S tmux)");
        let tmux = Tmux {
            tmux,
            socket: format!("tuitree-e2e-{}-{label}", std::process::id()),
        };
        let (width, height) = (size.0.to_string(), size.1.to_string());
        let cwd = cwd.display().to_string();
        let program = tuitree_bin().display().to_string();
        let args = [
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            SESSION,
            "-x",
            &width,
            "-y",
            &height,
            "-c",
            &cwd,
            &program,
        ];
        // The env goes on the tmux client rather than `new-session -e`: tmux gives new
        // sessions the client's PATH. /bin/sh as default shell works with any test PATH.
        let status = tmux
            .command(&args)
            .envs(env.iter().map(|(key, value)| (key, value)))
            .env("SHELL", "/bin/sh")
            .status()
            .expect("start tmux");
        assert!(status.success(), "tmux new-session failed");
        tmux
    }

    fn command<S: AsRef<std::ffi::OsStr>>(&self, args: &[S]) -> Command {
        let mut command = Command::new(&self.tmux);
        command.arg("-L").arg(&self.socket).args(args);
        command
    }

    /// Sends tmux key names such as `j`, `Tab`, `Enter`, `Escape`, `C-u`.
    pub fn keys(&self, keys: &[&str]) {
        for key in keys {
            self.command(&["send-keys", "-t", SESSION, key])
                .status()
                .expect("tmux send-keys");
            sleep(Duration::from_millis(60));
        }
        sleep(SETTLE);
    }

    pub fn type_text(&self, text: &str) {
        self.command(&["send-keys", "-t", SESSION, "-l", text])
            .status()
            .expect("tmux send-keys -l");
        sleep(SETTLE);
    }

    /// Writes raw bytes to the app's input, as a terminal would.
    fn send_bytes(&self, bytes: &[u8]) {
        let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut args = vec!["send-keys", "-t", SESSION, "-H"];
        args.extend(hex.iter().map(String::as_str));
        self.command(&args).status().expect("tmux send-keys -H");
    }

    /// Left click at a 0-based screen cell, sent as an SGR (1006) mouse press and release.
    pub fn click(&self, (col, row): (u16, u16)) {
        let (x, y) = (col + 1, row + 1);
        self.send_bytes(format!("\x1b[<0;{x};{y}M").as_bytes());
        sleep(Duration::from_millis(40));
        self.send_bytes(format!("\x1b[<0;{x};{y}m").as_bytes());
        sleep(SETTLE);
    }

    /// One mouse wheel notch at a 0-based screen cell.
    pub fn scroll(&self, (col, row): (u16, u16), down: bool) {
        let button = if down { 65 } else { 64 };
        self.send_bytes(format!("\x1b[<{button};{};{}M", col + 1, row + 1).as_bytes());
        sleep(SETTLE);
    }

    pub fn capture(&self) -> String {
        let out = self
            .command(&["capture-pane", "-p", "-t", SESSION])
            .output()
            .expect("tmux capture-pane");
        let text = String::from_utf8_lossy(&out.stdout);
        let mut screen: String = text
            .lines()
            .map(|l| format!("{}\n", l.trim_end()))
            .collect();
        screen.truncate(screen.trim_end().len());
        screen.push('\n');
        screen
    }

    /// Polls the screen until `ready` holds; returns whether it did and the last screen.
    pub fn wait_for(&self, timeout: Duration, ready: impl Fn(&str) -> bool) -> (bool, String) {
        let deadline = Instant::now() + timeout;
        loop {
            let screen = self.capture();
            if ready(&screen) {
                sleep(SETTLE);
                return (true, self.capture());
            }
            if Instant::now() >= deadline {
                return (false, screen);
            }
            sleep(Duration::from_millis(150));
        }
    }

    pub fn is_running(&self) -> bool {
        self.command(&["has-session", "-t", SESSION])
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// Waits for the app to exit (the session ends with it).
    pub fn wait_exit(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while self.is_running() {
            if Instant::now() >= deadline {
                return false;
            }
            sleep(Duration::from_millis(100));
        }
        true
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = self.command(&["kill-server"]).output();
    }
}

/// Words of a screen line, with the table and border glyphs dropped.
pub fn tokens(line: &str) -> Vec<&str> {
    line.split(|c: char| c.is_whitespace() || matches!(c, '│' | '┌' | '┐' | '└' | '┘' | '─'))
        .filter(|token| !token.is_empty())
        .collect()
}

/// The first line that has `word` as a whole token.
pub fn line_with_token<'a>(screen: &'a str, word: &str) -> Option<&'a str> {
    screen.lines().find(|line| tokens(line).contains(&word))
}

/// Whether `line` contains `expected` as consecutive tokens.
pub fn has_run(line: &str, expected: &[&str]) -> bool {
    tokens(line)
        .windows(expected.len())
        .any(|window| window == expected)
}

/// 0-based `(column, row)` of the first occurrence of `needle` starting at or right of
/// column `min_col`, counting one cell per character (true for everything tuitree draws).
pub fn locate(screen: &str, needle: &str, min_col: usize) -> Option<(u16, u16)> {
    screen.lines().enumerate().find_map(|(row, line)| {
        let chars: Vec<char> = line.chars().collect();
        let tail: String = chars.iter().skip(min_col).collect();
        let at = tail.find(needle)?;
        let col = min_col + tail[..at].chars().count();
        Some((col as u16, row as u16))
    })
}

/// Tokens of the main pane part of a line (right of the projects pane), or of the whole line.
pub fn main_tokens(line: &str) -> Vec<&str> {
    tokens(line.split_once("││").map_or(line, |(_, main)| main))
}

/// The Branch column of a worktree table line: the first main-pane token after the `▶`
/// cursor and `●` mark.
pub fn branch_of(line: &str) -> Option<&str> {
    main_tokens(line)
        .into_iter()
        .find(|t| !matches!(*t, "▶" | "●"))
}

/// The table line for `key`: the row whose branch is `key`, else the first with token `key`.
pub fn row_line<'a>(screen: &'a str, key: &str) -> Option<&'a str> {
    screen
        .lines()
        .find(|l| branch_of(l) == Some(key) && main_tokens(l).len() > 2)
        .or_else(|| line_with_token(screen, key))
}

/// Whether the row identified by `key` (branch, or any token) is the selected (`▶`) one.
pub fn is_selected(screen: &str, key: &str) -> bool {
    row_line(screen, key).is_some_and(|l| main_tokens(l).first() == Some(&"▶"))
}

/// Presses `j`/`k` until the table row for `key` (branch, or any token) is selected.
pub fn select_row(app: &Tmux, header: &str, key: &str) -> bool {
    let screen = app.capture();
    let target = row_below(&screen, header, |line| branch_of(line) == Some(key))
        .or_else(|| row_below(&screen, header, |line| main_tokens(line).contains(&key)));
    let current = row_below(&screen, header, |line| {
        main_tokens(line).first() == Some(&"▶")
    });
    let (Some(target), Some(current)) = (target, current) else {
        return false;
    };
    let key_name = if target > current { "j" } else { "k" };
    app.keys(&vec![key_name; target.abs_diff(current)]);
    is_selected(&app.capture(), key)
}

/// Checks that the row identified by `key` contains `expected` as consecutive tokens.
pub fn check_row(artifacts: &mut Artifacts, screen: &str, key: &str, expected: &[&str]) -> bool {
    let row = row_line(screen, key);
    let passed = row.is_some_and(|row| has_run(row, expected));
    artifacts.check(
        format!("row `{key}` shows {}", expected.join(" ")),
        passed,
        format!("found: {}", row.map_or("<no such row>", str::trim)),
    )
}

/// Position (0 = first row) of the first table row matching `pred` below the header row that
/// has the token `header`.
pub fn row_below(screen: &str, header: &str, pred: impl Fn(&str) -> bool) -> Option<usize> {
    screen
        .lines()
        .skip_while(|line| !tokens(line).contains(&header))
        .skip(1)
        .position(pred)
}

/// Worktree rows show `↑ahead ↓behind`.
pub fn has_ahead_behind(line: &str) -> bool {
    let tokens = tokens(line);
    tokens
        .windows(2)
        .any(|w| w[0].starts_with('↑') && w[1].starts_with('↓'))
}

/// Number of rows under the detail pane's `Added Deleted File` header.
pub fn file_rows(screen: &str) -> usize {
    screen
        .lines()
        .skip_while(|line| !has_run(line, &["Added", "Deleted", "File"]))
        .skip(1)
        .filter(|line| {
            tokens(line)
                .windows(2)
                .any(|w| w[0] == "bin" || (w[0].starts_with('+') && w[1].starts_with('-')))
        })
        .count()
}

/// Number of PR rows (a `#<number>` token) on screen.
pub fn pr_rows(screen: &str) -> usize {
    screen
        .lines()
        .filter(|line| {
            tokens(line).iter().any(|t| {
                t.strip_prefix('#')
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            })
        })
        .count()
}

/// Refresh finished, including the sync check, and no job is queued or running.
pub fn settled(screen: &str) -> bool {
    screen.contains("fetched")
        && !["fetching", "updating", "deleting", "syncing"]
            .iter()
            .any(|busy| screen.contains(busy))
        && !screen.contains("jobs: ")
        && screen.lines().any(|l| tokens(l).contains(&"Sync"))
}

/// Whether the worktree table has a row (with stats or an error) for `branch`.
pub fn has_worktree_row(screen: &str, branch: &str) -> bool {
    screen
        .lines()
        .any(|l| branch_of(l) == Some(branch) && (has_ahead_behind(l) || l.contains('⚠')))
}

pub fn git_ok(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Whether the local branch exists in the repository at `dir`.
pub fn branch_exists(dir: &Path, branch: &str) -> bool {
    git_ok(
        dir,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/heads/{branch}"),
        ],
    )
}
