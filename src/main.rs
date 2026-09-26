use std::{
    error::Error,
    ffi::{OsStr, OsString},
    fmt,
    io::{self, Write},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result};
use argh::FromArgs;

use filectrl::{
    app::{
        config::{DEFAULT_CONFIG_BASE, DEFAULT_THEME},
        events::quit_signal,
    },
    escape_for_terminal, print_keybindings, run, visible_os,
};

#[derive(FromArgs)]
#[argh(help_triggers("-h", "--help"))]
// Each bool is an `#[argh(switch)]`, one per command-line flag.
#[allow(clippy::struct_excessive_bools)]
/// FileCTRL is a light, opinionated, responsive, theme-able, and simple Text User Interface (TUI) file manager for Linux and macOS
struct Args {
    /// path to a configuration file
    #[argh(option, short = 'c', from_str_fn(decode_path))]
    config: Option<PathBuf>,

    /// include a TOML file to merge on top of the config (repeatable; later files take precedence)
    #[argh(option, short = 'i', from_str_fn(decode_path))]
    include: Vec<PathBuf>,

    /// use the 256-color theme instead of detecting truecolor support
    #[argh(switch)]
    no_truecolor: bool,

    /// print the default config, then exit
    #[argh(switch)]
    print_default_config: bool,

    /// print the default theme, then exit
    #[argh(switch)]
    print_default_theme: bool,

    /// print the keybindings, then exit
    #[argh(switch)]
    print_keybindings: bool,

    /// print the version, then exit
    #[argh(switch, short = 'V')]
    version: bool,

    /// path to a directory to navigate to
    #[argh(positional, from_str_fn(decode_path))]
    directory: Option<PathBuf>,
}

/// A mistake in the command line, printed like argh's own parse errors.
#[derive(Debug)]
struct UsageError(String);

impl fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl Error for UsageError {}

fn usage(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(UsageError(message.into()))
}

/// The flags that do one thing and exit. At most one may be given, with only
/// the arguments that can change what it does.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    DefaultConfig,
    DefaultTheme,
    Keybindings,
    Version,
}

impl Action {
    fn flag(self) -> &'static str {
        match self {
            Self::DefaultConfig => "--print-default-config",
            Self::DefaultTheme => "--print-default-theme",
            Self::Keybindings => "--print-keybindings",
            Self::Version => "--version",
        }
    }

    /// Only `--print-keybindings` reads the config, so only it accepts
    /// `--config` and `--include`; nothing accepts `--no-truecolor`.
    fn accepts(self, argument: &str) -> bool {
        self == Self::Keybindings && matches!(argument, "--config" | "--include")
    }
}

/// argh parses `&str` alone, so a non-UTF-8 argument is carried through it
/// encoded: each invalid byte becomes `ESCAPE_BASE` + byte (U+F780 to U+F7FF),
/// and a character already in that range is encoded byte by byte, so
/// `decode_arg` restores every argument exactly.
const ESCAPE_BASE: u32 = 0xF700;
const ESCAPE_RANGE: std::ops::RangeInclusive<char> = '\u{F780}'..='\u{F7FF}';

fn escape(byte: u8) -> char {
    char::from_u32(ESCAPE_BASE + u32::from(byte)).expect("U+F780 to U+F7FF are characters")
}

fn encode_arg(arg: &OsStr) -> String {
    let mut encoded = String::with_capacity(arg.len());
    for chunk in arg.as_bytes().utf8_chunks() {
        for c in chunk.valid().chars() {
            if ESCAPE_RANGE.contains(&c) {
                for byte in c.encode_utf8(&mut [0; 4]).bytes() {
                    encoded.push(escape(byte));
                }
            } else {
                encoded.push(c);
            }
        }
        for &byte in chunk.invalid() {
            encoded.push(escape(byte));
        }
    }
    encoded
}

fn decode_arg(encoded: &str) -> OsString {
    let mut bytes = Vec::with_capacity(encoded.len());
    for c in encoded.chars() {
        if ESCAPE_RANGE.contains(&c) {
            let byte = u8::try_from(u32::from(c) - ESCAPE_BASE)
                .expect("an escape character stands for one byte");
            bytes.push(byte);
        } else {
            bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
        }
    }
    OsString::from_vec(bytes)
}

// The signature argh's `from_str_fn` requires.
#[allow(clippy::unnecessary_wraps)]
fn decode_path(value: &str) -> Result<PathBuf, String> {
    Ok(PathBuf::from(decode_arg(value)))
}

