pub mod keybindings;
mod ls_colors;
mod serde;
pub mod theme;

use std::{
    collections::HashSet,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use ::serde::Deserialize;
use anyhow::{Result, anyhow};
use directories::ProjectDirs;
use log::{LevelFilter, debug, info};
use toml::Value;

use self::keybindings::{KeyBindings, TomlKeybindings};
use self::theme::Theme;

static CONFIG: OnceLock<Config> = OnceLock::new();

const CONFIG_RELATIVE_PATH: &str = "config.toml";
const DEFAULT_CONFIG_BASE: &str = include_str!("config/default_config.toml");
const DEFAULT_THEME: &str = include_str!("config/default_theme.toml");
const DEFAULT_THEME_FILENAME: &str = "theme.toml";

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
    pub show_hidden_files: bool,
    pub sort_directories_first: bool,
}

/// Runtime inputs that influence config resolution but originate from the
/// terminal/environment rather than the config file. Passed in by the caller
/// so that parsing stays pure and `Config` is correct-by-construction.
#[derive(Clone, Copy, Default)]
pub struct RuntimeEnv<'a> {
    pub is_truecolor: bool,
    pub ls_colors: Option<&'a str>,
}

#[derive(Deserialize)]
struct RawConfig {
    file_system: FileSystemConfig,
    keybindings: TomlKeybindings,
    log_level: LevelFilter,
    openers: PlatformOpeners,
    theme256: Theme,
    theme: Theme,
    ui: UiConfig,
}

pub struct Config {
    pub config_dir: PathBuf,
    pub file_system: FileSystemConfig,
    is_truecolor: bool,
    pub keybindings: KeyBindings,
    pub log_level: LevelFilter,
    pub openers: Openers,
    pub theme256: Theme,
    pub theme: Theme,
    pub ui: UiConfig,
}

impl Config {
    pub fn init(config: Config) {
        if CONFIG.set(config).is_err() {
            // Tests share one global Config across parallel cases; the first
            // init wins and later calls are intentional no-ops. In production
            // a second init is a bug (lib::run calls this exactly once).
            #[cfg(all(debug_assertions, not(test)))]
            panic!("Config::init called more than once outside tests");
        }
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

    pub fn load(
        env: RuntimeEnv<'_>,
        config_path: Option<PathBuf>,
        include_paths: Vec<PathBuf>,
    ) -> Result<Self> {
        let Some(path) = config_path else {
            return Self::try_from_default_path(env, include_paths);
        };

        // Absolutize so `parent()` yields the real containing directory.
        // For a bare filename like `--config config.toml`, `parent()` would
        // return `Some("")`, making `config_dir` empty and every path derived
        // from it (bookmarks, relative includes) CWD-relative by accident.
        let path = std::path::absolute(&path)
            .map_err(|err| anyhow!("Cannot resolve config path {}: {}", path.display(), err))?;

        debug!("Loading config from user-provided path: {}", path.display());
        match fs::read_to_string(&path) {
            Ok(content) => Self::parse(
                env,
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
        fs::write(&path, default_config_content())
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
        Ok(())
    }

    fn parse(
        env: RuntimeEnv<'_>,
        content: &str,
        config_dir: Option<PathBuf>,
        include_paths: &[PathBuf],
    ) -> Result<Self> {
        // Precedence (low → high): built-in defaults → user config file →
        // include_files from the user config → CLI --include paths.
        let defaults = merge_default_config()?;
        let mut value = merge_toml_values(defaults.clone(), parse_toml(content)?);
        value = Self::merge_config_includes(value, config_dir.as_deref())?;
        value = merge_include_paths(value, include_paths)?;
        // Reject typo'd / unknown keys before deserializing so a broken config
        // fails loudly instead of silently falling back to defaults.
        reject_unknown_keys(&value, &defaults, "")?;
        Self::parse_value(env, value, config_dir)
    }

    /// Resolves and merges files listed in the value's own `include_files`
    /// array. Relative entries resolve against `config_dir`.
    fn merge_config_includes(value: Value, config_dir: Option<&Path>) -> Result<Value> {
        let includes = Self::resolve_include_files(&value, config_dir)?;
        merge_include_paths(value, &includes)
    }

    /// The directory containing the resolved config file. Bookmarks live in a
    /// `bookmarks/` subdirectory beside it.
    pub fn bookmarks_dir(&self) -> PathBuf {
        self.config_dir.join("bookmarks")
    }

    /// Resolves the config's `include_files` array into absolute-or-relative
    /// paths. Relative entries are resolved against `config_dir`, falling back
    /// to the default config directory. Errors if neither is available rather
    /// than silently resolving relative entries against the CWD (consistent
    /// with `parse_value`). This is defensive: the fallback is currently
    /// unreachable because `config_dir == None` is only produced after
    /// `default_config_dir()` has already succeeded in the same run.
    fn resolve_include_files(value: &Value, config_dir: Option<&Path>) -> Result<Vec<PathBuf>> {
        let include_files: Vec<PathBuf> = value
            .get("include_files")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();

        if include_files.is_empty() {
            return Ok(Vec::new());
        }

        let resolve_dir = match config_dir {
            Some(dir) => dir.to_path_buf(),
            None => Self::default_config_dir()?,
        };

        Ok(include_files
            .into_iter()
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    resolve_dir.join(path)
                }
            })
            .collect())
    }

