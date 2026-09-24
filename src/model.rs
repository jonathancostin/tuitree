//! Data shared by the git and GitHub sides of the app.

/// Line changes for one file. `None` counts mean the file is binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub path: String,
    pub added: Option<u64>,
    pub deleted: Option<u64>,
    /// The file is untracked in the worktree (not in git yet).
    pub untracked: bool,
}

/// Summed line changes over a set of files; binary files count as files but not as lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub files: usize,
    pub added: u64,
    pub deleted: u64,
}

impl Totals {
    pub fn of(files: &[FileStat]) -> Self {
        files.iter().fold(
            Totals {
                files: files.len(),
                ..Totals::default()
            },
            |acc, file| Totals {
                added: acc.added + file.added.unwrap_or(0),
                deleted: acc.deleted + file.deleted.unwrap_or(0),
                ..acc
            },
        )
    }
}
