mod app;
mod command;
mod file_system;
mod terminal;
mod views;

use std::path::PathBuf;

use crate::{
    app::App,
    terminal::{close_terminal, open_terminal},
};
use anyhow::Result;

pub fn run(directory: Option<PathBuf>) -> Result<()> {
    let mut terminal = open_terminal()?;

    App::default().run(&mut terminal, directory)?;

    Ok(close_terminal(&mut terminal)?)
}
