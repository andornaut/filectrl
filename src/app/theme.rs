use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct Theme {
    #[serde(with = "color_to_tui")]
    error_bg: Color,
    #[serde(with = "color_to_tui")]
    error_fg: Color,

    #[serde(with = "color_to_tui")]
    header_active_bg: Color,
    #[serde(with = "color_to_tui")]
    header_active_fg: Color,
    #[serde(with = "color_to_tui")]
    header_bg: Color,
    #[serde(with = "color_to_tui")]
    header_fg: Color,

    #[serde(with = "color_to_tui")]
    help_bg: Color,
    #[serde(with = "color_to_tui")]
    help_fg: Color,

    #[serde(with = "color_to_tui")]
    prompt_input_bg: Color,
    #[serde(with = "color_to_tui")]
    prompt_input_fg: Color,
    #[serde(with = "color_to_tui")]
    prompt_label_bg: Color,
    #[serde(with = "color_to_tui")]
    prompt_label_fg: Color,

    #[serde(with = "color_to_tui")]
    status_directory_bg: Color,
    #[serde(with = "color_to_tui")]
    status_directory_fg: Color,
    #[serde(with = "color_to_tui")]
    status_directory_label_bg: Color,
    #[serde(with = "color_to_tui")]
    status_directory_label_fg: Color,
    #[serde(with = "color_to_tui")]
    status_filter_bg: Color,
    #[serde(with = "color_to_tui")]
    status_filter_fg: Color,
    #[serde(with = "color_to_tui")]
    status_selected_bg: Color,
    #[serde(with = "color_to_tui")]
    status_selected_fg: Color,
    #[serde(with = "color_to_tui")]
    status_selected_label_bg: Color,
    #[serde(with = "color_to_tui")]
    status_selected_label_fg: Color,

    #[serde(with = "color_to_tui")]
    table_header_active_bg: Color,
    #[serde(with = "color_to_tui")]
    table_header_active_fg: Color,
    #[serde(with = "color_to_tui")]
    table_header_bg: Color,
    #[serde(with = "color_to_tui")]
    table_header_fg: Color,

    #[serde(with = "color_to_tui")]
    table_name_block_device_bg: Color,
    #[serde(with = "color_to_tui")]
    table_name_block_device_fg: Color,
    #[serde(with = "color_to_tui")]
    table_name_character_device_bg: Color,
    #[serde(with = "color_to_tui")]
    table_name_character_device_fg: Color,
    #[serde(with = "color_to_tui")]
    table_name_directory_bg: Color,
    #[serde(with = "color_to_tui")]
    table_name_directory_fg: Color,
    #[serde(with = "color_to_tui")]
    table_name_fifo_bg: Color,
    #[serde(with = "color_to_tui")]
    table_name_fifo_fg: Color,
    #[serde(with = "color_to_tui")]
    table_name_file_bg: Color,
    #[serde(with = "color_to_tui")]
    table_name_file_fg: Color,
    #[serde(with = "color_to_tui")]
    table_name_setgid_bg: Color,
    #[serde(with = "color_to_tui")]
    table_name_setgid_fg: Color,
    #[serde(with = "color_to_tui")]
    table_name_setuid_bg: Color,
    #[serde(with = "color_to_tui")]
    table_name_setuid_fg: Color,
    #[serde(with = "color_to_tui")]
    table_name_socket_bg: Color,
    #[serde(with = "color_to_tui")]
    table_name_socket_fg: Color,
    #[serde(with = "color_to_tui")]
    table_name_sticky_bg: Color,
    #[serde(with = "color_to_tui")]
    table_name_sticky_fg: Color,
    #[serde(with = "color_to_tui")]
    table_name_symlink_bg: Color,
    #[serde(with = "color_to_tui")]
    table_name_symlink_fg: Color,
    #[serde(with = "color_to_tui")]
    table_name_symlink_broken_bg: Color,
    #[serde(with = "color_to_tui")]
    table_name_symlink_broken_fg: Color,

    #[serde(with = "color_to_tui")]
    table_scrollbar_thumb_bg: Color,
    #[serde(with = "color_to_tui")]
    table_scrollbar_thumb_fg: Color,
    #[serde(with = "color_to_tui")]
    table_scrollbar_track_bg: Color,
    #[serde(with = "color_to_tui")]
    table_scrollbar_track_fg: Color,

    #[serde(with = "color_to_tui")]
    table_selected_bg: Color,
    #[serde(with = "color_to_tui")]
    table_selected_fg: Color,
}

