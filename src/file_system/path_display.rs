use crate::app::command::{Command, CommandHandler};
use anyhow::{anyhow, Error, Result};
use chrono::{DateTime, Local};
use std::{
    cmp::Ordering,
    env,
    ffi::OsStr,
    fs,
    os::unix::prelude::PermissionsExt,
    path::{Path, PathBuf},
};

use super::{human::humanize_bytes, human::humanize_datetime};

#[derive(Default)]
pub struct FileSystem {}

impl FileSystem {
    pub fn cd_to_cwd(&self) -> Result<Command> {
        let directory = env::current_dir()?;
        let directory = PathDisplay::try_from(directory)?;
        self.cd(&directory)
    }

    fn cd(&self, directory: &PathDisplay) -> Result<Command> {
        let directory = directory.clone();
        let entries = fs::read_dir(&directory.path)?;
        let (children, errors): (Vec<_>, Vec<_>) = entries
            .map(|entry| -> Result<PathDisplay> { PathDisplay::try_from(entry?.path()) })
            .partition(Result::is_ok);
        if !errors.is_empty() {
            return Err(anyhow!("Some paths could not be read: {:?}", errors));
        }
        let mut children: Vec<PathDisplay> = children.into_iter().map(Result::unwrap).collect();
        children.sort();
        Ok(Command::UpdateCurrentDir(directory, children))
    }
}

impl CommandHandler for FileSystem {
    fn handle_command(&mut self, command: &Command) -> Option<Command> {
        match command {
            Command::_ChangeDir(directory) => {
                // TODO Propate errors by returning a Result here, and adding an error message Command in App
                Some(self.cd(directory).unwrap())
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq)]
pub struct PathDisplay {
    pub basename: String,
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub mode: String,
    pub modified: DateTime<Local>,
    pub path: String,
    pub size: u64,
}

impl PathDisplay {
    pub fn _breadcrumbs(&self) -> Vec<Self> {
        // Predicate: the path exists, otherwise this will panic
        PathBuf::from(&self.path)
            .ancestors()
            .into_iter()
            .map(|e| PathDisplay::try_from(PathBuf::from(e)).unwrap())
            .collect()
    }

    pub fn human_modified(&self) -> String {
        humanize_datetime(self.modified, Local::now())
    }

    pub fn human_size(&self) -> String {
        humanize_bytes(self.size)
    }
}

impl Ord for PathDisplay {
    fn cmp(&self, other: &Self) -> Ordering {
        to_comparable(self).cmp(&to_comparable(other))
    }
}

impl PartialEq for PathDisplay {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl PartialOrd for PathDisplay {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl TryFrom<&str> for PathDisplay {
    type Error = Error;

    fn try_from(path: &str) -> Result<Self, Self::Error> {
        let path_buf = PathBuf::from(path);
        Self::try_from(path_buf)
    }
}

impl TryFrom<String> for PathDisplay {
    type Error = Error;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        let path_buf = PathBuf::from(path);
        Self::try_from(path_buf)
    }
}

impl TryFrom<PathBuf> for PathDisplay {
    type Error = Error;

    fn try_from(path_buf: PathBuf) -> Result<Self, Self::Error> {
        // Only hold on to the data we care about, and drop DirEntry to avoid consuming File Handles on Unix.
        // Ref: https://doc.rust-lang.org/std/fs/struct.DirEntry.html#platform-specific-behavior
        //   On Unix, the DirEntry struct contains an internal reference to the open directory.
        //   Holding DirEntry objects will consume a file handle even after the ReadDir iterator is dropped.
        let path_buf = path_buf.canonicalize()?;
        let metadata = path_buf.metadata()?; // Will return an Error if the path doesn't exist
        let file_type = metadata.file_type();
        Ok(Self {
            basename: path_buf_to_basename(&path_buf)?,
            is_dir: file_type.is_dir(),
            is_file: file_type.is_file(),
            is_symlink: file_type.is_symlink(),
            mode: mode_to_string(metadata.permissions().mode()),
            modified: metadata.modified()?.into(),
            path: path_buf_to_string(&path_buf)?,
            size: metadata.len(),
        })
    }
}

fn mode_to_string(mode: u32) -> String {
    let mut mode = format!("{mode:o}");
    mode.split_off(mode.len() - 3)
}

fn osstr_to_string(os_str: &OsStr) -> Result<String> {
    // Ref. https://profpatsch.de/notes/rust-string-conversions
    os_str
        .to_os_string()
        .into_string()
        .map_err(|orig| anyhow!("Path cannot be converted to a string: {:?}", orig))
}

fn path_buf_to_basename(path: &Path) -> Result<String> {
    match path.file_name() {
        Some(name) => osstr_to_string(name),
        None => Ok(String::from("")),
    }
}

fn path_buf_to_string(path: &Path) -> Result<String> {
    // Ref. https://stackoverflow.com/a/42579588,
    // https://stackoverflow.com/a/67205030,
    // https://stackoverflow.com/a/31667995
    osstr_to_string(path.as_os_str())
}

fn to_comparable(path: &PathDisplay) -> String {
    path.basename.trim_start_matches('.').to_lowercase()
}
