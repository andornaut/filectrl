use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

use super::serde::{deserialize_color, deserialize_modifier};

fn empty_modifier() -> Modifier {
    Modifier::empty()
}

/// A triplet of style properties: foreground color, background color, and modifiers.
/// All fields are optional — omitted fields inherit defaults (no color, no modifiers).
/// `fg` and `bg` accept `""` in config to explicitly inherit from the parent widget.
#[derive(Copy, Clone, Deserialize)]
pub struct StyleConfig {
    #[serde(default, deserialize_with = "deserialize_color")]
    fg: Option<Color>,

    #[serde(default, deserialize_with = "deserialize_color")]
    bg: Option<Color>,

    #[serde(default = "empty_modifier", deserialize_with = "deserialize_modifier")]
    modifiers: Modifier,
}

impl Default for StyleConfig {
    fn default() -> Self {
        Self {
            fg: None,
            bg: None,
            modifiers: Modifier::empty(),
        }
    }
}

impl StyleConfig {
    fn new(fg: Option<Color>, bg: Option<Color>, modifiers: Modifier) -> Self {
        Self { fg, bg, modifiers }
    }
}

impl From<StyleConfig> for Style {
    fn from(style: StyleConfig) -> Self {
        let mut s = Style::default().add_modifier(style.modifiers);
        if let Some(bg) = style.bg {
            s = s.bg(bg);
        }
        if let Some(fg) = style.fg {
            s = s.fg(fg);
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

#[derive(Deserialize)]
pub struct FileType {
    // Whether to apply colors defined in the $LS_COLORS environment variable (if set) on top of colors configured below
    ls_colors_take_precedence: bool,

    block_device: StyleConfig,
    character_device: StyleConfig,
    directory: StyleConfig,
    directory_other_writable: StyleConfig,
    directory_sticky: StyleConfig,
    directory_sticky_other_writable: StyleConfig,
    door: StyleConfig,
    executable: StyleConfig,
    missing: StyleConfig,
    normal_file: StyleConfig,
    pipe: StyleConfig,
    regular_file: StyleConfig,
    setgid: StyleConfig,
    setuid: StyleConfig,
    socket: StyleConfig,
    symlink: StyleConfig,
    symlink_broken: StyleConfig,

    // Pattern-based styles
    #[serde(skip)]
    extension_styles: HashMap<String, StyleConfig>,
    #[serde(skip)]
    name_styles: HashMap<String, StyleConfig>,
}

impl FileType {
    style_getter!(block_device);
    style_getter!(character_device);
    style_getter!(directory);
    style_getter!(directory_other_writable);
    style_getter!(directory_sticky);
    style_getter!(directory_sticky_other_writable);
    style_getter!(door);
    style_getter!(executable);
    style_getter!(missing);
    style_getter!(normal_file);
    style_getter!(pipe);
    style_getter!(regular_file);
    style_getter!(setgid);
    style_getter!(setuid);
    style_getter!(socket);
    style_getter!(symlink);
    style_getter!(symlink_broken);

    pub(super) fn maybe_apply_ls_colors(&mut self, warn_on_rgb: bool) {
        if self.ls_colors_take_precedence
            && let Ok(ls_colors) = std::env::var("LS_COLORS")
        {
            self.apply_ls_colors(&ls_colors, warn_on_rgb);
        }
    }

    fn apply_ls_colors(&mut self, ls_colors: &str, warn_on_rgb: bool) {
        let mut found_rgb = false;
        for entry in ls_colors.split(':') {
            let Some((key, value)) = entry.split_once('=') else {
                continue;
            };

            let (fg, bg, attrs) = super::ls_colors::parse(value);
            if fg.is_none() && bg.is_none() && attrs == Modifier::empty() {
                continue;
            }

            if warn_on_rgb && matches!((fg, bg), (Some(Color::Rgb(..)), _) | (_, Some(Color::Rgb(..)))) {
                found_rgb = true;
            }

            let style = StyleConfig::new(fg, bg, attrs);
            match key {
                "bd" => self.block_device = style,
                "ca" => {} // capabilities not supported
                "cd" => self.character_device = style,
                "di" => self.directory = style,
                "do" => self.door = style,
                "ex" => self.executable = style,
                "fi" => self.regular_file = style,
                "ln" => self.symlink = style,
                "mi" => self.missing = style,
                "no" => self.normal_file = style,
                "or" => self.symlink_broken = style,
                "ow" => self.directory_other_writable = style,
                "pi" => self.pipe = style,
                "sg" => self.setgid = style,
                "so" => self.socket = style,
                "st" => self.directory_sticky = style,
                "tw" => self.directory_sticky_other_writable = style,
                "su" => self.setuid = style,
                key if key.starts_with("*.") => {
                    self.extension_styles
                        .insert(key.trim_start_matches("*.").to_string(), style);
                }
                key if key.starts_with('*') => {
                    self.name_styles
                        .insert(key.trim_start_matches('*').to_string(), style);
                }
                _ => {}
            }
        }
        if found_rgb {
            log::warn!(
                "$LS_COLORS contains truecolor (RGB) entries; these may not render correctly on a 256-color terminal"
            );
        }
    }

    pub fn pattern_styles(&self, name: &str) -> Option<Style> {
        // Extension pattern — only match if the dot is not the leading character,
        // so that dotfiles like ".bashrc" are not treated as having extension "bashrc".
        if let Some(&style) = name
            .rsplit_once('.')
            .filter(|(prefix, _)| !prefix.is_empty())
            .and_then(|(_, ext)| self.extension_styles.get(ext))
        {
            return Some(style.into());
        }

        // Name pattern
        for (pattern, &style) in &self.name_styles {
            if name.ends_with(pattern.as_str()) {
                return Some(style.into());
            }
        }

        None
    }
}

#[derive(Deserialize)]
pub struct FileSize {
    bytes: StyleConfig,
    kib: StyleConfig,
    mib: StyleConfig,
    gib: StyleConfig,
    tib: StyleConfig,
    pib: StyleConfig,
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
    less_than_minute: StyleConfig,
    less_than_hour: StyleConfig,
    less_than_day: StyleConfig,
    less_than_month: StyleConfig,
    less_than_year: StyleConfig,
    greater_than_year: StyleConfig,
}

impl FileModifiedDate {
    style_getter!(less_than_minute);
    style_getter!(less_than_hour);
    style_getter!(less_than_day);
    style_getter!(less_than_month);
    style_getter!(less_than_year);
    style_getter!(greater_than_year);
}

#[derive(Deserialize)]
pub struct Alert {
    #[serde(flatten)]
    base: StyleConfig,

    error: StyleConfig,
    info: StyleConfig,
    warn: StyleConfig,
}

impl Alert {
    style_getter!(base);
    style_getter!(error);
    style_getter!(info);
    style_getter!(warn);
}

#[derive(Deserialize)]
pub struct Breadcrumbs {
    #[serde(flatten)]
    base: StyleConfig,

    basename: StyleConfig,
    ancestor: StyleConfig,
    separator: StyleConfig,
}

impl Breadcrumbs {
    style_getter!(base);
    style_getter!(basename);
    style_getter!(ancestor);
    style_getter!(separator);
}

#[derive(Deserialize)]
pub struct Clipboard {
    copy: StyleConfig,
    cut: StyleConfig,
    delete: StyleConfig,
}

impl Clipboard {
    style_getter!(copy);
    style_getter!(cut);
    style_getter!(delete);
}

#[derive(Deserialize)]
pub struct Notice {
    filter: StyleConfig,
    progress: StyleConfig,
}

impl Notice {
    style_getter!(filter);
    style_getter!(progress);
}

#[derive(Deserialize)]
pub struct Prompt {
    cursor: StyleConfig,
    delete: StyleConfig,
    input: StyleConfig,
    label: StyleConfig,
    selected: StyleConfig,
}

impl Prompt {
    style_getter!(cursor);
    style_getter!(delete);
    style_getter!(input);
    style_getter!(label);
    style_getter!(selected);
}

#[derive(Deserialize)]
pub struct Status {
    detail: StyleConfig,
    label: StyleConfig,
}

impl Status {
    style_getter!(detail);
    style_getter!(label);
}

#[derive(Deserialize)]
pub struct ScrollbarConfig {
    ends: StyleConfig,
    thumb: StyleConfig,
    track: StyleConfig,
    show_ends: bool,
}

impl ScrollbarConfig {
    style_getter!(ends);
    style_getter!(thumb);
    style_getter!(track);

    pub fn show_ends(&self) -> bool {
        self.show_ends
    }
}

#[derive(Deserialize)]
pub struct Table {
    body: StyleConfig,
    #[serde(default)]
    delete: StyleConfig,
    header: StyleConfig,
    header_sorted: StyleConfig,
    #[serde(default)]
    marked: StyleConfig,
    selected: StyleConfig,
}

impl Table {
    style_getter!(body);
    style_getter!(delete);
    style_getter!(header);
    style_getter!(header_sorted);
    style_getter!(marked);
    style_getter!(selected);
}

#[derive(Deserialize)]
pub struct Help {
    #[serde(flatten)]
    base: StyleConfig,

    actions: StyleConfig,
    header: StyleConfig,
    shortcuts: StyleConfig,
}

impl Help {
    style_getter!(actions);
    style_getter!(base);
    style_getter!(header);
    style_getter!(shortcuts);
}

#[derive(Deserialize)]
pub struct Theme {
    #[serde(flatten)]
    base: StyleConfig,

    pub alert: Alert,
    pub breadcrumbs: Breadcrumbs,
    pub clipboard: Clipboard,
    pub file_modified_date: FileModifiedDate,
    pub file_size: FileSize,
    pub file_type: FileType,
    pub help: Help,
    pub notice: Notice,
    pub prompt: Prompt,
    pub scrollbar: ScrollbarConfig,
    pub status: Status,
    pub table: Table,
}

impl Theme {
    style_getter!(base);
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;
    use test_case::test_case;

    use super::*;

    fn make_file_type() -> FileType {
        let empty = StyleConfig::new(None, None, Modifier::empty());
        FileType {
            ls_colors_take_precedence: false,
            block_device: empty,
            character_device: empty,
            directory: empty,
            directory_other_writable: empty,
            directory_sticky: empty,
            directory_sticky_other_writable: empty,
            door: empty,
            executable: empty,
            missing: empty,
            normal_file: empty,
            pipe: empty,
            regular_file: empty,
            setgid: empty,
            setuid: empty,
            socket: empty,
            symlink: empty,
            symlink_broken: empty,
            extension_styles: HashMap::new(),
            name_styles: HashMap::new(),
        }
    }

    fn red() -> StyleConfig {
        StyleConfig::new(Some(Color::Red), None, Modifier::empty())
    }

    // --- pattern_styles: extension branch ---

    #[test_case("foo.rs", "rs", true ; "normal extension matches")]
    #[test_case("foo.tar.gz", "gz", true ; "last segment of compound name matches")]
    #[test_case(".bashrc", "bashrc", false ; "leading-dot file does not match extension pattern")]
    #[test_case("Makefile", "rs", false ; "file with no extension returns no match")]
    #[test_case("foo", "foo", false ; "extensionless file does not match")]
    fn extension_pattern(filename: &str, ext: &str, should_match: bool) {
        let mut ft = make_file_type();
        ft.extension_styles.insert(ext.to_string(), red());
        assert_eq!(ft.pattern_styles(filename).is_some(), should_match);
    }

    // --- pattern_styles: name branch ---

    #[test_case("Makefile", "Makefile", true ; "exact suffix match")]
    #[test_case("OldMakefile", "Makefile", true ; "longer name ending with pattern matches")]
    #[test_case("Makefile.bak", "Makefile", false ; "name with trailing suffix does not match")]
    #[test_case(".bashrc", "bashrc", true ; "dotfile matches name pattern by suffix")]
    fn name_pattern(filename: &str, pattern: &str, should_match: bool) {
        let mut ft = make_file_type();
        ft.name_styles.insert(pattern.to_string(), red());
        assert_eq!(ft.pattern_styles(filename).is_some(), should_match);
    }

    // --- apply_ls_colors round-trips ---

    #[test]
    fn apply_ls_colors_sets_directory_color() {
        let mut ft = make_file_type();
        ft.apply_ls_colors("di=34", false);
        assert_eq!(ft.directory().fg, Some(Color::Blue));
    }

    #[test]
    fn apply_ls_colors_sets_executable_color() {
        let mut ft = make_file_type();
        ft.apply_ls_colors("ex=32", false);
        assert_eq!(ft.executable().fg, Some(Color::Green));
    }

    #[test]
    fn apply_ls_colors_extension_pattern_populates_extension_styles() {
        let mut ft = make_file_type();
        ft.apply_ls_colors("*.rs=32", false);
        assert!(ft.extension_styles.contains_key("rs"));
    }

    #[test]
    fn apply_ls_colors_name_pattern_populates_name_styles() {
        let mut ft = make_file_type();
        ft.apply_ls_colors("*Makefile=33", false);
        assert!(ft.name_styles.contains_key("Makefile"));
    }

    #[test]
    fn apply_ls_colors_skips_empty_colon_separated_entries() {
        let mut ft = make_file_type();
        ft.apply_ls_colors("::di=34::", false);
        assert_eq!(ft.directory().fg, Some(Color::Blue));
    }
}
