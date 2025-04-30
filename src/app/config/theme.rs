use std::collections::HashMap;

use paste::paste;
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

use super::serialization::{
    deserialize_color, deserialize_modifier, serialize_color, serialize_modifier,
};

/// A triplet of style properties: foreground color, background color, and modifiers
#[derive(Deserialize, Serialize)]
pub struct ThemeStyle {
    #[serde(
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    fg: Color,

    #[serde(
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    bg: Color,

    #[serde(
        deserialize_with = "deserialize_modifier",
        serialize_with = "serialize_modifier"
    )]
    modifiers: Modifier,
}

impl From<&ThemeStyle> for Style {
    fn from(style: &ThemeStyle) -> Self {
        Style::default()
            .fg(style.fg)
            .bg(style.bg)
            .add_modifier(style.modifiers)
    }
}

macro_rules! style_getter {
    ($name:ident) => {
        paste! {
            pub fn $name(&self) -> Style {
                (&self.$name).into()
            }
        }
    };
}

macro_rules! style_getter_and_setter {
    ($name:ident) => {
        // Create the getter
        style_getter!($name);

        // Create the setter
        paste! {
            pub(super) fn [<set_ $name>](&mut self, fg: Color, bg: Color, modifiers: Modifier) {
                self.$name = ThemeStyle { fg, bg, modifiers};
            }
        }
    };
}

#[derive(Deserialize, Serialize)]
pub struct FileType {
    // Whether to apply colors defined in the $LS_COLORS environment variable (if set) on top of colors configured below
    ls_colors_take_precedence: bool,

    block_device: ThemeStyle,
    character_device: ThemeStyle,
    directory: ThemeStyle,
    directory_other_writable: ThemeStyle,
    directory_sticky: ThemeStyle,
    directory_sticky_other_writable: ThemeStyle,
    door: ThemeStyle,
    executable: ThemeStyle,
    missing: ThemeStyle,
    normal_file: ThemeStyle,
    pipe: ThemeStyle,
    regular_file: ThemeStyle,
    setgid: ThemeStyle,
    setuid: ThemeStyle,
    socket: ThemeStyle,
    symlink: ThemeStyle,
    symlink_broken: ThemeStyle,

    // Pattern-based styles
    #[serde(skip)]
    extension_styles: HashMap<String, ThemeStyle>,
    #[serde(skip)]
    name_styles: HashMap<String, ThemeStyle>,
}

impl FileType {
    style_getter_and_setter!(block_device);
    style_getter_and_setter!(character_device);
    style_getter_and_setter!(directory);
    style_getter_and_setter!(directory_other_writable);
    style_getter_and_setter!(directory_sticky);
    style_getter_and_setter!(directory_sticky_other_writable);
    style_getter_and_setter!(door);
    style_getter_and_setter!(executable);
    style_getter_and_setter!(missing);
    style_getter_and_setter!(normal_file);
    style_getter_and_setter!(pipe);
    style_getter_and_setter!(regular_file);
    style_getter_and_setter!(setgid);
    style_getter_and_setter!(setuid);
    style_getter_and_setter!(socket);
    style_getter_and_setter!(symlink);
    style_getter_and_setter!(symlink_broken);

    pub(super) fn add_pattern_style(
        &mut self,
        key: &str,
        fg: Color,
        bg: Color,
        modifiers: Modifier,
    ) {
        let theme_style = ThemeStyle { fg, bg, modifiers };

        if key.starts_with("*.") {
            // File extension patterns (*.ext=color)
            let extension = key.trim_start_matches("*.");
            self.extension_styles
                .insert(extension.to_string(), theme_style);
        } else if key.starts_with('*') {
            // File name patterns (*name=color)
            let name = key.trim_start_matches('*');
            self.name_styles.insert(name.to_string(), theme_style);
        }
    }

    pub fn pattern_styles(&self, name: &str) -> Option<Style> {
        // Extension pattern
        if let Some(ext) = name.rsplit('.').next() {
            if let Some(triplet) = self.extension_styles.get(ext) {
                return Some(triplet.into());
            }
        }

        // Name pattern
        for (pattern, triplet) in &self.name_styles {
            if name.contains(pattern) {
                return Some(triplet.into());
            }
        }

        None
    }

