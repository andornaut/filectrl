pub mod keybindings;
mod ls_colors;
mod serde;
pub mod theme;

use std::{fs, io::ErrorKind, path::PathBuf, sync::OnceLock};

use ::serde::Deserialize;
use anyhow::{Result, anyhow};
use directories::ProjectDirs;
use log::{LevelFilter, debug, info};
use toml::Value;

use self::theme::Theme;
use self::keybindings::{KeyBindings, TomlKeybindings};

static CONFIG: OnceLock<Config> = OnceLock::new();

const CONFIG_RELATIVE_PATH: &str = "config.toml";
const DEFAULT_CONFIG_BASE: &str = include_str!("config/default_config.toml");
const DEFAULT_THEME: &str = include_str!("config/default_theme.toml");
const DEFAULT_THEME256: &str = include_str!("config/default_theme256.toml");
const DEFAULT_THEME_FILENAME: &str = "theme.toml";
const DEFAULT_THEME256_FILENAME: &str = "theme256.toml";

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
    #[serde(default)]
    #[allow(dead_code)] // Consumed from raw Value before deserialization
    include_files: Vec<String>,
    keybindings: TomlKeybindings,
    log_level: LevelFilter,
    openers: PlatformOpeners,
    theme: Theme,
    theme256: Theme,
    ui: UiConfig,
}

pub struct Config {
    pub file_system: FileSystemConfig,
    pub is_truecolor: bool,
    pub keybindings: KeyBindings,
    pub log_level: LevelFilter,
    pub openers: Openers,
    pub theme: Theme,
    pub theme256: Theme,
    pub ui: UiConfig,
}

impl Config {
    pub fn init(config: Config) {
        let _ = CONFIG.set(config);
    }

    pub fn global() -> &'static Config {
        CONFIG.get().expect("config should be initialized")
    }

    pub fn theme(&self) -> &Theme {
        if self.is_truecolor {
            &self.theme
        } else {
            &self.theme256
        }
    }
}

impl Config {
    pub fn load(config_path: Option<PathBuf>, include_paths: Vec<PathBuf>) -> Result<Self> {
        let Some(path) = config_path else {
            return Self::try_from_default_path(include_paths);
        };

        debug!("Loading config from user-provided path: {}", path.display());
        match fs::read_to_string(&path) {
            Ok(content) => Self::parse(
                &content,
                path.parent().map(|p| p.to_path_buf()),
                &include_paths,
            ),
            Err(err) => Err(anyhow!(
                "Could not read config from user-supplied path ({}): {}",
                path.display(),
                err
            )),
        }
    }
}

