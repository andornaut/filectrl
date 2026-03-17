use std::collections::HashMap;

use paste::paste;
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

use super::serde::{deserialize_color, deserialize_modifier};

/// A triplet of style properties: foreground color, background color, and modifiers.
/// `fg` and `bg` are optional — `None` (from `""` in config) means inherit from the parent widget.
#[derive(Copy, Clone, Deserialize)]
pub struct ThemeStyle {
    #[serde(deserialize_with = "deserialize_color")]
    fg: Option<Color>,

    #[serde(deserialize_with = "deserialize_color")]
    bg: Option<Color>,

    #[serde(deserialize_with = "deserialize_modifier")]
    modifiers: Modifier,
}

impl ThemeStyle {
    pub(super) fn new(fg: Option<Color>, bg: Option<Color>, modifiers: Modifier) -> Self {
        Self { fg, bg, modifiers }
    }
}

impl From<ThemeStyle> for Style {
    fn from(style: ThemeStyle) -> Self {
        let mut s = Style::default().add_modifier(style.modifiers);
        if let Some(fg) = style.fg {
            s = s.fg(fg);
        }
        if let Some(bg) = style.bg {
            s = s.bg(bg);
        }
        s
    }
}

macro_rules! style_getter {
    ($name:ident) => {
        pub fn $name(&self) -> Style {
            self.$name.into()
        }
    };
}

macro_rules! style_getter_and_setter {
    ($name:ident) => {
        style_getter!($name);

        paste! {
            pub(super) fn [<set_ $name>](&mut self, fg: Option<Color>, bg: Option<Color>, modifiers: Modifier) {
                self.$name = ThemeStyle { fg, bg, modifiers };
            }
        }
    };
}

#[derive(Deserialize)]
pub struct FileType {
    // Whether to apply colors defined in the $LS_COLORS environment variable (if set) on top of colors configured below
    pub ls_colors_take_precedence: bool,

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

    pub(super) fn add_pattern_style(&mut self, key: &str, style: ThemeStyle) {
        if key.starts_with("*.") {
            // File extension patterns (*.ext=color)
            let extension = key.trim_start_matches("*.");
            self.extension_styles.insert(extension.to_string(), style);
        } else if key.starts_with('*') {
            // File name patterns (*name=color)
            let name = key.trim_start_matches('*');
            self.name_styles.insert(name.to_string(), style);
        }
    }

    pub fn pattern_styles(&self, name: &str) -> Option<Style> {
        // Extension pattern
        if let Some(&style) = name.rsplit('.').next().and_then(|ext| self.extension_styles.get(ext)) {
            return Some(style.into());
        }

        // Name pattern
        for (pattern, &style) in &self.name_styles {
            if name.contains(pattern) {
                return Some(style.into());
            }
        }

        None
    }
}

#[derive(Deserialize)]
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

#[derive(Deserialize)]
pub struct FileModifiedDate {
    less_than_minute: ThemeStyle,
    less_than_hour: ThemeStyle,
    less_than_day: ThemeStyle,
    less_than_month: ThemeStyle,
    less_than_year: ThemeStyle,
    greater_than_year: ThemeStyle,
}

impl FileModifiedDate {
    style_getter!(greater_than_year);
    style_getter!(less_than_day);
    style_getter!(less_than_hour);
    style_getter!(less_than_minute);
    style_getter!(less_than_month);
    style_getter!(less_than_year);
}

#[derive(Deserialize)]
pub struct Alert {
    #[serde(deserialize_with = "deserialize_color")]
    fg: Option<Color>,

    #[serde(deserialize_with = "deserialize_color")]
    bg: Option<Color>,

    #[serde(deserialize_with = "deserialize_modifier")]
    modifiers: Modifier,

    error: ThemeStyle,
    info: ThemeStyle,
    warning: ThemeStyle,
}

impl Alert {
    pub fn style(&self) -> Style {
        let mut s = Style::default().add_modifier(self.modifiers);
        if let Some(fg) = self.fg {
            s = s.fg(fg);
        }
        if let Some(bg) = self.bg {
            s = s.bg(bg);
        }
        s
    }

    style_getter!(error);
    style_getter!(info);
    style_getter!(warning);
}

#[derive(Deserialize)]
pub struct Header {
    #[serde(deserialize_with = "deserialize_color")]
    fg: Option<Color>,

    #[serde(deserialize_with = "deserialize_color")]
    bg: Option<Color>,

    #[serde(deserialize_with = "deserialize_modifier")]
    modifiers: Modifier,

    active: ThemeStyle,
}

impl Header {
    pub fn style(&self) -> Style {
        let mut s = Style::default().add_modifier(self.modifiers);
        if let Some(fg) = self.fg {
            s = s.fg(fg);
        }
        if let Some(bg) = self.bg {
            s = s.bg(bg);
        }
        s
    }

    style_getter!(active);
}

#[derive(Deserialize)]
pub struct Notice {
    clipboard: ThemeStyle,
    filter: ThemeStyle,
    progress: ThemeStyle,
}

impl Notice {
    style_getter!(clipboard);
    style_getter!(filter);
    style_getter!(progress);
}

#[derive(Deserialize)]
pub struct Prompt {
    cursor: ThemeStyle,
    input: ThemeStyle,
    label: ThemeStyle,
    selection: ThemeStyle,
}

impl Prompt {
    style_getter!(cursor);
    style_getter!(input);
    style_getter!(label);
    style_getter!(selection);
}

#[derive(Deserialize)]
pub struct Status {
    directory: ThemeStyle,
    directory_label: ThemeStyle,
    selected: ThemeStyle,
    selected_label: ThemeStyle,
}

impl Status {
    style_getter!(directory);
    style_getter!(directory_label);
    style_getter!(selected);
    style_getter!(selected_label);
}

#[derive(Deserialize)]
pub struct Table {
    body: ThemeStyle,
    copy: ThemeStyle,
    cut: ThemeStyle,
    header: ThemeStyle,
    header_active: ThemeStyle,
    scrollbar_begin: ThemeStyle,
    scrollbar_end: ThemeStyle,
    scrollbar_thumb: ThemeStyle,
    scrollbar_track: ThemeStyle,
    selected: ThemeStyle,
    pub scrollbar_show_begin_end_symbols: bool,
}

impl Table {
    style_getter!(body);
    style_getter!(copy);
    style_getter!(cut);
    style_getter!(header);
    style_getter!(header_active);
    style_getter!(scrollbar_begin);
    style_getter!(scrollbar_end);
    style_getter!(scrollbar_thumb);
    style_getter!(scrollbar_track);
    style_getter!(selected);
}

#[derive(Deserialize)]
pub struct Theme {
    #[serde(deserialize_with = "deserialize_color")]
    bg: Option<Color>,

    #[serde(deserialize_with = "deserialize_color")]
    fg: Option<Color>,

    pub alert: Alert,
    pub header: Header,
    help: ThemeStyle,
    pub notice: Notice,
    pub prompt: Prompt,
    pub status: Status,
    pub table: Table,

    pub file_type: FileType,
    pub file_size: FileSize,
    pub file_modified_date: FileModifiedDate,
}

impl Theme {
    pub fn background(&self) -> Style {
        let mut s = Style::default();
        if let Some(fg) = self.fg {
            s = s.fg(fg);
        }
        if let Some(bg) = self.bg {
            s = s.bg(bg);
        }
        s
    }

    style_getter!(help);

    pub fn maybe_apply_ls_colors(&mut self) {
        if self.file_type.ls_colors_take_precedence {
            super::ls_colors::apply_ls_colors(&mut self.file_type);
        }
    }
}
