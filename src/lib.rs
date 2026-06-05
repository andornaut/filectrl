pub mod app;
mod command;
mod file_system;
mod views;

use std::{
    env,
    io::{IsTerminal, Write},
    path::PathBuf,
};

use anyhow::{Context, Result, anyhow};
use env_logger::{Builder, DEFAULT_FILTER_ENV, Env};
use log::{LevelFilter, info};

use self::app::{
    App,
    config::Config,
    events::install_signal_handlers,
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

    // Validate the initial directory before entering raw mode so an invalid
    // positional argument fails fast with a clean stderr message and a nonzero
    // exit code, rather than silently opening the TUI in the current directory.
    let initial_directory = initial_directory
        .map(validate_initial_directory)
        .transpose()?;

    let mut config = Config::load(config_path, include_paths)?;
    apply_log_level(&config);

    let is_truecolor = supports_truecolor() && !colors_256;
    info!("Terminal truecolor support: {is_truecolor}");
    config.is_truecolor = is_truecolor;
    Config::init(config);

    // Install signal handlers before entering raw mode so that SIGTERM/SIGHUP
    // cause a graceful shutdown (terminal restored) rather than leaving the
    // shell in a broken state.
    install_signal_handlers()?;

    let terminal = CleanupOnDropTerminal::try_new()?;
    App::new(terminal).run(initial_directory)
}

pub fn print_keybindings(config_path: Option<PathBuf>, include_paths: Vec<PathBuf>) -> Result<()> {
    configure_logging();
    let config = Config::load(config_path, include_paths)?;
    let bold = std::io::stdout().is_terminal();
    print!(
        "{}",
        views::keybindings_help_text(&config.keybindings, bold)
    );
    Ok(())
}

fn validate_initial_directory(path: PathBuf) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("Cannot open {}", path.display()))?;
    if !canonical.is_dir() {
        return Err(anyhow!("Not a directory: {}", canonical.display()));
    }
    Ok(canonical)
}

fn apply_log_level(config: &Config) {
    if let Ok(level) = env::var(DEFAULT_FILTER_ENV) {
        // RUST_LOG is set; env_logger already applied it in configure_logging()
        info!("Log level set from environment variable: {DEFAULT_FILTER_ENV}={level}");
    } else {
        // No env override; apply the level from the config file
        let level = config.log_level;
        log::set_max_level(level);
        info!("Log level set from config: {level:?}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_initial_directory_accepts_a_directory() {
        let dir = env::temp_dir();
        let result = validate_initial_directory(dir.clone()).unwrap();
        assert_eq!(result, dir.canonicalize().unwrap());
    }

    #[test]
    fn validate_initial_directory_rejects_a_nonexistent_path() {
        let path = env::temp_dir().join("filectrl-does-not-exist-xyz");
        assert!(validate_initial_directory(path).is_err());
    }

    #[test]
    fn validate_initial_directory_rejects_a_regular_file() {
        let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let error = validate_initial_directory(file).unwrap_err();
        assert!(error.to_string().starts_with("Not a directory:"));
    }
}
