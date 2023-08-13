use std::path::PathBuf;

use anyhow::Result;
use argh::FromArgs;
use filectrl::run;

#[derive(FromArgs)]

/// FileCTRL is a light, opinionated, responsive, theme-able, and simple Text User Interface (TUI) file manager for Linux and macOS
struct Args {
    /// path to a configuration file
    #[argh(option, short = 'c')]
    config: Option<String>,

    /// path to a directory to navigate to
    #[argh(positional)]
    directory: Option<String>,
}

fn main() -> Result<()> {
    let args: Args = argh::from_env();

    let config = to_path_buf(args.config);
    let directory = to_path_buf(args.directory);
    run(config, directory)
}

fn to_path_buf(option: Option<String>) -> Option<PathBuf> {
    option.map(|directory| PathBuf::from(directory))
}
