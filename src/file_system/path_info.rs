use std::{
    borrow::Cow,
    cmp, env,
    fmt::{self, Display},
    io,
    os::unix::prelude::{MetadataExt, PermissionsExt},
    path::{MAIN_SEPARATOR, Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Error, Result};
use chrono::{DateTime, Datelike, Local};
use nix::unistd::{Gid, Group, Uid, User};

const FACTOR: u64 = 1024;
const UNITS: [&str; 6] = ["", "K", "M", "G", "T", "P"];

fn display_name(path: &Path) -> String {
    path.file_name()
        .map_or(String::new(), |n| n.to_string_lossy().into_owned())
}

fn breadcrumbs(path: &Path) -> Vec<String> {
    let mut parts: Vec<_> = path
        .ancestors()
        .map(|p| {
            p.file_name()
                .map_or(String::new(), |n| n.to_string_lossy().into_owned())
        })
        .collect();
    parts.reverse();
    parts
}

#[derive(Clone, Eq)]
pub struct PathInfo {
    pub path: PathBuf,
    pub display_name: String,
    pub modified: Option<DateTime<Local>>,
    pub size: u64,

    gid: u32,
    uid: u32,
    inode: u64,
    mode: u32,
    accessed: Option<DateTime<Local>>,
    created: Option<DateTime<Local>>,
}

impl PathInfo {
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    pub fn breadcrumbs(&self) -> Vec<String> {
        breadcrumbs(&self.path)
    }

    pub fn accessed(&self, relative_to: DateTime<Local>) -> Option<String> {
        maybe_time_to_string(&self.accessed, relative_to)
    }

    pub fn created(&self, relative_to: DateTime<Local>) -> Option<String> {
        maybe_time_to_string(&self.created, relative_to)
    }

    pub fn mode(&self) -> u32 {
        self.mode
    }

    pub fn unix_mode(&self) -> String {
        unix_mode::to_string(self.mode)
    }

    pub fn modified(&self, relative_to: DateTime<Local>) -> Option<String> {
        maybe_time_to_string(&self.modified, relative_to)
    }

    pub fn modified_comparator(&self) -> i64 {
        self.modified.map(|dt| dt.timestamp()).unwrap_or(0)
    }

    pub fn name(&self) -> Cow<'_, str> {
        if self.is_directory() {
            Cow::Owned(format!("{}{MAIN_SEPARATOR}", self.display_name))
        } else {
            Cow::Borrowed(&self.display_name)
        }
    }

    pub fn name_comparator(&self) -> String {
        self.display_name.trim_start_matches('.').to_lowercase()
    }

    pub fn is_hidden(&self) -> bool {
        self.display_name.starts_with('.')
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
        self.path
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
        (self.mode & 0o111) != 0
    }

    pub fn is_file(&self) -> bool {
        unix_mode::is_file(self.mode)
    }

    pub fn is_other_writable(&self) -> bool {
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
        // Use `try_exists` so a permission error on the target (or a parent
        // component) is not misreported as a broken link; only a confirmed
        // "does not exist" counts as broken.
        self.is_symlink() && matches!(self.path.try_exists(), Ok(false))
    }
}

impl fmt::Debug for PathInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.path)
    }
}

impl Default for PathInfo {
    fn default() -> Self {
        let path = env::current_dir()
            .or_else(|_| {
                directories::UserDirs::new()
                    .map(|dirs| dirs.home_dir().to_path_buf())
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))
            })
            .unwrap_or_else(|_| PathBuf::from("/"));
        path.as_path()
            .try_into()
            .expect("default directory should be a valid PathInfo")
    }
}

impl Display for PathInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path.display())
    }
}

impl PartialEq for PathInfo {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl std::hash::Hash for PathInfo {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.path.hash(state);
    }
}

impl TryFrom<&Path> for PathInfo {
    type Error = Error;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        let metadata = path.symlink_metadata()?;

