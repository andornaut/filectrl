use super::converters::{mode_to_string, path_to_basename, path_to_string, to_comparable};
use anyhow::{Error, Result};
use chrono::{DateTime, Datelike, Local, Timelike};
use std::time::SystemTime;
use std::{cmp, io};
use std::{
    cmp::Ordering,
    env,
    fmt::{self, Display},
    os::unix::prelude::PermissionsExt,
    path::{Path, PathBuf},
};

const FACTOR: u64 = 1024;
const UNITS: [&str; 6] = ["", "K", "M", "G", "T", "P"];

#[derive(Clone, Eq)]
pub struct HumanPath {
    pub basename: String,
    pub modified: DateTime<Local>,
    pub path: String,
    pub size: u64,

    mode: u32,
    accessed: Option<DateTime<Local>>,
    created: Option<DateTime<Local>>,
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

    pub fn accessed(&self) -> String {
        maybe_time_to_string(&self.accessed)
    }

    pub fn created(&self) -> String {
        maybe_time_to_string(&self.created)
    }

    pub fn modified(&self) -> String {
        humanize_datetime(self.modified, Local::now())
    }

    pub fn name(&self) -> String {
        let name = self.basename.clone();
        if self.is_dir() {
            name + "/"
        } else {
            name
        }
    }

    pub fn mode(&self) -> String {
        unix_mode::to_string(self.mode)
    }
    pub fn size(&self) -> String {
        humanize_bytes(self.size)
    }
    pub fn is_block_device(&self) -> bool {
        unix_mode::is_block_device(self.mode)
    }

    pub fn is_char_device(&self) -> bool {
        unix_mode::is_char_device(self.mode)
    }

    pub fn is_dir(&self) -> bool {
        unix_mode::is_dir(self.mode)
    }

    pub fn is_fifo(&self) -> bool {
        unix_mode::is_fifo(self.mode)
    }

    pub fn is_file(&self) -> bool {
        unix_mode::is_file(self.mode)
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

    pub fn parent(&self) -> Option<HumanPath> {
        let path_buf = PathBuf::from(self.path.clone());
        match path_buf.parent() {
            Some(parent) => Some(HumanPath::try_from(parent).unwrap()),
            None => None,
        }
    }
}

impl fmt::Debug for HumanPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path)
    }
}

impl Default for HumanPath {
    fn default() -> Self {
        let directory = env::current_dir().expect("Can get the CWD");
        HumanPath::try_from(&directory).expect("Can create a HumanPath from the CWD")
    }
}

impl Display for HumanPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path)
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

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        // Only hold on to the data we care about, and drop DirEntry to avoid consuming File Handles on Unix.
        // Ref: https://doc.rust-lang.org/std/fs/struct.DirEntry.html#platform-specific-behavior
        //   On Unix, the DirEntry struct contains an internal reference to the open directory.
        //   Holding DirEntry objects will consume a file handle even after the ReadDir iterator is dropped.
        let metadata = path.symlink_metadata()?; // Will return an Error if the path doesn't exist
        Ok(Self {
            basename: path_to_basename(&path)?,
            accessed: ok_time(metadata.accessed()),
            created: ok_time(metadata.created()),
            mode: metadata.permissions().mode(),
            modified: metadata.modified()?.into(),
            path: path_to_string(&path)?,
            size: metadata.len(),
        })
    }
}

fn ok_time(result: io::Result<SystemTime>) -> Option<DateTime<Local>> {
    match result {
        Err(_) => None,
        Ok(time) => Some(time.into()),
    }
}

fn humanize_bytes(bytes: u64) -> String {
    if bytes == 0 {
        // Avoid panic: "argument of integer logarithm must be positive"
        return "0".to_string();
    }
    let mut floor = bytes.ilog10() / FACTOR.ilog10();
    floor = cmp::min(floor, (UNITS.len() - 1) as u32);
    let rounded = ((bytes as f64) / (FACTOR.pow(floor) as f64)).round();
    let unit = UNITS[floor as usize];
    format!("{rounded}{unit}")
}

fn humanize_datetime(datetime: DateTime<Local>, relative_to_datetime: DateTime<Local>) -> String {
    let naive_relative_to_date = relative_to_datetime
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let naive_date = datetime.date_naive().and_hms_opt(0, 0, 0).unwrap();
    let format = if naive_date == naive_relative_to_date {
        // Formats: https://docs.rs/chrono/latest/chrono/format/strftime/index.html
        if datetime.hour() == relative_to_datetime.hour()
            && datetime.minute() == relative_to_datetime.minute()
        {
            "%I:%M:%S%P"
        } else {
            "%I:%M%P"
        }
    } else if naive_date.year() == naive_relative_to_date.year() {
        "%b %d"
    } else {
        "%b %d, %Y"
    };
    // Return eg. "6:00am" instead of "06:00am"
    let mut datetime = format!("{}", datetime.format(format));
    if datetime.starts_with('0') {
        datetime.remove(0);
    }
    datetime
}

fn maybe_time_to_string(time: &Option<DateTime<Local>>) -> String {
    match time {
        Some(time) => humanize_datetime(*time, Local::now()),
        None => "unknown".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDateTime, TimeZone};
    use test_case::test_case;

    const DATETIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

    #[test_case("0",  0u64 ; "zero bytes")]
    #[test_case("499",  499u64 ; "between 1 and 999 bytes")]
    #[test_case("10M",  10_000_000u64 ; "10 million bytes rounds up")]
    #[test_case("477M",  500 * 1000u64.pow(2) ; "500 million bytes rounds down")]
    #[test_case("500M",  500 * 1024u64.pow(2) ; "500 MB doesn't round")]
    #[test_case("1G",  1000_000_000u64 ; "1 billion bytes rounds up")]
    #[test_case("1P",  1024u64.pow(5); "max unit")]
    #[test_case("1096P",  1234_000_000_000_000_000u64; "greater than max unit")]
    fn humanize_bytes_is_correct(expected: &str, bytes: u64) {
        let result = humanize_bytes(bytes);

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
