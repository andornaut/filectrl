mod app;
mod component;
mod file_system;
mod terminal;
mod view;

use crate::{
    app::App,
    terminal::{start_terminal, stop_terminal},
};
use anyhow::Result;

pub fn run() -> Result<()> {
    let mut terminal = start_terminal()?;

    App::default().run(&mut terminal)?;

    Ok(stop_terminal(&mut terminal)?)
}
