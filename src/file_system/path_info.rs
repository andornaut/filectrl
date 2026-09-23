use std::{
    borrow::Cow,
    cmp, env,
    ffi::OsStr,
    fmt::{self, Display},
    io,
    os::unix::prelude::{MetadataExt, PermissionsExt},
    path::{MAIN_SEPARATOR, MAIN_SEPARATOR_STR, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Error, Result};
use chrono::{DateTime, Datelike, Local};
use nix::unistd::{Gid, Group, Uid, User};

const FACTOR: u64 = 1024;
const UNITS: [&str; 6] = ["", "K", "M", "G", "T", "P"];

fn display_name(path: &Path) -> String {
    path.file_name().map_or(String::new(), visible_name)
}

/// A file name as it is shown, through `crate::visible_os`.
pub(crate) fn visible_name(name: &OsStr) -> String {
    crate::visible_os(name).into_owned()
}

/// A whole path as it is shown, for a view that has room for it, through
/// `crate::visible_os`.
pub(crate) fn visible_path(path: &Path) -> String {
    crate::visible_os(path.as_os_str()).into_owned()
}

/// Trailing components a compacted path always keeps: the parent and the entry
/// itself, which together are what identifies it.
const KEPT_TAIL_COMPONENTS: usize = 2;
/// Component count above which the middle is elided. At or below it, the
/// ellipsis would replace no more than it costs.
const MAX_PATH_COMPONENTS: usize = 4;

/// A path rendered for a user-facing message: quoted, home directory as `~`, and
/// a long middle elided to the first component and the last two. A message naming
/// two paths otherwise wraps across several rows of the alerts view and pushes
/// everything else off screen; a path's middle costs the most and says least.
pub struct Compact<'a> {
    path: &'a Path,
    elide: bool,
}

pub fn compact(path: &Path) -> Compact<'_> {
    Compact { path, elide: true }
}

/// `compact` without the elision, for a confirmation whose answer depends on
/// exactly where the path leads.
pub fn quoted(path: &Path) -> Compact<'_> {
    Compact { path, elide: false }
}

/// Quoted, with a quote or backslash in the path escaped so the quotes
/// delimit it, every character `crate::is_disguising` names spelled out, and
/// each byte that is not valid UTF-8 spelled `\xNN`, as `crate::visible_os`
/// does.
impl Display for Compact<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use fmt::Write;

        f.write_char('"')?;
        for chunk in compact_bytes(self.path, self.elide).utf8_chunks() {
            for c in chunk.valid().chars() {
                match c {
                    '"' | '\\' => write!(f, "\\{c}")?,
                    c if crate::is_disguising(c) => write!(f, "{}", c.escape_default())?,
                    c => f.write_char(c)?,
                }
            }
            for byte in chunk.invalid() {
                write!(f, "\\x{byte:02x}")?;
            }
        }
        f.write_char('"')
    }
}

/// The home directory, looked up once: `compact` runs per message and the
/// lookup reads the environment and the password database.
fn home_dir() -> Option<&'static Path> {
    static HOME: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
        .as_deref()
}

/// The path's bytes with the home directory as `~` and, when `elide` is set, a
/// long middle replaced by an ellipsis. Bytes rather than a string, so a name
/// that is not UTF-8 reaches `Compact`'s escaping intact.
fn compact_bytes(path: &Path, elide: bool) -> Vec<u8> {
    let separator = MAIN_SEPARATOR_STR.as_bytes();
    let text = match home_dir().and_then(|home| path.strip_prefix(home).ok()) {
        // The home directory itself strips to an empty path.
        Some(rest) if rest.as_os_str().is_empty() => b"~".to_vec(),
        Some(rest) => [b"~", separator, rest.as_os_str().as_encoded_bytes()].concat(),
        None => path.as_os_str().as_encoded_bytes().to_vec(),
    };

    // Split on the separator rather than walking `Path::components`, so the
    // leading `/` of an absolute path and a `~` root are handled alike.
    let named_parts: Vec<&[u8]> = text
        .split(|byte| *byte == separator[0])
        .filter(|part| !part.is_empty())
        .collect();
    let named = named_parts.len();
    if !elide || named <= MAX_PATH_COMPONENTS {
        return text;
    }
    let root: &[u8] = if text.starts_with(separator) {
        separator
    } else {
        b""
    };
    let head = named_parts[0];
    let tail = named_parts[named - KEPT_TAIL_COMPONENTS..].join(separator);
    [root, head, separator, "…".as_bytes(), separator, &tail].concat()
}

