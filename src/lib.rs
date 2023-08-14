pub mod app;
mod command;
mod file_system;
mod views;

use crate::app::App;
use anyhow::Result;
use app::{config::Config, terminal::CleanupOnDropTerminal};
use std::path::PathBuf;

pub fn run(config_path: Option<PathBuf>, directory: Option<PathBuf>) -> Result<()> {
    let config = Config::try_from(config_path)?;
    let terminal = CleanupOnDropTerminal::try_new()?;
    App::new(config, terminal).run(directory)
}
