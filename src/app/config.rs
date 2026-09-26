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
use crate::file_system::path_info::quoted;

static CONFIG: OnceLock<Config> = OnceLock::new();

const CONFIG_RELATIVE_PATH: &str = "config.toml";
const DEFAULT_CONFIG_BASE: &str = include_str!("config/default_config.toml");
const DEFAULT_THEME: &str = include_str!("config/default_theme.toml");
const DEFAULT_THEME_FILENAME: &str = "theme.toml";
/// The floor shared by every recurring UI timer.
const MIN_REFRESH_DEBOUNCE_MILLISECONDS: u64 = 100;

#[derive(Debug, Deserialize)]
pub struct FileSystemConfig {
    pub refresh_debounce_milliseconds: u64,
    pub search_max_depth: u32,
    pub search_max_results: u32,
}

#[derive(Debug, Deserialize)]
pub struct Openers {
    pub open_directory: String,
    pub open_file: String,
    pub open_filectrl_window: String,
    /// Wraps a command that needs a terminal (`Terminal=true` desktop entries).
    /// `%s` is replaced by a command line rather than a path.
    pub run_in_terminal: String,
}

#[derive(Debug, Deserialize)]
struct PlatformOpeners {
    linux: Openers,
    macos: Openers,
}

// Independent settings.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct UiConfig {
    pub double_click_interval_milliseconds: u16,
    /// Whether `$LS_COLORS` overrides the theme's file type colors. Not a theme
    /// setting, so an included theme cannot turn it off.
    pub ls_colors_take_precedence: bool,
    pub natural_sort: bool,
    pub show_hidden_files: bool,
    pub sort_directories_first: bool,
}

/// Terminal and environment inputs to config resolution, passed in so parsing
/// stays pure.
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
    // Panics only in debug builds outside tests, which `assert!` cannot express.
    #[allow(clippy::manual_assert)]
    pub fn init(config: Config) {
        if CONFIG.set(config).is_err() {
            // Tests share one global Config, so the first init wins. Elsewhere a second
            // init is a bug.
            #[cfg(all(debug_assertions, not(test)))]
            panic!("Config::init called more than once outside tests");
        }
    }

    pub fn global() -> &'static Config {
        CONFIG.get().expect("config should be initialized")
    }

    /// A `Config` from the embedded defaults alone, so tests never read the host's
    /// config. `config_dir` is a reserved path that is never created.
    #[cfg(test)]
    pub(crate) fn builtin() -> Self {
        let config_dir = crate::test_support::TempDir::reserved("config")
            .path()
            .to_path_buf();
        Self::parse(RuntimeEnv::default(), None, "", &config_dir, &[])
            .expect("the embedded default config should parse")
    }

    /// Initializes the global config from [`Config::builtin`]; the first call wins.
    #[cfg(test)]
    pub(crate) fn init_test() {
        Self::init(Self::builtin());
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
        include_paths: &[PathBuf],
    ) -> Result<Self> {
        let is_default = config_path.is_none();
        Self::load_from(
            env,
            &Self::target_path(config_path)?,
            is_default,
            include_paths,
        )
    }

    /// `path` is absolute, so its parent is never empty. `is_default` means a
    /// missing file falls back to the built-in config rather than failing.
    fn load_from(
        env: RuntimeEnv<'_>,
        path: &Path,
        is_default: bool,
        include_paths: &[PathBuf],
    ) -> Result<Self> {
        debug!("Loading the config from {}", path.display());
        let (config_file, content) = match read_regular_file(path) {
            Ok(content) => (Some(path), content),
            // A dangling symlink also reads as NotFound, and is an error.
            Err(ReadFailure::Io(error))
                if is_default
                    && error.kind() == ErrorKind::NotFound
                    && path.symlink_metadata().is_err() =>
            {
                debug!("No config file found, using the built-in config");
                (None, String::new())
            }
            Err(failure) => return Err(failure.describe(CONFIG_FILE, path)),
        };
        let config_dir = path.parent().ok_or_else(|| {
            anyhow!(
                "Cannot load config file {}: it has no parent directory",
                quoted(path)
            )
        })?;
        // Canonical, so bookmark paths derived from it hold no `..` or `.`.
        let config_dir = canonical_or_raw(config_dir);
        Self::parse(env, config_file, &content, &config_dir, include_paths)
    }

    fn default_path() -> Result<PathBuf> {
        Ok(ProjectDirs::from("", "", "filectrl")
            .ok_or_else(|| anyhow!("Cannot determine the config directory"))?
            .config_dir()
            .join(CONFIG_RELATIVE_PATH))
    }

    /// The config file the CLI acts on: `--config`, or the default. Absolutized.
    fn target_path(config_path: Option<PathBuf>) -> Result<PathBuf> {
        match config_path {
            Some(path) => absolute_path(&path),
            None => Self::default_path(),
        }
    }

    /// Writes the config keys only; the theme is written by
    /// [`Config::write_default_themes`].
    pub fn write_default(config_path: Option<PathBuf>, force: bool) -> Result<PathBuf> {
        let path = Self::target_path(config_path)?;
        write_new(&path, DEFAULT_CONFIG_BASE, force)?;
        info!("Wrote the default config to {}", path.display());
        Ok(path)
    }

    /// Writes the theme beside the config, where a relative include resolves from.
    pub fn write_default_themes(config_path: Option<PathBuf>, force: bool) -> Result<PathBuf> {
        let config = Self::target_path(config_path)?;
        let dir = config.parent().ok_or_else(|| {
            anyhow!(
                "Cannot write the theme: {} has no parent directory",
                quoted(&config)
            )
        })?;
        let path = dir.join(DEFAULT_THEME_FILENAME);
        write_new(&path, DEFAULT_THEME, force)?;
        info!("Wrote the default theme to {}", path.display());
        Ok(path)
    }

    /// `config_file` is the file `content` was read from, if any; an include cycle
    /// back to it stops there.
    fn parse(
        env: RuntimeEnv<'_>,
        config_file: Option<&Path>,
        content: &str,
        config_dir: &Path,
        include_paths: &[PathBuf],
    ) -> Result<Self> {
        // Precedence (low → high): defaults → config file → its include_files → CLI
        // --include paths. Each file is validated before merging, so an error names it.
        let defaults = merge_default_config()?;
        let own = parse_toml(config_file, content)?;
        validate_file(config_file, &own, &defaults)?;
        let mut value = merge_toml_values(defaults.clone(), own);
        value = Self::merge_config_includes(config_file, value, config_dir, &defaults)?;
        value = merge_include_paths(config_file, value, include_paths, &defaults)?;
        Self::parse_value(env, value, config_dir)
    }

    /// Merges the files in the value's own `include_files`, relative to
    /// `config_dir`.
    fn merge_config_includes(
        config_file: Option<&Path>,
        value: Value,
        config_dir: &Path,
        defaults: &Value,
    ) -> Result<Value> {
        let includes = Self::resolve_include_files(&value, config_dir)?;
        merge_include_paths(config_file, value, &includes, defaults)
    }

    /// `bookmarks/` beside the resolved config file.
    pub fn bookmarks_dir(&self) -> PathBuf {
        self.config_dir.join("bookmarks")
    }

    /// Resolves `include_files` entries, relative to `config_dir`.
    fn resolve_include_files(value: &Value, config_dir: &Path) -> Result<Vec<PathBuf>> {
        Ok(include_entries(value)?
            .into_iter()
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    config_dir.join(path)
                }
            })
            .collect())
    }

    fn parse_value(env: RuntimeEnv<'_>, value: Value, config_dir: &Path) -> Result<Self> {
        let raw: RawConfig = value
            .try_into()
            .map_err(|error| deserialize_error(None, &error))?;

        validate_file_system(&raw.file_system)?;

        let openers = if cfg!(target_os = "macos") {
            raw.openers.macos
        } else {
            raw.openers.linux
        };

        let keybindings = KeyBindings::new(&raw.keybindings)?;

        let mut config = Config {
            config_dir: config_dir.to_path_buf(),
            file_system: raw.file_system,
            is_truecolor: env.is_truecolor,
            keybindings,
            log_level: raw.log_level,
            openers,
            theme: raw.theme,
            theme256: raw.theme256,
            ui: raw.ui,
        };
        // The RGB warning applies only to the theme this run renders with.
        if config.ui.ls_colors_take_precedence
            && let Some(ls_colors) = env.ls_colors
        {
            let warn_on_rgb = !env.is_truecolor;
            config.theme.file_type.apply_ls_colors(ls_colors, false);
            config
                .theme256
                .file_type
                .apply_ls_colors(ls_colors, warn_on_rgb);
        }
        Ok(config)
    }
}