    fn parse_value(env: RuntimeEnv<'_>, value: Value, config_dir: Option<PathBuf>) -> Result<Self> {
        // Fail rather than fall back to an empty path: an empty config_dir
        // would make bookmarks_dir() resolve to a relative "bookmarks" path
        // (CWD-dependent), silently misplacing bookmark files.
        let config_dir = match config_dir {
            Some(dir) => dir,
            None => Self::default_config_dir()?,
        };

        let raw: RawConfig = value
            .try_into()
            .map_err(|error| anyhow!("Cannot deserialize config: {error}"))?;

        validate_file_system(&raw.file_system)?;

        let openers = if cfg!(target_os = "macos") {
            raw.openers.macos
        } else {
            raw.openers.linux
        };

        let keybindings = KeyBindings::new(&raw.keybindings)?;

        let mut config = Config {
            config_dir,
            file_system: raw.file_system,
            is_truecolor: env.is_truecolor,
            keybindings,
            log_level: raw.log_level,
            openers,
            theme: raw.theme,
            theme256: raw.theme256,
            ui: raw.ui,
        };
        config
            .theme
            .file_type
            .maybe_apply_ls_colors(env.ls_colors, false);
        config
            .theme256
            .file_type
            .maybe_apply_ls_colors(env.ls_colors, true);
        Ok(config)
    }

