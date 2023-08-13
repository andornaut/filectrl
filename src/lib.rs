mod app;
mod command;
mod file_system;
mod terminal;
mod views;

use crate::{
    app::App,
    terminal::{close_terminal, open_terminal},
};
use anyhow::anyhow;
use anyhow::Result;
use app::config::Config;
use file_system::converters::path_to_string;
use std::{fs, path::PathBuf};

pub fn run(config_path: Option<PathBuf>, directory: Option<PathBuf>) -> Result<()> {
    let mut terminal = open_terminal()?;
    let config = create_config(config_path)?;

    let result = App::new(config).run(&mut terminal, directory);

    close_terminal(&mut terminal)?; // Cleanup even when there's an error
    result
}

fn create_config(path: Option<PathBuf>) -> Result<Config> {
    match path {
        Some(path) => match path_to_string(&path) {
            Ok(config) => config_from_file(config),
            Err(error) => return Err(anyhow!("Cannot read config file at: {path:?}: {error}")),
        },
        None => Ok(Config::default()),
    }
}

fn config_from_file(path: String) -> Result<Config> {
    let content = match fs::read_to_string(&path) {
        Ok(config) => config,
        Err(error) => return Err(anyhow!("Cannot read config file at: {path:?}: {error}")),
    };

    let config: Config = match toml::from_str(&content) {
        Ok(config) => config,
        Err(error) => return Err(anyhow!("Cannot parse config file at: {path:?}: {error}")),
    };
    Ok(config)
}