/// Each component of `path` from the root down, the root as an empty string.
pub(crate) fn breadcrumbs(path: &Path) -> Vec<String> {
    let mut parts: Vec<_> = path
        .ancestors()
        .map(|p| p.file_name().map_or(String::new(), visible_name))
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
    device: u64,
    inode: u64,
    mode: u32,
    /// Whether this is a symlink whose target does not exist, resolved when the
    /// entry is read. See `is_symlink_broken`.
    symlink_broken: bool,
    accessed: Option<DateTime<Local>>,
    created: Option<DateTime<Local>>,
}

impl PathInfo {
    /// An entry whose type and permission bits are set directly, for tests of
    /// code that dispatches on them. Some cannot be created on disk at all (a
    /// block device needs root, a door needs Solaris), and for the rest the
    /// mode is what every predicate reads anyway.
    #[cfg(test)]
    pub(crate) fn with_mode(mode: u32) -> Self {
        let mut info = Self::try_from(Path::new("/")).expect("the root should be readable");
        info.mode = mode;
        info.symlink_broken = false;
        info
    }

    /// Marks a symlink built by `with_mode` as one whose target is gone.
    #[cfg(test)]
    pub(crate) fn broken(mut self) -> Self {
        self.symlink_broken = true;
        self
    }

    pub fn as_path(&self) -> &Path {
        &self.path
    }

    pub fn breadcrumbs(&self) -> Vec<String> {
        breadcrumbs(&self.path)
    }

    pub fn accessed(&self, relative_to: DateTime<Local>) -> Option<String> {
        maybe_time_to_string(self.accessed.as_ref(), relative_to)
    }

    pub fn created(&self, relative_to: DateTime<Local>) -> Option<String> {
        maybe_time_to_string(self.created.as_ref(), relative_to)
    }

    pub fn mode(&self) -> u32 {
        self.mode
    }

    pub fn unix_mode(&self) -> String {
        unix_mode::to_string(self.mode)
    }

    pub fn modified(&self, relative_to: DateTime<Local>) -> Option<String> {
        maybe_time_to_string(self.modified.as_ref(), relative_to)
    }

    pub fn modified_comparator(&self) -> i64 {
        self.modified.map_or(0, |dt| dt.timestamp())
    }

    pub fn name(&self) -> Cow<'_, str> {
        if self.is_directory() {
            Cow::Owned(format!("{}{MAIN_SEPARATOR}", self.display_name))
        } else {
            Cow::Borrowed(&self.display_name)
        }
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

    // `self.mode` is read on Solaris; every other target answers false, so
    // the receiver only looks unused where the cfg below compiles it out.
    #[allow(clippy::unused_self)]
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
        // Inode numbers are only unique within one filesystem; entries from
        // different mounts (e.g. two mount points in one listing) can share
        // an inode number, so the device must match too.
        self.device == other.device && self.inode == other.inode
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

