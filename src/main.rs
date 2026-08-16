use std::{error::Error, fmt, path::PathBuf, process::ExitCode};

use anyhow::Result;
use argh::FromArgs;

use filectrl::{app::config::Config, print_keybindings, run};

#[derive(FromArgs)]
#[argh(help_triggers("-h", "--help", "help"))]
// Every bool here is an `#[argh(switch)]`, so the count is the number of
// command-line flags rather than state that a richer type could model.
#[allow(clippy::struct_excessive_bools)]
/// FileCTRL is a light, opinionated, responsive, theme-able, and simple Text User Interface (TUI) file manager for Linux and macOS
struct Args {
    /// path to a configuration file
    #[argh(option, short = 'c')]
    config: Option<String>,

    /// include a TOML file to merge on top of the config (repeatable; later files take precedence)
    #[argh(option, short = 'i')]
    include: Vec<String>,

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
    #[argh(positional)]
    directory: Option<String>,
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

fn main() -> ExitCode {
    let args: Args = argh::from_env();
    match dispatch(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            match error.downcast_ref::<UsageError>() {
                Some(usage) => {
                    eprintln!("{usage}\n\nRun filectrl --help for more information.");
                }
                // `{error:#}` flattens the cause chain onto one line, so a
                // failure here reads the same as the alert the app would show
                // for it.
                None => eprintln!("Error: {error:#}"),
            }
            ExitCode::FAILURE
        }
    }
}

fn dispatch(args: &Args) -> Result<()> {
    let action = selected_action(args)?;
    let config = args.config.as_deref().map(PathBuf::from);
    let include: Vec<PathBuf> = args.include.iter().map(PathBuf::from).collect();

    match action {
        Some(Action::PrintVersion) => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(Action::PrintKeybindings) => print_keybindings(config, &include),
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
            &include,
            args.directory.as_deref().map(PathBuf::from).as_deref(),
            args.no_truecolor,
        ),
    }
}

/// Names the file on stdout, resolved rather than as it was written, because
/// the config directory follows `$XDG_CONFIG_HOME` and need not be the
/// `~/.config` path the documentation names.
fn report_written(path: &std::path::Path) {
    println!("Wrote {}", path.display());
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

    #[test]
    fn an_argument_the_action_ignores_is_rejected() {
        let args = Args {
            write_default_config: true,
            include: vec!["theme.toml".to_string()],
            ..args()
        };
        let error = usage_error(&args);
        assert!(error.contains("--include"), "{error}");
        assert!(error.contains("--write-default-config"), "{error}");
    }

    #[test]
    fn a_run_only_flag_is_rejected_by_an_acting_flag() {
        // --no-truecolor only changes how the TUI renders, so it cannot
        // change what --print-keybindings prints.
        let args = Args {
            print_keybindings: true,
            no_truecolor: true,
            ..args()
        };
        let error = usage_error(&args);
        assert!(error.contains("--no-truecolor"), "{error}");
        assert!(error.contains("--print-keybindings"), "{error}");
    }

    #[test]
    fn a_directory_the_action_ignores_is_rejected() {
        let args = Args {
            print_keybindings: true,
            directory: Some("/tmp".to_string()),
            ..args()
        };
        assert!(usage_error(&args).contains("--print-keybindings"));
    }

    #[test]
    fn writing_accepts_the_config_path_and_force() {
        let args = Args {
            write_default_config: true,
            config: Some("/tmp/config.toml".to_string()),
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
            config: Some("/tmp/config.toml".to_string()),
            include: vec!["theme.toml".to_string()],
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
}