impl Theme {
    pub fn error(&self) -> Style {
        Style::default().bg(self.error_bg).fg(self.error_fg)
    }

    pub fn header(&self) -> Style {
        Style::default().bg(self.header_bg).fg(self.header_fg)
    }

    pub fn header_active(&self) -> Style {
        Style::default()
            .bg(self.header_active_bg)
            .fg(self.header_active_fg)
    }

    pub fn help(&self) -> Style {
        Style::default().bg(self.help_bg).fg(self.help_fg)
    }

    pub fn prompt_input(&self) -> Style {
        Style::default()
            .bg(self.prompt_input_bg)
            .fg(self.prompt_input_fg)
    }

    pub fn prompt_label(&self) -> Style {
        Style::default()
            .bg(self.prompt_label_bg)
            .fg(self.prompt_label_fg)
    }

    pub fn status_filter(&self) -> Style {
        Style::default()
            .bg(self.status_filter_bg)
            .fg(self.status_filter_fg)
    }

    pub fn status_directory(&self) -> Style {
        Style::default()
            .bg(self.status_directory_bg)
            .fg(self.status_directory_fg)
    }

    pub fn status_directory_label(&self) -> Style {
        Style::default()
            .bg(self.status_directory_label_bg)
            .fg(self.status_directory_label_fg)
    }

    pub fn status_selected(&self) -> Style {
        Style::default()
            .bg(self.status_selected_bg)
            .fg(self.status_selected_fg)
    }

    pub fn status_selected_label(&self) -> Style {
        Style::default()
            .bg(self.status_selected_label_bg)
            .fg(self.status_selected_label_fg)
    }

    pub fn table_header(&self) -> Style {
        Style::default()
            .bg(self.table_header_bg)
            .fg(self.table_header_fg)
    }

    pub fn table_header_active(&self) -> Style {
        Style::default()
            .add_modifier(Modifier::BOLD)
            .bg(self.table_header_active_bg)
            .fg(self.table_header_active_fg)
    }

    pub fn table_name_block_device(&self) -> Style {
        Style::default()
            .bg(self.table_name_block_device_bg)
            .fg(self.table_name_block_device_fg)
    }

    pub fn table_name_character_device(&self) -> Style {
        Style::default()
            .bg(self.table_name_character_device_bg)
            .fg(self.table_name_character_device_fg)
    }

    pub fn table_name_directory(&self) -> Style {
        Style::default()
            .bg(self.table_name_directory_bg)
            .fg(self.table_name_directory_fg)
    }

    pub fn table_name_fifo(&self) -> Style {
        Style::default()
            .bg(self.table_name_fifo_bg)
            .fg(self.table_name_fifo_fg)
    }

    pub fn table_name_file(&self) -> Style {
        Style::default()
            .bg(self.table_name_file_bg)
            .fg(self.table_name_file_fg)
    }

    pub fn table_name_setgid(&self) -> Style {
        Style::default()
            .bg(self.table_name_setgid_bg)
            .fg(self.table_name_setgid_fg)
    }

    pub fn table_name_setuid(&self) -> Style {
        Style::default()
            .bg(self.table_name_setuid_bg)
            .fg(self.table_name_setuid_fg)
    }

    pub fn table_name_socket(&self) -> Style {
        Style::default()
            .bg(self.table_name_socket_bg)
            .fg(self.table_name_socket_fg)
    }

    pub fn table_name_sticky(&self) -> Style {
        Style::default()
            .bg(self.table_name_sticky_bg)
            .fg(self.table_name_sticky_fg)
    }

    pub fn table_name_symlink(&self) -> Style {
        Style::default()
            .bg(self.table_name_symlink_bg)
            .fg(self.table_name_symlink_fg)
    }

    pub fn table_name_symlink_broken(&self) -> Style {
        Style::default()
            .bg(self.table_name_symlink_broken_bg)
            .fg(self.table_name_symlink_broken_fg)
    }

    pub fn table_scrollbar_thumb(&self) -> Style {
        Style::default()
            .bg(self.table_scrollbar_thumb_bg)
            .fg(self.table_scrollbar_thumb_fg)
    }

    pub fn table_scrollbar_track(&self) -> Style {
        Style::default()
            .bg(self.table_scrollbar_track_bg)
            .fg(self.table_scrollbar_track_fg)
    }

    pub fn table_selected(&self) -> Style {
        Style::default()
            .bg(self.table_selected_bg)
            .fg(self.table_selected_fg)
    }
}
