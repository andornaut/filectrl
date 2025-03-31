use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

use super::serialization::{
    deserialize_modifier, deserialize_optional_color, serialize_modifier, serialize_optional_color,
};

macro_rules! style_getter {
    ($fn_name:ident, $fg_field:ident, $bg_field:ident) => {
        pub fn $fn_name(&self) -> Style {
            let mut style = Style::default();

            if let Some(fg) = self.$fg_field {
                style = style.fg(fg);
            }

            if let Some(bg) = self.$bg_field {
                style = style.bg(bg);
            }

            style
        }
    };

    // Overload for methods that also need modifiers
    ($fn_name:ident, $fg_field:ident, $bg_field:ident, $modifier_field:ident) => {
        pub fn $fn_name(&self) -> Style {
            let mut style = Style::default();

            if let Some(fg) = self.$fg_field {
                style = style.fg(fg);
            }

            if let Some(bg) = self.$bg_field {
                style = style.bg(bg);
            }

            style.add_modifier(self.$modifier_field)
        }
    };
}

macro_rules! style_setter {
    ($fn_name:ident, $fg_field:ident, $bg_field:ident, $modifier_field:ident) => {
        pub(super) fn $fn_name(&mut self, fg: Option<Color>, bg: Option<Color>, attrs: Modifier) {
            self.$fg_field = fg;
            self.$bg_field = bg;
            self.$modifier_field = attrs;
        }
    };
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FileTheme {
    // Block device (bd)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    block_device_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    block_device_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    block_device_modifiers: Modifier,

    // Character device (cd)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    character_device_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    character_device_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    character_device_modifiers: Modifier,

    // Directory (di)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    directory_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    directory_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    directory_modifiers: Modifier,

    // Sticky directory (st)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    directory_sticky_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    directory_sticky_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    directory_sticky_modifiers: Modifier,

    // Sticky and other-writable directory (tw)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    directory_sticky_other_writable_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    directory_sticky_other_writable_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    directory_sticky_other_writable_modifiers: Modifier,

    // Other-writable directory (ow)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    directory_other_writable_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    directory_other_writable_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    directory_other_writable_modifiers: Modifier,

    // Door (do)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    door_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    door_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    door_modifiers: Modifier,

    // Executable (ex)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    executable_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    executable_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    executable_modifiers: Modifier,

    // Regular file (fi)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    regular_file_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    regular_file_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    regular_file_modifiers: Modifier,

    // Missing file (mi)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    missing_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    missing_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    missing_modifiers: Modifier,

    // Normal file default (no)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    normal_file_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    normal_file_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    normal_file_modifiers: Modifier,

    // Pipe/FIFO (pi)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    pipe_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    pipe_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    pipe_modifiers: Modifier,

    // Setgid (sg)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    setgid_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    setgid_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    setgid_modifiers: Modifier,

    // Setuid (su)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    setuid_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    setuid_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    setuid_modifiers: Modifier,

    // Socket (so)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    socket_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    socket_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    socket_modifiers: Modifier,

    // Symlink (ln)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    symlink_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    symlink_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    symlink_modifiers: Modifier,

    // Broken symlink (or)
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    symlink_broken_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    symlink_broken_fg: Option<Color>,
    #[serde(
        serialize_with = "serialize_modifier",
        deserialize_with = "deserialize_modifier"
    )]
    symlink_broken_modifiers: Modifier,

    // Pattern-based styles
    #[serde(skip)]
    extension_styles: HashMap<String, (Option<Color>, Option<Color>, Modifier)>,
    #[serde(skip)]
    name_styles: HashMap<String, (Option<Color>, Option<Color>, Modifier)>,
}

impl FileTheme {
    pub fn pattern_styles(&self, name: &str) -> Option<Style> {
        // Extension pattern
        if let Some(ext) = name.rsplit('.').next() {
            if let Some(&(fg, bg, modifier)) = self.extension_styles.get(ext) {
                let mut style = Style::default();

                if let Some(fg_color) = fg {
                    style = style.fg(fg_color);
                }

                if let Some(bg_color) = bg {
                    style = style.bg(bg_color);
                }

                return Some(style.add_modifier(modifier));
            }
        }

        // Name pattern
        for (pattern, &(fg, bg, modifier)) in &self.name_styles {
            if name.contains(pattern) {
                let mut style = Style::default();

                if let Some(fg_color) = fg {
                    style = style.fg(fg_color);
                }

                if let Some(bg_color) = bg {
                    style = style.bg(bg_color);
                }

                return Some(style.add_modifier(modifier));
            }
        }

        None
    }

