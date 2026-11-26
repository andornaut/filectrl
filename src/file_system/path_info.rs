use std::{
    borrow::Cow,
    cmp::{self, Ordering},
    collections::HashMap,
    ffi::OsStr,
    fmt::{self, Display},
    io,
    os::unix::prelude::{MetadataExt, PermissionsExt},
    path::{MAIN_SEPARATOR, MAIN_SEPARATOR_STR, Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Error, Result};
use nix::unistd::{Gid, Group, Uid, User};
use time::{OffsetDateTime, UtcOffset, macros::format_description};

const FACTOR: u64 = 1024;
const UNITS: [&str; 6] = ["", "K", "M", "G", "T", "P"];

fn display_name(path: &Path) -> String {
    path.file_name().map_or(String::new(), visible_name)
}

/// A file name as it is shown, through `crate::visible_os`.
pub(crate) fn visible_name(name: &OsStr) -> String {
    crate::visible_os(name).into_owned()
}

/// A whole path as it is shown, through `crate::visible_os`.
pub(crate) fn visible_path(path: &Path) -> String {
    crate::visible_os(path.as_os_str()).into_owned()
}

/// Trailing components a compacted path always keeps: the parent and the entry.
const KEPT_TAIL_COMPONENTS: usize = 2;
/// Component count above which the middle is elided.
const MAX_PATH_COMPONENTS: usize = 4;

/// A path rendered for a message: quoted, home as `~`, and a long middle elided
/// to the first component and the last two.
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

/// Quoted, escaping `"` and `\`, each character `crate::is_disguising` names, and
/// each byte that is not valid UTF-8 as `\xNN`.
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

/// The home directory, looked up once: the lookup reads the environment and the
/// password database.
fn home_dir() -> Option<&'static Path> {
    static HOME: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
        .as_deref()
}

/// The path's bytes with home as `~` and, when `elide` is set, a long middle
/// elided. Bytes, so a name that is not UTF-8 reaches `Compact`'s escaping intact.
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

/// User or group names by id, including ids with no name.
type NameCache = HashMap<u32, Option<String>>;

/// The name for `id`, looked up once per process, since a lookup can go to the
/// network (LDAP, NIS). A failed lookup is retried next time.
fn cached_name<E>(
    cache: &Mutex<NameCache>,
    id: u32,
    lookup: impl FnOnce(u32) -> Result<Option<String>, E>,
) -> Option<String> {
    // A poisoned cache still holds only complete entries.
    let mut names = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(name) = names.get(&id) {
        return name.clone();
    }
    let name = lookup(id).ok()?;
    names.insert(id, name.clone());
    name
}

#[derive(Clone, Eq)]
pub struct PathInfo {
    pub path: PathBuf,
    pub display_name: String,
    pub modified: Option<OffsetDateTime>,
    pub size: u64,

    gid: u32,
    uid: u32,
    device: u64,
    inode: u64,
    mode: u32,
    /// A symlink whose target does not exist, resolved when the entry is read.
    symlink_broken: bool,
    /// What a symlink names, read with the entry so a redraw never reads the link.
    symlink_target: Option<PathBuf>,
    accessed: Option<OffsetDateTime>,
    created: Option<OffsetDateTime>,
}

