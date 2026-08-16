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
    pub search_max_depth: u32,
    pub search_max_results: u32,
}

#[derive(Debug, Deserialize)]
pub struct Openers {
    pub open_directory: String,
    pub open_file: String,
    pub open_filectrl_window: String,
    /// Wraps a command that needs a terminal, so that a desktop entry marked
    /// `Terminal=true` can be offered by the "open with" picker. Unlike the
    /// other openers, `%s` is replaced by a command line rather than a path.
    pub run_in_terminal: String,
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
    // An assert! here would fire in tests and in release alike, and the point
    // of the cfg is that it must do neither.
    #[allow(clippy::manual_assert)]
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

    /// A `Config` built from the embedded defaults alone. Tests must not read
    /// the host's `~/.config/filectrl/config.toml`: a developer who changes a
    /// `[ui]` default there would otherwise see unrelated tests fail.
    ///
    /// `config_dir` exists only so paths derived from it (notably
    /// `bookmarks_dir`) resolve somewhere inert. The `TempDir` guard is dropped
    /// rather than held: the path is reserved, never created, so there is
    /// nothing to remove, and reserving keeps it unique across concurrent runs.
    ///
    /// A test that needs a config directory on disk should own a `TempDir` and
    /// set `config_dir` from it, as `app::claims` does.
    #[cfg(test)]
    pub(crate) fn builtin() -> Self {
        let config_dir = crate::test_support::TempDir::reserved("config")
            .path()
            .to_path_buf();
        Self::parse(RuntimeEnv::default(), "", Some(config_dir), &[])
            .expect("the embedded default config should parse")
    }

    /// Initializes the process-global config from [`Config::builtin`]. Tests
    /// share one global config, so the first call wins and the rest are
    /// no-ops; every test that reaches `Config::global` calls this first.
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
        let Some(path) = config_path else {
            return Self::try_from_default_path(env, include_paths);
        };

        // Absolutize so `parent()` yields the real containing directory.
        // For a bare filename like `--config config.toml`, `parent()` would
        // return `Some("")`, making `config_dir` empty and every path derived
        // from it (bookmarks, relative includes) CWD-relative by accident.
        let path = absolute_path(&path)?;

