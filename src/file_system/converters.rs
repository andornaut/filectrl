use super::human::HumanPath;
use anyhow::{anyhow, Result};
use std::{ffi::OsStr, path::Path};

pub fn path_to_string(path: &Path) -> Result<String> {
    // Ref. https://stackoverflow.com/a/42579588,
    // https://stackoverflow.com/a/67205030,
    // https://stackoverflow.com/a/31667995
    osstr_to_string(path.as_os_str())
}

const UNKNOWN_NAME: &'static str = "<unknown name>";

pub(super) fn path_to_basename(path: &Path) -> String {
    path.file_name().map_or(UNKNOWN_NAME.into(), |name| {
        osstr_to_string(name).unwrap_or(UNKNOWN_NAME.into())
    })
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
