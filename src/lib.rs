pub mod app;
mod command;
mod file_system;
#[cfg(test)]
mod test_support;
mod views;

use std::{
    env,
    io::{IsTerminal, Write, stdout},
    path::PathBuf,
};

use anyhow::{Context, Result, anyhow};
use env_logger::{Builder, DEFAULT_FILTER_ENV, Env};
use log::{LevelFilter, info};

use self::app::{
    App,
    config::{Config, RuntimeEnv},
    events::install_signal_handlers,
    terminal::{CleanupOnDropTerminal, supports_truecolor},
};

const MODULE_PREFIX: &str = concat!(env!("CARGO_PKG_NAME"), "::");

pub fn run(
    config_path: Option<PathBuf>,
    include_paths: Vec<PathBuf>,
    initial_directory: Option<PathBuf>,
    no_truecolor: bool,
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

    let is_truecolor = supports_truecolor() && !no_truecolor;
    let ls_colors = env::var("LS_COLORS").ok();
    let env = RuntimeEnv {
        is_truecolor,
        ls_colors: ls_colors.as_deref(),
    };

    let config = Config::load(env, config_path, include_paths)?;
    apply_log_level(&config);
    info!("Terminal truecolor support: {is_truecolor}");
    Config::init(config);

    // Install signal handlers before entering raw mode so that SIGTERM/SIGHUP
    // cause a graceful shutdown (terminal restored) rather than leaving the
    // shell in a broken state.
    install_signal_handlers()?;

    // Checked after everything the user typed has been validated, so that a bad
    // argument or config is reported before the environment is. Crossterm opens
    // the controlling terminal itself, so without this a redirected stdout
    // surfaces as a bare ENXIO that names nothing.
    if !stdout().is_terminal() {
        return Err(anyhow!("Cannot start: standard output is not a terminal"));
    }
    let terminal = CleanupOnDropTerminal::try_new().context("Failed to initialize the terminal")?;
    App::new(terminal).run(initial_directory)
}

pub fn print_keybindings(config_path: Option<PathBuf>, include_paths: Vec<PathBuf>) -> Result<()> {
    configure_logging();
    let config = Config::load(RuntimeEnv::default(), config_path, include_paths)?;
    let bold = std::io::stdout().is_terminal();
    print!(
        "{}",
        views::keybindings_help_text(&config.keybindings, bold)
    );
    Ok(())
}

fn validate_initial_directory(path: PathBuf) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(anyhow!("Cannot open an empty path"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| anyhow!("Failed to open {}: {error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(anyhow!(
            "Cannot open {}: not a directory",
            canonical.display()
        ));
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
    // When $RUST_LOG is unset, set env_logger's internal filter to the most
    // permissive level so that the level can later be raised above Info from the
    // config file. env_logger's internal filter is fixed at init() and cannot be
    // changed afterward, so gating is done solely through log::set_max_level().
    // When $RUST_LOG is set, it takes precedence and env_logger applies it.
    Builder::from_env(Env::default().default_filter_or(LevelFilter::Trace.as_str()))
        .format(|buf, record| {
            let path = record.module_path().unwrap_or_default();
            writeln!(
                buf,
                "[{} {}:{}] {}",
                record.level(),
                path.strip_prefix(MODULE_PREFIX).unwrap_or(path),
                record.line().unwrap_or_default(),
                record.args()
            )
        })
        .init();

    // Gate to Info for the pre-config phase so verbose internal messages don't
    // appear before the configured level is applied by apply_log_level(). When
    // $RUST_LOG is set, leave the level env_logger derived from it in place.
    if env::var(DEFAULT_FILTER_ENV).is_err() {
        log::set_max_level(LevelFilter::Info);
    }
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

    /// An attempt that the OS refused, so the message carries its cause.
    #[test]
    fn validate_initial_directory_rejects_a_nonexistent_path() {
        let path = env::temp_dir().join("filectrl-does-not-exist-xyz");
        let error = validate_initial_directory(path).unwrap_err().to_string();
        assert!(error.starts_with("Failed to open "), "{error}");
    }

    /// Refused by filectrl rather than by the OS, so the message gives its own
    /// reason instead of an errno.
    #[test]
    fn validate_initial_directory_rejects_a_regular_file() {
        let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let error = validate_initial_directory(file).unwrap_err().to_string();
        assert!(error.starts_with("Cannot open "), "{error}");
        assert!(error.ends_with(": not a directory"), "{error}");
    }

    /// An empty positional would otherwise reach `canonicalize` and be reported
    /// against a path that renders as nothing at all.
    #[test]
    fn validate_initial_directory_rejects_an_empty_path() {
        let error = validate_initial_directory(PathBuf::new())
            .unwrap_err()
            .to_string();
        assert_eq!("Cannot open an empty path", error);
    }
}
