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
    clipboard_entry: &Option<ClipboardEntry>,
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

pub(super) fn header_style(table: &Table, sort_column: &SortColumn, column: &SortColumn) -> Style {
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

    // Executable files
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
