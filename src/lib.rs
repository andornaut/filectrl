pub mod app;
mod command;
mod file_system;
#[cfg(test)]
mod test_support;
mod views;

use std::{
    borrow::Cow,
    env,
    ffi::OsStr,
    fmt::Write as _,
    fs,
    io::{IsTerminal, Write, stdout},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use env_logger::{Builder, DEFAULT_FILTER_ENV, Env};
use log::{LevelFilter, info};

use self::app::{
    App,
    config::{Config, RuntimeEnv},
    events::install_signal_handlers,
    terminal::{CleanupOnDropTerminal, supports_truecolor},
};

const MODULE_PREFIX: &str = concat!(env!("CARGO_PKG_NAME"), "::");

pub fn run(
    config_path: Option<PathBuf>,
    include_paths: &[PathBuf],
    initial_directory: Option<&Path>,
    no_truecolor: bool,
) -> Result<()> {
    // Configure logging with a default level before loading config, so that Info+ messages from the
    // config initialization are logged
    configure_logging();

    // Validate the initial directory before entering raw mode so an invalid
    // positional argument fails fast with a clean stderr message and a nonzero
    // exit code, rather than silently opening the TUI in the current directory.
    let initial_directory = initial_directory
        .map(validate_initial_directory)
        .transpose()?;

    let is_truecolor = supports_truecolor(env::var("COLORTERM").ok().as_deref()) && !no_truecolor;
    let ls_colors = env::var("LS_COLORS").ok();
    let env = RuntimeEnv {
        is_truecolor,
        ls_colors: ls_colors.as_deref(),
    };

    let config = Config::load(env, config_path, include_paths)?;
    apply_log_level(&config);
    info!("Terminal truecolor support: {is_truecolor}");
    // Records after this point would be drawn over the interface, so they are
    // kept only when stderr is redirected away from the terminal.
    if std::io::stderr().is_terminal() {
        log::set_max_level(LevelFilter::Off);
    }
    Config::init(config);

    // Install signal handlers before entering raw mode so that SIGTERM/SIGHUP
    // cause a graceful shutdown (terminal restored) rather than leaving the
    // shell in a broken state.
    install_signal_handlers().context("Failed to install the signal handlers")?;

    // Checked after everything the user typed has been validated, so that a bad
    // argument or config is reported before the environment is. Crossterm opens
    // the controlling terminal itself, so without this a redirected stdout
    // surfaces as a bare ENXIO that names nothing.
    if !stdout().is_terminal() {
        return Err(anyhow!("Cannot start: standard output is not a terminal"));
    }
    let terminal = CleanupOnDropTerminal::try_new().context("Failed to initialize the terminal")?;
    App::new(terminal).run(initial_directory)
}

pub fn print_keybindings(config_path: Option<PathBuf>, include_paths: &[PathBuf]) -> Result<()> {
    configure_logging();
    let config = Config::load(RuntimeEnv::default(), config_path, include_paths)?;
    let bold = std::io::stdout().is_terminal();
    print!(
        "{}",
        views::keybindings_help_text(&config.keybindings, bold)
    );
    Ok(())
}

fn validate_initial_directory(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(anyhow!("Cannot open an empty path"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| anyhow!("Failed to open {}: {error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(anyhow!(
            "Cannot open {}: not a directory",
            canonical.display()
        ));
    }
    // A directory that cannot be listed would start the UI on an empty table
    // with the reason only in an alert, where `ls` exits with it.
    fs::read_dir(&canonical)
        .map_err(|error| anyhow!("Failed to open {}: {error}", path.display()))?;
    Ok(canonical)
}

fn apply_log_level(config: &Config) {
    if let Ok(level) = env::var(DEFAULT_FILTER_ENV) {
        // RUST_LOG is set; env_logger already applied it in configure_logging()
        info!("Log level set from environment variable: {DEFAULT_FILTER_ENV}={level}");
    } else {
        // No env override; apply the level from the config file
        let level = config.log_level;
        log::set_max_level(level);
        info!("Log level set from config: {level:?}");
    }
}

fn configure_logging() {
    // When $RUST_LOG is unset, set env_logger's internal filter to the most
    // permissive level so that the level can later be raised above Info from the
    // config file. env_logger's internal filter is fixed at init() and cannot be
    // changed afterward, so gating is done solely through log::set_max_level().
    // When $RUST_LOG is set, it takes precedence and env_logger applies it.
    Builder::from_env(Env::default().default_filter_or(LevelFilter::Trace.as_str()))
        .format(|buf, record| {
            let path = record.module_path().unwrap_or_default();
            writeln!(
                buf,
                "[{} {}:{}] {}",
                record.level(),
                path.strip_prefix(MODULE_PREFIX).unwrap_or(path),
                record.line().unwrap_or_default(),
                visible(&record.args().to_string())
            )
        })
        .init();

    // Gate to Info for the pre-config phase so verbose internal messages don't
    // appear before the configured level is applied by apply_log_level(). When
    // $RUST_LOG is set, leave the level env_logger derived from it in place.
    if env::var(DEFAULT_FILTER_ENV).is_err() {
        log::set_max_level(LevelFilter::Info);
    }
}

/// Whether `c` would hide or disguise the text around it when shown: a control
/// character, which a terminal runs as a command and the renderer drops; a bidi
/// control, which reorders what is drawn after it, so `a\u{202e}txt.exe` reads
/// as `aexe.txt`; a line or paragraph separator; or a character that draws
/// nothing, so two different names look the same. The zero width joiner and
/// non-joiner, variation selectors and tag characters are left alone, since
/// scripts and emoji are spelled with them.
pub fn is_disguising(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            // Format characters and default ignorables, less the ones above.
            '\u{00ad}'
                | '\u{034f}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061c}'
                | '\u{06dd}'
                | '\u{070f}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08e2}'
                | '\u{115f}'..='\u{1160}'
                | '\u{17b4}'..='\u{17b5}'
                | '\u{180e}'
                | '\u{200b}'
                | '\u{200e}'..='\u{200f}'
                // Line and paragraph separators, then the bidi embeddings and
                // overrides.
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                // Braille pattern blank, which draws as a space.
                | '\u{2800}'
                | '\u{3164}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{fff0}'..='\u{fffb}'
                | '\u{110bd}'
                | '\u{110cd}'
                | '\u{13430}'..='\u{1343f}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0000}'..='\u{e001f}'
                | '\u{e0080}'..='\u{e00ff}'
                | '\u{e01f0}'..='\u{e0fff}'
        )
}