    style_getter!(
        block_device,
        block_device_fg,
        block_device_bg,
        block_device_modifiers
    );
    style_getter!(
        character_device,
        character_device_fg,
        character_device_bg,
        character_device_modifiers
    );
    style_getter!(directory, directory_fg, directory_bg, directory_modifiers);
    style_getter!(
        directory_sticky,
        directory_sticky_fg,
        directory_sticky_bg,
        directory_sticky_modifiers
    );
    style_getter!(
        directory_other_writable,
        directory_other_writable_fg,
        directory_other_writable_bg,
        directory_other_writable_modifiers
    );
    style_getter!(
        directory_sticky_other_writable,
        directory_sticky_other_writable_fg,
        directory_sticky_other_writable_bg,
        directory_sticky_other_writable_modifiers
    );
    style_getter!(door, door_fg, door_bg, door_modifiers);
    style_getter!(
        executable,
        executable_fg,
        executable_bg,
        executable_modifiers
    );
    style_getter!(
        regular_file,
        regular_file_fg,
        regular_file_bg,
        regular_file_modifiers
    );
    style_getter!(missing, missing_fg, missing_bg, missing_modifiers);
    style_getter!(
        normal_file,
        normal_file_fg,
        normal_file_bg,
        normal_file_modifiers
    );
    style_getter!(pipe, pipe_fg, pipe_bg, pipe_modifiers);
    style_getter!(setgid, setgid_fg, setgid_bg, setgid_modifiers);
    style_getter!(setuid, setuid_fg, setuid_bg, setuid_modifiers);
    style_getter!(socket, socket_fg, socket_bg, socket_modifiers);
    style_getter!(symlink, symlink_fg, symlink_bg, symlink_modifiers);
    style_getter!(
        symlink_broken,
        symlink_broken_fg,
        symlink_broken_bg,
        symlink_broken_modifiers
    );

    style_setter!(
        set_block_device,
        block_device_fg,
        block_device_bg,
        block_device_modifiers
    );
    style_setter!(
        set_character_device,
        character_device_fg,
        character_device_bg,
        character_device_modifiers
    );
    style_setter!(
        set_directory,
        directory_fg,
        directory_bg,
        directory_modifiers
    );
    style_setter!(set_door, door_fg, door_bg, door_modifiers);
    style_setter!(
        set_executable,
        executable_fg,
        executable_bg,
        executable_modifiers
    );
    style_setter!(
        set_regular_file,
        regular_file_fg,
        regular_file_bg,
        regular_file_modifiers
    );
    style_setter!(set_symlink, symlink_fg, symlink_bg, symlink_modifiers);
    style_setter!(set_missing, missing_fg, missing_bg, missing_modifiers);
    style_setter!(
        set_normal_file,
        normal_file_fg,
        normal_file_bg,
        normal_file_modifiers
    );
    style_setter!(
        set_symlink_broken,
        symlink_broken_fg,
        symlink_broken_bg,
        symlink_broken_modifiers
    );
    style_setter!(
        set_directory_other_writable,
        directory_other_writable_fg,
        directory_other_writable_bg,
        directory_other_writable_modifiers
    );
    style_setter!(set_pipe, pipe_fg, pipe_bg, pipe_modifiers);
    style_setter!(set_setgid, setgid_fg, setgid_bg, setgid_modifiers);
    style_setter!(set_socket, socket_fg, socket_bg, socket_modifiers);
    style_setter!(
        set_directory_sticky,
        directory_sticky_fg,
        directory_sticky_bg,
        directory_sticky_modifiers
    );
    style_setter!(
        set_directory_sticky_other_writable,
        directory_sticky_other_writable_fg,
        directory_sticky_other_writable_bg,
        directory_sticky_other_writable_modifiers
    );
    style_setter!(set_setuid, setuid_fg, setuid_bg, setuid_modifiers);

