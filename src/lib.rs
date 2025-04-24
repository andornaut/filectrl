pub mod app;
mod clipboard;
mod command;
mod file_system;
mod unicode;
mod views;

use std::{env, io::Write, path::PathBuf};

use anyhow::Result;
use env_logger::{Builder, Env, DEFAULT_FILTER_ENV};
use log::{info, LevelFilter};

use self::app::{config::Config, terminal::CleanupOnDropTerminal, App};

const PKG_NAME: Option<&str> = option_env!("CARGO_PKG_NAME");

pub fn run(config_path: Option<PathBuf>, initial_directory: Option<PathBuf>) -> Result<()> {
    // Configure logging with a default level before loading config, so that messages from the
    // config initialization are logged
    configure_logging();

    let config = Config::try_from(config_path)?;

    if let Ok(level) = env::var(DEFAULT_FILTER_ENV) {
        info!("Using log level from environment variable: {DEFAULT_FILTER_ENV}={level}");
    } else {
        let level = config.log_level.expect("log_level is required");
        log::set_max_level(level);
        info!("Changed log level to {level:?}");
    }

    let terminal = CleanupOnDropTerminal::try_new()?;
    App::new(config, terminal).run_once(initial_directory)
}

fn configure_logging() {
    let pkg_name = PKG_NAME.unwrap_or("filectrl");
    let prefix = format!("{pkg_name}::");

    Builder::from_env(Env::default().default_filter_or(LevelFilter::Info.as_str()))
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
