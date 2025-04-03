use std::{ffi::OsStr, path::Path};

use anyhow::{anyhow, Result};

pub(super) fn path_to_string(path: &Path) -> Result<String> {
    // Ref. https://stackoverflow.com/a/42579588,
    // https://stackoverflow.com/a/67205030,
    // https://stackoverflow.com/a/31667995
    osstr_to_string(path.as_os_str())
}

pub(super) fn path_to_basename(path: &Path) -> String {
    // file_name() is None for the root dir (eg. `/`)
    path.file_name()
        .map_or("".into(), |name| osstr_to_string(name).unwrap_or("".into()))
}

fn osstr_to_string(os_str: &OsStr) -> Result<String> {
    // Ref. https://profpatsch.de/notes/rust-string-conversions
    os_str
        .to_os_string()
        .into_string()
        .map_err(|orig| anyhow!("Path cannot be converted to a string: {:?}", orig))
}
