use std::{
    error::Error,
    ffi::{OsStr, OsString},
    fmt,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::Result;
use argh::FromArgs;

use filectrl::{app::config::Config, escape_for_terminal, print_keybindings, run};

#[derive(FromArgs)]
#[argh(help_triggers("-h", "--help"))]
// Every bool here is an `#[argh(switch)]`, so the count is the number of
// command-line flags rather than state that a richer type could model.
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

    /// print the keybindings, then exit
    #[argh(switch)]
    print_keybindings: bool,

    /// write the default config to the config path, then exit
    #[argh(switch)]
    write_default_config: bool,

    /// write the default theme beside the config as theme.toml, then exit
    #[argh(switch)]
    write_default_themes: bool,

    /// replace an existing file when writing defaults
    #[argh(switch)]
    force: bool,

    /// print the version, then exit
    #[argh(switch, short = 'V')]
    version: bool,

    /// path to a directory to navigate to
    #[argh(positional, from_str_fn(decode_path))]
    directory: Option<PathBuf>,
}

/// A mistake in the command line rather than a failure while carrying it out.
/// Printed like argh's own parse errors, with the same pointer to `--help`, so
/// that every way of getting the invocation wrong reads the same.
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

/// The flags that do one thing and exit. At most one may be given, and each
/// accepts only the arguments that can change what it does; anything else is a
/// mistake in the invocation rather than something to drop silently.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    PrintKeybindings,
    PrintVersion,
    WriteDefaultConfig,
    WriteDefaultThemes,
}

impl Action {
    fn flag(self) -> &'static str {
        match self {
            Self::PrintKeybindings => "--print-keybindings",
            Self::PrintVersion => "--version",
            Self::WriteDefaultConfig => "--write-default-config",
            Self::WriteDefaultThemes => "--write-default-themes",
        }
    }

    /// `--config` names the file to read or to write, so every action but
    /// `--version` takes it. Only printing resolves the whole chain, so only it
    /// takes `--include`. Only writing can replace a file, so only writing takes
    /// `--force`. None of them draw anything, so none take `--no-truecolor`.
    fn accepts(self, argument: &str) -> bool {
        match self {
            Self::PrintKeybindings => matches!(argument, "--config" | "--include"),
            Self::PrintVersion => false,
            Self::WriteDefaultConfig | Self::WriteDefaultThemes => {
                matches!(argument, "--config" | "--force")
            }
        }
    }
}

/// argh parses `&str` alone, so an argument that is not valid UTF-8 is carried
/// through it encoded: each byte outside a valid UTF-8 sequence becomes the
/// private use character `ESCAPE_BASE` + byte (U+F780 to U+F7FF, since such a
/// byte is never ASCII). A character already in that range is encoded byte by
/// byte the same way, so `decode_arg` restores every argument exactly. Only the
/// values of path arguments are decoded; an option name is ASCII either way.
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

// The signature is the one argh's `from_str_fn` requires.
#[allow(clippy::unnecessary_wraps)]
fn decode_path(value: &str) -> Result<PathBuf, String> {
    Ok(PathBuf::from(decode_arg(value)))
}

/// `argh::from_env`, but over `args_os` through `encode_arg`, where `from_env`
/// exits on the first argument that is not valid UTF-8.
fn parse_args() -> Args {
    let strings: Vec<String> = std::env::args_os().map(|arg| encode_arg(&arg)).collect();
    let Some((program, rest)) = strings.split_first() else {
        eprintln!("No program name, argv is empty");
        std::process::exit(1)
    };
    let command = Path::new(program)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(program);
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    Args::from_args(&[command], &rest).unwrap_or_else(|early_exit| {
        if early_exit.status.is_ok() {
            println!("{}", early_exit.output);
            std::process::exit(0)
        }
        eprintln!(
            "{}\nRun {command} --help for more information.",
            escape_for_terminal(&early_exit.output)
        );
        std::process::exit(1)
    })
}

fn main() -> ExitCode {
    let args = parse_args();
    match dispatch(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            match error.downcast_ref::<UsageError>() {
                Some(usage) => {
                    let usage = escape_for_terminal(&usage.to_string()).into_owned();
                    eprintln!("{usage}\n\nRun filectrl --help for more information.");
                }
                // `{error:#}` flattens the cause chain onto one line, so a
                // failure here reads the same as the alert the app would show
                // for it.
                None => eprintln!("Error: {}", escape_for_terminal(&format!("{error:#}"))),
            }
            ExitCode::FAILURE
        }
    }
}

fn dispatch(args: &Args) -> Result<()> {
    let action = selected_action(args)?;
    let config = args.config.clone();

    match action {
        Some(Action::PrintVersion) => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(Action::PrintKeybindings) => print_keybindings(config, &args.include),
        Some(Action::WriteDefaultConfig) => {
            report_written(&Config::write_default(config, args.force)?);
            Ok(())
        }
        Some(Action::WriteDefaultThemes) => {
            report_written(&Config::write_default_themes(config, args.force)?);
            Ok(())
        }
        None => run(
            config,
            &args.include,
            args.directory.as_deref(),
            args.no_truecolor,
        ),
    }
}