        debug!("Loading config from user-provided path: {}", path.display());
        match fs::read_to_string(&path) {
            Ok(content) => Self::parse(
                env,
                &content,
                path.parent().map(std::path::Path::to_path_buf),
                include_paths,
            ),
            Err(error) => Err(anyhow!(
                "Failed to read config file {}: {error}",
                path.display()
            )),
        }
    }

    fn default_config_dir() -> Result<PathBuf> {
        Ok(ProjectDirs::from("", "", "filectrl")
            .ok_or_else(|| anyhow!("Cannot determine the config directory"))?
            .config_dir()
            .to_path_buf())
    }

    fn default_path() -> Result<PathBuf> {
        Ok(Self::default_config_dir()?.join(CONFIG_RELATIVE_PATH))
    }

    /// The config file the CLI is acting on: the one `--config` names, or the
    /// default. Absolutized, so that what is reported back is the file that was
    /// touched rather than the argument as it was typed.
    fn target_path(config_path: Option<PathBuf>) -> Result<PathBuf> {
        match config_path {
            Some(path) => absolute_path(&path),
            None => Self::default_path(),
        }
    }

    /// Writes the config keys only. The theme is a separate file written by
    /// [`Config::write_default_themes`], so that the two flags produce two
    /// files that do not restate each other.
    pub fn write_default(config_path: Option<PathBuf>, force: bool) -> Result<PathBuf> {
        let path = Self::target_path(config_path)?;
        write_new(&path, DEFAULT_CONFIG_BASE, force)?;
        info!("Wrote the default config to {}", path.display());
        Ok(path)
    }

    /// Writes the theme beside the config, where a relative `include_files`
    /// entry resolves from.
    pub fn write_default_themes(config_path: Option<PathBuf>, force: bool) -> Result<PathBuf> {
        let config = Self::target_path(config_path)?;
        let dir = config.parent().ok_or_else(|| {
            anyhow!(
                "Cannot write the theme: {} has no parent directory",
                config.display()
            )
        })?;
        let path = dir.join(DEFAULT_THEME_FILENAME);
        write_new(&path, DEFAULT_THEME, force)?;
        info!("Wrote the default theme to {}", path.display());
        Ok(path)
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

    /// Resolves the config's `include_files` array. Relative entries resolve
    /// against `config_dir`, falling back to the default config directory, and
    /// error if neither is available rather than silently resolving against the
    /// CWD, as `parse_value` does. Defensive: the fallback is unreachable, since
    /// `config_dir == None` only follows a successful `default_config_dir()`.
    fn resolve_include_files(value: &Value, config_dir: Option<&Path>) -> Result<Vec<PathBuf>> {
        let Some(include_value) = value.get("include_files") else {
            return Ok(Vec::new());
        };
        // A malformed value must fail the load rather than silently yielding
        // no includes.
        let entries = include_value
            .as_array()
            .ok_or_else(|| anyhow!("'include_files' must be an array of file paths"))?;
        let include_files: Vec<PathBuf> = entries
            .iter()
            .map(|entry| {
                entry.as_str().map(PathBuf::from).ok_or_else(|| {
                    anyhow!("'include_files' entries must be strings, but found: {entry}")
                })
            })
            .collect::<Result<_>>()?;

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
            .map_err(|error| anyhow!("Failed to deserialize the config: {error}"))?;

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
        // Both themes are built, since only `theme()` decides which one is
        // read, but the RGB warning is about how this run will actually
        // render: an RGB entry cannot misrender on a truecolor terminal, whose
        // `theme256` is never consulted.
        let warn_on_rgb = !env.is_truecolor;
        config
            .theme
            .file_type
            .maybe_apply_ls_colors(env.ls_colors, false);
        config
            .theme256
            .file_type
            .maybe_apply_ls_colors(env.ls_colors, warn_on_rgb);
        Ok(config)
    }

    fn try_from_default_path(env: RuntimeEnv<'_>, include_paths: &[PathBuf]) -> Result<Self> {
        let default_path = Self::default_path()?;
        debug!(
            "Attempting to load the config from the default path: {}",
            default_path.display()
        );

        match fs::read_to_string(&default_path) {
            Ok(content) => Self::parse(
                env,
                &content,
                default_path.parent().map(std::path::Path::to_path_buf),
                include_paths,
            ),
            Err(err) if err.kind() == ErrorKind::NotFound => {
                debug!("No config file found, using the built-in config");
                Self::parse(env, "", None, include_paths)
            }
            Err(error) => Err(anyhow!(
                "Failed to read config file {}: {error}",
                default_path.display()
            )),
        }
    }
}

fn parse_toml(content: &str) -> Result<Value> {
    toml::from_str::<Value>(content).map_err(|error| anyhow!("Failed to parse TOML: {error}"))
}

/// Absolutizes without requiring the path to exist, which `canonicalize` does.
fn absolute_path(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path)
        .map_err(|error| anyhow!("Failed to resolve {}: {error}", path.display()))
}

