mod ls_colors;
mod serde;
pub mod theme;

use std::{convert::TryFrom, fs, io::ErrorKind, path::PathBuf};

use ::serde::Deserialize;
use anyhow::{Result, anyhow};
use directories::ProjectDirs;
use log::{LevelFilter, debug, info};

use self::theme::Theme;

const CONFIG_RELATIVE_PATH: &str = "config.toml";
const DEFAULT_CONFIG: &str = include_str!("config/default_config.toml");
const DEFAULT_THEME: &str = include_str!("config/default_theme.toml");
const DEFAULT_THEME256: &str = include_str!("config/default_theme256.toml");
const DEFAULT_THEME_FILENAME: &str = "default-theme.toml";
const DEFAULT_THEME256_FILENAME: &str = "default-theme-256.toml";

#[derive(Debug, Deserialize)]
pub struct FileSystemConfig {
    pub buffer_max_bytes: u64,
    pub buffer_min_bytes: u64,
    pub refresh_debounce_milliseconds: u64,
}

#[derive(Debug, Deserialize)]
pub struct Openers {
    pub open_current_directory: String,
    pub open_new_window: String,
    pub open_selected_file: String,
}

#[derive(Debug, Deserialize)]
struct PlatformOpeners {
    linux: Openers,
    macos: Openers,
}

#[derive(Debug, Deserialize)]
pub struct UiConfig {
    pub double_click_interval_milliseconds: u16,
}

#[derive(Deserialize)]
struct RawConfig {
    file_system: FileSystemConfig,
    log_level: LevelFilter,
    openers: PlatformOpeners,
    theme: Theme,
    theme_file: Option<String>,
    theme256: Theme,
    theme256_file: Option<String>,
    ui: UiConfig,
}

pub struct Config {
    pub file_system: FileSystemConfig,
    pub log_level: LevelFilter,
    pub openers: Openers,
    pub theme: Theme,
    pub theme256: Theme,
    pub ui: UiConfig,
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
            Ok(content) => Self::parse(&content, path.parent().map(|p| p.to_path_buf())),
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
        Self::parse(DEFAULT_CONFIG, None)
    }

    fn default_config_dir() -> Result<PathBuf> {
        Ok(ProjectDirs::from("", "", "filectrl")
            .ok_or_else(|| anyhow!("Cannot determine config directory"))?
            .config_dir()
            .to_path_buf())
    }

    fn default_path() -> Result<PathBuf> {
        Ok(Self::default_config_dir()?.join(CONFIG_RELATIVE_PATH))
    }

    pub fn write_default() -> Result<()> {
        let path = Self::default_path()?;
        fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow!("Config path has no parent directory"))?,
        )?;
        fs::write(&path, DEFAULT_CONFIG)
            .map_err(|error| anyhow!("Cannot write configuration file to {path:?}: {error}"))?;
        info!("Wrote the default config to {path:?}");
        Ok(())
    }

    pub fn write_default_themes() -> Result<()> {
        let dir = Self::default_config_dir()?;
        fs::create_dir_all(&dir)?;

        let theme_path = dir.join(DEFAULT_THEME_FILENAME);
        fs::write(&theme_path, DEFAULT_THEME)
            .map_err(|error| anyhow!("Cannot write theme file to {theme_path:?}: {error}"))?;
        info!("Wrote the default theme to {theme_path:?}");

        let theme256_path = dir.join(DEFAULT_THEME256_FILENAME);
        fs::write(&theme256_path, DEFAULT_THEME256).map_err(|error| {
            anyhow!("Cannot write 256-color theme file to {theme256_path:?}: {error}")
        })?;
        info!("Wrote the default 256-color theme to {theme256_path:?}");
        Ok(())
    }

    fn load_external_theme(theme_file: &str, config_dir: &PathBuf) -> Result<Theme> {
        let path = PathBuf::from(theme_file);
        let resolved = if path.is_absolute() {
            path
        } else {
            config_dir.join(path)
        };
        debug!("Loading external theme from: {}", resolved.display());
        let content = fs::read_to_string(&resolved).map_err(|error| {
            anyhow!(
                "Cannot read theme file ({}): {}",
                resolved.display(),
                error
            )
        })?;
        toml::from_str::<Theme>(&content).map_err(|error| {
            anyhow!(
                "Cannot parse theme file ({}): {}",
                resolved.display(),
                error
            )
        })
    }

    fn parse(content: &str, config_dir: Option<PathBuf>) -> Result<Self> {
        let raw = toml::from_str::<RawConfig>(content)
            .map_err(|error| anyhow!("Cannot parse config file content: {error}"))?;

        let openers = if cfg!(target_os = "macos") {
            raw.openers.macos
        } else {
            raw.openers.linux
        };

        let resolve_dir = || {
            config_dir
                .clone()
                .or_else(|| Self::default_config_dir().ok())
                .unwrap_or_default()
        };

        let theme = if let Some(ref theme_file) = raw.theme_file {
            Self::load_external_theme(theme_file, &resolve_dir())?
        } else {
            raw.theme
        };

        let theme256 = if let Some(ref theme256_file) = raw.theme256_file {
            Self::load_external_theme(theme256_file, &resolve_dir())?
        } else {
            raw.theme256
        };

        let mut config = Config {
            file_system: raw.file_system,
            log_level: raw.log_level,
            openers,
            theme,
            theme256,
            ui: raw.ui,
        };
        config.theme.file_type.maybe_apply_ls_colors(false);
        config.theme256.file_type.maybe_apply_ls_colors(true);
        Ok(config)
    }

    fn try_from_default_path() -> Result<Self> {
        let default_path = Self::default_path()?;
        debug!(
            "Attempting to load the config from the default path: {}",
            default_path.display()
        );

        match fs::read_to_string(&default_path) {
            Ok(content) => Self::parse(&content, default_path.parent().map(|p| p.to_path_buf())),
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

    #[test]
    fn default_theme_parses_successfully() {
        toml::from_str::<Theme>(DEFAULT_THEME).unwrap();
    }

    #[test]
    fn default_theme256_parses_successfully() {
        toml::from_str::<Theme>(DEFAULT_THEME256).unwrap();
    }
}