    pub(super) fn add_pattern_style(
        &mut self,
        key: &str,
        fg: Option<Color>,
        bg: Option<Color>,
        attrs: Modifier,
    ) {
        if key.starts_with("*.") {
            // File extension patterns (*.ext=color)
            let extension = key.trim_start_matches("*.");
            self.extension_styles
                .insert(extension.to_string(), (fg, bg, attrs));
        } else if key.starts_with('*') {
            // File name patterns (*name=color)
            let name = key.trim_start_matches('*');
            self.name_styles.insert(name.to_string(), (fg, bg, attrs));
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Theme {
    // Error
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    error_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    error_fg: Option<Color>,

    // Header
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    header_active_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    header_active_fg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    header_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    header_fg: Option<Color>,

    // Help
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    help_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    help_fg: Option<Color>,

    // Prompt
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    prompt_input_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    prompt_input_fg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    prompt_label_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    prompt_label_fg: Option<Color>,

    // Status clipboard
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_clipboard_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_clipboard_fg: Option<Color>,

    // Status directory
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_directory_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_directory_fg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_directory_label_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_directory_label_fg: Option<Color>,

    // Status filter
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_filter_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_filter_fg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    // Status progress
    status_progress_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_progress_fg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_progress_done_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_progress_done_fg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_progress_error_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_progress_error_fg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    // Status selected
    status_selected_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_selected_fg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_selected_label_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    status_selected_label_fg: Option<Color>,

    // Table body
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_body_fg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_body_bg: Option<Color>,

    // Table header
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_header_active_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_header_active_fg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_header_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_header_fg: Option<Color>,

    // Table scrollbar
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_scrollbar_track_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_scrollbar_track_fg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_scrollbar_thumb_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_scrollbar_thumb_fg: Option<Color>,

    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_scrollbar_begin_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_scrollbar_end_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_scrollbar_begin_fg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_scrollbar_end_fg: Option<Color>,

    // Table selected
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_selected_bg: Option<Color>,
    #[serde(
        deserialize_with = "deserialize_optional_color",
        serialize_with = "serialize_optional_color"
    )]
    table_selected_fg: Option<Color>,

    pub files: FileTheme,
}

impl Theme {
    style_getter!(error, error_fg, error_bg);
    style_getter!(header, header_fg, header_bg);
    style_getter!(header_active, header_active_fg, header_active_bg);
    style_getter!(help, help_fg, help_bg);
    style_getter!(prompt_input, prompt_input_fg, prompt_input_bg);
    style_getter!(prompt_label, prompt_label_fg, prompt_label_bg);
    style_getter!(status_clipboard, status_clipboard_fg, status_clipboard_bg);
    style_getter!(status_filter, status_filter_fg, status_filter_bg);
    style_getter!(status_directory, status_directory_fg, status_directory_bg);
    style_getter!(
        status_directory_label,
        status_directory_label_fg,
        status_directory_label_bg
    );
    style_getter!(status_progress, status_progress_fg, status_progress_bg);
    style_getter!(
        status_progress_done,
        status_progress_done_fg,
        status_progress_done_bg
    );
    style_getter!(
        status_progress_error,
        status_progress_error_fg,
        status_progress_error_bg
    );
    style_getter!(status_selected, status_selected_fg, status_selected_bg);
    style_getter!(
        status_selected_label,
        status_selected_label_fg,
        status_selected_label_bg
    );
    style_getter!(table_body, table_body_fg, table_body_bg);
    style_getter!(table_header, table_header_fg, table_header_bg);
    style_getter!(
        table_scrollbar_begin,
        table_scrollbar_begin_fg,
        table_scrollbar_begin_bg
    );
    style_getter!(
        table_scrollbar_end,
        table_scrollbar_end_fg,
        table_scrollbar_end_bg
    );
    style_getter!(
        table_scrollbar_thumb,
        table_scrollbar_thumb_fg,
        table_scrollbar_thumb_bg
    );
    style_getter!(
        table_scrollbar_track,
        table_scrollbar_track_fg,
        table_scrollbar_track_bg
    );
    style_getter!(table_selected, table_selected_fg, table_selected_bg);
    style_getter!(
        table_header_active,
        table_header_active_fg,
        table_header_active_bg
    );

    pub fn pattern_style(&self, name: &str) -> Option<Style> {
        // Extension match
        if let Some(ext) = name.rsplit('.').next() {
            if let Some(&(fg, bg, attrs)) = self.files.extension_styles.get(ext) {
                let mut style = Style::default();

                if let Some(fg_color) = fg {
                    style = style.fg(fg_color);
                }

                if let Some(bg_color) = bg {
                    style = style.bg(bg_color);
                }

                return Some(style.add_modifier(attrs));
            }
        }

        // Name pattern match
        for (pattern, &(fg, bg, attrs)) in &self.files.name_styles {
            if name.contains(pattern) {
                let mut style = Style::default();

                if let Some(fg_color) = fg {
                    style = style.fg(fg_color);
                }

                if let Some(bg_color) = bg {
                    style = style.bg(bg_color);
                }

                return Some(style.add_modifier(attrs));
            }
        }

        None
    }
}