/// The paths a file's `include_files` lists. A malformed value is an error.
fn include_entries(value: &Value) -> Result<Vec<PathBuf>> {
    let Some(include_value) = value.get("include_files") else {
        return Ok(Vec::new());
    };
    include_value
        .as_array()
        .ok_or_else(|| anyhow!("'include_files' must be an array of file paths"))?
        .iter()
        .map(|entry| {
            entry.as_str().map(PathBuf::from).ok_or_else(|| {
                anyhow!("'include_files' entries must be strings, but found: {entry}")
            })
        })
        .collect()
}

/// `file` names the file `content` came from, for the error message.
fn parse_toml(file: Option<&Path>, content: &str) -> Result<Value> {
    toml::from_str::<Value>(content).map_err(|error| match file {
        Some(file) => anyhow!("Failed to parse {}: {error}", quoted(file)),
        None => anyhow!("Failed to parse TOML: {error}"),
    })
}

/// Absolutizes without requiring the path to exist.
fn absolute_path(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path)
        .map_err(|error| anyhow!("Failed to resolve {}: {error}", quoted(path)))
}

const CONFIG_FILE: &str = "config file";
const INCLUDE_FILE: &str = "include file";

/// Why a config or include file could not be read.
enum ReadFailure {
    Io(std::io::Error),
    NotRegular,
}

impl ReadFailure {
    /// `kind` names the file's role for the message.
    fn describe(self, kind: &str, path: &Path) -> anyhow::Error {
        match self {
            Self::Io(error) => anyhow!("Failed to read {kind} {}: {error}", quoted(path)),
            Self::NotRegular => {
                anyhow!("Cannot read {kind} {}: not a regular file", quoted(path))
            }
        }
    }
}

/// Reads a file that must be a regular file, following symlinks: a FIFO or a
/// device is refused before reading. Opened `O_NONBLOCK` (a FIFO open would
/// wait for a writer) and `O_NOCTTY`, and typed from the open descriptor.
fn read_regular_file(path: &Path) -> std::result::Result<String, ReadFailure> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};

    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY)
        .open(path)
        .map_err(ReadFailure::Io)?;
    if !file.metadata().map_err(ReadFailure::Io)?.is_file() {
        return Err(ReadFailure::NotRegular);
    }
    let mut content = String::new();
    file.read_to_string(&mut content).map_err(ReadFailure::Io)?;
    Ok(content)
}

/// Writes `content` to `path`, creating the parent directory. An existing file
/// is replaced only with `force`; anything but a regular file (a symlink
/// included) is always refused. A replacement is written beside the file with
/// its permission bits and renamed over it, so a failed write keeps the old one.
fn write_new(path: &Path, content: &str, force: bool) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Cannot write {}: it has no parent directory", quoted(path)))?;
    fs::create_dir_all(parent)
        .map_err(|error| anyhow!("Failed to create directory {}: {error}", quoted(parent)))?;
    let existing = path.symlink_metadata().ok();
    // Refused even with `force`, so the message never suggests the flag.
    if let Some(metadata) = &existing {
        let file_type = metadata.file_type();
        if !file_type.is_file() {
            let what = if file_type.is_symlink() {
                "a symbolic link"
            } else if file_type.is_dir() {
                "a directory"
            } else {
                "not a regular file"
            };
            return Err(anyhow!("Cannot write {}: it is {what}", quoted(path)));
        }
    }
    let Some(metadata) = existing.filter(|_| force) else {
        return create_and_write(path, content).map_err(|error| {
            if error.kind() == ErrorKind::AlreadyExists {
                anyhow!(
                    "Cannot write {}: it already exists; pass --force to replace it",
                    quoted(path)
                )
            } else {
                anyhow!("Failed to write {}: {error}", quoted(path))
            }
        });
    };
    let mut staged = path.as_os_str().to_owned();
    staged.push(format!(".{}.tmp", std::process::id()));
    let staged = PathBuf::from(staged);
    let failed = |error: std::io::Error| anyhow!("Failed to replace {}: {error}", quoted(path));
    let written = create_and_write(&staged, content).and_then(|()| {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        fs::set_permissions(&staged, fs::Permissions::from_mode(mode))
    });
    // Removed only if this call created it.
    if let Err(error) = written.and_then(|()| fs::rename(&staged, path)) {
        if error.kind() != ErrorKind::AlreadyExists {
            let _ = fs::remove_file(&staged);
        }
        return Err(failed(error));
    }
    Ok(())
}

/// Creates `path` exclusively and writes `content` to it.
fn create_and_write(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(content.as_bytes())?;
    // Synced before a rename can put it in place of the old file.
    file.sync_all()
}

/// Merges `include_paths` onto `value`, recursing into each file's own
/// `include_files`. A visited set of canonical paths, seeded with
/// `config_file`, breaks cycles and skips duplicates.
fn merge_include_paths(
    config_file: Option<&Path>,
    mut value: Value,
    include_paths: &[PathBuf],
    defaults: &Value,
) -> Result<Value> {
    let mut visited: HashSet<PathBuf> = config_file.map(canonical_or_raw).into_iter().collect();
    for path in include_paths {
        value = merge_include_file(value, path, &mut visited, defaults)?;
    }
    Ok(value)
}

