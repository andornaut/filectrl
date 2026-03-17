mod ls_colors;
mod serde;
pub mod theme;

use std::{convert::TryFrom, fs, io::ErrorKind, path::PathBuf};

use anyhow::{anyhow, Result};
use etcetera::{choose_base_strategy, BaseStrategy};
use log::{debug, info, LevelFilter};
use ::serde::Deserialize;

use self::theme::Theme;

const CONFIG_RELATIVE_PATH: &str = "filectrl/config.toml";
const DEFAULT_CONFIG: &str = include_str!("config/default_config.toml");

#[derive(Debug, Deserialize)]
pub struct FileSystemConfig {
    pub buffer_max_bytes: u64,
    pub buffer_min_bytes: u64,
    pub refresh_debounce_milliseconds: u64,
}

#[derive(Debug, Deserialize)]
pub struct Templates {
    pub open_current_directory: String,
    pub open_new_window: String,
    pub open_selected_file: String,
}

#[derive(Debug, Deserialize)]
struct OsTemplates {
    linux: Templates,
    macos: Templates,
}

#[derive(Debug, Deserialize)]
pub struct UiConfig {
    pub double_click_interval_milliseconds: u16,
}

#[derive(Deserialize)]
struct RawConfig {
    file_system: FileSystemConfig,
    log_level: LevelFilter,
    templates: OsTemplates,
    theme: Theme,
    theme256: Theme,
    ui: UiConfig,
}

pub struct Config {
    pub file_system: FileSystemConfig,
    pub log_level: LevelFilter,
    pub templates: Templates,
    pub theme: Theme,
    pub theme256: Theme,
    pub ui: UiConfig,
}

pub fn write_default_config() -> Result<()> {
    let path = Config::default_path()?;
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow!("Config path has no parent directory"))?,
    )?;
    fs::write(&path, DEFAULT_CONFIG)
        .map_err(|error| anyhow!("Cannot write configuration file to {path:?}: {error}"))?;
    info!("Wrote the default config to {path:?}");
    Ok(())
}

impl TryFrom<Option<PathBuf>> for Config {
    type Error = anyhow::Error;

    fn try_from(value: Option<PathBuf>) -> Result<Self> {
        // Try to use the user-provided path if available
        let Some(path) = value else {
            return Self::try_from_default_path();
        };

        debug!("Loading config from user-provided path: {}", path.display());
        match fs::read_to_string(&path) {
            Ok(content) => Self::parse(&content),
            Err(err) => Err(anyhow!(
                "Could not read config from user-supplied path ({}): {}",
                path.display(),
                err
            )),
        }
    }

}

impl Config {
    fn from_default_file() -> Result<Self> {
        Self::parse(DEFAULT_CONFIG)
    }

    fn default_path() -> Result<PathBuf> {
        Ok(choose_base_strategy()
            .map_err(|e| anyhow!("Cannot determine config directory: {e}"))?
            .config_dir()
            .join(CONFIG_RELATIVE_PATH))
    }

    fn parse(content: &str) -> Result<Self> {
        let raw = toml::from_str::<RawConfig>(content)
            .map_err(|error| anyhow!("Cannot parse config file content: {error}"))?;

        let templates = if cfg!(target_os = "macos") {
            raw.templates.macos
        } else {
            raw.templates.linux
        };

        let mut config = Config {
            file_system: raw.file_system,
            log_level: raw.log_level,
            templates,
            theme: raw.theme,
            theme256: raw.theme256,
            ui: raw.ui,
        };
        config.theme.maybe_apply_ls_colors();
        Ok(config)
    }

    fn try_from_default_path() -> Result<Self> {
        let default_path = Self::default_path()?;
        debug!(
            "Attempting to load the config from the default path: {}",
            default_path.display()
        );

        match fs::read_to_string(&default_path) {
            Ok(content) => Self::parse(&content),
            Err(err) if err.kind() == ErrorKind::NotFound => {
                debug!("No config file found, using the built-in config");
                Self::from_default_file()
            }
            Err(err) => Err(anyhow!(
                "Could not read the config file from the default path ({}): {}",
                default_path.display(),
                err
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses_successfully() {
        Config::from_default_file().unwrap();
    }
}
