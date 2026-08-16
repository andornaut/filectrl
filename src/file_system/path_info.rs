use std::{
    borrow::Cow,
    cmp, env,
    fmt::{self, Display},
    io,
    os::unix::prelude::{MetadataExt, PermissionsExt},
    path::{MAIN_SEPARATOR, MAIN_SEPARATOR_STR, Path, PathBuf},
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
pub struct Compact<'a>(&'a Path);

pub fn compact(path: &Path) -> Compact<'_> {
    Compact(path)
}

impl Display for Compact<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", compact_str(self.0))
    }
}

/// The home directory, looked up once: `compact` runs per message and the
/// lookup reads the environment and the password database.
fn home_dir() -> Option<&'static Path> {
    static HOME: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
        .as_deref()
}

fn compact_str(path: &Path) -> String {
    let text = match home_dir().and_then(|home| path.strip_prefix(home).ok()) {
        // The home directory itself strips to an empty path.
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~{MAIN_SEPARATOR}{}", rest.display()),
        None => path.display().to_string(),
    };

    // Split on the separator rather than walking `Path::components`, so the
    // leading `/` of an absolute path and a `~` root are handled alike.
    let parts: Vec<&str> = text.split(MAIN_SEPARATOR).collect();
    let named = parts.iter().filter(|part| !part.is_empty()).count();
    if named <= MAX_PATH_COMPONENTS {
        return text;
    }
    let is_absolute = parts.first().is_some_and(|first| first.is_empty());
    let named_parts: Vec<&str> = parts.into_iter().filter(|part| !part.is_empty()).collect();
    let head = named_parts[0];
    let tail = named_parts[named - KEPT_TAIL_COMPONENTS..].join(MAIN_SEPARATOR_STR);
    let root = if is_absolute {
        MAIN_SEPARATOR.to_string()
    } else {
        String::new()
    };
    format!("{root}{head}{MAIN_SEPARATOR}…{MAIN_SEPARATOR}{tail}")
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

fn maybe_time(result: io::Result<SystemTime>) -> Option<DateTime<Local>> {
    result.ok().map(Into::into)
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

    // compact: home as `~`, long middles elided to first + last two

    #[test_case("/tmp/a.txt" => "\"/tmp/a.txt\"" ; "short absolute path is unchanged")]
    #[test_case("/tmp/one/two/a.txt" => "\"/tmp/one/two/a.txt\"" ; "at the component limit is unchanged")]
    #[test_case("/tmp/one/two/three/a.txt" => "\"/tmp/…/three/a.txt\"" ; "past the limit keeps the first and last two")]
    #[test_case("/a/b/c/d/e/f/g/h.txt" => "\"/a/…/g/h.txt\"" ; "a deep path collapses to four parts")]
    #[test_case("relative/one/two/three/a.txt" => "\"relative/…/three/a.txt\"" ; "a relative path keeps no leading separator")]
    #[test_case("/" => "\"/\"" ; "the root is unchanged")]
    #[test_case("a.txt" => "\"a.txt\"" ; "a bare name is unchanged")]
    fn compact_is_correct(path: &str) -> String {
        compact(Path::new(path)).to_string()
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

    // name_comparator: strips all leading dots, then lowercases

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
    fn name_comparator_is_correct(name: &str, expected: &str) {
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
