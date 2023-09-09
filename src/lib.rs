pub mod app;
mod command;
mod file_system;
mod views;

use crate::app::{config::Config, terminal::CleanupOnDropTerminal, App};
use anyhow::Result;
use env_logger::Env;
use std::{io::Write, path::PathBuf};

const PKG_NAME: Option<&str> = option_env!("CARGO_PKG_NAME");

pub fn run(config_path: Option<PathBuf>, initial_directory: Option<PathBuf>) -> Result<()> {
    let config = Config::try_from(config_path)?;
    configure_logging(&config);
    let terminal = CleanupOnDropTerminal::try_new()?;
    App::new(config, terminal).run(initial_directory)
}

fn configure_logging(config: &Config) {
    let level = config.log_level.map_or("off", |level| level.as_str());
    let pkg_name = PKG_NAME.unwrap_or("unknown");
    let prefix = format!("{pkg_name}::");
    env_logger::Builder::from_env(Env::default().default_filter_or(level))
        // Include line number
        // Exclude the pkg_name prefix
        .format(move |buf, record| {
            let path = record.module_path().unwrap_or_default();
            writeln!(
                buf,
                "[{} {}:{}] {}",
                record.level(),
                path.strip_prefix(&prefix).unwrap_or(path),
                record.line().unwrap_or_default(),
                record.args()
            )
        })
        .init();
}
