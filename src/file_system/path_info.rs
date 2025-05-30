use std::{
    cmp, env,
    ffi::OsStr,
    fmt::{self, Display},
    io,
    os::unix::prelude::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf, MAIN_SEPARATOR},
    time::SystemTime,
};

use anyhow::{anyhow, Error, Result};
use chrono::{DateTime, Datelike, Local};
use nix::unistd::{Gid, Group, Uid, User};

const FACTOR: u64 = 1024;
const UNITS: [&str; 6] = ["", "K", "M", "G", "T", "P"];

trait PathExt {
    fn to_string(&self) -> Result<String>;
    fn to_basename(&self) -> String;
}

impl PathExt for Path {
    fn to_string(&self) -> Result<String> {
        // Ref. https://stackoverflow.com/a/42579588,
        // https://stackoverflow.com/a/67205030,
        // https://stackoverflow.com/a/31667995
        osstr_to_string(self.as_os_str())
    }

    fn to_basename(&self) -> String {
        // file_name() is None for the root dir (eg. `/`)
        self.file_name()
            .map_or("".into(), |name| osstr_to_string(name).unwrap_or("".into()))
    }
}

fn osstr_to_string(os_str: &OsStr) -> Result<String> {
    // Ref. https://profpatsch.de/notes/rust-string-conversions
    os_str
        .to_os_string()
        .into_string()
        .map_err(|orig| anyhow!("Path cannot be converted to a string: {:?}", orig))
}

#[derive(Clone, Eq, Hash)]
pub struct PathInfo {
    pub basename: String,
    pub modified: Option<DateTime<Local>>,
    pub path: String,
    pub size: u64,

    gid: u32,
    uid: u32,
    inode: u64, // Unique identifier for the file on the system
    mode: u32,
    accessed: Option<DateTime<Local>>,
    created: Option<DateTime<Local>>,
}

impl PathInfo {
    pub fn as_path(&self) -> &Path {
        Path::new(&self.path)
    }

    pub fn breadcrumbs(&self) -> Vec<String> {
        let mut breadcrumbs: Vec<_> = self
            .as_path()
            .ancestors()
            .into_iter()
            .map(|path| path.to_basename())
            .collect();
        breadcrumbs.reverse();
        breadcrumbs
    }

    pub fn accessed(&self, relative_to: DateTime<Local>) -> Option<String> {
        maybe_time_to_string(&self.accessed, relative_to)
    }

    pub fn created(&self, relative_to: DateTime<Local>) -> Option<String> {
        maybe_time_to_string(&self.created, relative_to)
    }

    pub fn mode(&self) -> String {
        unix_mode::to_string(self.mode)
    }

    pub fn modified(&self, relative_to: DateTime<Local>) -> Option<String> {
        maybe_time_to_string(&self.modified, relative_to)
    }

    pub fn modified_comparator(&self) -> i64 {
        self.modified.map(|dt| dt.timestamp()).unwrap_or(0)
    }

    pub fn name(&self) -> String {
        if self.is_directory() {
            format!("{}{MAIN_SEPARATOR}", self.basename)
        } else {
            self.basename.to_string()
        }
    }

    pub fn name_comparator(self: &PathInfo) -> String {
        self.basename.trim_start_matches('.').to_lowercase()
    }

    pub fn group(&self) -> Option<String> {
        Group::from_gid(Gid::from_raw(self.gid))
            .ok()
            .flatten()
            .map(|group| group.name)
    }

    pub fn owner(&self) -> Option<String> {
        User::from_uid(Uid::from_raw(self.uid))
            .ok()
            .flatten()
            .map(|user| user.name)
    }

    pub fn parent(&self) -> Option<PathInfo> {
        self.as_path()
            .parent()
            .and_then(|parent| PathInfo::try_from(parent).ok())
    }

    pub fn size(&self) -> String {
        humanize_bytes(self.size, self.size_unit_index())
    }

    pub fn size_unit_index(&self) -> usize {
        unit_index(self.size)
    }

    pub fn is_block_device(&self) -> bool {
        unix_mode::is_block_device(self.mode)
    }

    pub fn is_character_device(&self) -> bool {
        unix_mode::is_char_device(self.mode)
    }

    pub fn is_directory(&self) -> bool {
        unix_mode::is_dir(self.mode)
    }