/// `text` with every character `is_disguising` names spelled out as an escape,
/// so it shows exactly what it holds. The one function every piece of text
/// from outside filectrl passes through on its way to the screen or the log: a
/// file name, a message naming one, a desktop entry's name. A newline is
/// escaped too, so a name cannot forge a log record or a second line.
pub fn visible(text: &str) -> Cow<'_, str> {
    escape_disguising(text, |_| false)
}

/// `visible` for text that need not be UTF-8, such as a file name: each byte
/// that is not part of a valid sequence is spelled `\xNN`, so names that differ
/// only in such bytes, or in a byte and U+FFFD, do not look the same.
pub fn visible_os(text: &OsStr) -> Cow<'_, str> {
    let bytes = text.as_encoded_bytes();
    if let Ok(text) = std::str::from_utf8(bytes) {
        return visible(text);
    }
    let mut shown = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        shown.push_str(&visible(chunk.valid()));
        for byte in chunk.invalid() {
            let _ = write!(shown, "\\x{byte:02x}");
        }
    }
    Cow::Owned(shown)
}

/// `visible` for text printed to the terminal outside the interface, which can
/// carry a path from the command line, a symlink or a config file. A newline is
/// kept, since usage text and some error messages span lines.
pub fn escape_for_terminal(text: &str) -> Cow<'_, str> {
    escape_disguising(text, |c| c == '\n')
}

