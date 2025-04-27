mod default_config;
mod ls_colors;
mod serialization;
pub mod theme;

use std::{fs, io::ErrorKind, path::PathBuf};

use anyhow::{anyhow, Error, Result};
use etcetera::{choose_base_strategy, BaseStrategy};
use log::{debug, info, LevelFilter};
use serde::{Deserialize, Serialize};

use self::{default_config::DEFAULT_CONFIG_TOML, ls_colors::apply_ls_colors, theme::Theme};

const CONFIG_RELATIVE_PATH: &str = "filectrl/config.toml";

#[derive(Debug, Deserialize, Serialize)]
pub struct FileSystemConfig {
    pub buffer_max_bytes: u64,
    pub buffer_min_bytes: u64,
    pub update_threshold_milliseconds: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Templates {
    pub open_current_directory: String,
    pub open_new_window: String,
    pub open_selected_file: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UiConfig {
    pub double_click_threshold_milliseconds: u16,
    pub tick_rate_milliseconds: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub file_system: FileSystemConfig,
    pub log_level: LevelFilter,
    pub templates: Templates,
    pub theme: Theme,
    pub ui: UiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self::parse(DEFAULT_CONFIG_TOML).expect("Default configuration should be valid")
    }
}

impl TryFrom<Option<PathBuf>> for Config {
    type Error = Error;

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
    pub fn write_default_config() -> Result<()> {
        let config = Self::default();
        let content = toml::to_string_pretty(&config)?;
        let path = Self::default_path();

        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, &content)
            .map_err(|error| anyhow!("Cannot write configuration file to {path:?}: {error}"))?;
        info!("Wrote the default config to {path:?}");
        Ok(())
    }

    fn default_path() -> PathBuf {
        choose_base_strategy()
            .unwrap()
            .config_dir()
            .join(CONFIG_RELATIVE_PATH)
    }

    fn parse(content: &str) -> Result<Self> {
        // Parse the TOML directly
        let mut config = toml::from_str::<Config>(content)
            .map_err(|error| anyhow!("Cannot parse config file content: {error}"))?;

        if config.theme.file_types.ls_colors_take_precedence {
            apply_ls_colors(&mut config.theme.file_types);
        }

        Ok(config)
    }

    fn try_from_default_path() -> Result<Self> {
        let default_path = Self::default_path();
        debug!(
            "Attempting to load the config from the default path: {}",
            default_path.display()
        );

        match fs::read_to_string(&default_path) {
            Ok(content) => Self::parse(&content),
            Err(err) if err.kind() == ErrorKind::NotFound => {
                debug!("No config file found, using the built-in config");
                Ok(Self::default())
            }
            Err(err) => Err(anyhow!(
                "Could not read the config file from the default path ({}): {}",
                default_path.display(),
                err
            )),
        }
    }
}
