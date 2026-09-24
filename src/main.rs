//! tuitree: a small LazyGit-style dashboard for git worktrees and GitHub pull requests.

mod app;
mod cmd;
mod config;
mod gh;
mod git;
mod model;
mod ui;
mod worker;

use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::app::App;
use crate::config::Config;
use crate::worker::Update;

/// How long to wait for input before redrawing (keeps the spinner moving and picks up results).
const TICK: Duration = Duration::from_millis(100);

fn main() -> Result<()> {
    let config_path = config::config_path()?;
    let config = Config::load(&config_path)?;
    let (tx, rx) = mpsc::channel();
    let mut app = App::new(config_path, config, tx);
    // `ratatui::run` restores the terminal on exit, and its panic hook restores it on panic.
    ratatui::run(|terminal| run(terminal, &mut app, &rx))
}

fn run(terminal: &mut DefaultTerminal, app: &mut App, updates: &Receiver<Update>) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, app))?;
        if event::poll(TICK)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.on_key(key);
        }
        for update in updates.try_iter() {
            app.on_update(update);
        }
    }
    Ok(())
}
