use super::{default_config::DEFAULT_CONFIG_TOML, theme::Theme};
use anyhow::{anyhow, Error, Result};
use etcetera::{choose_base_strategy, BaseStrategy};
use serde::{Deserialize, Serialize};
use std::{fs, io::ErrorKind, path::PathBuf};

const CONFIG_RELATIVE_PATH: &'static str = "filectrl/config.toml";

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub open_current_directory_template: Option<String>,
    pub open_selected_file_template: Option<String>,
    pub theme: Theme,
}

impl Config {
    pub fn write_default_config() -> Result<()> {
        let config = Config::default();
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
        toml::from_str::<Config>(content)
            .map_err(|error| anyhow!("Cannot parse config file: {error}"))
    }

    fn read_default_path() -> Result<Config> {
        let path = Self::default_path();
        match fs::read_to_string(&path) {
            Err(error) => match error.kind() {
                ErrorKind::NotFound => Ok(Config::default()),
                _ => Err(error.into()),
            },
            Ok(content) => Self::parse(&content),
        }
    }

    fn read_user_path(path: PathBuf) -> Result<Self> {
        let content = fs::read_to_string(&path)
            .map_err(|error| anyhow!("Cannot read config file at: {path:?}: {error}"))?;
        Self::parse(&content)
    }
}

impl TryFrom<Option<PathBuf>> for Config {
    type Error = Error;

    fn try_from(value: Option<PathBuf>) -> Result<Self> {
        match value {
            Some(path) => Self::read_user_path(path),
            None => Self::read_default_path(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        toml::from_str::<Self>(DEFAULT_CONFIG_TOML).unwrap()
    }
}
