use std::path::PathBuf;

use anyhow::Result;
use argh::FromArgs;

use filectrl::{app::config::write_default_config, run};

#[derive(FromArgs)]

/// FileCTRL is a light, opinionated, responsive, theme-able, and simple Text User Interface (TUI) file manager for Linux and macOS
struct Args {
    /// path to a configuration file
    #[argh(option, short = 'c')]
    config: Option<String>,

    /// write the default config to ~/.config/filectrl/config.toml, then exit
    #[argh(switch)]
    write_default_config: bool,

    /// force 256-color theme (disables truecolor detection)
    #[argh(switch)]
    colors_256: bool,

    /// path to a directory to navigate to
    #[argh(positional)]
    directory: Option<String>,
}

fn main() -> Result<()> {
    let args: Args = argh::from_env();

    if args.write_default_config {
        return write_default_config();
    }

    let config = args.config.map(PathBuf::from);
    let directory = args.directory.map(PathBuf::from);
    run(config, directory, args.colors_256)
}