/// Writes `content` to `path`, creating the parent directory. Refuses to
/// replace an existing file unless `force`, so that a hand-edited config is not
/// lost to a flag whose only other output is the path it wrote.
///
/// Existence is tested without following symlinks: a config symlinked into a
/// dotfiles repository is a file to refuse, not one to write through.
fn write_new(path: &Path, content: &str, force: bool) -> Result<()> {
    if !force && path.symlink_metadata().is_ok() {
        return Err(anyhow!(
            "Cannot write {}: it already exists; pass --force to replace it",
            path.display()
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        anyhow!(
            "Cannot write {}: it has no parent directory",
            path.display()
        )
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| anyhow!("Failed to create directory {}: {error}", parent.display()))?;
    fs::write(path, content).map_err(|error| anyhow!("Failed to write {}: {error}", path.display()))
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
    // Fall back to the raw path if canonicalization fails: a missing file or
    // permission error will then surface from `fs::read_to_string` below with
    // a more informative message.
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    // Resolve this file's own include_files relative to its real directory.
    // Deriving the directory from the canonical (absolute, when canonicalize
    // succeeds) path means a bare filename (whose `parent()` is "") still
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
        .map_err(|error| anyhow!("Failed to read include file {}: {error}", path.display()))?;
    let include_value = parse_toml(&content)?;

    let nested = Config::resolve_include_files(&include_value, base_dir.as_deref())?;

    // Merge the file's content first, then its nested includes on top, the
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

/// Style properties that may appear on any style table. The embedded default
/// omits them where they are unset (`[theme.alert]` lists only `fg`), so they
/// are permitted on any table inside a theme rather than validated against the
/// default's shape, which would wrongly reject a user adding `bg` there.
const STYLE_KEYS: &[&str] = &["fg", "bg", "modifiers"];

/// Whether `path` names somewhere inside a theme, which is the only place a
/// style property belongs. Allowing the names everywhere would let `[ui] bg`
/// or a bare top-level `fg` load and then be dropped by deserialization, when
/// every other unrecognized key is an error.
fn is_theme_path(path: &str) -> bool {
    matches!(path.split('.').next(), Some("theme" | "theme256"))
}

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
        if is_theme_path(path) && STYLE_KEYS.contains(&key.as_str()) {
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
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use test_case::test_case;

    use super::{keybindings::Action, *};
    use crate::test_support::TempDir;

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
        // Set on both platforms, since `parse_value` picks the section by
        // target and the assertion below must hold on either.
        let partial = r#"
[openers.linux]
open_directory = "alacritty --working-directory %s"
[openers.macos]
open_directory = "alacritty --working-directory %s"
"#;
        let defaults = Config::parse(RuntimeEnv::default(), "", None, &[]).unwrap();
        let merged = Config::parse(RuntimeEnv::default(), partial, None, &[]).unwrap();

        // The named key is replaced, and the ones the partial does not mention
        // keep the built-in defaults rather than being blanked by the merge.
        assert_eq!(
            "alacritty --working-directory %s",
            merged.openers.open_directory
        );
        assert_eq!(defaults.openers.open_file, merged.openers.open_file);
        assert!(!merged.openers.open_file.is_empty());
    }

    /// Parse a config that is expected to fail, returning the error message.
    /// (`Config` is not `Debug`, so `unwrap_err` is unavailable.)
    fn parse_err(toml: &str) -> String {
        match Config::parse(RuntimeEnv::default(), toml, None, &[]) {
            Ok(_) => panic!("expected config parse to fail"),
            Err(error) => error.to_string(),
        }
    }

    /// Both files are embedded source rather than a re-serialized merge, so
    /// their inline documentation survives being written out, and each must
    /// round-trip through the loader on its own.
    #[test_case(DEFAULT_CONFIG_BASE ; "config")]
    #[test_case(DEFAULT_THEME ; "theme")]
    fn a_written_default_parses_and_keeps_its_comments(content: &str) {
        Config::parse(RuntimeEnv::default(), content, None, &[]).unwrap();
        assert!(content.contains('#'), "comments should be preserved");
    }

    /// The two write flags produce two files that do not restate each other:
    /// the theme keys belong to the theme file alone.
    #[test]
    fn the_default_config_and_theme_do_not_overlap() {
        assert!(
            !DEFAULT_CONFIG_BASE.contains("[theme"),
            "theme keys in config"
        );
        assert!(DEFAULT_THEME.contains("[theme"), "no theme keys in theme");
    }

    #[test_case("not_a_key = 1", "not_a_key" ; "top-level key")]
    #[test_case("[file_system]\nbuffer_max_byte = 1\n", "file_system.buffer_max_byte" ; "nested key (dotted path)")]
    #[test_case("[keybindings]\nserach = \"/\"\n", "serach" ; "keybinding name")]
    // A style property is only a style property inside a theme. Elsewhere it
    // deserializes to nothing, so accepting it would drop it silently while
    // every neighbouring typo is an error.
    #[test_case("fg = \"#ff0000\"\n", "fg" ; "style name at the top level")]
    #[test_case("[ui]\nbg = 42\n", "ui.bg" ; "style name in a non-theme table")]
    #[test_case("[file_system]\nmodifiers = [\"bold\"]\n", "file_system.modifiers" ; "modifiers in a non-theme table")]
    fn unknown_key_is_rejected(toml: &str, expected: &str) {
        let err = parse_err(toml);
        assert!(err.contains(expected), "error should name the key: {err}");
    }

    #[test_case("[theme.alert]\nbg = \"#000000\"\nmodifiers = [\"bold\"]\n" ; "nested theme table")]
    #[test_case("[theme256.alert]\nbg = \"#000000\"\n" ; "nested theme256 table")]
    #[test_case("[theme]\nfg = \"#ffffff\"\n" ; "theme root")]
    fn style_property_absent_from_default_is_accepted(toml: &str) {
        // The default `[theme.alert]` lists only `fg`; adding `bg`/`modifiers`
        // must not be mistaken for an unknown key.
        Config::parse(RuntimeEnv::default(), toml, None, &[]).unwrap();
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

    #[test_case("[file_system]\nbuffer_min_bytes = 200\nbuffer_max_bytes = 100\n" ; "min exceeds max")]
    #[test_case("[file_system]\nbuffer_min_bytes = 0\n" ; "min is zero")]
    fn invalid_buffer_sizes_are_rejected(toml: &str) {
        let err = parse_err(toml);
        assert!(
            err.contains("buffer_min_bytes"),
            "error should explain the invariant: {err}"
        );
    }

    // ── writing the defaults ────────────────────────────────────────────────
    //
    // Always through an explicit path: `None` resolves to the real user config
    // directory, which a test must never write to.

    #[test]
    fn write_default_writes_the_config_and_reports_where() {
        let dir = TempDir::reserved("config_write");
        let path = dir.join("sub").join("config.toml");

        let written = Config::write_default(Some(path.clone()), false).unwrap();

        // The absolute path is reported because the config directory follows
        // $XDG_CONFIG_HOME, so the user cannot infer it from the flag alone.
        assert_eq!(path, written);
        assert_eq!(DEFAULT_CONFIG_BASE, fs::read_to_string(&path).unwrap());
    }

    #[test]
    fn write_default_themes_writes_the_theme_beside_the_config() {
        let dir = TempDir::reserved("config_write_theme");
        let config = dir.join("mine.toml");

        let written = Config::write_default_themes(Some(config), false).unwrap();

        // Beside it, so a relative `include_files` entry resolves.
        assert_eq!(dir.join(DEFAULT_THEME_FILENAME), written);
        assert_eq!(DEFAULT_THEME, fs::read_to_string(&written).unwrap());
    }

    #[test]
    fn a_written_default_is_a_config_the_loader_accepts() {
        let dir = TempDir::reserved("config_round_trip");
        let config = Config::write_default(Some(dir.join("config.toml")), false).unwrap();
        let theme = Config::write_default_themes(Some(config.clone()), false).unwrap();

        // Both files have to load, separately and together: the theme is
        // includable precisely because it does not restate the config.
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
    fn writing_a_default_refuses_to_follow_a_symlink() {
        let dir = TempDir::new("config_symlink");
        let real = dir.join("real.toml");
        let link = dir.join("config.toml");
        fs::write(&real, b"# hand written\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // A config symlinked into a dotfiles repository is a file to refuse,
        // not one to write through: `create_new` would follow it and replace
        // the checked-in file instead.
        assert!(Config::write_default(Some(link), false).is_err());
        assert_eq!("# hand written\n", fs::read_to_string(&real).unwrap());
    }

    // ── loading, and what a bad path reports ────────────────────────────────

    /// Load a config that is expected to fail, returning the error message.
    /// (`Config` is not `Debug`, so `unwrap_err` is unavailable.)
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
        assert!(error.contains(&path.display().to_string()), "{error}");
    }

    #[test]
    fn a_missing_include_path_is_reported_by_name() {
        let dir = TempDir::new("config_missing_include");
        let config = dir.join("config.toml");
        fs::write(&config, b"").unwrap();
        let include = dir.join("absent.toml");

        let error = load_err(Some(config), std::slice::from_ref(&include));

        // Named as an include rather than as the config, so the user knows
        // which of the two files to go and look at.
        assert!(error.starts_with("Failed to read include file"), "{error}");
        assert!(error.contains(&include.display().to_string()), "{error}");
    }

    #[test]
    fn malformed_toml_is_reported() {
        assert!(
            parse_err("this is not toml =\n").starts_with("Failed to parse TOML"),
            "{}",
            parse_err("this is not toml =\n")
        );
    }

    // ── precedence, lowest to highest ───────────────────────────────────────
    //
    // `select_next` defaults to `j`, and `e`, `i` and `u` are unbound, so
    // whichever key ends up on SelectNext names the layer that won.

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
        let include = binds_select_next(&dir, "over.toml", 'e');

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[include]).unwrap();

        assert!(select_next_key(&merged, 'e'));
        assert!(!select_next_key(&merged, 'u'));
    }

    #[test]
    fn the_last_cli_include_wins() {
        let dir = TempDir::new("config_precedence_order");
        let config = binds_select_next(&dir, "config.toml", 'u');
        let first = binds_select_next(&dir, "first.toml", 'e');
        let second = binds_select_next(&dir, "second.toml", 'i');

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[first, second]).unwrap();

        assert!(select_next_key(&merged, 'i'));
    }

    #[test]
    fn a_configs_own_include_files_override_the_config_that_lists_them() {
        let dir = TempDir::new("config_precedence_listed");
        let listed = binds_select_next(&dir, "listed.toml", 'e');
        let config = dir.join("config.toml");
        // `include_files` is a top-level key, so it precedes the first table.
        fs::write(
            &config,
            format!(
                "include_files = [\"{}\"]\n[keybindings]\nselect_next = \"u\"\n",
                listed.display()
            ),
        )
        .unwrap();

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[]).unwrap();

        assert!(select_next_key(&merged, 'e'));
    }

    #[test]
    fn a_cli_include_overrides_the_configs_own_include_files() {
        let dir = TempDir::new("config_precedence_both");
        let listed = binds_select_next(&dir, "listed.toml", 'e');
        let cli = binds_select_next(&dir, "cli.toml", 'i');
        let config = dir.join("config.toml");
        fs::write(
            &config,
            format!("include_files = [\"{}\"]\n", listed.display()),
        )
        .unwrap();

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[cli]).unwrap();

        assert!(select_next_key(&merged, 'i'));
    }

    #[test]
    fn an_explicit_config_replaces_the_default_rather_than_merging_with_it() {
        let dir = TempDir::new("config_explicit");
        let config = dir.join("other.toml");
        fs::write(&config, b"[keybindings]\nselect_previous = \"e\"\n").unwrap();

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[]).unwrap();

        // Only the built-in defaults sit under it, so `select_next` keeps `j`.
        assert!(select_next_key(&merged, 'j'));
        assert_eq!(
            Some(Action::SelectPrevious),
            merged
                .keybindings
                .normal_action(KeyCode::Char('e'), KeyModifiers::NONE)
        );
    }

    #[test]
    fn an_include_cycle_is_broken_rather_than_recursing_forever() {
        let dir = TempDir::new("config_cycle");
        let a = dir.join("a.toml");
        let b = dir.join("b.toml");
        // Each file includes the other; the visited set is what ends this.
        fs::write(
            &a,
            format!(
                "include_files = [\"{}\"]\n[keybindings]\nselect_next = \"e\"\n",
                b.display()
            ),
        )
        .unwrap();
        fs::write(&b, format!("include_files = [\"{}\"]\n", a.display())).unwrap();

        let merged = Config::load(RuntimeEnv::default(), Some(a), &[]).unwrap();

        assert!(select_next_key(&merged, 'e'));
    }
}
