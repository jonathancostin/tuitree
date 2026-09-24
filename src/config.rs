//! The persisted project list in `$XDG_CONFIG_HOME/tuitree/config.toml`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub projects: Vec<ProjectEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub path: PathBuf,
}

impl Config {
    /// Loads the config; a missing file is an empty config.
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Writes the config atomically (temp file + rename), creating the directory if needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path
            .parent()
            .context("config path has no parent directory")?;
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let text = toml::to_string_pretty(self).context("cannot serialize config")?;
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text).with_context(|| format!("cannot write {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))
    }
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// `$XDG_CONFIG_HOME/tuitree/config.toml`, or `~/.config/tuitree/config.toml` when unset.
pub fn config_path() -> Result<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute());
    let base = match (xdg, home_dir()) {
        (Some(xdg), _) => xdg,
        (None, Some(home)) => home.join(".config"),
        (None, None) => bail!("neither XDG_CONFIG_HOME nor HOME is set"),
    };
    Ok(base.join("tuitree").join("config.toml"))
}

/// Expands a leading `~` or `~/` to the home directory.
pub fn expand_tilde(input: &str, home: Option<&Path>) -> PathBuf {
    match (input, home) {
        ("~", Some(home)) => home.to_path_buf(),
        (_, Some(home)) if input.starts_with("~/") => home.join(&input[2..]),
        _ => PathBuf::from(input),
    }
}

/// Shortens paths under the home directory to `~/...` for display.
pub fn display_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
