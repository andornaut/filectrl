mod ls_colors;
mod serialization;
pub mod theme;

use std::{fs, io::ErrorKind, path::PathBuf};

use anyhow::{anyhow, Result};
use etcetera::{choose_base_strategy, BaseStrategy};
use log::{debug, info, LevelFilter};
use serde::{Deserialize, Serialize};

use self::theme::Theme;

const CONFIG_RELATIVE_PATH: &str = "filectrl/config.toml";
const DEFAULT_CONFIG_PATH: &str = "src/app/config/default_config.toml";

#[derive(Debug, Deserialize, Serialize)]
pub struct FileSystemConfig {
    pub buffer_max_bytes: u64,
    pub buffer_min_bytes: u64,
    pub refresh_debounce_milliseconds: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Templates {
    pub open_current_directory: String,
    pub open_new_window: String,
    pub open_selected_file: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UiConfig {
    pub double_click_interval_milliseconds: u16,
    pub frame_delay_milliseconds: u64,
}

#[derive(Deserialize, Serialize)]
pub struct Config {
    pub file_system: FileSystemConfig,
    pub log_level: LevelFilter,
    pub templates: Templates,
    pub theme: Theme,
    pub theme256: Theme,
    pub ui: UiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self::from_default_file().expect("Default configuration file should be valid")
    }
}

impl Config {
    pub fn try_from(value: Option<PathBuf>) -> Result<Self> {
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

    fn from_default_file() -> Result<Self> {
        let content = fs::read_to_string(DEFAULT_CONFIG_PATH)
            .map_err(|e| anyhow!("Could not read default config file: {e}"))?;
        Self::parse(&content)
    }

    pub fn write_default_config() -> Result<()> {
        let path = Self::default_path();
        fs::create_dir_all(path.parent().unwrap())?;
        fs::copy(DEFAULT_CONFIG_PATH, &path)
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

        config.theme.maybe_apply_ls_colors();
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