/// `argh::from_env` over `args_os` through `encode_arg`; `from_env` exits on
/// the first argument that is not valid UTF-8.
fn parse_args() -> Args {
    let strings: Vec<String> = std::env::args_os().map(|arg| encode_arg(&arg)).collect();
    let Some((program, rest)) = strings.split_first() else {
        print_error(format_args!("No program name, argv is empty"));
        std::process::exit(1)
    };
    let command = Path::new(program)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(program);
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    Args::from_args(&[command], &rest).unwrap_or_else(|early_exit| {
        if early_exit.status.is_ok() {
            if let Err(error) = print_line(format_args!("{}", early_exit.output)) {
                print_error(format_args!("Error: {error:#}"));
                std::process::exit(1)
            }
            std::process::exit(0)
        }
        print_error(format_args!(
            "{}\nRun {command} --help for more information.",
            shown_argh_output(&early_exit.output)
        ));
        std::process::exit(1)
    })
}

/// argh's usage error message, with `encode_arg`'s escapes decoded and invalid
/// bytes shown as `\xNN`.
fn shown_argh_output(output: &str) -> String {
    decode_arg(output)
        .as_bytes()
        .split(|byte| *byte == b'\n')
        .map(|line| visible_os(OsStr::from_bytes(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Writes to stdout, returning a failure where `print!` would panic.
fn print(text: &str) -> Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .context("Failed to write to standard output")
}

fn print_line(args: fmt::Arguments<'_>) -> Result<()> {
    print(&format!("{args}\n"))
}

/// Writes a line to stderr, ignoring a failure: `eprintln!` would panic on a
/// terminal that hung up.
fn print_error(args: fmt::Arguments<'_>) {
    let _ = writeln!(io::stderr(), "{args}");
}

fn main() -> ExitCode {
    let args = parse_args();
    let result = dispatch(&args);
    // After a quit signal an error is most likely a draw to a terminal that hung
    // up, so the status alone reports the run.
    if let Some(signal) = quit_signal() {
        return success_status(Some(signal));
    }
    match result {
        Ok(()) => success_status(None),
        Err(error) => {
            match error.downcast_ref::<UsageError>() {
                Some(usage) => {
                    let usage = escape_for_terminal(&usage.to_string()).into_owned();
                    print_error(format_args!(
                        "{usage}\n\nRun filectrl --help for more information."
                    ));
                }
                // `{error:#}` flattens the cause chain onto one line.
                None => print_error(format_args!(
                    "Error: {}",
                    escape_for_terminal(&format!("{error:#}"))
                )),
            }
            ExitCode::FAILURE
        }
    }
}

/// 0, or 128 plus the signal number when a termination signal ended the run.
fn success_status(signal: Option<i32>) -> ExitCode {
    signal
        .and_then(|signal| u8::try_from(128 + signal).ok())
        .map_or(ExitCode::SUCCESS, ExitCode::from)
}

fn dispatch(args: &Args) -> Result<()> {
    let action = selected_action(args)?;
    let config = args.config.clone();

    match action {
        Some(Action::DefaultConfig) => print(DEFAULT_CONFIG_BASE),
        Some(Action::DefaultTheme) => print(DEFAULT_THEME),
        Some(Action::Version) => print_line(format_args!(
            "{} {}",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        )),
        Some(Action::Keybindings) => print_keybindings(config, &args.include),
        None => run(
            config,
            &args.include,
            args.directory.as_deref(),
            args.no_truecolor,
        ),
    }
}

fn selected_action(args: &Args) -> Result<Option<Action>> {
    let selected: Vec<Action> = [
        (args.print_default_config, Action::DefaultConfig),
        (args.print_default_theme, Action::DefaultTheme),
        (args.print_keybindings, Action::Keybindings),
        (args.version, Action::Version),
    ]
    .into_iter()
    .filter_map(|(given, action)| given.then_some(action))
    .collect();

    match selected.as_slice() {
        [] => Ok(None),
        [action] => {
            reject_unused(args, *action)?;
            Ok(Some(*action))
        }
        // In the order listed above; argh does not preserve typed order.
        [first, second, ..] => Err(usage(format!(
            "{} and {} cannot be combined.",
            first.flag(),
            second.flag()
        ))),
    }
}

fn reject_unused(args: &Args, action: Action) -> Result<()> {
    let given = [
        (args.config.is_some(), "--config"),
        (!args.include.is_empty(), "--include"),
        (args.no_truecolor, "--no-truecolor"),
    ];
    for (present, argument) in given {
        if present && !action.accepts(argument) {
            return Err(usage(format!(
                "{argument} has no effect with {}.",
                action.flag()
            )));
        }
    }
    if args.directory.is_some() {
        return Err(usage(format!(
            "A directory argument has no effect with {}.",
            action.flag()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test_case(None => ExitCode::SUCCESS ; "a quit from the keyboard")]
    #[test_case(Some(1) => ExitCode::from(129) ; "sighup")]
    #[test_case(Some(15) => ExitCode::from(143) ; "sigterm")]
    fn a_quit_by_signal_exits_as_the_signal_would(signal: Option<i32>) -> ExitCode {
        success_status(signal)
    }

    fn args() -> Args {
        Args {
            config: None,
            include: Vec::new(),
            no_truecolor: false,
            print_default_config: false,
            print_default_theme: false,
            print_keybindings: false,
            version: false,
            directory: None,
        }
    }

    fn usage_error(args: &Args) -> String {
        match selected_action(args) {
            Ok(_) => panic!("expected a usage error"),
            Err(error) => {
                assert!(
                    error.downcast_ref::<UsageError>().is_some(),
                    "expected a usage error, got: {error:#}"
                );
                error.to_string()
            }
        }
    }

    #[test]
    fn no_action_flag_runs_the_app() {
        assert!(selected_action(&args()).unwrap().is_none());
    }

    #[test]
    fn a_single_action_flag_is_selected() {
        let args = Args {
            version: true,
            ..args()
        };
        assert_eq!(Some(Action::Version), selected_action(&args).unwrap());
    }

    #[test]
    fn two_action_flags_are_rejected() {
        let args = Args {
            print_default_config: true,
            print_default_theme: true,
            ..args()
        };
        let error = usage_error(&args);
        assert!(error.contains("--print-default-config"), "{error}");
        assert!(error.contains("--print-default-theme"), "{error}");
    }

    /// `action`'s flag, plus the argument `name` names in the usage error.
    fn args_with(action: Action, name: &str) -> Args {
        let mut args = args();
        match action {
            Action::DefaultConfig => args.print_default_config = true,
            Action::DefaultTheme => args.print_default_theme = true,
            Action::Keybindings => args.print_keybindings = true,
            Action::Version => args.version = true,
        }
        match name {
            "--config" => args.config = Some(PathBuf::from("config.toml")),
            "--include" => args.include = vec![PathBuf::from("theme.toml")],
            "--no-truecolor" => args.no_truecolor = true,
            "A directory argument" => args.directory = Some(PathBuf::from("/tmp")),
            _ => unreachable!("no such argument: {name}"),
        }
        args
    }

    #[test_case(Action::DefaultConfig, "--config" ; "config with the default config")]
    #[test_case(Action::DefaultTheme, "--include" ; "include with the default theme")]
    #[test_case(Action::Keybindings, "--no-truecolor" ; "a run-only flag with printing")]
    #[test_case(Action::Version, "--config" ; "config with version")]
    #[test_case(Action::Keybindings, "A directory argument" ; "a directory with printing")]
    fn an_argument_the_action_ignores_is_rejected(action: Action, name: &str) {
        assert_eq!(
            format!("{name} has no effect with {}.", action.flag()),
            usage_error(&args_with(action, name))
        );
    }

    #[test]
    fn printing_keybindings_accepts_the_config_chain() {
        let args = Args {
            print_keybindings: true,
            config: Some(PathBuf::from("/tmp/config.toml")),
            include: vec![PathBuf::from("theme.toml")],
            ..args()
        };
        assert_eq!(Some(Action::Keybindings), selected_action(&args).unwrap());
    }

    #[test]
    fn a_path_argument_that_is_not_utf8_reaches_the_args_intact() {
        let directory = OsStr::from_bytes(b"/tmp/caf\xe9");
        let config = OsStr::from_bytes(b"/tmp/\xff.toml");
        let argv = [
            encode_arg(OsStr::new("--config")),
            encode_arg(config),
            encode_arg(directory),
        ];
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();

        let parsed = Args::from_args(&["filectrl"], &argv).unwrap();

        assert_eq!(Some(Path::new(config)), parsed.config.as_deref());
        assert_eq!(Some(Path::new(directory)), parsed.directory.as_deref());
    }

    /// `help` is a directory name like any other word; `-h` and `--help` print
    /// usage.
    #[test]
    fn help_is_a_directory_and_only_the_flags_print_usage() {
        let parsed = Args::from_args(&["filectrl"], &["help"]).unwrap();
        assert_eq!(Some(Path::new("help")), parsed.directory.as_deref());

        for flag in ["-h", "--help"] {
            assert!(Args::from_args(&["filectrl"], &[flag]).is_err(), "{flag}");
        }
    }

    #[test]
    fn encoding_round_trips_every_argument() {
        let cases: [&[u8]; 5] = [
            b"plain",
            b"caf\xc3\xa9",
            // Invalid bytes, including a truncated sequence.
            b"\xe9\xff\xc3",
            // Characters in the escape range itself.
            "\u{F780}\u{F7FF}".as_bytes(),
            b"",
        ];
        for bytes in cases {
            let arg = OsStr::from_bytes(bytes);
            assert_eq!(arg, decode_arg(&encode_arg(arg)), "{bytes:?}");
        }
        // Valid UTF-8 outside the escape range reaches argh unchanged.
        assert_eq!(
            "--config=caf\u{e9}",
            encode_arg(OsStr::new("--config=caf\u{e9}"))
        );
    }
}