impl Config {
    fn from_default_file(include_paths: &[PathBuf]) -> Result<Self> {
        let mut merged = merge_default_config()?;
        merged = merge_include_paths(merged, include_paths)?;
        Self::parse_value(merged)
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
        let merged = merge_default_config()?;
        let content = toml::to_string_pretty(&merged)
            .map_err(|error| anyhow!("Cannot serialize default config: {error}"))?;
        fs::write(&path, content)
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

    fn parse(
        content: &str,
        config_dir: Option<PathBuf>,
        include_paths: &[PathBuf],
    ) -> Result<Self> {
        let mut value = parse_toml(content)?;

        // Process include_files: merge each include on top of the base config
        let include_files = value
            .get("include_files")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if !include_files.is_empty() {
            let resolve_dir = config_dir
                .clone()
                .or_else(|| Self::default_config_dir().ok())
                .unwrap_or_default();

            for include_file in &include_files {
                let path = PathBuf::from(include_file);
                let resolved = if path.is_absolute() {
                    path
                } else {
                    resolve_dir.join(path)
                };
                debug!("Loading include file: {}", resolved.display());
                let include_content = fs::read_to_string(&resolved).map_err(|error| {
                    anyhow!(
                        "Cannot read include file ({}): {}",
                        resolved.display(),
                        error
                    )
                })?;
                let include_value = parse_toml(&include_content)?;
                value = merge_toml_values(value, include_value);
            }
        }

        // CLI --include paths are merged last, so they take precedence over everything
        value = merge_include_paths(value, include_paths)?;

        Self::parse_value(value)
    }

    fn parse_value(value: Value) -> Result<Self> {
        let raw: RawConfig = value
            .try_into()
            .map_err(|error| anyhow!("Cannot deserialize config: {error}"))?;

        let openers = if cfg!(target_os = "macos") {
            raw.openers.macos
        } else {
            raw.openers.linux
        };

        let keybindings = KeyBindings::new(&raw.keybindings)?;

        let mut config = Config {
            file_system: raw.file_system,
            is_truecolor: false,
            keybindings,
            log_level: raw.log_level,
            openers,
            theme: raw.theme,
            theme256: raw.theme256,
            ui: raw.ui,
        };
        config.theme.file_type.maybe_apply_ls_colors(false);
        config.theme256.file_type.maybe_apply_ls_colors(true);
        Ok(config)
    }

    fn try_from_default_path(include_paths: Vec<PathBuf>) -> Result<Self> {
        let default_path = Self::default_path()?;
        debug!(
            "Attempting to load the config from the default path: {}",
            default_path.display()
        );

        match fs::read_to_string(&default_path) {
            Ok(content) => Self::parse(
                &content,
                default_path.parent().map(|p| p.to_path_buf()),
                &include_paths,
            ),
            Err(err) if err.kind() == ErrorKind::NotFound => {
                debug!("No config file found, using the built-in config");
                Self::from_default_file(&include_paths)
            }
            Err(err) => Err(anyhow!(
                "Could not read the config file from the default path ({}): {}",
                default_path.display(),
                err
            )),
        }
    }
}

fn parse_toml(content: &str) -> Result<Value> {
    toml::from_str::<Value>(content).map_err(|error| anyhow!("Cannot parse TOML: {error}"))
}

/// Merges CLI --include files on top of an existing config value.
fn merge_include_paths(mut value: Value, include_paths: &[PathBuf]) -> Result<Value> {
    for path in include_paths {
        debug!("Loading CLI include file: {}", path.display());
        let content = fs::read_to_string(path)
            .map_err(|error| anyhow!("Cannot read include file ({}): {}", path.display(), error))?;
        let include_value = parse_toml(&content)?;
        value = merge_toml_values(value, include_value);
    }
    Ok(value)
}

/// Merges the embedded default config from its three source files:
/// base config + truecolor theme + 256-color theme.
fn merge_default_config() -> Result<Value> {
    let base = parse_toml(DEFAULT_CONFIG_BASE)?;
    let theme = parse_toml(DEFAULT_THEME)?;
    let theme256 = parse_toml(DEFAULT_THEME256)?;
    Ok(merge_toml_values(merge_toml_values(base, theme), theme256))
}

/// Deep-merges two TOML values. Tables are merged recursively;
/// all other value types in `overlay` replace those in `base`.
pub fn merge_toml_values(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Table(mut base_table), Value::Table(overlay_table)) => {
            for (key, overlay_val) in overlay_table {
                let merged = match base_table.remove(&key) {
                    Some(base_val) => merge_toml_values(base_val, overlay_val),
                    None => overlay_val,
                };
                base_table.insert(key, merged);
            }
            Value::Table(base_table)
        }
        (_, overlay) => overlay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses_successfully() {
        Config::from_default_file(&[]).unwrap();
    }

    #[test]
    fn default_theme_merges_successfully() {
        let base = parse_toml(DEFAULT_CONFIG_BASE).unwrap();
        let theme = parse_toml(DEFAULT_THEME).unwrap();
        let merged = merge_toml_values(base, theme);
        // The merged value should contain the theme section
        assert!(merged.get("theme").is_some());
    }

    #[test]
    fn default_theme256_merges_successfully() {
        let base = parse_toml(DEFAULT_CONFIG_BASE).unwrap();
        let theme256 = parse_toml(DEFAULT_THEME256).unwrap();
        let merged = merge_toml_values(base, theme256);
        assert!(merged.get("theme256").is_some());
    }

    #[test]
    fn merge_overlay_overrides_base_values() {
        let base = parse_toml("key = \"base\"").unwrap();
        let overlay = parse_toml("key = \"overlay\"").unwrap();
        let merged = merge_toml_values(base, overlay);
        assert_eq!(merged.get("key").unwrap().as_str().unwrap(), "overlay");
    }

    #[test]
    fn merge_preserves_base_keys_not_in_overlay() {
        let base = parse_toml("a = 1\nb = 2").unwrap();
        let overlay = parse_toml("b = 3").unwrap();
        let merged = merge_toml_values(base, overlay);
        assert_eq!(merged.get("a").unwrap().as_integer().unwrap(), 1);
        assert_eq!(merged.get("b").unwrap().as_integer().unwrap(), 3);
    }
}
