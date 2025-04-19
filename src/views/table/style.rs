use chrono::{DateTime, Local};
use ratatui::style::Style;

use super::SortColumn;
use crate::{
    app::config::theme::{FileTheme, Theme},
    clipboard::ClipboardCommand,
    file_system::path_info::{datetime_age, DateTimeAge, PathInfo},
};

pub(super) fn clipboard_style(
    theme: &Theme,
    clipboard_command: &Option<ClipboardCommand>,
    item: &PathInfo,
) -> Option<Style> {
    match clipboard_command {
        Some(ClipboardCommand::Copy(ref path)) if path == item => Some(theme.table_copied()),
        Some(ClipboardCommand::Move(ref path)) if path == item => Some(theme.table_cut()),
        _ => None,
    }
}

pub(super) fn header_style(theme: &Theme, sort_column: &SortColumn, column: &SortColumn) -> Style {
    if sort_column == column {
        theme.table_header_active()
    } else {
        theme.table_header()
    }
}

pub(super) fn name_style(theme: &FileTheme, path: &PathInfo) -> Style {
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
    relative_to_datetime: DateTime<Local>,
) -> Style {
    let modified = item.modified.unwrap_or(relative_to_datetime);
    let age = datetime_age(modified, relative_to_datetime);

    match age {
        DateTimeAge::LessThanMinute => theme.modified_less_than_minute(),
        DateTimeAge::LessThanDay => theme.modified_less_than_day(),
        DateTimeAge::LessThanMonth => theme.modified_less_than_month(),
        DateTimeAge::LessThanYear => theme.modified_less_than_year(),
        DateTimeAge::GreaterThanYear => theme.modified_greater_than_year(),
    }
}

pub(super) fn size_style(theme: &Theme, item: &PathInfo) -> Style {
    match item.size_unit_index() {
        0 => theme.size_bytes(),
        1 => theme.size_kib(),
        2 => theme.size_mib(),
        3 => theme.size_gib(),
        4 => theme.size_tib(),
        5 => theme.size_pib(),
        _ => theme.size_pib(),
    }
}
