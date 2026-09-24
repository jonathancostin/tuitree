//! tuitree: a small LazyGit-style dashboard for git worktrees and GitHub pull requests.

mod app;
mod cmd;
mod config;
mod gh;
mod git;
mod model;
mod ui;
mod worker;

use std::io::stdout;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind,
};
use ratatui::crossterm::execute;

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

    // `ratatui::init` installs a panic hook that restores the terminal; ours runs first and
    // also turns mouse reporting off, so a panic leaves a usable shell behind.
    let mut terminal = ratatui::init();
    let restore_terminal = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableMouseCapture);
        restore_terminal(info);
    }));
    let result = execute!(stdout(), EnableMouseCapture)
        .map_err(anyhow::Error::from)
        .and_then(|()| run(&mut terminal, &mut app, &rx));
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, app: &mut App, updates: &Receiver<Update>) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, app))?;
        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                Event::Mouse(mouse) => app.on_mouse(mouse),
                _ => {}
            }
        }
        for update in updates.try_iter() {
            app.on_update(update);
        }
    }
    Ok(())
}
