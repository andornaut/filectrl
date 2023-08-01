mod app;
mod file_system;
mod terminal;
mod views;

use crate::{
    app::App,
    terminal::{close_terminal, open_terminal},
};
use anyhow::Result;

pub fn run() -> Result<()> {
    let mut terminal = open_terminal()?;

    App::default().run(&mut terminal)?;

    Ok(close_terminal(&mut terminal)?)
}
