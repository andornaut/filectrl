use crate::app::command::{Command, CommandHandler};
use anyhow::{anyhow, Context, Error, Result};
use chrono::{DateTime, Local};
use std::{
    cmp::Ordering, env, ffi::OsStr, fs, os::unix::prelude::PermissionsExt, path::Path as stdPath,
    path::PathBuf,
};

#[derive(Default)]
pub struct FileSystem {}

impl FileSystem {
    pub fn cd_to_cwd(&self) -> Result<Command> {
        let directory = env::current_dir()?;
        let directory = Path::try_from(directory)?;
        self.cd(&directory)
    }

    fn cd(&self, directory: &Path) -> Result<Command> {
        let directory = directory.clone();
        let entries = fs::read_dir(&directory.path)?;
        let (children, errors): (Vec<_>, Vec<_>) = entries
            .map(|entry| -> Result<Path> { Path::try_from(entry?.path()) })
            .partition(Result::is_ok);
        if !errors.is_empty() {
            return Err(anyhow!("Some paths could not be read: {:?}", errors));
        }
        let mut children: Vec<Path> = children.into_iter().map(Result::unwrap).collect();
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
pub struct Path {
    pub basename: String,
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub mode: u32,
    pub modified: DateTime<Local>,
    pub path: String,
}

impl Path {
    pub fn _breadcrumbs(&self) -> Vec<Self> {
        // Predicate: the path exists, otherwise this will panic
        PathBuf::from(&self.path)
            .ancestors()
            .into_iter()
            .map(|e| Path::try_from(PathBuf::from(e)).unwrap())
            .collect()
    }
}

impl Ord for Path {
    fn cmp(&self, other: &Self) -> Ordering {
        to_comparable(self).cmp(&to_comparable(other))
    }
}

impl PartialEq for Path {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl PartialOrd for Path {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl TryFrom<String> for Path {
    type Error = Error;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        let path_buf = PathBuf::from(path);
        Self::try_from(path_buf)
    }
}

impl TryFrom<PathBuf> for Path {
    type Error = Error;

    fn try_from(path_buf: PathBuf) -> Result<Self, Self::Error> {
        // Only hold on to the data we care about, and drop DirEntry to avoid consuming File Handles on Unix.
        // Ref: https://doc.rust-lang.org/std/fs/struct.DirEntry.html#platform-specific-behavior
        //   On Unix, the DirEntry struct contains an internal reference to the open directory.
        //   Holding DirEntry objects will consume a file handle even after the ReadDir iterator is dropped.
        let path_buf = path_buf.canonicalize()?;
        let metadata = path_buf.metadata()?; // Will reeturn an Error if the path doesn't exist
        let file_type = metadata.file_type();
        let basename = osstr_to_string(path_buf.file_name().context(format!(
            "Path cannot be converted to a string {:?}",
            path_buf
        ))?)?;
        Ok(Self {
            basename,
            is_dir: file_type.is_dir(),
            is_file: file_type.is_file(),
            is_symlink: file_type.is_symlink(),
            mode: metadata.permissions().mode(),
            modified: metadata.modified()?.into(),
            path: path_buf_to_string(path_buf.as_path())?,
        })
    }
}

fn osstr_to_string(os_str: &OsStr) -> Result<String> {
    // Ref. https://profpatsch.de/notes/rust-string-conversions
    os_str
        .to_os_string()
        .into_string()
        .map_err(|orig| anyhow!("Path cannot be converted to a string: {:?}", orig))
}

fn path_buf_to_string(path_buf: &stdPath) -> Result<String> {
    // Ref. https://stackoverflow.com/a/42579588,
    // https://stackoverflow.com/a/67205030,
    // https://stackoverflow.com/a/31667995
    osstr_to_string(path_buf.as_os_str())
}

fn to_comparable(path: &Path) -> String {
    path.basename.trim_start_matches('.').to_lowercase()
}
