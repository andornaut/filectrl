use super::{
    converters::{mode_to_string, path_to_basename, path_to_string, to_comparable},
    human::{humanize_bytes, humanize_datetime},
};
use anyhow::{Error, Result};
use chrono::{DateTime, Local};
use std::{
    cmp::Ordering,
    env,
    os::unix::prelude::PermissionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq)]
pub struct HumanPath {
    pub basename: String,
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub mode: String,
    pub modified: DateTime<Local>,
    pub path: String,
    pub size: u64,
}

impl HumanPath {
    pub fn breadcrumbs(&self) -> Vec<Self> {
        // Predicate: the path exists, otherwise this will panic
        PathBuf::from(self.path.clone())
            .ancestors()
            .into_iter()
            .map(|path| HumanPath::try_from(&PathBuf::from(path)).unwrap())
            .collect()
    }

    pub fn human_modified(&self) -> String {
        humanize_datetime(self.modified, Local::now())
    }

    pub fn human_size(&self) -> String {
        humanize_bytes(self.size)
    }

    pub fn parent(&self) -> Option<HumanPath> {
        let path_buf = PathBuf::from(self.path.clone());
        match path_buf.parent() {
            Some(parent) => Some(HumanPath::try_from(parent).unwrap()),
            None => None,
        }
    }
}

impl Default for HumanPath {
    fn default() -> Self {
        let directory = env::current_dir().expect("Can get the CWD");
        HumanPath::try_from(&directory).expect("Can create a PathDisplay from the CWD")
    }
}

impl Ord for HumanPath {
    fn cmp(&self, other: &Self) -> Ordering {
        to_comparable(self).cmp(&to_comparable(other))
    }
}

impl PartialEq for HumanPath {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl PartialOrd for HumanPath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl TryFrom<&str> for HumanPath {
    type Error = Error;

    fn try_from(path: &str) -> Result<Self, Self::Error> {
        let path_buf = PathBuf::from(path);
        Self::try_from(&path_buf)
    }
}

impl TryFrom<String> for HumanPath {
    type Error = Error;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        let path_buf = PathBuf::from(path);
        Self::try_from(&path_buf)
    }
}

impl TryFrom<&PathBuf> for HumanPath {
    type Error = Error;

    fn try_from(path_buf: &PathBuf) -> Result<Self, Self::Error> {
        Self::try_from(path_buf.as_path())
    }
}

impl TryFrom<&Path> for HumanPath {
    type Error = Error;

    fn try_from(path_buf: &Path) -> Result<Self, Self::Error> {
        // Only hold on to the data we care about, and drop DirEntry to avoid consuming File Handles on Unix.
        // Ref: https://doc.rust-lang.org/std/fs/struct.DirEntry.html#platform-specific-behavior
        //   On Unix, the DirEntry struct contains an internal reference to the open directory.
        //   Holding DirEntry objects will consume a file handle even after the ReadDir iterator is dropped.
        let path_buf = path_buf.canonicalize()?;
        let metadata = path_buf.metadata()?; // Will return an Error if the path doesn't exist
        let file_type = metadata.file_type();
        Ok(Self {
            basename: path_to_basename(&path_buf)?,
            is_dir: file_type.is_dir(),
            is_file: file_type.is_file(),
            is_symlink: file_type.is_symlink(),
            mode: mode_to_string(metadata.permissions().mode()),
            modified: metadata.modified()?.into(),
            path: path_to_string(&path_buf)?,
            size: metadata.len(),
        })
    }
}