fn canonical_or_raw(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn merge_include_file(
    value: Value,
    path: &Path,
    visited: &mut HashSet<PathBuf>,
    defaults: &Value,
) -> Result<Value> {
    // A failed canonicalize is reported by `read_regular_file` below.
    let canonical = canonical_or_raw(path);
    if !visited.insert(canonical.clone()) {
        debug!(
            "Skipping already-included file (cycle or duplicate): {}",
            path.display()
        );
        return Ok(value);
    }

    debug!("Loading include file: {}", path.display());
    let content =
        read_regular_file(path).map_err(|failure| failure.describe(INCLUDE_FILE, path))?;
    let include_value = parse_toml(Some(path), &content)?;
    validate_file(Some(path), &include_value, defaults)?;

    // Nested includes resolve from the directory of the path as named, not of a
    // symlink's target. Absolutized first, since a bare filename's parent is "".
    let path = absolute_path(path)?;
    let base_dir = canonical_or_raw(path.parent().unwrap_or(Path::new("/")));
    let nested = Config::resolve_include_files(&include_value, &base_dir)?;

    // The file first, then its nested includes on top.
    let mut value = merge_toml_values(value, include_value);
    for nested_path in &nested {
        value = merge_include_file(value, nested_path, visited, defaults)?;
    }
    Ok(value)
}

/// Validates `file_system` invariants that deserialization cannot express.
fn validate_file_system(fs: &FileSystemConfig) -> Result<()> {
    if fs.refresh_debounce_milliseconds < MIN_REFRESH_DEBOUNCE_MILLISECONDS {
        return Err(anyhow!(
            "file_system.refresh_debounce_milliseconds ({}) must be at least {MIN_REFRESH_DEBOUNCE_MILLISECONDS}",
            fs.refresh_debounce_milliseconds
        ));
    }
    if fs.search_max_depth == 0 {
        return Err(anyhow!(
            "file_system.search_max_depth must be greater than 0"
        ));
    }
    if fs.search_max_results == 0 {
        return Err(anyhow!(
            "file_system.search_max_results must be greater than 0"
        ));
    }
    Ok(())
}

/// Style properties allowed on any theme style table, even where the default
/// omits them. See `takes_style_keys`.
const STYLE_KEYS: &[&str] = &["fg", "bg", "modifiers"];

/// Whether a theme table in the default `schema` is a style (a leaf, or a
/// section with its own style keys) rather than a container.
fn takes_style_keys(schema: &toml::map::Map<String, Value>) -> bool {
    schema.values().all(|value| !value.is_table())
        || STYLE_KEYS.iter().any(|key| schema.contains_key(*key))
}

/// Whether `path` is inside a theme, the only place style properties belong.
fn is_theme_path(path: &str) -> bool {
    matches!(path.split('.').next(), Some("theme" | "theme256"))
}

/// Checks one file's keys, types and 256-color indexes against `defaults`
/// before merging, so an error names the file. `file` is `None` for content
/// read from no file.
fn validate_file(file: Option<&Path>, value: &Value, defaults: &Value) -> Result<()> {
    let located = |error: anyhow::Error| match file {
        Some(file) => anyhow!("Cannot load {}: {error:#}", quoted(file)),
        None => error,
    };
    reject_unknown_keys(value, defaults, "").map_err(located)?;
    include_entries(value).map_err(located)?;
    if let Some(theme256) = value.get("theme256") {
        reject_unindexed_colors(theme256, "theme256").map_err(located)?;
    }
    let raw = merge_toml_values(defaults.clone(), value.clone())
        .try_into::<RawConfig>()
        .map_err(|error| deserialize_error(file, &error))?;
    // Keybinding conflicts are checked only on the merged config, since a later
    // file can resolve them.
    validate_file_system(&raw.file_system).map_err(located)?;
    validate_openers(&raw.openers).map_err(located)?;
    KeyBindings::check(&raw.keybindings).map_err(located)?;
    Ok(())
}

/// Rejects a non-empty opener template without `%s` as its own unquoted word
/// (see `file_system::shell`).
fn validate_openers(openers: &PlatformOpeners) -> Result<()> {
    for (platform, openers) in [("linux", &openers.linux), ("macos", &openers.macos)] {
        for (name, template) in [
            ("open_directory", &openers.open_directory),
            ("open_file", &openers.open_file),
            ("open_filectrl_window", &openers.open_filectrl_window),
            ("run_in_terminal", &openers.run_in_terminal),
        ] {
            if !template.trim().is_empty() && !has_unquoted_placeholder(template) {
                return Err(anyhow!(
                    "openers.{platform}.{name} ({template:?}) must contain %s as its own unquoted word"
                ));
            }
        }
    }
    Ok(())
}

/// Whether `template` holds `%s` outside quotes, delimited by the start or
/// end, whitespace, or a shell operator.
fn has_unquoted_placeholder(template: &str) -> bool {
    let is_boundary = |c: Option<char>| {
        c.is_none_or(|c| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '<' | '>'))
    };
    let chars: Vec<char> = template.chars().collect();
    let (mut single, mut double, mut escaped) = (false, false, false);
    for (i, &c) in chars.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if !single => escaped = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '%' if !single
                && !double
                && chars.get(i + 1) == Some(&'s')
                && is_boundary(i.checked_sub(1).map(|j| chars[j]))
                && is_boundary(chars.get(i + 2).copied()) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// A deserialization error, naming its file if any, joined onto one line.
fn deserialize_error(file: Option<&Path>, error: &toml::de::Error) -> anyhow::Error {
    let message = error.to_string();
    let message = message.lines().map(str::trim).collect::<Vec<_>>().join(" ");
    let message = message.trim_end();
    match file {
        Some(file) => anyhow!("Cannot load {}: {message}", quoted(file)),
        None => anyhow!("Cannot load the config: {message}"),
    }
}

/// Rejects an `fg` or `bg` under `[theme256]` that is neither empty nor a
/// decimal index from 0 to 255. `path` is the dotted key path of `value`.
fn reject_unindexed_colors(value: &Value, path: &str) -> Result<()> {
    let Value::Table(table) = value else {
        return Ok(());
    };
    for (key, child) in table {
        let key_path = format!("{path}.{key}");
        match (key.as_str(), child) {
            ("fg" | "bg", Value::String(color)) if !color.is_empty() && !is_color_index(color) => {
                return Err(anyhow!(
                    "{key_path}: {color:?} is not a 256-color index (0-255)"
                ));
            }
            (_, Value::Table(_)) => reject_unindexed_colors(child, &key_path)?,
            _ => {}
        }
    }
    Ok(())
}

/// Whether `color` is a decimal index from 0 to 255, with no sign.
fn is_color_index(color: &str) -> bool {
    color.bytes().all(|byte| byte.is_ascii_digit()) && color.parse::<u8>().is_ok()
}

/// Recursively rejects keys absent from the default `schema`. The top-level
/// `include_files` is allowed. `path` is the dotted key path for messages.
fn reject_unknown_keys(value: &Value, schema: &Value, path: &str) -> Result<()> {
    let (Value::Table(value_table), Value::Table(schema_table)) = (value, schema) else {
        return Ok(());
    };
    for (key, child) in value_table {
        if path.is_empty() && key == "include_files" {
            continue;
        }
        if is_theme_path(path)
            && STYLE_KEYS.contains(&key.as_str())
            && takes_style_keys(schema_table)
        {
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

/// The embedded default config: base config plus theme.
fn merge_default_config() -> Result<Value> {
    let base = parse_toml(None, DEFAULT_CONFIG_BASE)?;
    let theme = parse_toml(None, DEFAULT_THEME)?;
    Ok(merge_toml_values(base, theme))
}

/// Deep-merges two TOML values: tables recursively, anything else replaced.
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
    use ratatui::{
        crossterm::event::{KeyCode, KeyModifiers},
        style::Color,
    };
    use test_case::test_case;

    use super::{keybindings::Action, *};
    use crate::test_support::TempDir;

    /// A reserved config directory that is never created.
    fn inert_dir() -> PathBuf {
        TempDir::reserved("config_parse").path().to_path_buf()
    }

    #[test]
    fn merge_overrides_shared_keys_and_preserves_the_rest() {
        // Nested, to exercise the recursive arm.
        let base = parse_toml(None, "[t]\na = 1\nb = 2").unwrap();
        let overlay = parse_toml(None, "[t]\nb = 3").unwrap();
        let merged = merge_toml_values(base, overlay);
        let table = merged.get("t").unwrap();
        assert_eq!(1, table.get("a").unwrap().as_integer().unwrap());
        assert_eq!(3, table.get("b").unwrap().as_integer().unwrap());
    }

    #[test]
    fn partial_user_config_merges_with_defaults() {
        // Both platforms, since `parse_value` picks one by target.
        let partial = r#"
[openers.linux]
open_directory = "alacritty --working-directory %s"
[openers.macos]
open_directory = "alacritty --working-directory %s"
"#;
        let defaults = Config::parse(RuntimeEnv::default(), None, "", &inert_dir(), &[]).unwrap();
        let merged =
            Config::parse(RuntimeEnv::default(), None, partial, &inert_dir(), &[]).unwrap();

        assert_eq!(
            "alacritty --working-directory %s",
            merged.openers.open_directory
        );
        assert_eq!(defaults.openers.open_file, merged.openers.open_file);
        assert!(!merged.openers.open_file.is_empty());
    }

    /// Parses a config expected to fail; `Config` is not `Debug`.
    fn parse_err(toml: &str) -> String {
        match Config::parse(RuntimeEnv::default(), None, toml, &inert_dir(), &[]) {
            Ok(_) => panic!("expected config parse to fail"),
            Err(error) => error.to_string(),
        }
    }

    #[test_case("#ff0000" ; "hex")]
    #[test_case("Red" ; "named")]
    #[test_case("256" ; "past the last index")]
    #[test_case("+5" ; "a sign")]
    fn a_theme256_color_that_is_not_an_index_is_refused(color: &str) {
        let error = parse_err(&format!("[theme256.table.body]\nfg = {color:?}\n"));

        assert_eq!(
            format!("theme256.table.body.fg: {color:?} is not a 256-color index (0-255)"),
            error
        );
    }

    #[test_case("0" ; "the first index")]
    #[test_case("255" ; "the last index")]
    #[test_case("" ; "inherited")]
    fn a_theme256_index_is_accepted(color: &str) {
        Config::parse(
            RuntimeEnv::default(),
            None,
            &format!("[theme256.table.body]\nbg = {color:?}\n"),
            &inert_dir(),
            &[],
        )
        .unwrap();
    }

    /// Embedded source, so comments survive being written out.
    #[test_case(DEFAULT_CONFIG_BASE ; "config")]
    #[test_case(DEFAULT_THEME ; "theme")]
    fn a_written_default_parses_and_keeps_its_comments(content: &str) {
        Config::parse(RuntimeEnv::default(), None, content, &inert_dir(), &[]).unwrap();
        assert!(content.contains('#'), "comments should be preserved");
    }

    #[test_case(include_str!("../../themes/42km.toml") ; "42km")]
    #[test_case(include_str!("../../themes/ibm1970.toml") ; "ibm1970")]
    fn a_bundled_theme_parses(content: &str) {
        Config::parse(RuntimeEnv::default(), None, content, &inert_dir(), &[]).unwrap();
    }

    #[test]
    fn the_ibm1970_theme_is_the_default_theme() {
        assert_eq!(DEFAULT_THEME, include_str!("../../themes/ibm1970.toml"));
    }

    #[test]
    fn a_type_error_has_no_trailing_newline() {
        let error = parse_err("[file_system]\nsearch_max_depth = \"deep\"\n");

        assert!(error.starts_with("Cannot load the config:"), "{error}");
        assert_eq!(error.trim_end(), error);
    }

    #[test]
    fn the_default_config_and_theme_do_not_overlap() {
        assert!(
            !DEFAULT_CONFIG_BASE.contains("[theme"),
            "theme keys in config"
        );
        assert!(DEFAULT_THEME.contains("[theme"), "no theme keys in theme");
    }

    #[test_case("not_a_key = 1", "not_a_key" ; "top-level key")]
    #[test_case("[file_system]\nsearch_max_dept = 1\n", "file_system.search_max_dept" ; "nested key (dotted path)")]
    #[test_case("[keybindings]\nserach = \"/\"\n", "serach" ; "keybinding name")]
    // Style names outside a theme, or on a theme container, would be dropped.
    #[test_case("fg = \"#ff0000\"\n", "fg" ; "style name at the top level")]
    #[test_case("[ui]\nbg = 42\n", "ui.bg" ; "style name in a non-theme table")]
    #[test_case("[file_system]\nmodifiers = [\"bold\"]\n", "file_system.modifiers" ; "modifiers in a non-theme table")]
    #[test_case("[theme.clipboard]\nfg = \"Red\"\n", "theme.clipboard.fg" ; "style name on a theme container")]
    #[test_case("[theme256.file_modified_date]\nbg = \"4\"\n", "theme256.file_modified_date.bg" ; "style name on a theme256 container")]
    fn unknown_key_is_rejected(toml: &str, expected: &str) {
        let err = parse_err(toml);
        assert!(err.contains(expected), "error should name the key: {err}");
    }

    #[test_case("[theme.alert]\nbg = \"#000000\"\nmodifiers = [\"bold\"]\n" ; "nested theme table")]
    #[test_case("[theme256.alert]\nbg = \"0\"\n" ; "nested theme256 table")]
    #[test_case("[theme]\nfg = \"#ffffff\"\n" ; "theme root")]
    fn style_property_absent_from_default_is_accepted(toml: &str) {
        Config::parse(RuntimeEnv::default(), None, toml, &inert_dir(), &[]).unwrap();
    }

    #[test_case("include_files = \"theme.toml\"" ; "string instead of array")]
    #[test_case("include_files = [42]" ; "non-string element")]
    fn malformed_include_files_is_rejected(toml: &str) {
        let err = parse_err(toml);
        assert!(
            err.contains("include_files"),
            "error should name the key: {err}"
        );
    }

    #[test_case(0 ; "zero")]
    #[test_case(99 ; "just below the floor")]
    fn a_refresh_debounce_below_the_floor_is_rejected(milliseconds: u64) {
        let err = parse_err(&format!(
            "[file_system]\nrefresh_debounce_milliseconds = {milliseconds}\n"
        ));
        assert!(
            err.contains("refresh_debounce_milliseconds") && err.contains("at least 100"),
            "error should explain the floor: {err}"
        );
    }

    #[test]
    fn a_refresh_debounce_at_the_floor_is_accepted() {
        Config::parse(
            RuntimeEnv::default(),
            None,
            "[file_system]\nrefresh_debounce_milliseconds = 100\n",
            &inert_dir(),
            &[],
        )
        .unwrap();
    }

    #[test_case("search_max_depth" ; "depth")]
    #[test_case("search_max_results" ; "results")]
    fn a_search_bound_of_zero_is_rejected(key: &str) {
        let err = parse_err(&format!("[file_system]\n{key} = 0\n"));
        assert_eq!(format!("file_system.{key} must be greater than 0"), err);
    }

    #[test]
    fn the_theme_read_is_the_one_for_the_terminals_color_support() {
        let toml = "[theme]\nfg = \"#010203\"\n[theme256]\nfg = \"4\"\n";
        let parse = |is_truecolor| {
            let env = RuntimeEnv {
                is_truecolor,
                ls_colors: None,
            };
            Config::parse(env, None, toml, &inert_dir(), &[]).unwrap()
        };

        assert_eq!(Some(Color::Rgb(1, 2, 3)), parse(true).theme().base().fg);
        assert_eq!(Some(Color::Indexed(4)), parse(false).theme().base().fg);
    }

    #[test]
    fn the_openers_are_the_ones_for_the_build_target() {
        let toml = "[openers.linux]\nopen_file = \"linux %s\"\n[openers.macos]\nopen_file = \"macos %s\"\n";
        let config = Config::parse(RuntimeEnv::default(), None, toml, &inert_dir(), &[]).unwrap();
        let expected = if cfg!(target_os = "macos") {
            "macos %s"
        } else {
            "linux %s"
        };
        assert_eq!(expected, config.openers.open_file);
    }

    #[test_case(true, Some(Color::Red), Some(Color::Red) ; "applied when they take precedence")]
    #[test_case(false, Some(Color::Rgb(1, 2, 3)), Some(Color::Indexed(5)) ; "ignored otherwise")]
    fn ls_colors_reach_both_themes(
        take_precedence: bool,
        expected: Option<Color>,
        expected256: Option<Color>,
    ) {
        let env = RuntimeEnv {
            is_truecolor: false,
            ls_colors: Some("di=31"),
        };
        let toml = format!(
            "[ui]\nls_colors_take_precedence = {take_precedence}\n\
             [theme.file_type.directory]\nfg = \"#010203\"\n\
             [theme256.file_type.directory]\nfg = \"5\"\n"
        );
        let config = Config::parse(env, None, &toml, &inert_dir(), &[]).unwrap();

        assert_eq!(expected, config.theme.file_type.directory().fg);
        assert_eq!(expected256, config.theme256.file_type.directory().fg);
    }

    // Writing the defaults: always to an explicit path, never the user's config.

    #[test]
    fn write_default_writes_the_config_and_reports_where() {
        let dir = TempDir::reserved("config_write");
        let path = dir.join("sub").join("config.toml");

        let written = Config::write_default(Some(path.clone()), false).unwrap();

        assert_eq!(path, written);
        assert_eq!(DEFAULT_CONFIG_BASE, fs::read_to_string(&path).unwrap());
    }

    #[test]
    fn write_default_themes_writes_the_theme_beside_the_config() {
        let dir = TempDir::reserved("config_write_theme");
        let config = dir.join("mine.toml");

        let written = Config::write_default_themes(Some(config), false).unwrap();

        assert_eq!(dir.join(DEFAULT_THEME_FILENAME), written);
        assert_eq!(DEFAULT_THEME, fs::read_to_string(&written).unwrap());
    }

    #[test]
    fn a_written_default_is_a_config_the_loader_accepts() {
        let dir = TempDir::reserved("config_round_trip");
        let config = Config::write_default(Some(dir.join("config.toml")), false).unwrap();
        let theme = Config::write_default_themes(Some(config.clone()), false).unwrap();

        Config::load(RuntimeEnv::default(), Some(config.clone()), &[]).unwrap();
        Config::load(RuntimeEnv::default(), Some(config), &[theme]).unwrap();
    }

    #[test]
    fn writing_a_default_refuses_to_replace_an_existing_file() {
        let dir = TempDir::new("config_no_clobber");
        let path = dir.join("config.toml");
        fs::write(&path, b"# hand written\n").unwrap();

        let error = Config::write_default(Some(path.clone()), false)
            .expect_err("an existing config must not be replaced")
            .to_string();

        assert!(error.contains("already exists; pass --force"), "{error}");
        assert_eq!("# hand written\n", fs::read_to_string(&path).unwrap());
    }

    #[test]
    fn force_replaces_an_existing_file() {
        let dir = TempDir::new("config_force");
        let path = dir.join("config.toml");
        fs::write(&path, b"# hand written\n").unwrap();

        Config::write_default(Some(path.clone()), true).unwrap();

        assert_eq!(DEFAULT_CONFIG_BASE, fs::read_to_string(&path).unwrap());
    }

    #[test]
    fn force_keeps_the_replaced_files_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("config_force_mode");
        let path = dir.join("config.toml");
        fs::write(&path, b"# hand written\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        Config::write_default(Some(path.clone()), true).unwrap();

        assert_eq!(
            0o600,
            fs::metadata(&path).unwrap().permissions().mode() & 0o777
        );
    }

    #[test]
    fn writing_a_default_refuses_to_follow_a_symlink() {
        let dir = TempDir::new("config_symlink");
        let target = dir.join("dotfiles.toml");
        let link = dir.join("config.toml");
        // Dangling, so only a check that does not follow the link finds it.
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let error = Config::write_default(Some(link.clone()), false)
            .expect_err("a symlink must not be written through")
            .to_string();

        assert_eq!(
            format!("Cannot write {}: it is a symbolic link", quoted(&link)),
            error
        );
        assert!(!target.exists());
    }

    #[test]
    fn force_keeps_the_old_file_when_the_new_one_cannot_be_written() {
        let dir = TempDir::new("config_force_failed");
        let path = dir.join("config.toml");
        fs::write(&path, "old").unwrap();
        let mut staged = path.as_os_str().to_owned();
        staged.push(format!(".{}.tmp", std::process::id()));
        fs::create_dir(&staged).unwrap();

        let error = write_new(&path, "new", true)
            .expect_err("the staged name is taken")
            .to_string();

        assert!(error.starts_with("Failed to replace"), "{error}");
        assert_eq!("old", fs::read_to_string(&path).unwrap());
        assert!(Path::new(&staged).is_dir());
    }

    /// Dangling, so only a check that does not follow the link finds it.
    #[test]
    fn force_refuses_a_symlink() {
        let dir = TempDir::new("config_force_symlink");
        let target = dir.join("dotfiles.toml");
        let link = dir.join("config.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let error = Config::write_default(Some(link.clone()), true)
            .expect_err("a symlink must be refused even with --force")
            .to_string();

        assert_eq!(
            format!("Cannot write {}: it is a symbolic link", quoted(&link)),
            error
        );
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(!target.exists());
    }

    #[test_case(false, false ; "a directory")]
    #[test_case(true, false ; "a directory with force")]
    #[test_case(false, true ; "a fifo")]
    #[test_case(true, true ; "a fifo with force")]
    fn writing_a_default_refuses_what_is_not_a_regular_file(force: bool, fifo: bool) {
        let dir = TempDir::new("config_not_a_file");
        let path = dir.join("config.toml");
        if fifo {
            nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        } else {
            fs::create_dir(&path).unwrap();
        }

        let error = Config::write_default(Some(path.clone()), force)
            .expect_err("only a regular file is replaced")
            .to_string();

        let what = if fifo {
            "not a regular file"
        } else {
            "a directory"
        };
        assert_eq!(
            format!("Cannot write {}: it is {what}", quoted(&path)),
            error
        );
        let file_type = path.symlink_metadata().unwrap().file_type();
        assert!(if fifo {
            !file_type.is_file() && !file_type.is_dir()
        } else {
            file_type.is_dir()
        });
    }

    #[test]
    fn force_writes_a_missing_file() {
        let dir = TempDir::new("config_force_missing");
        let path = dir.join("config.toml");

        Config::write_default(Some(path.clone()), true).unwrap();

        assert_eq!(DEFAULT_CONFIG_BASE, fs::read_to_string(&path).unwrap());
    }

    // Loading, and what a bad path reports.

    /// Loads a config expected to fail; `Config` is not `Debug`.
    fn load_err(config_path: Option<PathBuf>, includes: &[PathBuf]) -> String {
        match Config::load(RuntimeEnv::default(), config_path, includes) {
            Ok(_) => panic!("expected the load to fail"),
            Err(error) => format!("{error:#}"),
        }
    }

    #[test]
    fn a_missing_config_path_is_reported_by_name() {
        let dir = TempDir::reserved("config_missing");
        let path = dir.join("absent.toml");

        let error = load_err(Some(path.clone()), &[]);

        assert!(error.starts_with("Failed to read config file"), "{error}");
        assert!(error.contains(&quoted(&path).to_string()), "{error}");
    }

    #[test]
    fn a_missing_default_config_falls_back_to_the_built_in_one() {
        let dir = TempDir::reserved("config_default_missing");

        let config =
            Config::load_from(RuntimeEnv::default(), &dir.join("config.toml"), true, &[]).unwrap();

        assert_eq!(dir.path(), config.config_dir);
        assert!(select_next_key(&config, 'j'));
    }

    #[test]
    fn a_dangling_default_config_symlink_is_an_error() {
        let dir = TempDir::new("config_default_dangling");
        let path = dir.join("config.toml");
        std::os::unix::fs::symlink(dir.join("missing.toml"), &path).unwrap();

        let error = match Config::load_from(RuntimeEnv::default(), &path, true, &[]) {
            Ok(_) => panic!("expected the load to fail"),
            Err(error) => error.to_string(),
        };

        assert!(
            error.starts_with(&format!("Failed to read config file {}:", quoted(&path))),
            "{error}"
        );
    }

    #[test]
    fn an_unreadable_default_config_is_an_error() {
        let dir = TempDir::new("config_default_unreadable");
        let file = dir.join("file");
        fs::write(&file, b"").unwrap();
        // ENOTDIR rather than ENOENT.
        let path = file.join("config.toml");

        let error = match Config::load_from(RuntimeEnv::default(), &path, true, &[]) {
            Ok(_) => panic!("expected the load to fail"),
            Err(error) => error.to_string(),
        };

        assert!(
            error.starts_with(&format!("Failed to read config file {}:", quoted(&path))),
            "{error}"
        );
    }

    #[test]
    fn a_missing_include_path_is_reported_by_name() {
        let dir = TempDir::new("config_missing_include");
        let config = dir.join("config.toml");
        fs::write(&config, b"").unwrap();
        let include = dir.join("absent.toml");

        let error = load_err(Some(config), std::slice::from_ref(&include));

        assert!(error.starts_with("Failed to read include file"), "{error}");
        assert!(error.contains(&quoted(&include).to_string()), "{error}");
    }

    /// A FIFO, or `/dev/null` for a device: it parses as a valid empty config, so
    /// only the type check refuses it.
    fn not_a_regular_file(dir: &TempDir, fifo: bool) -> PathBuf {
        if fifo {
            let path = dir.join("fifo.toml");
            nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap();
            path
        } else {
            PathBuf::from("/dev/null")
        }
    }

    #[test_case(true ; "a fifo")]
    #[test_case(false ; "a device")]
    fn a_config_that_is_not_a_regular_file_is_refused(fifo: bool) {
        let dir = TempDir::new("config_not_regular");
        let path = not_a_regular_file(&dir, fifo);

        let error = load_err(Some(path.clone()), &[]);

        assert_eq!(
            format!(
                "Cannot read config file {}: not a regular file",
                quoted(&path)
            ),
            error
        );
    }

    #[test_case(true ; "a fifo")]
    #[test_case(false ; "a device")]
    fn an_include_that_is_not_a_regular_file_is_refused(fifo: bool) {
        let dir = TempDir::new("config_include_not_regular");
        let config = dir.join("config.toml");
        fs::write(&config, b"").unwrap();
        let include = not_a_regular_file(&dir, fifo);

        let error = load_err(Some(config), std::slice::from_ref(&include));

        assert_eq!(
            format!(
                "Cannot read include file {}: not a regular file",
                quoted(&include)
            ),
            error
        );
    }

    #[test]
    fn a_symlinked_include_file_is_read() {
        let dir = TempDir::new("config_include_symlink");
        let config = dir.join("config.toml");
        fs::write(&config, b"").unwrap();
        let target = binds_select_next(&dir, "target.toml", 'a');
        let link = dir.join("link.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[link]).unwrap();

        assert!(select_next_key(&merged, 'a'));
    }

    #[test]
    fn malformed_toml_is_reported() {
        let error = parse_err("this is not toml =\n");
        assert!(error.starts_with("Failed to parse TOML"), "{error}");
    }

    // Precedence: `select_next` defaults to `j`; the key bound to it names the
    // layer that won.

    fn binds_select_next(dir: &TempDir, name: &str, key: char) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("[keybindings]\nselect_next = \"{key}\"\n")).unwrap();
        path
    }

    fn select_next_key(config: &Config, key: char) -> bool {
        config
            .keybindings
            .normal_action(KeyCode::Char(key), KeyModifiers::NONE)
            == Some(Action::SelectNext)
    }

    #[test]
    fn a_cli_include_overrides_the_config_it_is_merged_onto() {
        let dir = TempDir::new("config_precedence_cli");
        let config = binds_select_next(&dir, "config.toml", 'u');
        let include = binds_select_next(&dir, "over.toml", 'a');

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[include]).unwrap();

        assert!(select_next_key(&merged, 'a'));
        assert!(!select_next_key(&merged, 'u'));
    }

    #[test]
    fn the_last_cli_include_wins() {
        let dir = TempDir::new("config_precedence_order");
        let config = binds_select_next(&dir, "config.toml", 'u');
        let first = binds_select_next(&dir, "first.toml", 'a');
        let second = binds_select_next(&dir, "second.toml", '1');

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[first, second]).unwrap();

        assert!(select_next_key(&merged, '1'));
    }

    #[test]
    fn a_configs_own_include_files_override_the_config_that_lists_them() {
        let dir = TempDir::new("config_precedence_listed");
        let listed = binds_select_next(&dir, "listed.toml", 'a');
        let config = dir.join("config.toml");
        fs::write(
            &config,
            format!(
                "include_files = [\"{}\"]\n[keybindings]\nselect_next = \"u\"\n",
                listed.display()
            ),
        )
        .unwrap();

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[]).unwrap();

        assert!(select_next_key(&merged, 'a'));
    }

    #[test]
    fn a_cli_include_overrides_the_configs_own_include_files() {
        let dir = TempDir::new("config_precedence_both");
        let listed = binds_select_next(&dir, "listed.toml", 'a');
        let cli = binds_select_next(&dir, "cli.toml", '1');
        let config = dir.join("config.toml");
        fs::write(
            &config,
            format!("include_files = [\"{}\"]\n", listed.display()),
        )
        .unwrap();

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[cli]).unwrap();

        assert!(select_next_key(&merged, '1'));
    }

    #[test]
    fn an_explicit_config_replaces_the_default_rather_than_merging_with_it() {
        let dir = TempDir::new("config_explicit");
        let config = dir.join("other.toml");
        fs::write(&config, b"[keybindings]\nselect_previous = \"a\"\n").unwrap();

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[]).unwrap();

        assert!(select_next_key(&merged, 'j'));
        assert_eq!(
            Some(Action::SelectPrevious),
            merged
                .keybindings
                .normal_action(KeyCode::Char('a'), KeyModifiers::NONE)
        );
    }

    #[test]
    fn an_include_cycle_is_broken_rather_than_recursing_forever() {
        let dir = TempDir::new("config_cycle");
        let a = dir.join("a.toml");
        let b = dir.join("b.toml");
        fs::write(
            &a,
            format!(
                "include_files = [\"{}\"]\n[keybindings]\nselect_next = \"a\"\n",
                b.display()
            ),
        )
        .unwrap();
        fs::write(&b, format!("include_files = [\"{}\"]\n", a.display())).unwrap();

        let merged = Config::load(RuntimeEnv::default(), Some(a), &[]).unwrap();

        assert!(select_next_key(&merged, 'a'));
    }

    #[test]
    fn an_include_cycle_back_to_the_config_keeps_the_include_on_top() {
        let dir = TempDir::new("config_cycle_main");
        let config = dir.join("config.toml");
        let include = dir.join("include.toml");
        fs::write(
            &config,
            format!(
                "include_files = [\"{}\"]\n[keybindings]\nselect_next = \"a\"\n",
                include.display()
            ),
        )
        .unwrap();
        fs::write(
            &include,
            format!(
                "include_files = [\"{}\"]\n[keybindings]\nselect_next = \"1\"\n",
                config.display()
            ),
        )
        .unwrap();

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[]).unwrap();

        assert!(select_next_key(&merged, '1'));
        assert!(!select_next_key(&merged, 'a'));
    }

    #[test]
    fn a_relative_include_file_resolves_from_the_file_that_lists_it() {
        let dir = TempDir::new("config_relative_include");
        fs::create_dir(dir.join("sub")).unwrap();
        let config = dir.join("config.toml");
        fs::write(&config, "include_files = [\"sub/listed.toml\"]\n").unwrap();
        // Resolved from `sub/`: only it holds `nested.toml`.
        fs::write(
            dir.join("sub").join("listed.toml"),
            "include_files = [\"nested.toml\"]\n",
        )
        .unwrap();
        binds_select_next(&dir, "sub/nested.toml", 'a');

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[]).unwrap();

        assert!(select_next_key(&merged, 'a'));
    }

    /// Both directories hold a `nested.toml` binding different keys.
    #[test_case(true ; "a symlinked config file")]
    #[test_case(false ; "a symlinked include file")]
    fn a_symlinked_files_relative_include_resolves_from_the_links_directory(is_config: bool) {
        let dir = TempDir::new("config_symlinked_nested");
        fs::create_dir(dir.join("config")).unwrap();
        fs::create_dir(dir.join("dotfiles")).unwrap();
        let target = dir.join("dotfiles/listed.toml");
        fs::write(&target, "include_files = [\"nested.toml\"]\n").unwrap();
        binds_select_next(&dir, "config/nested.toml", 'a');
        binds_select_next(&dir, "dotfiles/nested.toml", '1');
        let link = dir.join("config/link.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let merged = if is_config {
            Config::load(RuntimeEnv::default(), Some(link), &[]).unwrap()
        } else {
            let config = dir.join("config/config.toml");
            fs::write(&config, b"").unwrap();
            Config::load(RuntimeEnv::default(), Some(config), &[link]).unwrap()
        };

        assert!(select_next_key(&merged, 'a'));
        assert!(!select_next_key(&merged, '1'));
    }

    #[test]
    fn a_malformed_include_file_is_reported_by_name() {
        let dir = TempDir::new("config_malformed_include");
        let config = dir.join("config.toml");
        fs::write(&config, "include_files = [\"good.toml\", \"bad.toml\"]\n").unwrap();
        fs::write(dir.join("good.toml"), "log_level = \"warn\"\n").unwrap();
        fs::write(dir.join("bad.toml"), "this is not toml =\n").unwrap();

        let error = load_err(Some(config), &[]);

        // Includes resolve from the canonical directory (a symlink on macOS).
        let bad = dir.path().canonicalize().unwrap().join("bad.toml");
        let expected = format!("Failed to parse {}: ", quoted(&bad));
        assert!(error.starts_with(&expected), "{error}");
    }

    #[test_case("xdg-open %s" => true ; "a word of its own")]
    #[test_case("%s" => true ; "the whole template")]
    #[test_case("cd %s && exec xterm" => true ; "before an operator")]
    #[test_case("(cd %s)" => true ; "inside a subshell")]
    #[test_case("xdg-open" => false ; "absent")]
    #[test_case("xdg-open \"%s\"" => false ; "in double quotes")]
    #[test_case("xdg-open '%s'" => false ; "in single quotes")]
    #[test_case("xdg-open --file=%s" => false ; "part of a word")]
    #[test_case("xdg-open %sx" => false ; "followed by more of its word")]
    #[test_case("echo \\%s" => false ; "escaped")]
    #[test_case("echo '\"' %s" => true ; "after a quoted double quote")]
    fn an_opener_needs_an_unquoted_placeholder(template: &str) -> bool {
        has_unquoted_placeholder(template)
    }

    #[test]
    fn a_blank_opener_needs_no_placeholder() {
        let toml = "[openers.linux]\nrun_in_terminal = \"\"\n[openers.macos]\nopen_file = \" \"\n";
        assert!(Config::parse(RuntimeEnv::default(), None, toml, &inert_dir(), &[]).is_ok());
    }

    #[test_case(true, "[theme.table]\nbodyy = {}\n" => "Cannot load {}: Unknown configuration key: 'theme.table.bodyy'" ; "an unknown key in the config")]
    #[test_case(false, "[theme.table]\nbodyy = {}\n" => "Cannot load {}: Unknown configuration key: 'theme.table.bodyy'" ; "an unknown key in an include")]
    #[test_case(false, "[theme256.table.body]\nfg = \"#ff0000\"\n" => "Cannot load {}: theme256.table.body.fg: \"#ff0000\" is not a 256-color index (0-255)" ; "a hex color under theme256 in an include")]
    #[test_case(false, "[file_system]\nsearch_max_depth = \"deep\"\n" => "Cannot load {}: invalid type: string \"deep\", expected u32 in `file_system.search_max_depth`" ; "a type error in an include")]
    #[test_case(false, "[keybindings]\nquit = 1\n" => "Cannot load {}: expected a key string or an array of key strings in `keybindings.quit`" ; "a key that is not a string in an include")]
    #[test_case(false, "include_files = \"c.toml\"\n" => "Cannot load {}: 'include_files' must be an array of file paths" ; "an include list that is not an array in an include")]
    #[test_case(false, "include_files = [1]\n" => "Cannot load {}: 'include_files' entries must be strings, but found: 1" ; "an include entry that is not a string in an include")]
    #[test_case(false, "[file_system]\nrefresh_debounce_milliseconds = 50\n" => "Cannot load {}: file_system.refresh_debounce_milliseconds (50) must be at least 100" ; "a value out of range in an include")]
    #[test_case(false, "[keybindings]\nquit = \"Nope\"\n" => "Cannot load {}: Invalid keybinding for quit: Unknown key: 'Nope'" ; "an invalid key in an include")]
    #[test_case(false, "[keybindings]\nquit = []\n" => "Cannot load {}: Invalid keybinding for quit: no key given" ; "an empty key list in an include")]
    #[test_case(false, "[openers.linux]\nopen_file = \"xdg-open\"\n" => "Cannot load {}: openers.linux.open_file (\"xdg-open\") must contain %s as its own unquoted word" ; "an opener without its placeholder in an include")]
    fn a_mistake_names_the_file_it_is_in(in_config: bool, mistake: &str) -> String {
        let dir = TempDir::new("config_mistake_names_file");
        let config = dir.join("config.toml");
        let includes = "include_files = [\"good.toml\", \"bad.toml\"]\n";
        let config_content = if in_config {
            format!("{includes}{mistake}")
        } else {
            includes.to_string()
        };
        fs::write(&config, config_content).unwrap();
        fs::write(dir.join("good.toml"), "log_level = \"warn\"\n").unwrap();
        fs::write(dir.join("bad.toml"), if in_config { "" } else { mistake }).unwrap();

        let error = load_err(Some(config.clone()), &[]);

        // Includes resolve from the canonical directory (a symlink on macOS).
        let named = if in_config {
            config
        } else {
            dir.path().canonicalize().unwrap().join("bad.toml")
        };
        error.replacen(&quoted(&named).to_string(), "{}", 1)
    }

    #[test]
    fn an_empty_config_path_is_quoted_in_its_error() {
        let error = load_err(Some(PathBuf::new()), &[]);

        assert_eq!(
            "Failed to resolve \"\": cannot make an empty path absolute",
            error
        );
    }

    #[test]
    fn a_relative_config_path_resolves_from_the_working_directory() {
        let name = "filectrl-absent-config-xyz.toml";

        let error = load_err(Some(PathBuf::from(name)), &[]);

        let absolute = std::env::current_dir().unwrap().join(name);
        assert!(
            error.starts_with(&format!(
                "Failed to read config file {}:",
                quoted(&absolute)
            )),
            "{error}"
        );
    }

    /// Runs every path-carrying default opener against a directory named to
    /// break a shell, with stubs recording what each program received.
    #[test]
    fn the_default_openers_pass_a_hostile_name_through_intact() {
        use std::os::unix::ffi::OsStrExt;

        use crate::file_system::shell;

        let dir = TempDir::new("config_openers");
        let bin = dir.join("bin");
        fs::create_dir(&bin).unwrap();
        for program in ["xterm", "xdg-open", "osascript", "open"] {
            crate::test_support::write_executable(
                &bin.join(program),
                "#!/bin/sh\nprintf '%s\\0' \"$(pwd)\" \"$@\" > \"$STUB_OUT\"\n",
            );
        }
        let target = dir.join("it's a $(touch pwned);\"x\"");
        fs::create_dir(&target).unwrap();
        let out = dir.join("out");

        let value = parse_toml(None, DEFAULT_CONFIG_BASE).unwrap();
        let openers: PlatformOpeners = value.get("openers").unwrap().clone().try_into().unwrap();
        for openers in [&openers.linux, &openers.macos] {
            for template in [
                &openers.open_directory,
                &openers.open_file,
                &openers.open_filectrl_window,
            ] {
                let _ = fs::remove_file(&out);
                let argv = shell::command(template, [target.as_os_str().to_os_string()]);
                // `PATH` holds only the stubs.
                let status = std::process::Command::new("/bin/sh")
                    .args(&argv[1..])
                    .current_dir(dir.path())
                    .env("PATH", &bin)
                    .env("STUB_OUT", &out)
                    .status()
                    .unwrap();
                assert!(status.success(), "{template:?} failed");

                let recorded = fs::read(&out).unwrap();
                let fields: Vec<&[u8]> = recorded.split(|&byte| byte == 0).collect();
                // The working directory for `cd %s && exec xterm`, an argument otherwise.
                assert!(
                    fields.contains(&target.as_os_str().as_bytes()),
                    "{template:?} did not receive the path intact: {:?}",
                    String::from_utf8_lossy(&recorded)
                );
                assert!(!dir.join("pwned").exists(), "{template:?} ran the name");
                assert!(!target.join("pwned").exists(), "{template:?} ran the name");
            }
        }
    }
}
