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

#[derive(Clone, Copy, Debug, Deserialize)]
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
        Self::parse(RuntimeEnv::default(), None, "", &config_dir, &[])
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
        let is_default = config_path.is_none();
        Self::load_from(
            env,
            &Self::target_path(config_path)?,
            is_default,
            include_paths,
        )
    }

    /// `path` is absolute, so its parent is the real containing directory and
    /// never the empty path a bare filename has, which would leave bookmarks
    /// and relative includes resolving from the working directory.
    ///
    /// `is_default` is whether it is the default path rather than one
    /// `--config` named, and so whether its absence means the built-in config
    /// rather than an error.
    fn load_from(
        env: RuntimeEnv<'_>,
        path: &Path,
        is_default: bool,
        include_paths: &[PathBuf],
    ) -> Result<Self> {
        debug!("Loading the config from {}", path.display());
        let (config_file, content) = match read_regular_file(path) {
            Ok(content) => (Some(path), content),
            Err(ReadFailure::Io(error)) if is_default && error.kind() == ErrorKind::NotFound => {
                debug!("No config file found, using the built-in config");
                (None, String::new())
            }
            Err(failure) => return Err(failure.describe(CONFIG_FILE, path)),
        };
        // Unreachable: only `/` has no parent, and it is not a regular file.
        let config_dir = path.parent().ok_or_else(|| {
            anyhow!(
                "Cannot load config file {}: it has no parent directory",
                path.display()
            )
        })?;
        // Canonical, so that a `..` or `.` in the path does not reach the
        // bookmark paths derived from it, which a clipboard entry must not hold.
        let config_dir = canonical_or_raw(config_dir);
        Self::parse(env, config_file, &content, &config_dir, include_paths)
    }

    fn default_path() -> Result<PathBuf> {
        Ok(ProjectDirs::from("", "", "filectrl")
            .ok_or_else(|| anyhow!("Cannot determine the config directory"))?
            .config_dir()
            .join(CONFIG_RELATIVE_PATH))
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

    /// `config_file` is the file `content` was read from, if any. An include
    /// cycle that leads back to it is broken there, rather than merging the
    /// config again on top of the files it included.
    fn parse(
        env: RuntimeEnv<'_>,
        config_file: Option<&Path>,
        content: &str,
        config_dir: &Path,
        include_paths: &[PathBuf],
    ) -> Result<Self> {
        // Precedence (low → high): built-in defaults → user config file →
        // include_files from the user config → CLI --include paths.
        let defaults = merge_default_config()?;
        let mut value = merge_toml_values(defaults.clone(), parse_toml(config_file, content)?);
        value = Self::merge_config_includes(config_file, value, config_dir)?;
        value = merge_include_paths(config_file, value, include_paths)?;
        // Reject typo'd / unknown keys before deserializing so a broken config
        // fails loudly instead of silently falling back to defaults.
        reject_unknown_keys(&value, &defaults, "")?;
        Self::parse_value(env, value, config_dir)
    }

    /// Resolves and merges files listed in the value's own `include_files`
    /// array. Relative entries resolve against `config_dir`.
    fn merge_config_includes(
        config_file: Option<&Path>,
        value: Value,
        config_dir: &Path,
    ) -> Result<Value> {
        let includes = Self::resolve_include_files(&value, config_dir)?;
        merge_include_paths(config_file, value, &includes)
    }

    /// The directory containing the resolved config file. Bookmarks live in a
    /// `bookmarks/` subdirectory beside it.
    pub fn bookmarks_dir(&self) -> PathBuf {
        self.config_dir.join("bookmarks")
    }

    /// Resolves the config's `include_files` array. Relative entries resolve
    /// against `config_dir`.
    fn resolve_include_files(value: &Value, config_dir: &Path) -> Result<Vec<PathBuf>> {
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

        Ok(include_files
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
            .map_err(|error| anyhow!("Failed to deserialize the config: {error}"))?;

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
}

/// `file` names the file `content` was read from, so that a mistake in one of
/// several included files says which.
fn parse_toml(file: Option<&Path>, content: &str) -> Result<Value> {
    toml::from_str::<Value>(content).map_err(|error| match file {
        Some(file) => anyhow!("Failed to parse {}: {error}", file.display()),
        None => anyhow!("Failed to parse TOML: {error}"),
    })
}

/// Absolutizes without requiring the path to exist, which `canonicalize` does.
fn absolute_path(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path)
        .map_err(|error| anyhow!("Failed to resolve {}: {error}", path.display()))
}

const CONFIG_FILE: &str = "config file";
const INCLUDE_FILE: &str = "include file";

/// Why a config or include file could not be read.
enum ReadFailure {
    Io(std::io::Error),
    NotRegular,
}

impl ReadFailure {
    /// `kind` names the role the file plays, so the user knows which of the
    /// files they passed to go and look at.
    fn describe(self, kind: &str, path: &Path) -> anyhow::Error {
        match self {
            Self::Io(error) => anyhow!("Failed to read {kind} {}: {error}", path.display()),
            Self::NotRegular => {
                anyhow!("Cannot read {kind} {}: not a regular file", path.display())
            }
        }
    }
}

/// Reads a file that must be a regular file, following symlinks. A FIFO would
/// block the read until a writer appears, and a device such as `/dev/zero`
/// would fill memory, so either is refused before anything is read.
///
/// Opened non-blocking, so that opening a FIFO does not itself wait for a
/// writer, and the type is taken from the open descriptor, so it is the type
/// of the file that is then read. `O_NOCTTY`, so that opening a terminal
/// device cannot make it the process's controlling terminal.
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

/// Writes `content` to `path`, creating the parent directory. Refuses to
/// replace an existing file unless `force`, so that a hand-edited config is not
/// lost to a flag whose only other output is the path it wrote.
///
/// The file is created exclusively (`O_CREAT | O_EXCL`), which fails on
/// anything already at `path`, a symlink included, in the same syscall that
/// creates it. `force` removes a file first but refuses a symlink, even a
/// dangling one: a config symlinked into a dotfiles repository is left alone
/// rather than replaced or written through to its target.
fn write_new(path: &Path, content: &str, force: bool) -> Result<()> {
    use std::io::Write;

    let parent = path.parent().ok_or_else(|| {
        anyhow!(
            "Cannot write {}: it has no parent directory",
            path.display()
        )
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| anyhow!("Failed to create directory {}: {error}", parent.display()))?;
    // A failed lookup leaves the path to `create_new`, which reports whatever
    // is there.
    if force && let Ok(metadata) = path.symlink_metadata() {
        if metadata.file_type().is_symlink() {
            return Err(anyhow!(
                "Cannot write {}: it is a symbolic link",
                path.display()
            ));
        }
        fs::remove_file(path)
            .map_err(|error| anyhow!("Failed to replace {}: {error}", path.display()))?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == ErrorKind::AlreadyExists {
                anyhow!(
                    "Cannot write {}: it already exists; pass --force to replace it",
                    path.display()
                )
            } else {
                anyhow!("Failed to write {}: {error}", path.display())
            }
        })?;
    file.write_all(content.as_bytes())
        .map_err(|error| anyhow!("Failed to write {}: {error}", path.display()))
}

/// Merges the given include files on top of an existing config value.
/// Each include file's own `include_files` are resolved (relative to that
/// file's directory) and merged recursively. A visited set keyed by
/// canonicalized path breaks cycles and skips duplicate includes. It starts
/// with `config_file`, which is already merged.
fn merge_include_paths(
    config_file: Option<&Path>,
    mut value: Value,
    include_paths: &[PathBuf],
) -> Result<Value> {
    let mut visited: HashSet<PathBuf> = config_file.map(canonical_or_raw).into_iter().collect();
    for path in include_paths {
        value = merge_include_file(value, path, &mut visited)?;
    }
    Ok(value)
}

fn canonical_or_raw(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn merge_include_file(value: Value, path: &Path, visited: &mut HashSet<PathBuf>) -> Result<Value> {
    // Canonicalize so the same file referenced via different paths is detected.
    // Fall back to the raw path if canonicalization fails: a missing file or
    // permission error will then surface from `read_regular_file` below with
    // a more informative message.
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

    // Resolve this file's own include_files relative to its real directory.
    // The file was just read, so canonicalization will have succeeded and the
    // path is absolute: a bare filename (whose `parent()` is "") still
    // resolves nested includes against the file's directory, not the CWD.
    // Only `/` has no parent, and it is not a regular file.
    let base_dir = canonical.parent().unwrap_or(Path::new("/"));
    let nested = Config::resolve_include_files(&include_value, base_dir)?;

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

/// Style properties that may appear on a style table. The embedded default
/// omits them where they are unset (`[theme.alert]` lists only `fg`), so they
/// are not validated key by key against the default's shape, which would
/// wrongly reject a user adding `bg` there. See `takes_style_keys` for where
/// they belong.
const STYLE_KEYS: &[&str] = &["fg", "bg", "modifiers"];

/// Whether a theme table, as the default `schema` has it, is a style: a leaf
/// (`[theme.table.selected]`), or a section styled itself as well as holding
/// sub-styles (`[theme.alert]`, whose default sets `fg`). A container such as
/// `[theme.clipboard]` is neither, and a style key there would be dropped by
/// deserialization.
fn takes_style_keys(schema: &toml::map::Map<String, Value>) -> bool {
    schema.values().all(|value| !value.is_table())
        || STYLE_KEYS.iter().any(|key| schema.contains_key(*key))
}

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

/// Merges the embedded default config from its two source files:
/// base config + theme (which includes both truecolor and 256-color variants).
fn merge_default_config() -> Result<Value> {
    let base = parse_toml(None, DEFAULT_CONFIG_BASE)?;
    let theme = parse_toml(None, DEFAULT_THEME)?;
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
    use ratatui::{
        crossterm::event::{KeyCode, KeyModifiers},
        style::Color,
    };
    use test_case::test_case;

    use super::{keybindings::Action, *};
    use crate::test_support::TempDir;

    /// The config directory for a config parsed from a string: reserved and
    /// never created, so nothing resolved from it can be read by accident.
    fn inert_dir() -> PathBuf {
        TempDir::reserved("config_parse").path().to_path_buf()
    }

    #[test]
    fn merge_overrides_shared_keys_and_preserves_the_rest() {
        // Nested, so the recursive arm is exercised: a table in the overlay
        // merges into its counterpart rather than replacing it whole.
        let base = parse_toml(None, "[t]\na = 1\nb = 2").unwrap();
        let overlay = parse_toml(None, "[t]\nb = 3").unwrap();
        let merged = merge_toml_values(base, overlay);
        let table = merged.get("t").unwrap();
        assert_eq!(1, table.get("a").unwrap().as_integer().unwrap());
        assert_eq!(3, table.get("b").unwrap().as_integer().unwrap());
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
        let defaults = Config::parse(RuntimeEnv::default(), None, "", &inert_dir(), &[]).unwrap();
        let merged =
            Config::parse(RuntimeEnv::default(), None, partial, &inert_dir(), &[]).unwrap();

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
        match Config::parse(RuntimeEnv::default(), None, toml, &inert_dir(), &[]) {
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
        Config::parse(RuntimeEnv::default(), None, content, &inert_dir(), &[]).unwrap();
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
    // A theme table that only groups styles is not a style itself, so a style
    // property there would be dropped just the same.
    #[test_case("[theme.clipboard]\nfg = \"Red\"\n", "theme.clipboard.fg" ; "style name on a theme container")]
    #[test_case("[theme256.file_modified_date]\nbg = \"4\"\n", "theme256.file_modified_date.bg" ; "style name on a theme256 container")]
    fn unknown_key_is_rejected(toml: &str, expected: &str) {
        let err = parse_err(toml);
        assert!(err.contains(expected), "error should name the key: {err}");
    }

    #[test_case("[theme.alert]\nbg = \"#000000\"\nmodifiers = [\"bold\"]\n" ; "nested theme table")]
    #[test_case("[theme256.alert]\nbg = \"#000000\"\n" ; "nested theme256 table")]
    #[test_case("[theme]\nfg = \"#ffffff\"\n" ; "theme root")]
    // The default leaves this leaf empty, so only its being a leaf says it is
    // a style.
    #[test_case("[theme.file_type.missing]\nfg = \"Red\"\n" ; "a leaf the default leaves empty")]
    fn style_property_absent_from_default_is_accepted(toml: &str) {
        // The default `[theme.alert]` lists only `fg`; adding `bg`/`modifiers`
        // must not be mistaken for an unknown key.
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

    #[test_case("[file_system]\nbuffer_min_bytes = 200\nbuffer_max_bytes = 100\n" ; "min exceeds max")]
    #[test_case("[file_system]\nbuffer_min_bytes = 0\n" ; "min is zero")]
    fn invalid_buffer_sizes_are_rejected(toml: &str) {
        let err = parse_err(toml);
        assert!(
            err.contains("buffer_min_bytes"),
            "error should explain the invariant: {err}"
        );
    }

    #[test]
    fn equal_buffer_sizes_are_accepted() {
        // The bound is "must not exceed", so one buffer size for every copy is
        // a valid configuration rather than the degenerate case of the check.
        Config::parse(
            RuntimeEnv::default(),
            None,
            "[file_system]\nbuffer_min_bytes = 100\nbuffer_max_bytes = 100\n",
            &inert_dir(),
            &[],
        )
        .unwrap();
    }

    // A zero bound descends into nothing or collects nothing, so a search
    // would report no results instead of the config failing to load.
    #[test_case("search_max_depth" ; "depth")]
    #[test_case("search_max_results" ; "results")]
    fn a_search_bound_of_zero_is_rejected(key: &str) {
        let err = parse_err(&format!("[file_system]\n{key} = 0\n"));
        assert_eq!(format!("file_system.{key} must be greater than 0"), err);
    }

    #[test]
    fn the_theme_read_is_the_one_for_the_terminals_color_support() {
        let toml = "[theme]\nfg = \"#010203\"\n[theme256]\nfg = \"#040506\"\n";
        let parse = |is_truecolor| {
            let env = RuntimeEnv {
                is_truecolor,
                ls_colors: None,
            };
            Config::parse(env, None, toml, &inert_dir(), &[]).unwrap()
        };

        assert_eq!(Some(Color::Rgb(1, 2, 3)), parse(true).theme().base().fg);
        assert_eq!(Some(Color::Rgb(4, 5, 6)), parse(false).theme().base().fg);
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

    #[test_case(true, Some(Color::Red) ; "applied when they take precedence")]
    #[test_case(false, Some(Color::Rgb(1, 2, 3)) ; "ignored otherwise")]
    fn ls_colors_reach_both_themes(take_precedence: bool, expected: Option<Color>) {
        let env = RuntimeEnv {
            is_truecolor: false,
            ls_colors: Some("di=31"),
        };
        let toml = format!(
            "[theme.file_type]\nls_colors_take_precedence = {take_precedence}\n\
             [theme.file_type.directory]\nfg = \"#010203\"\n\
             [theme256.file_type]\nls_colors_take_precedence = {take_precedence}\n\
             [theme256.file_type.directory]\nfg = \"#010203\"\n"
        );
        let config = Config::parse(env, None, &toml, &inert_dir(), &[]).unwrap();

        assert_eq!(expected, config.theme.file_type.directory().fg);
        assert_eq!(expected, config.theme256.file_type.directory().fg);
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
        let target = dir.join("dotfiles.toml");
        let link = dir.join("config.toml");
        // Dangling, so only a check that does not follow the link finds
        // anything there. A config symlinked into a dotfiles repository is a
        // file to refuse, not one to write through to its target.
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let error = Config::write_default(Some(link), false)
            .expect_err("a symlink must not be written through")
            .to_string();

        assert!(error.contains("already exists; pass --force"), "{error}");
        assert!(!target.exists());
    }

    /// `--force` replaces a file, never a link: removing the link would detach
    /// the config from the repository, and writing through it would overwrite
    /// the repository's copy. Dangling, so only a check that does not follow
    /// the link finds one there.
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
            format!("Cannot write {}: it is a symbolic link", link.display()),
            error
        );
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(!target.exists());
    }

    /// Nothing to replace is not an error: `--force` permits a replacement
    /// rather than requiring one.
    #[test]
    fn force_writes_a_missing_file() {
        let dir = TempDir::new("config_force_missing");
        let path = dir.join("config.toml");

        Config::write_default(Some(path.clone()), true).unwrap();

        assert_eq!(DEFAULT_CONFIG_BASE, fs::read_to_string(&path).unwrap());
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

    /// Only the default path may be absent: `--config` names a file the user
    /// expects to be read, which `a_missing_config_path_is_reported_by_name`
    /// pins.
    #[test]
    fn a_missing_default_config_falls_back_to_the_built_in_one() {
        let dir = TempDir::reserved("config_default_missing");

        let config =
            Config::load_from(RuntimeEnv::default(), &dir.join("config.toml"), true, &[]).unwrap();

        // Bookmarks still live beside where the config would be.
        assert_eq!(dir.path(), config.config_dir);
        assert!(select_next_key(&config, 'j'));
    }

    /// Only absence falls back: a default config that exists but cannot be
    /// read is the user's file, and ignoring it would drop their settings.
    #[test]
    fn an_unreadable_default_config_is_an_error() {
        let dir = TempDir::new("config_default_unreadable");
        let file = dir.join("file");
        fs::write(&file, b"").unwrap();
        // A path under a regular file fails with ENOTDIR rather than ENOENT.
        let path = file.join("config.toml");

        let error = match Config::load_from(RuntimeEnv::default(), &path, true, &[]) {
            Ok(_) => panic!("expected the load to fail"),
            Err(error) => error.to_string(),
        };

        assert!(
            error.starts_with(&format!("Failed to read config file {}:", path.display())),
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

        // Named as an include rather than as the config, so the user knows
        // which of the two files to go and look at.
        assert!(error.starts_with("Failed to read include file"), "{error}");
        assert!(error.contains(&include.display().to_string()), "{error}");
    }

    /// A FIFO would block the read until a writer appeared, and a device can
    /// be read forever, so neither is read at all. `/dev/null` stands in for
    /// the device: it reads as an empty, valid config, so only the type check
    /// can refuse it.
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
                path.display()
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
                include.display()
            ),
            error
        );
    }

    /// A symlink is followed: what must be a regular file is what it names.
    #[test]
    fn a_symlinked_include_file_is_read() {
        let dir = TempDir::new("config_include_symlink");
        let config = dir.join("config.toml");
        fs::write(&config, b"").unwrap();
        let target = binds_select_next(&dir, "target.toml", 'e');
        let link = dir.join("link.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[link]).unwrap();

        assert!(select_next_key(&merged, 'e'));
    }

    #[test]
    fn malformed_toml_is_reported() {
        let error = parse_err("this is not toml =\n");
        assert!(error.starts_with("Failed to parse TOML"), "{error}");
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

    /// The config is merged first, so a cycle that leads back to it must stop
    /// there: merging it again would put it on top of the file it included.
    #[test]
    fn an_include_cycle_back_to_the_config_keeps_the_include_on_top() {
        let dir = TempDir::new("config_cycle_main");
        let config = dir.join("config.toml");
        let include = dir.join("include.toml");
        fs::write(
            &config,
            format!(
                "include_files = [\"{}\"]\n[keybindings]\nselect_next = \"e\"\n",
                include.display()
            ),
        )
        .unwrap();
        fs::write(
            &include,
            format!(
                "include_files = [\"{}\"]\n[keybindings]\nselect_next = \"i\"\n",
                config.display()
            ),
        )
        .unwrap();

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[]).unwrap();

        assert!(select_next_key(&merged, 'i'));
        assert!(!select_next_key(&merged, 'e'));
    }

    #[test]
    fn a_relative_include_file_resolves_from_the_file_that_lists_it() {
        let dir = TempDir::new("config_relative_include");
        fs::create_dir(dir.join("sub")).unwrap();
        let config = dir.join("config.toml");
        // Listed by the config, so resolved from the config's directory.
        fs::write(&config, "include_files = [\"sub/listed.toml\"]\n").unwrap();
        // Listed by a file in `sub/`, so resolved from `sub/`: neither the
        // config's directory nor the working directory holds a `nested.toml`.
        fs::write(
            dir.join("sub").join("listed.toml"),
            "include_files = [\"nested.toml\"]\n",
        )
        .unwrap();
        binds_select_next(&dir, "sub/nested.toml", 'e');

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[]).unwrap();

        assert!(select_next_key(&merged, 'e'));
    }

    /// A symlinked include's own includes resolve from the directory of the
    /// file it names, not from the directory holding the link.
    #[test]
    fn a_symlinked_includes_relative_include_resolves_from_its_target() {
        let dir = TempDir::new("config_symlinked_include_nested");
        fs::create_dir(dir.join("config")).unwrap();
        fs::create_dir(dir.join("dotfiles")).unwrap();
        let config = dir.join("config/config.toml");
        fs::write(&config, b"").unwrap();
        let target = dir.join("dotfiles/listed.toml");
        fs::write(&target, "include_files = [\"nested.toml\"]\n").unwrap();
        // Only `dotfiles/` holds a `nested.toml`.
        binds_select_next(&dir, "dotfiles/nested.toml", 'e');
        let link = dir.join("config/link.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let merged = Config::load(RuntimeEnv::default(), Some(config), &[link]).unwrap();

        assert!(select_next_key(&merged, 'e'));
    }

    /// The config is valid and so is the first include, so only the file named
    /// in the message tells the user where the mistake is.
    #[test]
    fn a_malformed_include_file_is_reported_by_name() {
        let dir = TempDir::new("config_malformed_include");
        let config = dir.join("config.toml");
        fs::write(&config, "include_files = [\"good.toml\", \"bad.toml\"]\n").unwrap();
        fs::write(dir.join("good.toml"), "log_level = \"warn\"\n").unwrap();
        fs::write(dir.join("bad.toml"), "this is not toml =\n").unwrap();

        let error = load_err(Some(config), &[]);

        let expected = format!("Failed to parse {}: ", dir.join("bad.toml").display());
        assert!(error.starts_with(&expected), "{error}");
    }

    #[test]
    fn a_relative_config_path_resolves_from_the_working_directory() {
        let name = "filectrl-absent-config-xyz.toml";

        let error = load_err(Some(PathBuf::from(name)), &[]);

        // Absolutized, so that the config's directory, which bookmarks and
        // relative includes resolve from, is never the empty path.
        let absolute = std::env::current_dir().unwrap().join(name);
        assert!(
            error.starts_with(&format!(
                "Failed to read config file {}:",
                absolute.display()
            )),
            "{error}"
        );
    }

    /// Runs every path-carrying default opener, on both platforms, against a
    /// directory whose name is hostile to a shell. Each program the templates
    /// launch is replaced by a stub that records its working directory and
    /// arguments, so the check is what the program would have received.
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
                let argv = shell::command(
                    template,
                    shell::Parameters::One,
                    [target.as_os_str().to_os_string()],
                );
                // `PATH` holds only the stubs, so the shell is named in full.
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
                // The working directory for `cd %s && exec xterm`, the last
                // argument for the rest.
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
