pub mod app;
mod command;
mod file_system;
mod utf8;
mod views;

use std::{io::Write, path::PathBuf};

use anyhow::Result;
use env_logger::Env;

use self::app::{config::Config, terminal::CleanupOnDropTerminal, App};

const DEFAULT_LOG_LEVEL: &str = "info";
const PKG_NAME: Option<&str> = option_env!("CARGO_PKG_NAME");

pub fn run(config_path: Option<PathBuf>, initial_directory: Option<PathBuf>) -> Result<()> {
    // Configure logging with a default level before loading config
    configure_logging();

    let config = Config::try_from(config_path)?;
    // Update the log level from config
    if let Some(level) = config.log_level {
        log::set_max_level(level);
    }

    let terminal = CleanupOnDropTerminal::try_new()?;
    App::new(config, terminal).run(initial_directory)
}

fn configure_logging() {
    let pkg_name = PKG_NAME.unwrap_or("filectrl");
    let prefix = format!("{pkg_name}::");
    let mut builder =
        env_logger::Builder::from_env(Env::default().default_filter_or(DEFAULT_LOG_LEVEL));
    builder.format(move |buf, record| {
        let path = record.module_path().unwrap_or_default();
        writeln!(
            buf,
            "[{} {}:{}] {}",
            record.level(),
            path.strip_prefix(&prefix).unwrap_or(path),
            record.line().unwrap_or_default(),
            record.args()
        )
    });
    builder.init();
}
