//! Running external programs (`git`, `gh`) without ever touching the TUI's terminal.

use std::io::ErrorKind;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Why an external command did not produce output.
#[derive(Debug)]
pub enum CmdError {
    /// The program is not installed (not found on `PATH`).
    Missing,
    /// The program ran and failed, or could not be started; carries a readable reason.
    Failed(String),
}

/// Runs `program args...` inside `dir` and returns its stdout.
///
/// The child gets no stdin and runs in its own session, so it has no controlling terminal:
/// git, ssh and gh cannot prompt for passwords or passphrases and garble the UI.
pub fn run(program: &str, dir: &Path, args: &[&str]) -> Result<Vec<u8>, CmdError> {
    if !dir.is_dir() {
        return Err(CmdError::Failed(format!(
            "directory not found: {}",
            dir.display()
        )));
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1");
    // SAFETY: the closure runs between fork and exec and only calls setsid(2), which is
    // async-signal-safe and does not touch any memory shared with the parent.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let output = command.output().map_err(|err| match err.kind() {
        ErrorKind::NotFound => CmdError::Missing,
        _ => CmdError::Failed(format!("could not run {program}: {err}")),
    })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(CmdError::Failed(if stderr.is_empty() {
            format!("{program} {} failed ({})", args.join(" "), output.status)
        } else {
            stderr
        }))
    }
}
