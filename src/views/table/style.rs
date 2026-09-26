use std::collections::HashSet;
use std::path::PathBuf;

use chrono::{DateTime, Local};
use ratatui::style::Style;

use super::columns::SortColumn;
use crate::{
    app::clipboard::ClipboardEntry,
    app::config::theme::{Clipboard, FileModifiedDate, FileSize, FileType, Table},
    file_system::path_info::{DateTimeAge, PathInfo, datetime_age},
};

/// Paths indexed for the render's per-row membership test.
#[derive(Default)]
pub(super) struct PathSet(HashSet<PathBuf>);

impl PathSet {
    pub(super) fn new(paths: &[PathInfo]) -> Self {
        Self(paths.iter().map(|path| path.path.clone()).collect())
    }

    pub(super) fn contains(&self, item: &PathInfo) -> bool {
        self.0.contains(&item.path)
    }
}

/// The clipboard as the table highlights it: whether pasting removes the
/// source, and the paths it holds.
pub(super) struct ClipboardHighlight {
    is_cut: bool,
    paths: PathSet,
}

impl From<&ClipboardEntry> for ClipboardHighlight {
    fn from(entry: &ClipboardEntry) -> Self {
        Self {
            is_cut: matches!(entry, ClipboardEntry::Move(_)),
            paths: PathSet::new(entry.paths()),
        }
    }
}

pub(super) fn clipboard_style(
    clipboard: &Clipboard,
    highlight: Option<&ClipboardHighlight>,
    item: &PathInfo,
) -> Option<Style> {
    let highlight = highlight?;
    if !highlight.paths.contains(item) {
        return None;
    }
    Some(if highlight.is_cut {
        clipboard.cut()
    } else {
        clipboard.copy()
    })
}

pub(super) fn header_style(table: &Table, sort_column: SortColumn, column: SortColumn) -> Style {
    if sort_column == column {
        table.header_sorted()
    } else {
        table.header()
    }
}

/// The style `ls` would give `path`, in its order of precedence. A key reset to
/// uncolored falls through to the next rule, as in `ls`.
pub(super) fn name_style(theme: &FileType, path: &PathInfo) -> Style {
    if path.is_symlink_broken() && theme.is_colored("or") {
        return theme.symlink_broken();
    }
    if path.is_symlink() {
        return theme.symlink();
    }

    if path.is_directory() {
        if path.is_sticky() && path.is_other_writable() && theme.is_colored("tw") {
            return theme.directory_sticky_other_writable();
        }
        if path.is_other_writable() && theme.is_colored("ow") {
            return theme.directory_other_writable();
        }
        if path.is_sticky() && theme.is_colored("st") {
            return theme.directory_sticky();
        }
        return theme.directory();
    }

    if path.is_setuid() && theme.is_colored("su") {
        return theme.setuid();
    }
    if path.is_setgid() && theme.is_colored("sg") {
        return theme.setgid();
    }

    if path.is_block_device() {
        return theme.block_device();
    }
    if path.is_character_device() {
        return theme.character_device();
    }
    if path.is_pipe() {
        return theme.pipe();
    }
    if path.is_socket() {
        return theme.socket();
    }
    if path.is_door() {
        return theme.door();
    }

    if path.is_executable() && theme.is_colored("ex") {
        return theme.executable();
    }

    if let Some(style) = theme.pattern_styles(&path.name()) {
        return style;
    }

    if path.is_file() {
        return theme.regular_file();
    }

    theme.normal_file()
}

pub(super) fn modified_date_style(
    file_modified_date: &FileModifiedDate,
    item: &PathInfo,
    relative_to: DateTime<Local>,
) -> Style {
    let modified = item.modified.unwrap_or(relative_to);
    let age = datetime_age(modified, relative_to);

    match age {
        DateTimeAge::LessThanMinute => file_modified_date.less_than_minute(),
        DateTimeAge::LessThanHour => file_modified_date.less_than_hour(),
        DateTimeAge::LessThanDay => file_modified_date.less_than_day(),
        DateTimeAge::LessThanMonth => file_modified_date.less_than_month(),
        DateTimeAge::LessThanYear => file_modified_date.less_than_year(),
        DateTimeAge::GreaterThanYear => file_modified_date.greater_than_year(),
    }
}

