pub mod app;
mod command;
mod file_system;
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
    include_paths: Vec<PathBuf>,
    initial_directory: Option<PathBuf>,
    colors_256: bool,
) -> Result<()> {
    // Configure logging with a default level before loading config, so that Info+ messages from the
    // config initialization are logged
    configure_logging();

    let mut config = Config::load(config_path, include_paths)?;
    apply_log_level(&config);

    let is_truecolor = supports_truecolor() && !colors_256;
    info!("Terminal truecolor support: {is_truecolor}");
    config.is_truecolor = is_truecolor;
    Config::init(config);

    let terminal = CleanupOnDropTerminal::try_new()?;
    App::new(terminal).run(initial_directory)
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
