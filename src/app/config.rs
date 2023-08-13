use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

#[derive(Copy, Clone, Debug, Deserialize)]
pub struct Config {
    pub theme: Theme,
}

const DEFAULT_CONFIG: &'static str = r##"
[theme]
error_bg = "#373424"
error_fg = "#dc322f"
header_bg = "#373424"
header_fg = "#ccc8b0"
header_active_bg = "#ccc8b0"
header_active_fg = "#1d1f21"
help_bg = "#373424"
help_fg = "#ccc8b0"
prompt_input_bg = "#373424"
prompt_input_fg = "#ccc8b0"
prompt_label_bg = "#9c9977"
prompt_label_fg = "#1d1f21"
status_directory_bg = "#70c0b1"
status_directory_fg = "#1d1f21"
status_directory_label_bg = "#006B6B"
status_directory_label_fg = "#ccc8b0"
status_filter_mode_bg = "#70c0b1"
status_filter_mode_fg = "#1d1f21"
status_normal_mode_bg = "#70c0b1"
status_normal_mode_fg = "#1d1f21"
status_selected_bg = "#70c0b1"
status_selected_fg = "#1d1f21"
status_selected_label_bg = "#006B6B"
status_selected_label_fg = "#ccc8b0"
table_header_bg = "#777755"
table_header_fg = "#1d1f21"
table_header_active_bg = "#9c9977"
table_header_active_fg = "#1d1f21"
table_selected_bg = "#ccc8b0"
table_selected_fg = "#1d1f21"

table_block_device_bg = "#81a2be"
table_block_device_fg = "#f0c674"
table_character_device_bg = "#81a2be"
table_character_device_fg = "#c5c8c6"
table_directory_bg = "#423f2e"
table_directory_fg = "#81a2be"
table_fifo_bg = "#81a2be"
table_fifo_fg = "#1d1f21"
table_file_bg = "#423f2e"
table_file_fg = "#ccc8b0"
table_setgid_bg = "#f0c674"
table_setgid_fg = "#1d1f21"
table_setuid_bg = "#cc6666"
table_setuid_fg = "#c5c8c6"
table_socket_bg = "#81a2be"
table_socket_fg = "#b294bb"
table_sticky_bg = "#423f2e"
table_sticky_fg = "#ccc8b0"
table_symlink_bg = "#423f2e"
table_symlink_fg = "#b294bb"
"##;

impl Default for Config {
    fn default() -> Self {
        toml::from_str::<Self>(DEFAULT_CONFIG).unwrap()
    }
}

#[derive(Copy, Clone, Debug, Deserialize)]
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
    status_filter_mode_bg: Color,
    #[serde(with = "color_to_tui")]
    status_filter_mode_fg: Color,
    #[serde(with = "color_to_tui")]
    status_normal_mode_bg: Color,
    #[serde(with = "color_to_tui")]
    status_normal_mode_fg: Color,
    #[serde(with = "color_to_tui")]
    status_directory_label_bg: Color,
    #[serde(with = "color_to_tui")]
    status_directory_label_fg: Color,
    #[serde(with = "color_to_tui")]
    status_directory_bg: Color,
    #[serde(with = "color_to_tui")]
    status_directory_fg: Color,
    #[serde(with = "color_to_tui")]
    status_selected_label_bg: Color,
    #[serde(with = "color_to_tui")]
    status_selected_label_fg: Color,
    #[serde(with = "color_to_tui")]
    status_selected_bg: Color,
    #[serde(with = "color_to_tui")]
    status_selected_fg: Color,
    #[serde(with = "color_to_tui")]
    table_header_active_bg: Color,
    #[serde(with = "color_to_tui")]
    table_header_active_fg: Color,
    #[serde(with = "color_to_tui")]
    table_header_bg: Color,
    #[serde(with = "color_to_tui")]
    table_header_fg: Color,
    #[serde(with = "color_to_tui")]
    table_selected_bg: Color,
    #[serde(with = "color_to_tui")]
    table_selected_fg: Color,

    #[serde(with = "color_to_tui")]
    table_block_device_bg: Color,
    #[serde(with = "color_to_tui")]
    table_block_device_fg: Color,
    #[serde(with = "color_to_tui")]
    table_character_device_bg: Color,
    #[serde(with = "color_to_tui")]
    table_character_device_fg: Color,
    #[serde(with = "color_to_tui")]
    table_directory_bg: Color,
    #[serde(with = "color_to_tui")]
    table_directory_fg: Color,
    #[serde(with = "color_to_tui")]
    table_fifo_bg: Color,
    #[serde(with = "color_to_tui")]
    table_fifo_fg: Color,
    #[serde(with = "color_to_tui")]
    table_file_bg: Color,
    #[serde(with = "color_to_tui")]
    table_file_fg: Color,
    #[serde(with = "color_to_tui")]
    table_setgid_bg: Color,
    #[serde(with = "color_to_tui")]
    table_setgid_fg: Color,
    #[serde(with = "color_to_tui")]
    table_setuid_bg: Color,
    #[serde(with = "color_to_tui")]
    table_setuid_fg: Color,
    #[serde(with = "color_to_tui")]
    table_socket_bg: Color,
    #[serde(with = "color_to_tui")]
    table_socket_fg: Color,
    #[serde(with = "color_to_tui")]
    table_sticky_bg: Color,
    #[serde(with = "color_to_tui")]
    table_sticky_fg: Color,
    #[serde(with = "color_to_tui")]
    table_symlink_bg: Color,
    #[serde(with = "color_to_tui")]
    table_symlink_fg: Color,
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

    pub fn status_filtered_mode(&self) -> Style {
        Style::default()
            .bg(self.status_filter_mode_bg)
            .fg(self.status_filter_mode_fg)
    }

    pub fn status_normal_mode(&self) -> Style {
        Style::default()
            .bg(self.status_normal_mode_bg)
            .fg(self.status_normal_mode_fg)
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

    pub fn table_selected(&self) -> Style {
        Style::default()
            .bg(self.table_selected_bg)
            .fg(self.table_selected_fg)
    }

    pub fn table_block_device(&self) -> Style {
        Style::default()
            .bg(self.table_block_device_bg)
            .fg(self.table_block_device_fg)
    }

    pub fn table_character_device(&self) -> Style {
        Style::default()
            .bg(self.table_character_device_bg)
            .fg(self.table_character_device_fg)
    }

    pub fn table_directory(&self) -> Style {
        Style::default()
            .bg(self.table_directory_bg)
            .fg(self.table_directory_fg)
    }

    pub fn table_fifo(&self) -> Style {
        Style::default()
            .bg(self.table_fifo_bg)
            .fg(self.table_fifo_fg)
    }

    pub fn table_file(&self) -> Style {
        Style::default()
            .bg(self.table_file_bg)
            .fg(self.table_file_fg)
    }

    pub fn table_setgid(&self) -> Style {
        Style::default()
            .bg(self.table_setgid_bg)
            .fg(self.table_setgid_fg)
    }

    pub fn table_setuid(&self) -> Style {
        Style::default()
            .bg(self.table_setuid_bg)
            .fg(self.table_setuid_fg)
    }

    pub fn table_socket(&self) -> Style {
        Style::default()
            .bg(self.table_socket_bg)
            .fg(self.table_socket_fg)
    }

    pub fn table_sticky(&self) -> Style {
        Style::default()
            .bg(self.table_sticky_bg)
            .fg(self.table_sticky_fg)
    }

    pub fn table_symlink(&self) -> Style {
        Style::default()
            .bg(self.table_symlink_bg)
            .fg(self.table_symlink_fg)
    }
}
