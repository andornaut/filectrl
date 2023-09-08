pub mod app;
mod command;
mod file_system;
mod views;

use crate::app::{config::Config, terminal::CleanupOnDropTerminal, App};
use anyhow::Result;
use env_logger::Env;
use std::path::PathBuf;

pub fn run(config_path: Option<PathBuf>, initial_directory: Option<PathBuf>) -> Result<()> {
    let config = Config::try_from(config_path)?;
    configure_logging(&config);
    let terminal = CleanupOnDropTerminal::try_new()?;
    App::new(config, terminal).run(initial_directory)
}

fn configure_logging(config: &Config) {
    let level = config.log_level.map_or("off", |level| level.as_str());
    env_logger::Builder::from_env(Env::default().default_filter_or(level))
        .format_timestamp(None)
        .init();
}