    fn try_from_default_path(env: RuntimeEnv<'_>, include_paths: Vec<PathBuf>) -> Result<Self> {
        let default_path = Self::default_path()?;
        debug!(
            "Attempting to load the config from the default path: {}",
            default_path.display()
        );

        match fs::read_to_string(&default_path) {
            Ok(content) => Self::parse(
                env,
                &content,
                default_path.parent().map(|p| p.to_path_buf()),
                &include_paths,
            ),
            Err(err) if err.kind() == ErrorKind::NotFound => {
                debug!("No config file found, using the built-in config");
                Self::parse(env, "", None, &include_paths)
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

/// Merges the given include files on top of an existing config value.
/// Each include file's own `include_files` are resolved (relative to that
/// file's directory) and merged recursively. A visited set keyed by
/// canonicalized path breaks cycles and skips duplicate includes.
fn merge_include_paths(mut value: Value, include_paths: &[PathBuf]) -> Result<Value> {
    let mut visited = HashSet::new();
    for path in include_paths {
        value = merge_include_file(value, path, &mut visited)?;
    }
    Ok(value)
}

fn merge_include_file(value: Value, path: &Path, visited: &mut HashSet<PathBuf>) -> Result<Value> {
    // Canonicalize so the same file referenced via different paths is detected.
    // Fall back to the raw path if canonicalization fails — a missing file or
    // permission error will then surface from `fs::read_to_string` below with
    // a more informative message.
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    // Resolve this file's own include_files relative to its real directory.
    // Deriving the directory from the canonical (absolute, when canonicalize
    // succeeds) path means a bare filename — whose `parent()` is "" — still
    // resolves nested includes against the file's directory, not the CWD.
    let base_dir = canonical
        .parent()
        .map(Path::to_path_buf)
        .or_else(|| path.parent().map(Path::to_path_buf));

    if !visited.insert(canonical) {
        debug!(
            "Skipping already-included file (cycle or duplicate): {}",
            path.display()
        );
        return Ok(value);
    }

    debug!("Loading include file: {}", path.display());
    let content = fs::read_to_string(path)
        .map_err(|error| anyhow!("Cannot read include file ({}): {}", path.display(), error))?;
    let include_value = parse_toml(&content)?;

    let nested = Config::resolve_include_files(&include_value, base_dir.as_deref())?;

    // Merge the file's content first, then its nested includes on top — the
    // same precedence rule the top level uses (includes override the config
    // that requested them).
    let mut value = merge_toml_values(value, include_value);
    for nested_path in &nested {
        value = merge_include_file(value, nested_path, visited)?;
    }
    Ok(value)
}

/// Validates `file_system` invariants that TOML deserialization cannot express,
/// so a nonsensical config fails the load rather than misbehaving at runtime.
fn validate_file_system(fs: &FileSystemConfig) -> Result<()> {
    if fs.buffer_min_bytes == 0 {
        return Err(anyhow!(
            "file_system.buffer_min_bytes must be greater than 0"
        ));
    }
    if fs.buffer_min_bytes > fs.buffer_max_bytes {
        return Err(anyhow!(
            "file_system.buffer_min_bytes ({}) must not exceed buffer_max_bytes ({})",
            fs.buffer_min_bytes,
            fs.buffer_max_bytes
        ));
    }
    Ok(())
}

/// Style properties that may appear on any style table. The embedded default
/// omits these where they are unset (e.g. `[theme.alert]` lists only `fg`), so
/// they are permitted everywhere rather than validated against the default's
/// shape — otherwise a user adding `bg`/`modifiers` to such an entry would be
/// wrongly rejected. Misplaced occurrences are harmless and rare.
const STYLE_KEYS: &[&str] = &["fg", "bg", "modifiers"];

/// Recursively rejects any key in `value` that is absent from the embedded
/// default `schema`, so typo'd or unrecognized config keys fail loudly. The
/// top-level `include_files` directive is allowed (it is consumed before
/// deserialization and is not part of the schema). `path` is the dotted key
/// path used in error messages.
fn reject_unknown_keys(value: &Value, schema: &Value, path: &str) -> Result<()> {
    let (Value::Table(value_table), Value::Table(schema_table)) = (value, schema) else {
        return Ok(());
    };
    for (key, child) in value_table {
        if path.is_empty() && key == "include_files" {
            continue;
        }
        if STYLE_KEYS.contains(&key.as_str()) {
            continue;
        }
        let key_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        match schema_table.get(key) {
            Some(schema_child) => reject_unknown_keys(child, schema_child, &key_path)?,
            None => return Err(anyhow!("Unknown configuration key: '{key_path}'")),
        }
    }
    Ok(())
}

/// The default config written by `--write-default-config`: the two embedded
/// source files concatenated, rather than a serialized merged TOML value, so
/// their inline documentation comments are preserved. The base config and
/// theme define disjoint top-level keys, so concatenation yields valid,
/// complete TOML.
fn default_config_content() -> String {
    format!("{DEFAULT_CONFIG_BASE}\n{DEFAULT_THEME}")
}

/// Merges the embedded default config from its two source files:
/// base config + theme (which includes both truecolor and 256-color variants).
fn merge_default_config() -> Result<Value> {
    let base = parse_toml(DEFAULT_CONFIG_BASE)?;
    let theme = parse_toml(DEFAULT_THEME)?;
    Ok(merge_toml_values(base, theme))
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
    use test_case::test_case;

    use super::*;

    #[test]
    fn default_config_parses_successfully() {
        Config::parse(RuntimeEnv::default(), "", None, &[]).unwrap();
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

    #[test]
    fn partial_user_config_merges_with_defaults() {
        let partial = r#"
[openers.linux]
open_current_directory = "alacritty --working-directory %s"
"#;
        Config::parse(RuntimeEnv::default(), partial, None, &[]).unwrap();
    }

    /// Parse a config that is expected to fail, returning the error message.
    /// (`Config` is not `Debug`, so `unwrap_err` is unavailable.)
    fn parse_err(toml: &str) -> String {
        match Config::parse(RuntimeEnv::default(), toml, None, &[]) {
            Ok(_) => panic!("expected config parse to fail"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn written_default_config_parses_and_preserves_comments() {
        let content = default_config_content();
        // The written file must round-trip through the loader.
        Config::parse(RuntimeEnv::default(), &content, None, &[]).unwrap();
        // Concatenation (not re-serialization) keeps the documentation comments.
        assert!(content.contains('#'), "comments should be preserved");
    }

    #[test_case("not_a_key = 1", "not_a_key" ; "top-level key")]
    #[test_case("[file_system]\nbuffer_max_byte = 1\n", "file_system.buffer_max_byte" ; "nested key (dotted path)")]
    #[test_case("[keybindings]\nserach = \"/\"\n", "serach" ; "keybinding name")]
    fn unknown_key_is_rejected(toml: &str, expected: &str) {
        let err = parse_err(toml);
        assert!(err.contains(expected), "error should name the key: {err}");
    }

    #[test]
    fn style_property_absent_from_default_is_accepted() {
        // The default `[theme.alert]` lists only `fg`; adding `bg`/`modifiers`
        // must not be mistaken for an unknown key.
        let toml = r##"
[theme.alert]
bg = "#000000"
modifiers = ["bold"]
"##;
        Config::parse(RuntimeEnv::default(), toml, None, &[]).unwrap();
    }

    #[test_case("[file_system]\nbuffer_min_bytes = 200\nbuffer_max_bytes = 100\n" ; "min exceeds max")]
    #[test_case("[file_system]\nbuffer_min_bytes = 0\n" ; "min is zero")]
    fn invalid_buffer_sizes_are_rejected(toml: &str) {
        let err = parse_err(toml);
        assert!(
            err.contains("buffer_min_bytes"),
            "error should explain the invariant: {err}"
        );
    }
}