    pub fn is_door(&self) -> bool {
        // Check if the file is a door (Solaris IPC mechanism)
        // On non-Solaris systems, this will always return false
        #[cfg(target_os = "solaris")]
        {
            unix_mode::is_door(self.mode)
        }

        #[cfg(not(target_os = "solaris"))]
        {
            false
        }
    }
    pub fn is_executable(&self) -> bool {
        // Check if the file is executable by anyone (user, group, or other)
        // The executable bits are 0o111 in octal (73 in decimal)
        (self.mode & 0o111) != 0
    }

    pub fn is_file(&self) -> bool {
        unix_mode::is_file(self.mode)
    }

    pub fn is_other_writable(&self) -> bool {
        // Check if the directory is writable by others (o+w)
        // The other-writable bit is 0o002 in octal (2 in decimal)
        (self.mode & 0o002) != 0
    }
    pub fn is_pipe(&self) -> bool {
        unix_mode::is_fifo(self.mode)
    }
    pub fn is_same_inode(&self, other: &Self) -> bool {
        self.inode == other.inode
    }
    pub fn is_setgid(&self) -> bool {
        unix_mode::is_setgid(self.mode)
    }

    pub fn is_setuid(&self) -> bool {
        unix_mode::is_setuid(self.mode)
    }

    pub fn is_socket(&self) -> bool {
        unix_mode::is_socket(self.mode)
    }

    pub fn is_sticky(&self) -> bool {
        unix_mode::is_sticky(self.mode)
    }

    pub fn is_symlink(&self) -> bool {
        unix_mode::is_symlink(self.mode)
    }

    pub fn is_symlink_broken(&self) -> bool {
        self.is_symlink() && !self.as_path().exists()
    }
}

impl fmt::Debug for PathInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "\"{}\"", self.path)
    }
}

impl Default for PathInfo {
    fn default() -> Self {
        env::current_dir()
            .expect("Can get the CWD")
            .as_path()
            .try_into()
            .expect("Can create a PathInfo from the CWD")
    }
}

impl Display for PathInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path)
    }
}

impl PartialEq for PathInfo {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl TryFrom<&Path> for PathInfo {
    type Error = Error;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        // Only hold on to the data we care about, and drop DirEntry to avoid consuming File Handles on Unix.
        // Ref: https://doc.rust-lang.org/std/fs/struct.DirEntry.html#platform-specific-behavior
        //   On Unix, the DirEntry struct contains an internal reference to the open directory.
        //   Holding DirEntry objects will consume a file handle even after the ReadDir iterator is dropped.
        let metadata = path.symlink_metadata()?;

        Ok(Self {
            accessed: maybe_time(metadata.accessed()),
            basename: path.to_basename(),
            created: maybe_time(metadata.created()),
            inode: metadata.ino(),
            mode: metadata.permissions().mode(),
            modified: maybe_time(metadata.modified()),
            path: path.to_string()?,
            size: metadata.len(),
            gid: metadata.gid(),
            uid: metadata.uid(),
        })
    }
}

impl TryFrom<&PathBuf> for PathInfo {
    type Error = Error;

    fn try_from(path_buf: &PathBuf) -> Result<Self, Self::Error> {
        Self::try_from(path_buf.as_path())
    }
}

impl TryFrom<&str> for PathInfo {
    type Error = Error;

    fn try_from(path: &str) -> Result<Self, Self::Error> {
        let path_buf = PathBuf::from(path);
        Self::try_from(&path_buf)
    }
}

impl TryFrom<String> for PathInfo {
    type Error = Error;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        let path_buf = PathBuf::from(path);
        Self::try_from(&path_buf)
    }
}

fn humanize_bytes(bytes: u64, unit_index: usize) -> String {
    if bytes == 0 {
        return "0".to_string();
    }

    let divisor = FACTOR.pow(unit_index as u32) as f64;
    let value = (bytes as f64) / divisor;

    // Format based on value:
    // - For values >= 10, show no decimal places
    // - For values with fractional part, show one decimal place
    // - For whole numbers < 10, show no decimal places
    let formatted_value = if value >= 10.0 {
        format!("{:.0}", value)
    } else if value.fract() != 0.0 {
        format!("{:.1}", value)
    } else {
        format!("{:.0}", value)
    };

    format!("{}{}", formatted_value, UNITS[unit_index])
}

