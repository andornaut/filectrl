use std::path::PathBuf;

use anyhow::Result;
use argh::FromArgs;
use filectrl::run;

#[derive(FromArgs)]

/// FileCTRL is a light, opinionated, responsive, theme-able, and simple Text User Interface (TUI) file manager for Linux and macOS
struct Args {
    /// path to a configuration file
    #[argh(option, short = 'c')]
    config_path: Option<String>,

    /// path to a directory to navigate to
    #[argh(positional, arg_name = "directory-path")]
    directory_path: Option<String>,
}

fn main() -> Result<()> {
    let args: Args = argh::from_env();

    let directory = args
        .directory_path
        .map(|directory| PathBuf::from(directory));
    run(directory)
}
