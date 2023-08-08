use super::human::HumanPath;
use anyhow::{anyhow, Result};
use std::{ffi::OsStr, path::Path};

pub(super) fn mode_to_string(mode: u32) -> String {
    let mut mode = format!("{mode:o}");
    mode.split_off(mode.len() - 3)
}

pub(super) fn path_to_basename(path: &Path) -> Result<String> {
    match path.file_name() {
        Some(name) => osstr_to_string(name),
        None => Ok(String::from("")),
    }
}

pub(super) fn path_to_string(path: &Path) -> Result<String> {
    // Ref. https://stackoverflow.com/a/42579588,
    // https://stackoverflow.com/a/67205030,
    // https://stackoverflow.com/a/31667995
    osstr_to_string(path.as_os_str())
}

pub(super) fn to_comparable(path: &HumanPath) -> String {
    path.basename.trim_start_matches('.').to_lowercase()
}

fn osstr_to_string(os_str: &OsStr) -> Result<String> {
    // Ref. https://profpatsch.de/notes/rust-string-conversions
    os_str
        .to_os_string()
        .into_string()
        .map_err(|orig| anyhow!("Path cannot be converted to a string: {:?}", orig))
}