fn escape_disguising(text: &str, keep: impl Fn(char) -> bool) -> Cow<'_, str> {
    let is_escaped = |c: char| is_disguising(c) && !keep(c);
    if !text.contains(is_escaped) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|c| {
                if is_escaped(c) {
                    c.escape_default().to_string()
                } else {
                    c.to_string()
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test]
    fn validate_initial_directory_accepts_a_directory_as_its_canonical_path() {
        let dir = test_support::TempDir::new("initial_directory");
        std::fs::create_dir(dir.join("sub")).unwrap();
        // Spelled with a `..` component, so only the canonicalized path equals
        // the directory.
        let result = validate_initial_directory(&dir.join("sub").join("..")).unwrap();
        assert_eq!(dir.path().canonicalize().unwrap(), result);
    }

    /// An attempt that the OS refused, so the message carries its cause.
    #[test]
    fn validate_initial_directory_rejects_a_nonexistent_path() {
        let path = env::temp_dir().join("filectrl-does-not-exist-xyz");
        let error = validate_initial_directory(&path).unwrap_err().to_string();
        assert!(error.starts_with("Failed to open "), "{error}");
    }

    /// Refused by filectrl rather than by the OS, so the message gives its own
    /// reason instead of an errno.
    #[test]
    fn validate_initial_directory_rejects_a_regular_file() {
        let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let error = validate_initial_directory(&file).unwrap_err().to_string();
        assert!(error.starts_with("Cannot open "), "{error}");
        assert!(error.ends_with(": not a directory"), "{error}");
    }

    #[test]
    fn validate_initial_directory_rejects_a_directory_it_cannot_list() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_support::TempDir::new("lib_unlistable");
        let locked = dir.join("locked");
        std::fs::create_dir(&locked).unwrap();
        // Search permission only, so `canonicalize` and `is_dir` succeed and
        // only the listing is refused.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o100)).unwrap();
        // Root lists a mode-100 directory anyway; probe rather than inspect
        // the euid.
        let is_unlistable = std::fs::read_dir(&locked).is_err();

        let result = validate_initial_directory(&locked);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();

        if !is_unlistable {
            return;
        }
        let error = result.unwrap_err().to_string();
        assert!(
            error.starts_with(&format!("Failed to open {}:", locked.display())),
            "{error}"
        );
        assert!(error.contains("Permission denied"), "{error}");
    }

    /// An empty positional would otherwise reach `canonicalize` and be reported
    /// against a path that renders as nothing at all.
    #[test]
    fn validate_initial_directory_rejects_an_empty_path() {
        let error = validate_initial_directory(&PathBuf::new())
            .unwrap_err()
            .to_string();
        assert_eq!("Cannot open an empty path", error);
    }

    #[test_case("plain caf\u{e9}" => "plain caf\u{e9}" ; "printable text is unchanged")]
    #[test_case("a\u{1b}]52;c;eA==\u{7}b" => "a\\u{1b}]52;c;eA==\\u{7}b" ; "an escape sequence is escaped")]
    #[test_case("a\u{9b}2Jb" => "a\\u{9b}2Jb" ; "a C1 control is escaped")]
    #[test_case("a\nb\tc" => "a\\nb\\tc" ; "a newline and a tab are escaped")]
    #[test_case("a\u{202e}b" => "a\\u{202e}b" ; "a bidi override is escaped")]
    #[test_case("a\u{2063}b\u{3164}c" => "a\\u{2063}b\\u{3164}c" ; "invisible characters are escaped")]
    #[test_case("a\u{2800}b" => "a\\u{2800}b" ; "a braille blank is escaped")]
    #[test_case("a\u{200d}b\u{fe0f}" => "a\u{200d}b\u{fe0f}" ; "a joiner and a variation selector are kept")]
    fn visible_produces(message: &str) -> String {
        visible(message).into_owned()
    }

    #[test_case(b"caf\xc3\xa9" => "caf\u{e9}" ; "valid UTF-8 is unchanged")]
    #[test_case(b"caf\xe9" => "caf\\xe9" ; "an invalid byte is spelled out")]
    #[test_case(b"caf\xff" => "caf\\xff" ; "a different invalid byte is spelled differently")]
    #[test_case("caf\u{fffd}".as_bytes() => "caf\u{fffd}" ; "a replacement character is not an invalid byte")]
    #[test_case(b"\xe9\xe2\x80\xae" => "\\xe9\\u{202e}" ; "a valid run after an invalid byte is still escaped")]
    fn visible_os_produces(bytes: &[u8]) -> String {
        use std::os::unix::ffi::OsStrExt;

        visible_os(OsStr::from_bytes(bytes)).into_owned()
    }

    #[test_case("a\nb" => "a\nb" ; "a newline is kept")]
    #[test_case("a\u{1b}]0;t\u{7}\rb" => "a\\u{1b}]0;t\\u{7}\\rb" ; "an escape sequence and a carriage return are escaped")]
    fn escape_for_terminal_produces(text: &str) -> String {
        escape_for_terminal(text).into_owned()
    }
}
