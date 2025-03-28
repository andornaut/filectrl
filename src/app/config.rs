mod default_config;
mod ls_colors;
mod serialization;
pub mod theme;

use std::{fs, io::ErrorKind, path::PathBuf};

use anyhow::{anyhow, Error, Result};
use etcetera::{choose_base_strategy, BaseStrategy};
use log::LevelFilter;
use ls_colors::apply_ls_colors;
use serde::{Deserialize, Serialize};

use self::{default_config::DEFAULT_CONFIG_TOML, theme::Theme};

const CONFIG_RELATIVE_PATH: &'static str = "filectrl/config.toml";

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub apply_ls_colors: bool,
    pub double_click_threshold_milliseconds: Option<u16>,
    pub log_level: Option<LevelFilter>,
    pub open_current_directory_template: Option<String>,
    pub open_new_window_template: Option<String>,
    pub open_selected_file_template: Option<String>,
    pub theme: Theme,
}

impl Config {
    pub fn write_default_config() -> Result<()> {
        let config = Self::default();
        let content = toml::to_string_pretty(&config)?;
        let path = Self::default_path();

        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, &content)
            .map_err(|error| anyhow!("Cannot write configuration file to {path:?}: {error}"))?;
        println!("Wrote the default config to {path:?}");
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

        if config.apply_ls_colors {
            apply_ls_colors(&mut config.theme.files);
        }

        Ok(config)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::parse(DEFAULT_CONFIG_TOML).expect("Default configuration should be valid")
    }
}

impl TryFrom<Option<PathBuf>> for Config {
    type Error = Error;

    fn try_from(value: Option<PathBuf>) -> Result<Self> {
        // Try to use the user-provided path
        if let Some(path) = value {
            return match fs::read_to_string(&path) {
                Ok(content) => Self::parse(&content),
                Err(err) => Err(anyhow!(
                    "Could not read config from user-supplied path ({}): {}",
                    path.display(),
                    err
                )),
            };
        }

        // No user-provided path provided, so try the default path
        let default_path = Self::default_path();
        match fs::read_to_string(&default_path) {
            Ok(content) => Self::parse(&content),
            Err(err) => {
                if err.kind() == ErrorKind::NotFound {
                    // Fallback to the built-in config
                    return Ok(Self::default());
                }
                Err(anyhow!(
                    "could not read config from the default path ({}): {}",
                    default_path.display(),
                    err
                ))
            }
        }
    }
}