/// Names the file on stdout, resolved rather than as it was written, because
/// the config directory follows `$XDG_CONFIG_HOME` and need not be the
/// `~/.config` path the documentation names.
fn report_written(path: &Path) {
    println!("Wrote {}", escape_for_terminal(&path.display().to_string()));
}

fn selected_action(args: &Args) -> Result<Option<Action>> {
    let selected: Vec<Action> = [
        (args.print_keybindings, Action::PrintKeybindings),
        (args.version, Action::PrintVersion),
        (args.write_default_config, Action::WriteDefaultConfig),
        (args.write_default_themes, Action::WriteDefaultThemes),
    ]
    .into_iter()
    .filter_map(|(given, action)| given.then_some(action))
    .collect();

    match selected.as_slice() {
        [] => {
            if args.force {
                return Err(usage(
                    "--force has no effect without --write-default-config or --write-default-themes.",
                ));
            }
            Ok(None)
        }
        [action] => {
            reject_unused(args, *action)?;
            Ok(Some(*action))
        }
        // Reported in the order the actions are listed above rather than the
        // order they were typed, which argh does not preserve.
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
        (args.force, "--force"),
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

    /// Every field defaulted, so a test names only what it is exercising.
    fn args() -> Args {
        Args {
            config: None,
            include: Vec::new(),
            no_truecolor: false,
            print_keybindings: false,
            write_default_config: false,
            write_default_themes: false,
            force: false,
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
        assert_eq!(Some(Action::PrintVersion), selected_action(&args).unwrap());
    }

    #[test]
    fn two_action_flags_are_rejected() {
        let args = Args {
            write_default_config: true,
            write_default_themes: true,
            ..args()
        };
        let error = usage_error(&args);
        assert!(error.contains("--write-default-config"), "{error}");
        assert!(error.contains("--write-default-themes"), "{error}");
    }

    /// `action`'s flag, plus the argument that `name` names as it appears in
    /// the usage error.
    fn args_with(action: Action, name: &str) -> Args {
        let mut args = args();
        match action {
            Action::PrintKeybindings => args.print_keybindings = true,
            Action::PrintVersion => args.version = true,
            Action::WriteDefaultConfig => args.write_default_config = true,
            Action::WriteDefaultThemes => args.write_default_themes = true,
        }
        match name {
            "--config" => args.config = Some(PathBuf::from("config.toml")),
            "--include" => args.include = vec![PathBuf::from("theme.toml")],
            "--force" => args.force = true,
            "--no-truecolor" => args.no_truecolor = true,
            "A directory argument" => args.directory = Some(PathBuf::from("/tmp")),
            _ => unreachable!("no such argument: {name}"),
        }
        args
    }

    // --no-truecolor only changes how the TUI renders, and --version prints a
    // constant, so even the flag every other action reads cannot change it.
    #[test_case(Action::WriteDefaultConfig, "--include" ; "include with a write")]
    #[test_case(Action::PrintKeybindings, "--force" ; "force with printing")]
    #[test_case(Action::PrintKeybindings, "--no-truecolor" ; "a run-only flag with printing")]
    #[test_case(Action::PrintVersion, "--config" ; "config with version")]
    #[test_case(Action::PrintKeybindings, "A directory argument" ; "a directory with printing")]
    fn an_argument_the_action_ignores_is_rejected(action: Action, name: &str) {
        assert_eq!(
            format!("{name} has no effect with {}.", action.flag()),
            usage_error(&args_with(action, name))
        );
    }

    #[test]
    fn writing_accepts_the_config_path_and_force() {
        let args = Args {
            write_default_config: true,
            config: Some(PathBuf::from("/tmp/config.toml")),
            force: true,
            ..args()
        };
        assert_eq!(
            Some(Action::WriteDefaultConfig),
            selected_action(&args).unwrap()
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
        assert_eq!(
            Some(Action::PrintKeybindings),
            selected_action(&args).unwrap()
        );
    }

    #[test]
    fn force_without_a_write_flag_is_rejected() {
        let args = Args {
            force: true,
            ..args()
        };
        assert!(usage_error(&args).contains("--force"));
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

    /// There are no subcommands, so `help` names a directory like any other
    /// word; `-h` and `--help` still print usage.
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
            // Characters in the escape range itself, which must not decode as
            // the bytes they would stand for.
            "\u{F780}\u{F7FF}".as_bytes(),
            b"",
        ];
        for bytes in cases {
            let arg = OsStr::from_bytes(bytes);
            assert_eq!(arg, decode_arg(&encode_arg(arg)), "{bytes:?}");
        }
        // Valid UTF-8 outside the escape range reaches argh unchanged, so flags
        // and argh's messages about them read as typed.
        assert_eq!(
            "--config=caf\u{e9}",
            encode_arg(OsStr::new("--config=caf\u{e9}"))
        );
    }
}