impl PathInfo {
    /// An entry with its type and permission bits set directly, for types that
    /// cannot be created on disk (a block device needs root, a door needs Solaris).
    #[cfg(test)]
    pub(crate) fn with_mode(mode: u32) -> Self {
        let mut info = Self::try_from(Path::new("/")).expect("the root should be readable");
        info.mode = mode;
        info.symlink_broken = false;
        info.symlink_target = None;
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

    pub fn accessed(&self, relative_to: OffsetDateTime) -> Option<String> {
        maybe_time_to_string(relative_to, self.accessed)
    }

    pub fn created(&self, relative_to: OffsetDateTime) -> Option<String> {
        maybe_time_to_string(relative_to, self.created)
    }

    pub fn mode(&self) -> u32 {
        self.mode
    }

    pub fn unix_mode(&self) -> String {
        unix_mode::to_string(self.mode)
    }

    pub fn modified(&self, relative_to: OffsetDateTime) -> Option<String> {
        maybe_time_to_string(relative_to, self.modified)
    }

    pub fn modified_comparator(&self) -> i64 {
        self.modified.map_or(0, OffsetDateTime::unix_timestamp)
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
        static NAMES: LazyLock<Mutex<NameCache>> = LazyLock::new(Mutex::default);
        cached_name(&NAMES, self.gid, |gid| {
            Group::from_gid(Gid::from_raw(gid)).map(|group| group.map(|group| group.name))
        })
    }

    pub fn owner(&self) -> Option<String> {
        static NAMES: LazyLock<Mutex<NameCache>> = LazyLock::new(Mutex::default);
        cached_name(&NAMES, self.uid, |uid| {
            User::from_uid(Uid::from_raw(uid)).map(|user| user.map(|user| user.name))
        })
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

    // `self.mode` is read only on Solaris.
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
        // Inode numbers are unique only within one filesystem.
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

    /// What a symlink names, as stored in the link: relative targets stay
    /// relative to the link's directory.
    pub fn symlink_target(&self) -> Option<&Path> {
        self.symlink_target.as_deref()
    }

    /// Whether this is a symlink whose target does not exist, as of when the entry
    /// was read. Resolved once, since the renderer asks on every frame.
    pub fn is_symlink_broken(&self) -> bool {
        self.symlink_broken
    }
}

impl fmt::Debug for PathInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.path)
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
            // `try_exists` follows the link; only a confirmed "does not exist" counts, not a
            // permission error.
            symlink_broken: unix_mode::is_symlink(mode) && matches!(path.try_exists(), Ok(false)),
            symlink_target: unix_mode::is_symlink(mode)
                .then(|| std::fs::read_link(path).ok())
                .flatten(),
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

// Display only; f64 is exact for integers below 2^53.
#[allow(clippy::cast_precision_loss)]
fn humanize_bytes(bytes: u64, unit_index: usize) -> String {
    if bytes == 0 {
        return "0".to_string();
    }

    let exponent = u32::try_from(unit_index).unwrap_or(0);
    let divisor = FACTOR.pow(exponent) as f64;
    let value = (bytes as f64) / divisor;

    // One decimal only below 10 and when not whole once rounded; rounding first
    // keeps 9.96 from rendering as "10.0".
    let tenths = (value * 10.0).round() / 10.0;
    let formatted_value = if tenths < 10.0 && tenths.fract() != 0.0 {
        format!("{tenths:.1}")
    } else {
        format!("{value:.0}")
    };

    format!("{}{}", formatted_value, UNITS[unit_index])
}

fn unit_index(bytes: u64) -> usize {
    // Below 1 KiB stay in bytes, or 1000..=1023 would render as "1.0K".
    if bytes < FACTOR {
        return 0;
    }
    // Grouped by decimal digit count, so the next unit starts slightly early (1e9
    // bytes renders as "0.9G").
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

/// The Name column's sort key: `name_text`, with digit runs compared as numbers
/// when `natural` (`file2` before `file10`). Ties fall back to the text, then
/// the name, so distinct names never tie.
pub fn name_key(name: &str, natural: bool) -> NameKey {
    NameKey {
        text: name_text(name),
        name: name.to_string(),
        natural,
    }
}

/// Ignores case and leading dots per path segment, like `ls -a` under a UTF-8
/// locale. Takes the shown name, which is relative to the search root while
/// searching.
fn name_text(name: &str) -> String {
    let mut key = String::with_capacity(name.len());
    for (index, segment) in name.split(MAIN_SEPARATOR).enumerate() {
        if index > 0 {
            key.push(MAIN_SEPARATOR);
        }
        key.push_str(segment.trim_start_matches('.'));
    }
    key.to_lowercase()
}

#[derive(Clone, Debug)]
pub struct NameKey {
    text: String,
    name: String,
    natural: bool,
}

impl Ord for NameKey {
    fn cmp(&self, other: &Self) -> Ordering {
        let text = if self.natural || other.natural {
            natural_cmp(&self.text, &other.text).then_with(|| self.text.cmp(&other.text))
        } else {
            self.text.cmp(&other.text)
        };
        text.then_with(|| self.name.cmp(&other.name))
    }
}

/// Equal exactly when `cmp` says so, which ignores the `natural` flag.
impl PartialEq for NameKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for NameKey {}

impl PartialOrd for NameKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Compares bytewise, except that two runs of ASCII digits compare as numbers
/// (leading zeros dropped), so `01` and `1` compare equal.
fn natural_cmp(left: &str, right: &str) -> Ordering {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    let (mut at_left, mut at_right) = (0, 0);
    loop {
        match (left.get(at_left), right.get(at_right)) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(l), Some(r)) if l.is_ascii_digit() && r.is_ascii_digit() => {
                let (left_run, left_end) = digit_run(left, at_left);
                let (right_run, right_end) = digit_run(right, at_right);
                let order = left_run
                    .len()
                    .cmp(&right_run.len())
                    .then_with(|| left_run.cmp(right_run));
                if order != Ordering::Equal {
                    return order;
                }
                (at_left, at_right) = (left_end, right_end);
            }
            (Some(l), Some(r)) => {
                if l != r {
                    return l.cmp(r);
                }
                at_left += 1;
                at_right += 1;
            }
        }
    }
}

