use crate::{app::theme::Theme, file_system::human::HumanPath};
use ratatui::style::Style;

use super::SortColumn;

pub(super) fn header_style(theme: &Theme, sort_column: &SortColumn, column: &SortColumn) -> Style {
    if sort_column == column {
        theme.table_header_active()
    } else {
        theme.table_header()
    }
}

pub(super) fn name_style(path: &HumanPath, theme: &Theme) -> Style {
    if path.is_block_device() {
        return theme.table_name_block_device();
    }
    if path.is_character_device() {
        return theme.table_name_character_device();
    }
    if path.is_directory() {
        return theme.table_name_directory();
    }
    if path.is_fifo() {
        return theme.table_name_fifo();
    }
    if path.is_setgid() {
        return theme.table_name_setgid();
    }
    if path.is_setuid() {
        return theme.table_name_setuid();
    }
    if path.is_socket() {
        return theme.table_name_socket();
    }
    if path.is_sticky() {
        return theme.table_name_sticky();
    }
    if path.is_symlink() {
        return theme.table_name_symlink();
    }
    // catch-all
    return theme.table_name_file();
}
