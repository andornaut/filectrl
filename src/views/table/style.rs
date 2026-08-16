use chrono::{DateTime, Local};
use ratatui::style::Style;

use super::columns::SortColumn;
use crate::{
    app::clipboard::ClipboardEntry,
    app::config::theme::{Clipboard, FileModifiedDate, FileSize, FileType, Table},
    file_system::path_info::{DateTimeAge, PathInfo, datetime_age},
};

pub(super) fn clipboard_style(
    clipboard: &Clipboard,
    clipboard_entry: Option<&ClipboardEntry>,
    item: &PathInfo,
) -> Option<Style> {
    let entry = clipboard_entry.as_ref()?;
    if !entry.paths().iter().any(|p| p == item) {
        return None;
    }
    Some(match entry {
        ClipboardEntry::Copy(_) => clipboard.copy(),
        ClipboardEntry::Move(_) => clipboard.cut(),
    })
}

pub(super) fn header_style(table: &Table, sort_column: SortColumn, column: SortColumn) -> Style {
    if sort_column == column {
        table.header_sorted()
    } else {
        table.header()
    }
}

pub(super) fn name_style(theme: &FileType, path: &PathInfo) -> Style {
    // Symlinks should be checked first (highest precedence in ls)
    if path.is_symlink_broken() {
        return theme.symlink_broken();
    }
    if path.is_symlink() {
        return theme.symlink();
    }

    if path.is_directory() {
        if path.is_sticky() && path.is_other_writable() {
            return theme.directory_sticky_other_writable();
        }
        if path.is_other_writable() {
            return theme.directory_other_writable();
        }
        if path.is_sticky() {
            return theme.directory_sticky();
        }
        return theme.directory();
    }

    // Special permission bits (higher precedence than file types in ls)
    if path.is_setuid() {
        return theme.setuid();
    }
    if path.is_setgid() {
        return theme.setgid();
    }

    // Special file types
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

    if path.is_executable() {
        return theme.executable();
    }

    // Pattern-based matches
    if let Some(style) = theme.pattern_styles(&path.name()) {
        return style;
    }

    // Regular files (fi) - if the file is a regular file
    if path.is_file() {
        return theme.regular_file();
    }

    // Normal files (no) - default fallback for anything else
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

    // File type and permission bits, named so the precedence cases below read
    // as the entries they stand for.
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

    /// `name_style` walks a ladder of type and permission checks in `ls`
    /// order, and every entry matches more than one rung: a directory is
    /// executable, a setuid binary is executable, a symlink to a directory is
    /// both. Reordering the arms is silent, so each rung names the style it
    /// must reach.
    ///
    /// The pattern rung is absent: its styles come from `$LS_COLORS`, which the
    /// built-in theme does not carry. `theme.rs` covers the lookup itself.
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

    #[test]
    fn a_broken_symlink_outranks_the_symlink_it_still_is() {
        let theme = file_type();
        let broken = PathInfo::with_mode(SYMLINK).broken();

        // Both predicates answer true for this entry, so the order of the two
        // checks is the whole behavior.
        assert_eq!(theme.symlink_broken(), name_style(theme, &broken));
        assert_ne!(theme.symlink(), name_style(theme, &broken));
    }

    #[test]
    fn a_symlink_is_styled_as_one_even_when_it_points_at_a_directory() {
        let theme = file_type();
        // The mode of a symlink is the link's own, never its target's, so this
        // is what a link to a directory looks like: the directory rung must
        // not be reached through it.
        let link = PathInfo::with_mode(SYMLINK);

        assert_eq!(theme.symlink(), name_style(theme, &link));
        assert_ne!(theme.directory(), name_style(theme, &link));
    }

    #[test]
    fn the_clipboard_style_marks_only_the_entries_it_holds() {
        Config::init_test();
        let clipboard = &Config::global().theme().clipboard;
        let held = PathInfo::try_from("/tmp").unwrap();
        let other = PathInfo::try_from("/").unwrap();

        let cut = ClipboardEntry::Move(vec![held.clone()]);
        assert_eq!(
            Some(clipboard.cut()),
            clipboard_style(clipboard, Some(&cut), &held)
        );
        // Cut and copy are told apart by the style, which is the only thing on
        // screen that says whether pasting will remove the source.
        assert_eq!(
            Some(clipboard.copy()),
            clipboard_style(
                clipboard,
                Some(&ClipboardEntry::Copy(vec![held.clone()])),
                &held
            )
        );
        assert_eq!(None, clipboard_style(clipboard, Some(&cut), &other));
        assert_eq!(None, clipboard_style(clipboard, None, &held));
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
