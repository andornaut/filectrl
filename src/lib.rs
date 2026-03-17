pub mod app;
mod clipboard;
mod command;
mod file_system;
mod unicode;
mod views;

use std::{env, io::Write, path::PathBuf};

use anyhow::Result;
use env_logger::{Builder, DEFAULT_FILTER_ENV, Env};
use log::{LevelFilter, info};

use self::app::{
    App,
    config::Config,
    terminal::{CleanupOnDropTerminal, supports_truecolor},
};

const PKG_NAME: &str = env!("CARGO_PKG_NAME");

pub fn run(
    config_path: Option<PathBuf>,
    initial_directory: Option<PathBuf>,
    colors_256: bool,
) -> Result<()> {
    // Configure logging with a default level before loading config, so that Info+ messages from the
    // config initialization are logged
    configure_logging();

    let config = config_path.try_into()?;
    apply_log_level(&config);

    let has_truecolor = supports_truecolor() && !colors_256;
    info!("Terminal truecolor support: {}", has_truecolor);
    if !has_truecolor {
        info!("Using 256-color theme fallback");
    }

    let terminal = CleanupOnDropTerminal::try_new()?;
    App::new(config, terminal, has_truecolor).run_once(initial_directory)
}

fn apply_log_level(config: &Config) {
    if let Ok(level) = env::var(DEFAULT_FILTER_ENV) {
        // RUST_LOG is set; env_logger already applied it in configure_logging()
        info!("Log level set from environment variable: {DEFAULT_FILTER_ENV}={level}");
    } else {
        // No env override; apply the level from the config file
        let level = config.log_level;
        info!("Log level set from config: {level:?}");
        log::set_max_level(level);
    }
}

fn configure_logging() {
    let prefix = format!("{PKG_NAME}::");

    // Set the log level to the value of $RUST_LOG or default to Info
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