fn unit_index(bytes: u64) -> usize {
    match bytes {
        0 => 0,
        b => {
            let index = b.ilog10() / FACTOR.ilog10();
            cmp::min(index, (UNITS.len() - 1) as u32) as usize
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum DateTimeAge {
    LessThanMinute,
    LessThanDay,
    LessThanMonth,
    LessThanYear,
    GreaterThanYear,
}

pub fn datetime_age(datetime: DateTime<Local>, relative_to: DateTime<Local>) -> DateTimeAge {
    let duration = relative_to.signed_duration_since(datetime);

    match duration {
        d if d.num_minutes() == 0 => DateTimeAge::LessThanMinute,
        d if d.num_days() == 0 => DateTimeAge::LessThanDay,
        d if d.num_days() < 30 => DateTimeAge::LessThanMonth,
        d if d.num_days() < 365 => DateTimeAge::LessThanYear,
        _ => DateTimeAge::GreaterThanYear,
    }
}

fn humanize_datetime(datetime: DateTime<Local>, relative_to: DateTime<Local>) -> String {
    let age = datetime_age(datetime, relative_to);
    let format = match age {
        DateTimeAge::LessThanMinute => "%I:%M:%S%P",
        DateTimeAge::LessThanDay => "%I:%M%P",
        DateTimeAge::LessThanMonth | DateTimeAge::LessThanYear => {
            // Show year if dates are from different calendar years
            if datetime.year() != relative_to.year() {
                "%b %d, %Y"
            } else {
                "%b %d"
            }
        }
        DateTimeAge::GreaterThanYear => "%b %d, %Y",
    };
    // Return eg. "6:00:00am" instead of "06:00:00am"
    let mut datetime = format!("{}", datetime.format(format));
    if datetime.starts_with('0') {
        datetime.remove(0);
    }
    datetime
}

fn maybe_time(result: io::Result<SystemTime>) -> Option<DateTime<Local>> {
    result.ok().map(Into::into)
}

fn maybe_time_to_string(
    time: &Option<DateTime<Local>>,
    relative_to: DateTime<Local>,
) -> Option<String> {
    time.map(|time| humanize_datetime(time, relative_to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDateTime, TimeZone};
    use test_case::test_case;

    const DATETIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

    #[test_case("0",  0u64 ; "zero bytes")]
    #[test_case("499",  499u64 ; "between 1 and 999 bytes")]
    #[test_case("9.7K",  9900u64 ; "9900 bytes")]
    #[test_case("10K",  10400u64 ; "10400 bytes")]
    #[test_case("9.5M",  10_000_000u64 ; "10 million bytes (MB)")]
    #[test_case("10M",  1024u64.pow(2) * 10; "10 MiB")]
    #[test_case("1G",  1024u64.pow(3); "1 GiB")]
    #[test_case("477M",  500 * 1000u64.pow(2) ; "500 million bytes (MB)")]
    #[test_case("500M",  500 * 1024u64.pow(2) ; "500 MiB")]
    #[test_case("0.9G",  1000_000_000u64 ; "1 billion bytes (MB)")]
    #[test_case("1P",  1024u64.pow(5); "1 PiB")]
    #[test_case("1024P",   1024u64.pow(6); "greater than 1 PiB")]
    fn humanize_bytes_success_with(expected: &str, bytes: u64) {
        let result = humanize_bytes(bytes, unit_index(bytes));

        assert_eq!(expected, result);
    }

    #[test_case("6:00:00am", "2023-07-12 6:00:00", "2023-07-12 6:00:00"; "same time, strip leading 0")]
    #[test_case("12:30:10pm", "2023-07-12 12:30:10", "2023-07-12 12:30:20"; "different second")]
    #[test_case("12:30pm", "2023-07-12 12:30:10", "2023-07-12 12:31:10"; "different minute")]
    #[test_case("12:30pm", "2023-07-12 12:30:10", "2023-07-12 11:30:10"; "different hour")]
    #[test_case("Jul 12", "2023-07-12 12:30:10", "2023-07-13 12:30:10"; "different day")]
    #[test_case("Jul 12", "2023-07-12 12:30:10", "2023-07-13 12:30:10"; "different month")]
    #[test_case("Jul 12, 2023", "2023-07-12 12:30:10", "2022-07-13 12:30:10"; "different year")]
    fn humanize_datetime_is_correct(expected: &str, datetime: &str, relative_to: &str) {
        let result = humanize_datetime(to_local_datetime(datetime), to_local_datetime(relative_to));

        assert_eq!(expected, result);
    }

    fn to_local_datetime(datetime: &str) -> DateTime<Local> {
        let datetime = NaiveDateTime::parse_from_str(datetime, DATETIME_FORMAT).unwrap();
        Local.from_local_datetime(&datetime).unwrap()
    }
}