pub(super) fn size_style(file_size: &FileSize, item: &PathInfo) -> Style {
    match item.size_unit_index() {
        0 => file_size.bytes(),
        1 => file_size.kib(),
        2 => file_size.mib(),
        3 => file_size.gib(),
        4 => file_size.tib(),
        _ => file_size.pib(),
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;
    use crate::app::config::Config;

    const REGULAR: u32 = 0o100_644;
    const EXECUTABLE: u32 = 0o100_755;
    const SETUID: u32 = 0o104_755;
    const SETGID: u32 = 0o102_755;
    const DIRECTORY: u32 = 0o040_755;
    const DIRECTORY_STICKY: u32 = 0o041_755;
    const DIRECTORY_OTHER_WRITABLE: u32 = 0o040_757;
    const DIRECTORY_STICKY_OTHER_WRITABLE: u32 = 0o041_757;
    const SYMLINK: u32 = 0o120_777;
    const FIFO: u32 = 0o010_644;
    const SOCKET: u32 = 0o140_644;
    const BLOCK_DEVICE: u32 = 0o060_644;
    const CHARACTER_DEVICE: u32 = 0o020_644;

    fn file_type() -> &'static FileType {
        Config::init_test();
        &Config::global().theme().file_type
    }

    /// Every entry matches more than one rung, so each case pins the style it
    /// must reach. Patterns are covered in `theme.rs`.
    #[test_case(REGULAR, FileType::regular_file ; "a plain file")]
    #[test_case(EXECUTABLE, FileType::executable ; "the execute bit outranks a plain file")]
    #[test_case(SETUID, FileType::setuid ; "setuid outranks the execute bit it implies")]
    #[test_case(SETGID, FileType::setgid ; "setgid outranks the execute bit it implies")]
    #[test_case(DIRECTORY, FileType::directory ; "a directory, though its execute bit is set")]
    #[test_case(DIRECTORY_STICKY, FileType::directory_sticky ; "sticky outranks a plain directory")]
    #[test_case(DIRECTORY_OTHER_WRITABLE, FileType::directory_other_writable ; "other-writable outranks a plain directory")]
    #[test_case(DIRECTORY_STICKY_OTHER_WRITABLE, FileType::directory_sticky_other_writable ; "both bits outrank either alone")]
    #[test_case(SYMLINK, FileType::symlink ; "a symlink, whatever it points at")]
    #[test_case(FIFO, FileType::pipe ; "a fifo")]
    #[test_case(SOCKET, FileType::socket ; "a socket")]
    #[test_case(BLOCK_DEVICE, FileType::block_device ; "a block device")]
    #[test_case(CHARACTER_DEVICE, FileType::character_device ; "a character device")]
    fn name_style_resolves(mode: u32, expected: fn(&FileType) -> Style) {
        let theme = file_type();
        assert_eq!(
            expected(theme),
            name_style(theme, &PathInfo::with_mode(mode))
        );
    }

    #[test_case("ow=00", DIRECTORY_OTHER_WRITABLE, FileType::directory ; "other-writable falls to a directory")]
    #[test_case("tw=00", DIRECTORY_STICKY_OTHER_WRITABLE, FileType::directory_other_writable ; "sticky other-writable falls to other-writable")]
    #[test_case("st=00", DIRECTORY_STICKY, FileType::directory ; "sticky falls to a directory")]
    #[test_case("su=00", SETUID, FileType::executable ; "setuid falls to the execute bit")]
    #[test_case("sg=00", SETGID, FileType::executable ; "setgid falls to the execute bit")]
    #[test_case("ex=00", EXECUTABLE, FileType::regular_file ; "executable falls to a plain file")]
    #[test_case("ex=", EXECUTABLE, FileType::regular_file ; "an empty value falls through too")]
    fn a_reset_key_falls_to_the_next_rule(
        ls_colors: &str,
        mode: u32,
        next: fn(&FileType) -> Style,
    ) {
        let theme = file_type().with_ls_colors(ls_colors);
        let own = name_style(file_type(), &PathInfo::with_mode(mode));
        assert_ne!(
            own,
            next(&theme),
            "the fixture cannot tell the two rules apart"
        );

        assert_eq!(next(&theme), name_style(&theme, &PathInfo::with_mode(mode)));
    }

    #[test]
    fn a_broken_symlink_whose_key_is_reset_is_styled_as_a_symlink() {
        let theme = file_type().with_ls_colors("or=00");
        let broken = PathInfo::with_mode(SYMLINK).broken();
        assert_ne!(theme.symlink_broken(), theme.symlink());

        assert_eq!(theme.symlink(), name_style(&theme, &broken));
    }

    /// `ls` falls through only on exactly `0` or `00`; `0;00` is a color.
    #[test]
    fn reset_codes_that_are_not_exactly_a_reset_render_plain() {
        let theme = file_type().with_ls_colors("ex=0;00");
        assert_ne!(Style::default(), theme.regular_file());

        assert_eq!(
            Style::default(),
            name_style(&theme, &PathInfo::with_mode(EXECUTABLE))
        );
    }

    #[test]
    fn a_reset_directory_key_is_plain() {
        let theme = file_type().with_ls_colors("di=00");
        assert_eq!(
            Style::default(),
            name_style(&theme, &PathInfo::with_mode(DIRECTORY))
        );
    }

    #[test]
    fn a_key_set_after_its_reset_colors_again() {
        let theme = file_type().with_ls_colors("ow=00:ow=01;32");
        assert_eq!(
            theme.directory_other_writable(),
            name_style(&theme, &PathInfo::with_mode(DIRECTORY_OTHER_WRITABLE))
        );
        assert_ne!(theme.directory(), theme.directory_other_writable());
    }

    #[test]
    fn a_broken_symlink_outranks_the_symlink_it_still_is() {
        let theme = file_type();
        let broken = PathInfo::with_mode(SYMLINK).broken();

        assert_eq!(theme.symlink_broken(), name_style(theme, &broken));
        assert_ne!(theme.symlink(), name_style(theme, &broken));
    }

    #[test]
    fn the_clipboard_style_marks_only_the_entries_it_holds() {
        Config::init_test();
        let clipboard = &Config::global().theme().clipboard;
        let held = PathInfo::try_from("/tmp").unwrap();
        let other = PathInfo::try_from("/").unwrap();

        let cut = ClipboardHighlight::from(&ClipboardEntry::Move(vec![held.clone()]));
        assert_eq!(
            Some(clipboard.cut()),
            clipboard_style(clipboard, Some(&cut), &held)
        );
        assert_eq!(
            Some(clipboard.copy()),
            clipboard_style(
                clipboard,
                Some(&ClipboardHighlight::from(&ClipboardEntry::Copy(vec![
                    held.clone()
                ]))),
                &held
            )
        );
        assert_eq!(None, clipboard_style(clipboard, Some(&cut), &other));
        assert_eq!(None, clipboard_style(clipboard, None, &held));
    }

    #[test_case(0, FileSize::bytes ; "bytes")]
    #[test_case(1 << 10, FileSize::kib ; "kibibytes")]
    #[test_case(1 << 20, FileSize::mib ; "mebibytes")]
    #[test_case(1 << 30, FileSize::gib ; "gibibytes")]
    #[test_case(1 << 40, FileSize::tib ; "tebibytes")]
    #[test_case(1 << 50, FileSize::pib ; "pebibytes")]
    fn size_style_follows_the_unit_shown(size: u64, expected: fn(&FileSize) -> Style) {
        Config::init_test();
        let theme = &Config::global().theme().file_size;
        let mut item = PathInfo::with_mode(REGULAR);
        item.size = size;

        assert_eq!(expected(theme), size_style(theme, &item));
    }

    #[test_case(0, FileModifiedDate::less_than_minute ; "just now")]
    #[test_case(60 * 5, FileModifiedDate::less_than_hour ; "minutes ago")]
    #[test_case(3600 * 5, FileModifiedDate::less_than_day ; "hours ago")]
    #[test_case(86400 * 5, FileModifiedDate::less_than_month ; "days ago")]
    #[test_case(86400 * 60, FileModifiedDate::less_than_year ; "months ago")]
    #[test_case(86400 * 400, FileModifiedDate::greater_than_year ; "years ago")]
    fn modified_date_style_follows_the_age(
        seconds_ago: i64,
        expected: fn(&FileModifiedDate) -> Style,
    ) {
        Config::init_test();
        let theme = &Config::global().theme().file_modified_date;
        let now = Local::now();
        let mut item = PathInfo::with_mode(REGULAR);
        item.modified = Some(now - chrono::Duration::seconds(seconds_ago));

        assert_eq!(expected(theme), modified_date_style(theme, &item, now));
    }

    #[test]
    fn only_the_sorted_column_gets_the_sorted_header_style() {
        Config::init_test();
        let table = &Config::global().theme().table;

        assert_eq!(
            table.header_sorted(),
            header_style(table, SortColumn::Name, SortColumn::Name)
        );
        assert_eq!(
            table.header(),
            header_style(table, SortColumn::Name, SortColumn::Size)
        );
    }
}
