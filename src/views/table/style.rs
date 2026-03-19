use chrono::{DateTime, Local};
use ratatui::style::Style;

use super::columns::SortColumn;
use crate::{
    app::clipboard::ClipboardEntry,
    app::config::theme::{Clipboard, FileType, Table, Theme},
    file_system::path_info::{datetime_age, DateTimeAge, PathInfo},
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
    theme: &Theme,
    item: &PathInfo,
    relative_to: DateTime<Local>,
) -> Style {
    let modified = item.modified.unwrap_or(relative_to);
    let age = datetime_age(modified, relative_to);

    get_date_style(theme, age)
}

pub(super) fn size_style(theme: &Theme, item: &PathInfo) -> Style {
    get_size_style(theme, item.size_unit_index())
}

fn get_date_style(theme: &Theme, age: DateTimeAge) -> Style {
    match age {
        DateTimeAge::LessThanMinute => theme.file_modified_date.less_than_minute(),
        DateTimeAge::LessThanHour => theme.file_modified_date.less_than_hour(),
        DateTimeAge::LessThanDay => theme.file_modified_date.less_than_day(),
        DateTimeAge::LessThanMonth => theme.file_modified_date.less_than_month(),
        DateTimeAge::LessThanYear => theme.file_modified_date.less_than_year(),
        DateTimeAge::GreaterThanYear => theme.file_modified_date.greater_than_year(),
    }
}

fn get_size_style(theme: &Theme, unit_index: usize) -> Style {
    match unit_index {
        0 => theme.file_size.bytes(),
        1 => theme.file_size.kib(),
        2 => theme.file_size.mib(),
        3 => theme.file_size.gib(),
        4 => theme.file_size.tib(),
        _ => theme.file_size.pib(),
    }
}