/// The run of ASCII digits in `bytes` from `start`, without its leading
/// zeros, and the index just past the run.
fn digit_run(bytes: &[u8], start: usize) -> (&[u8], usize) {
    let end = start
        + bytes[start..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
    let digits = &bytes[start..end];
    let significant = digits
        .iter()
        .position(|&digit| digit != b'0')
        .unwrap_or(digits.len());
    (&digits[significant..], end)
}

pub fn datetime_age(relative_to: OffsetDateTime, datetime: OffsetDateTime) -> DateTimeAge {
    let duration = relative_to - datetime;

    match duration {
        d if d.whole_minutes() == 0 => DateTimeAge::LessThanMinute,
        d if d.whole_hours() == 0 => DateTimeAge::LessThanHour,
        d if d.whole_days() == 0 => DateTimeAge::LessThanDay,
        d if d.whole_days() < 30 => DateTimeAge::LessThanMonth,
        d if d.whole_days() < 365 => DateTimeAge::LessThanYear,
        _ => DateTimeAge::GreaterThanYear,
    }
}

fn humanize_datetime(relative_to: OffsetDateTime, datetime: OffsetDateTime) -> String {
    let format = match datetime_age(relative_to, datetime) {
        DateTimeAge::LessThanMinute => {
            format_description!("[hour repr:12 padding:none]:[minute]:[second][period case:lower]")
        }
        DateTimeAge::LessThanHour | DateTimeAge::LessThanDay => {
            format_description!("[hour repr:12 padding:none]:[minute][period case:lower]")
        }
        DateTimeAge::LessThanMonth | DateTimeAge::LessThanYear
            if datetime.year() == relative_to.year() =>
        {
            format_description!("[month repr:short] [day padding:none]")
        }
        DateTimeAge::LessThanMonth | DateTimeAge::LessThanYear | DateTimeAge::GreaterThanYear => {
            format_description!("[month repr:short] [day padding:none], [year]")
        }
    };
    datetime
        .format(format)
        .expect("a valid date and time format")
}

pub(crate) fn now_local() -> OffsetDateTime {
    in_local_offset(OffsetDateTime::now_utc())
}

fn in_local_offset(utc: OffsetDateTime) -> OffsetDateTime {
    UtcOffset::local_offset_at(utc)
        .ok()
        .and_then(|offset| utc.checked_to_offset(offset))
        .unwrap_or(utc)
}

/// A file owner can set an out-of-range time. Keep the supported UTC year
/// range for file dates and display unknown times as blank.
fn maybe_time(result: io::Result<SystemTime>) -> Option<OffsetDateTime> {
    let nanoseconds = match result.ok()?.duration_since(UNIX_EPOCH) {
        Ok(after) => i128::try_from(after.as_nanos()).ok()?,
        Err(before) => -i128::try_from(before.duration().as_nanos()).ok()?,
    };
    let utc = OffsetDateTime::from_unix_timestamp_nanos(nanoseconds).ok()?;
    (-262_143..=262_142)
        .contains(&utc.year())
        .then(|| in_local_offset(utc))
}

fn maybe_time_to_string(
    relative_to: OffsetDateTime,
    time: Option<OffsetDateTime>,
) -> Option<String> {
    time.map(|time| humanize_datetime(relative_to, time))
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;
    use time::{SignedDuration, macros::datetime};

    #[test_case("0",  0u64 ; "zero bytes")]
    #[test_case("499",  499u64 ; "between 1 and 999 bytes")]
    #[test_case("1000",  1000u64 ; "1000 bytes stays in byte unit")]
    #[test_case("1023",  1023u64 ; "1023 bytes stays in byte unit")]
    #[test_case("1K",  1024u64 ; "1024 bytes is exactly 1K")]
    #[test_case("9.7K",  9900u64 ; "9900 bytes")]
    #[test_case("10K",  10400u64 ; "10400 bytes")]
    #[test_case("10K",  10200u64 ; "just below 10K rounds up to a whole 10K")]
    #[test_case("9.9K",  10188u64 ; "the largest value that keeps a decimal")]
    #[test_case("10K",  10189u64 ; "the smallest value that rounds up to 10K")]
    #[test_case("1K",  1065u64 ; "a fraction that rounds to a whole number drops the decimal")]
    #[test_case("1.1K",  1076u64 ; "a fraction that rounds up keeps the decimal")]
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

    #[test_case("6:00:00am", datetime!(2023-07-12 06:00:00 UTC), datetime!(2023-07-12 06:00:00 UTC); "same time, strip leading 0")]
    #[test_case("12:30:10pm", datetime!(2023-07-12 12:30:10 UTC), datetime!(2023-07-12 12:30:20 UTC); "different second")]
    #[test_case("12:30pm", datetime!(2023-07-12 12:30:10 UTC), datetime!(2023-07-12 12:31:10 UTC); "different minute")]
    #[test_case("12:30pm", datetime!(2023-07-12 12:30:10 UTC), datetime!(2023-07-12 11:30:10 UTC); "different hour")]
    #[test_case("Jul 12", datetime!(2023-07-12 12:30:10 UTC), datetime!(2023-07-13 12:30:10 UTC); "different day")]
    #[test_case("Jul 12", datetime!(2023-07-12 12:30:10 UTC), datetime!(2023-08-13 12:30:10 UTC); "different month")]
    #[test_case("Jul 12, 2023", datetime!(2023-07-12 12:30:10 UTC), datetime!(2022-07-13 12:30:10 UTC); "different year")]
    #[test_case("Jul 9", datetime!(2023-07-09 12:30:10 UTC), datetime!(2023-07-13 12:30:10 UTC); "single digit day has no leading zero")]
    fn humanize_datetime_shows_more_of_the_date_as_it_ages(
        expected: &str,
        datetime: OffsetDateTime,
        relative_to: OffsetDateTime,
    ) {
        let result = humanize_datetime(relative_to, datetime);

        assert_eq!(expected, result);
    }

    #[test]
    fn humanize_datetime_uses_the_local_year_and_elapsed_time() {
        let modified = datetime!(2023-12-31 23:59:45 -05:00);
        let now = datetime!(2024-01-01 00:00:15 -05:00);
        assert_eq!("11:59:45pm", humanize_datetime(now, modified));

        let now = datetime!(2024-01-02 00:00:15 -05:00);
        assert_eq!("Dec 31, 2023", humanize_datetime(now, modified));

        let modified = datetime!(2024-03-10 01:59:45 -05:00);
        let now = datetime!(2024-03-10 03:00:15 -04:00);
        assert_eq!("1:59:45am", humanize_datetime(now, modified));
    }

    #[test_case(UNIX_EPOCH + std::time::Duration::from_secs(1 << 62) ; "far in the future")]
    #[test_case(UNIX_EPOCH - std::time::Duration::from_secs(1 << 62) ; "far in the past")]
    #[test_case(SystemTime::from(datetime!(+262143-01-01 00:00 UTC)) ; "past the newest supported year")]
    #[test_case(SystemTime::from(datetime!(-262144-12-31 23:59:59 UTC)) ; "before the oldest supported year")]
    fn a_time_outside_the_supported_range_is_unknown(time: SystemTime) {
        assert_eq!(None, maybe_time(Ok(time)));
    }

    #[test_case(UNIX_EPOCH + std::time::Duration::from_millis(1_500) => (1, 1_500_000_000) ; "after the epoch")]
    #[test_case(UNIX_EPOCH - std::time::Duration::from_millis(1_500) => (-2, -1_500_000_000) ; "before the epoch with a fraction")]
    #[test_case(UNIX_EPOCH - std::time::Duration::from_secs(2) => (-2, -2_000_000_000) ; "before the epoch on a whole second")]
    fn a_representable_time_converts_exactly(time: SystemTime) -> (i64, i128) {
        let datetime = maybe_time(Ok(time)).unwrap();
        (datetime.unix_timestamp(), datetime.unix_timestamp_nanos())
    }

    #[test_case(datetime!(-262143-01-01 12:00 UTC) => "Jan 1, -262143" ; "oldest supported year")]
    #[test_case(datetime!(+262142-12-31 12:00 UTC) => "Dec 31, +262142" ; "newest supported year")]
    fn the_extremes_of_the_supported_range_render(datetime: OffsetDateTime) -> String {
        let local = maybe_time(Ok(datetime.into())).unwrap();
        assert_eq!(datetime, local);
        humanize_datetime(datetime!(2024-06-15 12:00 UTC), datetime)
    }

    fn age(seconds_ago: i64) -> DateTimeAge {
        let now = datetime!(2024-06-15 12:00 UTC);
        datetime_age(now, now - SignedDuration::seconds(seconds_ago))
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

    #[test]
    fn a_name_is_looked_up_once_per_id() {
        let cache = Mutex::default();
        let mut lookups = Vec::new();
        let mut lookup = |id: u32| {
            lookups.push(id);
            match id {
                1 => Ok(Some("one".to_string())),
                2 => Ok(None),
                _ => Err(()),
            }
        };

        assert_eq!(Some("one".to_string()), cached_name(&cache, 1, &mut lookup));
        assert_eq!(Some("one".to_string()), cached_name(&cache, 1, &mut lookup));
        // An id with no name is remembered too, so it is not asked again.
        assert_eq!(None, cached_name(&cache, 2, &mut lookup));
        assert_eq!(None, cached_name(&cache, 2, &mut lookup));
        // A failed lookup is not, so it is asked again.
        assert_eq!(None, cached_name(&cache, 3, &mut lookup));
        assert_eq!(None, cached_name(&cache, 3, &mut lookup));
        assert_eq!(vec![1, 2, 3, 3], lookups);
    }

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

    // Non-home paths isolate elision and escaping from home abbreviation.
    #[test_case("/fixture/a.txt" => "\"/fixture/a.txt\"" ; "short absolute path is unchanged")]
    #[test_case("/fixture/one/two/a.txt" => "\"/fixture/one/two/a.txt\"" ; "at the component limit is unchanged")]
    #[test_case("/fixture/one/two/three/a.txt" => "\"/fixture/…/three/a.txt\"" ; "past the limit keeps the first and last two")]
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

        let path = Path::new(OsStr::from_bytes(b"/fixture/one/two/three/caf\xe9"));
        assert_eq!("\"/fixture/…/three/caf\\xe9\"", compact(path).to_string());
        let literal = Path::new("/fixture/caf\\xe9");
        assert_eq!("\"/fixture/caf\\\\xe9\"", compact(literal).to_string());
    }

    #[test]
    fn quoted_keeps_the_middle_of_a_long_path() {
        let path = Path::new("/fixture/one/two/three/a\u{202e}.txt");
        assert_eq!(
            "\"/fixture/one/two/three/a\\u{202e}.txt\"",
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
        b.device = b.device.wrapping_add(1);
        assert!(!a.is_same_inode(&b));
    }

    #[test_case(".bashrc",  "bashrc"  ; "strips single leading dot")]
    #[test_case("..hidden", "hidden"  ; "strips all leading dots")]
    #[test_case("Makefile", "makefile"; "lowercases")]
    #[test_case(".README",  "readme"  ; "strips dot and lowercases")]
    #[test_case("Docs/Notes.md", "docs/notes.md" ; "a relative path is normalized whole")]
    // Per segment, like `ls -a`.
    #[test_case("projects/.zshrc", "projects/zshrc" ; "strips a dot below the root")]
    #[test_case("a/.b/c", "a/b/c" ; "strips a dot on an interior segment")]
    #[test_case(".a/.b", "a/b" ; "strips a dot on every segment")]
    fn the_name_text_ignores_case_and_leading_dots(name: &str, expected: &str) {
        assert_eq!(expected, name_text(name));
    }

    #[test_case("file2", "file10", true => Ordering::Less ; "natural reads a run of digits as a number")]
    #[test_case("file2", "file10", false => Ordering::Greater ; "plain compares character by character")]
    #[test_case("v1.9", "v1.10", true => Ordering::Less ; "each run is its own number")]
    #[test_case("a-1", "a1", true => Ordering::Less ; "a character before a digit keeps its place")]
    #[test_case("01", "1", true => Ordering::Less ; "equal numbers fall back to the text")]
    // By the raw name alone `a01` would follow `A1`.
    #[test_case("a01", "A1", true => Ordering::Less ; "the text decides before the name does")]
    #[test_case("B", "a", true => Ordering::Greater ; "case is still ignored")]
    #[test_case("File", "file", true => Ordering::Less ; "names equal but for case are ordered by the name")]
    #[test_case(".bashrc", "bashrc", false => Ordering::Less ; "names equal but for a dot are ordered by the name")]
    fn names_order_by_their_key(a: &str, b: &str, natural: bool) -> Ordering {
        name_key(a, natural).cmp(&name_key(b, natural))
    }

    #[test]
    fn a_symlink_carries_its_target_and_nothing_else_does() {
        use std::os::unix::fs::symlink;

        use crate::test_support::TempDir;

        let fx = TempDir::new("path_info_target");
        let file = fx.join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        let link = fx.join("link");
        symlink("file.txt", &link).unwrap();

        assert_eq!(
            Some(Path::new("file.txt")),
            PathInfo::try_from(&link).unwrap().symlink_target()
        );
        assert_eq!(None, PathInfo::try_from(&file).unwrap().symlink_target());
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

        let target = PathInfo::try_from(&target).unwrap();
        assert!(!target.is_symlink());
        assert!(!target.is_symlink_broken());
    }

    /// Under root the directory stays searchable and this degrades to an intact link.
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

        // Deliberately stale: the answer belongs to the listing, which a reload replaces.
        assert!(!info.is_symlink_broken());
        assert!(PathInfo::try_from(&link).unwrap().is_symlink_broken());
    }
}