    /// Whether this is a symlink whose target does not exist, as of when the
    /// entry was read. Answering means following the link, so it is resolved once
    /// at construction: the renderer asks for every visible symlink on every
    /// frame, and only a change on disk can invalidate the answer, which is what
    /// the watcher reloads the listing for.
    pub fn is_symlink_broken(&self) -> bool {
        self.symlink_broken
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
        let mode = metadata.permissions().mode();

        Ok(Self {
            accessed: maybe_time(metadata.accessed()),
            created: maybe_time(metadata.created()),
            device: metadata.dev(),
            display_name: display_name(path),
            gid: metadata.gid(),
            inode: metadata.ino(),
            mode,
            modified: maybe_time(metadata.modified()),
            path: path.to_path_buf(),
            size: metadata.len(),
            // Only a symlink can be broken, so nothing else pays for the
            // second look at the path. `try_exists` follows the link, so a
            // permission error on the target (or on a parent component) is not
            // misreported as broken; only a confirmed "does not exist" counts.
            symlink_broken: unix_mode::is_symlink(mode) && matches!(path.try_exists(), Ok(false)),
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

// Display-only scaling. f64 carries 53 bits of integer precision, so a size
// would have to exceed 8 exabytes before the rendered figure moved, and the
// unit index is bounded by UNITS.
#[allow(clippy::cast_precision_loss)]
fn humanize_bytes(bytes: u64, unit_index: usize) -> String {
    if bytes == 0 {
        return "0".to_string();
    }

    let exponent = u32::try_from(unit_index).unwrap_or(0);
    let divisor = FACTOR.pow(exponent) as f64;
    let value = (bytes as f64) / divisor;

    // Show one decimal place only for fractional values below 10; otherwise
    // round to a whole number.
    let formatted_value = if value < 10.0 && value.fract() != 0.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
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
    let index = (bytes.ilog10() / FACTOR.ilog10()) as usize;
    cmp::min(index, UNITS.len() - 1)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DateTimeAge {
    LessThanMinute,
    LessThanHour,
    LessThanDay,
    LessThanMonth,
    LessThanYear,
    GreaterThanYear,
}

/// The Name column's ordering rule: case and leading dots are ignored, so a dot
/// file sorts next to its undotted neighbours. What `ls -a` does under a UTF-8
/// locale, whose collation drops the dot rather than hoisting every hidden entry
/// to the top the way `LC_ALL=C` does.
///
/// Takes the name rather than a `PathInfo` because the column shows the path
/// relative to the search root while searching, and the order has to follow what
/// is on screen. Applied per segment for the same reason the locale's is, so a
/// dot file deep in the tree sorts next to its neighbours rather than at the top
/// of its subtree.
pub fn name_comparator(name: &str) -> String {
    let mut key = String::with_capacity(name.len());
    for (index, segment) in name.split(MAIN_SEPARATOR).enumerate() {
        if index > 0 {
            key.push(MAIN_SEPARATOR);
        }
        key.push_str(segment.trim_start_matches('.'));
    }
    key.to_lowercase()
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
            if datetime.year() == relative_to.year() {
                "%b %-d"
            } else {
                "%b %-d, %Y"
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

/// `None` for a time chrono cannot represent, rather than the panic that
/// `DateTime::from(SystemTime)` raises there. A file's owner sets its times, and
/// tmpfs or btrfs store 64-bit seconds, so any listing can contain one.
fn maybe_time(result: io::Result<SystemTime>) -> Option<DateTime<Local>> {
    let (seconds, nanoseconds) = match result.ok()?.duration_since(UNIX_EPOCH) {
        Ok(after) => (i64::try_from(after.as_secs()).ok()?, after.subsec_nanos()),
        // Before the epoch: whole seconds round down, so a positive nanosecond
        // part is counted up from the second before.
        Err(before) => {
            let before = before.duration();
            let seconds = i64::try_from(before.as_secs()).ok()?.checked_neg()?;
            match before.subsec_nanos() {
                0 => (seconds, 0),
                nanoseconds => (seconds.checked_sub(1)?, 1_000_000_000 - nanoseconds),
            }
        }
    };
    Some(DateTime::from_timestamp(seconds, nanoseconds)?.with_timezone(&Local))
}

fn maybe_time_to_string(
    time: Option<&DateTime<Local>>,
    relative_to: DateTime<Local>,
) -> Option<String> {
    time.map(|time| humanize_datetime(*time, relative_to))
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
    fn humanize_bytes_picks_the_unit_by_magnitude(expected: &str, bytes: u64) {
        let result = humanize_bytes(bytes, unit_index(bytes));

        assert_eq!(expected, result);
    }

    #[test_case("6:00:00am", "2023-07-12 6:00:00", "2023-07-12 6:00:00"; "same time, strip leading 0")]
    #[test_case("12:30:10pm", "2023-07-12 12:30:10", "2023-07-12 12:30:20"; "different second")]
    #[test_case("12:30pm", "2023-07-12 12:30:10", "2023-07-12 12:31:10"; "different minute")]
    #[test_case("12:30pm", "2023-07-12 12:30:10", "2023-07-12 11:30:10"; "different hour")]
    #[test_case("Jul 12", "2023-07-12 12:30:10", "2023-07-13 12:30:10"; "different day")]
    #[test_case("Jul 12", "2023-07-12 12:30:10", "2023-08-13 12:30:10"; "different month")]
    #[test_case("Jul 12, 2023", "2023-07-12 12:30:10", "2022-07-13 12:30:10"; "different year")]
    #[test_case("Jul 9", "2023-07-09 12:30:10", "2023-07-13 12:30:10"; "single digit day has no leading zero")]
    fn humanize_datetime_shows_more_of_the_date_as_it_ages(
        expected: &str,
        datetime: &str,
        relative_to: &str,
    ) {
        let result = humanize_datetime(to_local_datetime(datetime), to_local_datetime(relative_to));

        assert_eq!(expected, result);
    }

    fn to_local_datetime(datetime: &str) -> DateTime<Local> {
        let datetime = NaiveDateTime::parse_from_str(datetime, DATETIME_FORMAT).unwrap();
        Local.from_local_datetime(&datetime).unwrap()
    }

    #[test_case(UNIX_EPOCH + std::time::Duration::from_secs(1 << 62) ; "far in the future")]
    #[test_case(UNIX_EPOCH - std::time::Duration::from_secs(1 << 62) ; "far in the past")]
    fn a_time_chrono_cannot_represent_is_unknown(time: SystemTime) {
        assert_eq!(None, maybe_time(Ok(time)));
    }

    #[test_case(UNIX_EPOCH + std::time::Duration::from_millis(1_500) => "1970-01-01T00:00:01.500" ; "after the epoch")]
    #[test_case(UNIX_EPOCH - std::time::Duration::from_millis(1_500) => "1969-12-31T23:59:58.500" ; "before the epoch with a fraction")]
    #[test_case(UNIX_EPOCH - std::time::Duration::from_secs(2) => "1969-12-31T23:59:58.000" ; "before the epoch on a whole second")]
    fn a_representable_time_converts_exactly(time: SystemTime) -> String {
        maybe_time(Ok(time))
            .unwrap()
            .naive_utc()
            .format("%Y-%m-%dT%H:%M:%S%.3f")
            .to_string()
    }

    #[test]
    fn the_extremes_chrono_can_represent_render() {
        let now = Local::now();
        for extreme in [
            DateTime::<chrono::Utc>::MAX_UTC,
            DateTime::<chrono::Utc>::MIN_UTC,
        ] {
            let time = extreme.with_timezone(&Local);
            assert!(maybe_time_to_string(Some(&time), now).is_some());
        }
    }

    // datetime_age boundary tests

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

    // breadcrumbs: root first, the root itself as an empty segment

    #[test_case("/" => vec![String::new()] ; "the root alone")]
    #[test_case("/a/b" => vec![String::new(), "a".to_string(), "b".to_string()] ; "root first, leaf last")]
    fn breadcrumbs_run_from_the_root_down(path: &str) -> Vec<String> {
        breadcrumbs(Path::new(path))
    }

    #[test_case("report.pdf" => "report.pdf" ; "a plain name is unchanged")]
    #[test_case("caf\u{e9} \u{1f600}" => "caf\u{e9} \u{1f600}" ; "accents and emoji are unchanged")]
    #[test_case("\u{645}\u{6cc}\u{200c}\u{62e}\u{648}\u{627}\u{647}\u{645}" => "\u{645}\u{6cc}\u{200c}\u{62e}\u{648}\u{627}\u{647}\u{645}" ; "a joiner a script needs is unchanged")]
    #[test_case("invoice\u{202e}fdp.exe" => "invoice\\u{202e}fdp.exe" ; "a bidi override is escaped")]
    #[test_case("a\u{2067}b\u{2069}" => "a\\u{2067}b\\u{2069}" ; "bidi isolates are escaped")]
    #[test_case("report\u{200b}.pdf" => "report\\u{200b}.pdf" ; "a zero width space is escaped")]
    #[test_case("report\n.pdf" => "report\\n.pdf" ; "a control character is escaped")]
    #[test_case("a\u{2800}" => "a\\u{2800}" ; "a braille blank is escaped")]
    fn visible_name_spells_out_what_would_disguise_it(name: &str) -> String {
        visible_name(OsStr::new(name))
    }

    /// Lossy decoding would show all three as `caf\u{fffd}`.
    #[test]
    fn names_that_differ_in_bytes_that_are_not_utf8_look_different() {
        use std::os::unix::ffi::OsStrExt;

        let shown = [
            visible_name(OsStr::from_bytes(b"caf\xe9")),
            visible_name(OsStr::from_bytes(b"caf\xff")),
            visible_name(OsStr::new("caf\u{fffd}")),
        ];
        assert_eq!(["caf\\xe9", "caf\\xff", "caf\u{fffd}"], shown);
        assert_eq!(
            "/a/caf\\xe9",
            visible_path(Path::new(OsStr::from_bytes(b"/a/caf\xe9")))
        );
    }

    #[test]
    fn a_display_name_and_breadcrumbs_show_a_disguised_name_escaped() {
        let path = Path::new("/a\u{202e}b/c\u{200b}d");
        assert_eq!("c\\u{200b}d", display_name(path));
        assert_eq!(vec!["", "a\\u{202e}b", "c\\u{200b}d"], breadcrumbs(path));
    }

    // compact: home as `~`, long middles elided to first + last two

    #[test_case("/tmp/a.txt" => "\"/tmp/a.txt\"" ; "short absolute path is unchanged")]
    #[test_case("/tmp/one/two/a.txt" => "\"/tmp/one/two/a.txt\"" ; "at the component limit is unchanged")]
    #[test_case("/tmp/one/two/three/a.txt" => "\"/tmp/…/three/a.txt\"" ; "past the limit keeps the first and last two")]
    #[test_case("/a/b/c/d/e/f/g/h.txt" => "\"/a/…/g/h.txt\"" ; "a deep path collapses to four parts")]
    #[test_case("relative/one/two/three/a.txt" => "\"relative/…/three/a.txt\"" ; "a relative path keeps no leading separator")]
    #[test_case("/" => "\"/\"" ; "the root is unchanged")]
    #[test_case("a.txt" => "\"a.txt\"" ; "a bare name is unchanged")]
    fn compact_elides_the_middle_of_a_long_path(path: &str) -> String {
        compact(Path::new(path)).to_string()
    }

    /// A literal backslash is escaped, so it cannot pass for an escape that
    /// `compact` wrote itself.
    #[test]
    fn compact_spells_out_bytes_that_are_not_utf8() {
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(OsStr::from_bytes(b"/tmp/one/two/three/caf\xe9"));
        assert_eq!("\"/tmp/…/three/caf\\xe9\"", compact(path).to_string());
        let literal = Path::new("/tmp/caf\\xe9");
        assert_eq!("\"/tmp/caf\\\\xe9\"", compact(literal).to_string());
    }

    #[test]
    fn quoted_keeps_the_middle_of_a_long_path() {
        let path = Path::new("/tmp/one/two/three/a\u{202e}.txt");
        assert_eq!(
            "\"/tmp/one/two/three/a\\u{202e}.txt\"",
            quoted(path).to_string()
        );
    }

    #[test]
    fn compact_renders_the_home_directory_as_a_tilde() {
        let home = directories::UserDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();

        assert_eq!("\"~\"", compact(&home).to_string());
        assert_eq!("\"~/a.txt\"", compact(&home.join("a.txt")).to_string());
        // Still elided past the limit, counting `~` as the first component.
        assert_eq!(
            "\"~/…/three/a.txt\"",
            compact(&home.join("one/two/three/a.txt")).to_string()
        );
    }

    #[test]
    fn is_same_inode_requires_matching_device() {
        let a = PathInfo::try_from(Path::new(".")).unwrap();
        let mut b = a.clone();
        assert!(a.is_same_inode(&b));
        // Same inode number on a different filesystem is a different file.
        b.device = b.device.wrapping_add(1);
        assert!(!a.is_same_inode(&b));
    }

    #[test_case(".bashrc",  "bashrc"  ; "strips single leading dot")]
    #[test_case("..hidden", "hidden"  ; "strips all leading dots")]
    #[test_case("Makefile", "makefile"; "lowercases")]
    #[test_case(".README",  "readme"  ; "strips dot and lowercases")]
    #[test_case("Docs/Notes.md", "docs/notes.md" ; "a relative path is normalized whole")]
    // Per segment, matching `ls -a`: a dot file below the search root sorts
    // next to its own neighbours, not at the top of its subtree.
    #[test_case("projects/.zshrc", "projects/zshrc" ; "strips a dot below the root")]
    #[test_case("a/.b/c", "a/b/c" ; "strips a dot on an interior segment")]
    #[test_case(".a/.b", "a/b" ; "strips a dot on every segment")]
    fn name_comparator_ignores_case_and_leading_dots(name: &str, expected: &str) {
        assert_eq!(expected, name_comparator(name));
    }

    #[test]
    fn a_symlink_is_broken_only_when_its_target_is_missing() {
        use std::os::unix::fs::symlink;

        use crate::test_support::TempDir;

        let fx = TempDir::new("path_info");
        let target = fx.join("target.txt");
        std::fs::write(&target, b"x").unwrap();

        let intact = fx.join("intact");
        symlink(&target, &intact).unwrap();
        let broken = fx.join("broken");
        symlink(fx.join("absent.txt"), &broken).unwrap();

        let intact = PathInfo::try_from(&intact).unwrap();
        assert!(intact.is_symlink());
        assert!(!intact.is_symlink_broken());

        let broken = PathInfo::try_from(&broken).unwrap();
        assert!(broken.is_symlink());
        assert!(broken.is_symlink_broken());

        // A plain file is neither, and never pays for the second look.
        let target = PathInfo::try_from(&target).unwrap();
        assert!(!target.is_symlink());
        assert!(!target.is_symlink_broken());
    }

    /// A link whose target cannot be checked is not reported broken: only a
    /// confirmed "does not exist" is. Under root the directory stays
    /// searchable and the case degrades to an intact link.
    #[test]
    fn a_symlink_through_an_unsearchable_directory_is_not_broken() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        use crate::test_support::TempDir;

        let fx = TempDir::new("path_info");
        let locked = fx.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("target.txt"), b"x").unwrap();
        let link = fx.join("link");
        symlink(locked.join("target.txt"), &link).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let info = PathInfo::try_from(&link);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(!info.unwrap().is_symlink_broken());
    }

    #[test]
    fn a_directory_name_carries_a_trailing_separator() {
        let mut directory = PathInfo::with_mode(0o040_755);
        directory.display_name = "docs".to_string();
        let mut file = PathInfo::with_mode(0o100_644);
        file.display_name = "docs".to_string();

        assert_eq!(format!("docs{MAIN_SEPARATOR}"), directory.name());
        assert_eq!("docs", file.name());
    }

    #[test]
    fn a_broken_symlink_is_resolved_when_the_entry_is_read() {
        use std::os::unix::fs::symlink;

        use crate::test_support::TempDir;

        let fx = TempDir::new("path_info");
        let target = fx.join("target.txt");
        std::fs::write(&target, b"x").unwrap();
        let link = fx.join("link");
        symlink(&target, &link).unwrap();

        let info = PathInfo::try_from(&link).unwrap();
        std::fs::remove_file(&target).unwrap();

        // Deliberately stale: the answer is a property of the listing, which a
        // reload replaces. Rendering must not have to look at the disk again.
        assert!(!info.is_symlink_broken());
        assert!(PathInfo::try_from(&link).unwrap().is_symlink_broken());
    }
}