        Ok(Self {
            accessed: maybe_time(metadata.accessed()),
            created: maybe_time(metadata.created()),
            display_name: display_name(path),
            gid: metadata.gid(),
            inode: metadata.ino(),
            mode: metadata.permissions().mode(),
            modified: maybe_time(metadata.modified()),
            path: path.to_path_buf(),
            size: metadata.len(),
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

    // Show one decimal place only for fractional values below 10; otherwise
    // round to a whole number.
    let formatted_value = if value < 10.0 && value.fract() != 0.0 {
        format!("{:.1}", value)
    } else {
        format!("{:.0}", value)
    };

    format!("{}{}", formatted_value, UNITS[unit_index])
}

fn unit_index(bytes: u64) -> usize {
    // Below one KiB there is no fractional rendering, so keep these values in
    // the byte unit; otherwise 1000..=1023 would be mislabelled as "1.0K".
    if bytes < FACTOR {
        return 0;
    }
    // For larger values, group by decimal-digit count. This deliberately
    // promotes to the next unit slightly before it is numerically full (e.g.
    // 1e9 bytes renders as "0.9G"), which is the intended display style.
    let index = bytes.ilog10() / FACTOR.ilog10();
    cmp::min(index, (UNITS.len() - 1) as u32) as usize
}

#[derive(Debug, PartialEq)]
pub enum DateTimeAge {
    LessThanMinute,
    LessThanHour,
    LessThanDay,
    LessThanMonth,
    LessThanYear,
    GreaterThanYear,
}

pub fn datetime_age(datetime: DateTime<Local>, relative_to: DateTime<Local>) -> DateTimeAge {
    let duration = relative_to.signed_duration_since(datetime);

    match duration {
        d if d.num_minutes() == 0 => DateTimeAge::LessThanMinute,
        d if d.num_hours() == 0 => DateTimeAge::LessThanHour,
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
        DateTimeAge::LessThanHour | DateTimeAge::LessThanDay => "%I:%M%P",
        DateTimeAge::LessThanMonth | DateTimeAge::LessThanYear => {
            // Show year if dates are from different calendar years
            if datetime.year() != relative_to.year() {
                "%b %-d, %Y"
            } else {
                "%b %-d"
            }
        }
        DateTimeAge::GreaterThanYear => "%b %-d, %Y",
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
    use chrono::{Duration, NaiveDateTime, TimeZone};
    use test_case::test_case;

    const DATETIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

    #[test_case("0",  0u64 ; "zero bytes")]
    #[test_case("499",  499u64 ; "between 1 and 999 bytes")]
    #[test_case("1000",  1000u64 ; "1000 bytes stays in byte unit")]
    #[test_case("1023",  1023u64 ; "1023 bytes stays in byte unit")]
    #[test_case("1K",  1024u64 ; "1024 bytes is exactly 1K")]
    #[test_case("9.7K",  9900u64 ; "9900 bytes")]
    #[test_case("10K",  10400u64 ; "10400 bytes")]
    #[test_case("9.5M",  10_000_000u64 ; "10 million bytes (MB)")]
    #[test_case("10M",  1024u64.pow(2) * 10; "10 MiB")]
    #[test_case("1G",  1024u64.pow(3); "1 GiB")]
    #[test_case("477M",  500 * 1000u64.pow(2) ; "500 million bytes (MB)")]
    #[test_case("500M",  500 * 1024u64.pow(2) ; "500 MiB")]
    #[test_case("0.9G",  1_000_000_000u64 ; "1 billion bytes (MB)")]
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
    #[test_case("Jul 9", "2023-07-09 12:30:10", "2023-07-13 12:30:10"; "single digit day has no leading zero")]
    fn humanize_datetime_is_correct(expected: &str, datetime: &str, relative_to: &str) {
        let result = humanize_datetime(to_local_datetime(datetime), to_local_datetime(relative_to));

        assert_eq!(expected, result);
    }

    fn to_local_datetime(datetime: &str) -> DateTime<Local> {
        let datetime = NaiveDateTime::parse_from_str(datetime, DATETIME_FORMAT).unwrap();
        Local.from_local_datetime(&datetime).unwrap()
    }

    // datetime_age boundary tests
    //
    // The match arms are:
    //   num_minutes() == 0  → LessThanMinute
    //   num_hours()   == 0  → LessThanHour
    //   num_days()    == 0  → LessThanDay
    //   num_days()    < 30  → LessThanMonth
    //   num_days()    < 365 → LessThanYear
    //   _                   → GreaterThanYear

    fn age(seconds_ago: i64) -> DateTimeAge {
        let now = to_local_datetime("2024-06-15 12:00:00");
        datetime_age(now - Duration::seconds(seconds_ago), now)
    }

    #[test_case(0,                        DateTimeAge::LessThanMinute  ; "0 seconds")]
    #[test_case(59,                       DateTimeAge::LessThanMinute  ; "59 seconds, still < 1 minute")]
    #[test_case(60,                       DateTimeAge::LessThanHour    ; "60 seconds crosses into less than hour")]
    #[test_case(3599,                     DateTimeAge::LessThanHour    ; "3599 seconds, still < 1 hour")]
    #[test_case(3600,                     DateTimeAge::LessThanDay     ; "3600 seconds crosses into less than day")]
    #[test_case(23 * 3600 + 59 * 60 + 59, DateTimeAge::LessThanDay    ; "just under one day")]
    #[test_case(24 * 3600,                DateTimeAge::LessThanMonth   ; "exactly one day crosses into less than month")]
    #[test_case(29 * 24 * 3600,           DateTimeAge::LessThanMonth   ; "29 days")]
    #[test_case(30 * 24 * 3600,           DateTimeAge::LessThanYear    ; "30 days crosses into less than year")]
    #[test_case(364 * 24 * 3600,          DateTimeAge::LessThanYear    ; "364 days")]
    #[test_case(365 * 24 * 3600,          DateTimeAge::GreaterThanYear ; "365 days crosses into greater than year")]
    fn datetime_age_boundary(seconds_ago: i64, expected: DateTimeAge) {
        assert_eq!(expected, age(seconds_ago));
    }

    // name_comparator: strips all leading dots, then lowercases

    #[test_case(".bashrc",  "bashrc"  ; "strips single leading dot")]
    #[test_case("..hidden", "hidden"  ; "strips all leading dots")]
    #[test_case("Makefile", "makefile"; "lowercases")]
    #[test_case(".README",  "readme"  ; "strips dot and lowercases")]
    fn name_comparator_is_correct(basename: &str, expected: &str) {
        let mut info = PathInfo::try_from(Path::new(".")).unwrap();
        info.display_name = basename.to_string();
        assert_eq!(expected, info.name_comparator());
    }
}