    fn ls_colors_take_precedence(&self) -> bool {
        self.ls_colors_take_precedence
    }
}

#[derive(Deserialize, Serialize)]
pub struct FileSize {
    bytes: ThemeStyle,
    kib: ThemeStyle,
    mib: ThemeStyle,
    gib: ThemeStyle,
    tib: ThemeStyle,
    pib: ThemeStyle,
}

impl FileSize {
    style_getter!(bytes);
    style_getter!(kib);
    style_getter!(mib);
    style_getter!(gib);
    style_getter!(tib);
    style_getter!(pib);
}

#[derive(Deserialize, Serialize)]
pub struct FileModifiedDate {
    less_than_minute: ThemeStyle,
    less_than_day: ThemeStyle,
    less_than_month: ThemeStyle,
    less_than_year: ThemeStyle,
    greater_than_year: ThemeStyle,
}

impl FileModifiedDate {
    style_getter!(greater_than_year);
    style_getter!(less_than_day);
    style_getter!(less_than_minute);
    style_getter!(less_than_month);
    style_getter!(less_than_year);
}

#[derive(Deserialize, Serialize)]
pub struct Theme {
    alert: ThemeStyle,
    alert_error: ThemeStyle,
    alert_info: ThemeStyle,
    alert_warning: ThemeStyle,
    header: ThemeStyle,
    header_active: ThemeStyle,
    help: ThemeStyle,
    notice_clipboard: ThemeStyle,
    notice_filter: ThemeStyle,
    notice_progress: ThemeStyle,
    prompt_cursor: ThemeStyle,
    prompt_input: ThemeStyle,
    prompt_label: ThemeStyle,
    prompt_selection: ThemeStyle,
    status_directory: ThemeStyle,
    status_directory_label: ThemeStyle,
    status_selected: ThemeStyle,
    status_selected_label: ThemeStyle,
    table_body: ThemeStyle,
    table_copied: ThemeStyle,
    table_cut: ThemeStyle,
    table_header: ThemeStyle,
    table_header_active: ThemeStyle,
    table_scrollbar_begin: ThemeStyle,
    table_scrollbar_end: ThemeStyle,
    table_scrollbar_thumb: ThemeStyle,
    table_scrollbar_track: ThemeStyle,
    table_selected: ThemeStyle,

    table_scrollbar_begin_end_enabled: bool,

    file_type: FileType,
    file_size: FileSize,
    file_modified_date: FileModifiedDate,
}

impl Theme {
    style_getter!(alert);
    style_getter!(alert_error);
    style_getter!(alert_info);
    style_getter!(alert_warning);
    style_getter!(header);
    style_getter!(header_active);
    style_getter!(help);
    style_getter!(notice_clipboard);
    style_getter!(notice_filter);
    style_getter!(notice_progress);
    style_getter!(prompt_cursor);
    style_getter!(prompt_input);
    style_getter!(prompt_label);
    style_getter!(prompt_selection);
    style_getter!(status_directory);
    style_getter!(status_directory_label);
    style_getter!(status_selected);
    style_getter!(status_selected_label);
    style_getter!(table_body);
    style_getter!(table_copied);
    style_getter!(table_cut);
    style_getter!(table_header);
    style_getter!(table_header_active);
    style_getter!(table_scrollbar_begin);
    style_getter!(table_scrollbar_end);
    style_getter!(table_scrollbar_thumb);
    style_getter!(table_scrollbar_track);
    style_getter!(table_selected);

    pub fn maybe_apply_ls_colors(&mut self) {
        if self.file_type.ls_colors_take_precedence() {
            super::ls_colors::apply_ls_colors(&mut self.file_type);
        }
    }

    pub fn file_modified_date(&self) -> &FileModifiedDate {
        &self.file_modified_date
    }

    pub fn file_size(&self) -> &FileSize {
        &self.file_size
    }

    pub fn file_type(&self) -> &FileType {
        &self.file_type
    }

    pub fn table_scrollbar_begin_end_enabled(&self) -> bool {
        self.table_scrollbar_begin_end_enabled
    }
}
