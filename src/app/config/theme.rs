use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

use super::serde::{deserialize_color, deserialize_modifier};

/// A triplet of style properties: foreground color, background color, and modifiers.
/// All fields are optional — omitted fields inherit defaults (no color, no modifiers).
/// `fg` and `bg` accept `""` in config to explicitly inherit from the parent widget.
#[derive(Copy, Clone, Default, Deserialize)]
pub struct StyleConfig {
    #[serde(default, deserialize_with = "deserialize_color")]
    fg: Option<Color>,

    #[serde(default, deserialize_with = "deserialize_color")]
    bg: Option<Color>,

    #[serde(default, deserialize_with = "deserialize_modifier")]
    modifiers: Modifier,
}

impl StyleConfig {
    fn new(fg: Option<Color>, bg: Option<Color>, modifiers: Modifier) -> Self {
        Self { fg, bg, modifiers }
    }
}

impl From<StyleConfig> for Style {
    fn from(config: StyleConfig) -> Self {
        let mut style = Style::default().add_modifier(config.modifiers);
        if let Some(bg) = config.bg {
            style = style.bg(bg);
        }
        if let Some(fg) = config.fg {
            style = style.fg(fg);
        }
        style
    }
}

macro_rules! style_getter {
    ($name:ident) => {
        pub fn $name(&self) -> Style {
            self.$name.into()
        }
    };
}

/// Declares a theme sub-struct whose fields are all `StyleConfig`, deriving
/// `Deserialize` and a `Style` getter for each field. Prefix the field list
/// with `base,` to add a `#[serde(flatten)]`-ed `base` style.
macro_rules! style_struct {
    ($name:ident { base, $($field:ident),+ $(,)? }) => {
        #[derive(Deserialize)]
        pub struct $name {
            #[serde(flatten)]
            base: StyleConfig,
            $($field: StyleConfig,)+
        }

        impl $name {
            style_getter!(base);
            $(style_getter!($field);)+
        }
    };
    ($name:ident { $($field:ident),+ $(,)? }) => {
        #[derive(Deserialize)]
        pub struct $name {
            $($field: StyleConfig,)+
        }

        impl $name {
            $(style_getter!($field);)+
        }
    };
}

/// Declares the `FileType` struct from a single `field => "dircolors-key"`
/// table, generating the `StyleConfig` fields, their `Style` getters, and
/// `set_ls_color` (the `LS_COLORS` key → field dispatch). This keeps the field
/// list, getters, and `LS_COLORS` mapping from drifting apart.
macro_rules! file_type {
    ($($field:ident => $ls_key:literal),+ $(,)?) => {
        #[derive(Deserialize, Default)]
        pub struct FileType {
            /// Whether to apply colors from the $LS_COLORS environment variable
            /// (if set) on top of the colors configured below.
            ls_colors_take_precedence: bool,

            $($field: StyleConfig,)+

            // Pattern-based styles
            #[serde(skip)]
            extension_styles: HashMap<String, StyleConfig>,
            // Insertion-ordered (LS_COLORS order). Lookup picks the longest
            // matching pattern so results are deterministic and the most
            // specific pattern wins.
            #[serde(skip)]
            name_styles: Vec<(String, StyleConfig)>,
        }

        impl FileType {
            $(style_getter!($field);)+

            /// Apply a parsed `LS_COLORS` style for a dircolors file-type key.
            /// Returns false if `key` is not a recognized file-type key.
            fn set_ls_color(&mut self, key: &str, style: StyleConfig) -> bool {
                match key {
                    $($ls_key => self.$field = style,)+
                    _ => return false,
                }
                true
            }
        }
    };
}

file_type! {
    block_device => "bd",
    character_device => "cd",
    directory => "di",
    directory_other_writable => "ow",
    directory_sticky => "st",
    directory_sticky_other_writable => "tw",
    door => "do",
    executable => "ex",
    missing => "mi",
    normal_file => "no",
    pipe => "pi",
    regular_file => "fi",
    setgid => "sg",
    setuid => "su",
    socket => "so",
    symlink => "ln",
    symlink_broken => "or",
}

impl FileType {
    /// Applies `LS_COLORS` (passed in by the caller, not read from the
    /// environment here, so config parsing stays pure) on top of the
    /// configured colors when `ls_colors_take_precedence` is set.
    pub(super) fn maybe_apply_ls_colors(&mut self, ls_colors: Option<&str>, warn_on_rgb: bool) {
        if self.ls_colors_take_precedence
            && let Some(ls_colors) = ls_colors
        {
            self.apply_ls_colors(ls_colors, warn_on_rgb);
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

            if warn_on_rgb
                && matches!(
                    (fg, bg),
                    (Some(Color::Rgb(..)), _) | (_, Some(Color::Rgb(..)))
                )
            {
                found_rgb = true;
            }

            let style = StyleConfig::new(fg, bg, attrs);
            if self.set_ls_color(key, style) {
                // Recognized file-type key — handled by set_ls_color.
            } else if let Some(ext) = key.strip_prefix("*.") {
                self.extension_styles.insert(ext.to_string(), style);
            } else if let Some(name) = key.strip_prefix('*') {
                self.name_styles.push((name.to_string(), style));
            }
            // Otherwise unrecognized (e.g. "ca" capabilities) — ignored.
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

        // Name pattern: longest matching suffix wins (most specific).
        self.name_styles
            .iter()
            .filter(|(pattern, _)| name.ends_with(pattern.as_str()))
            .max_by_key(|(pattern, _)| pattern.len())
            .map(|(_, style)| (*style).into())
    }
}

style_struct!(FileSize {
    bytes,
    kib,
    mib,
    gib,
    tib,
    pib
});

style_struct!(FileModifiedDate {
    less_than_minute,
    less_than_hour,
    less_than_day,
    less_than_month,
    less_than_year,
    greater_than_year,
});

style_struct!(Alert {
    base,
    error,
    info,
    warn
});

style_struct!(Breadcrumbs {
    base,
    ancestor,
    basename,
    bookmarks,
    search,
    separator,
});

style_struct!(Clipboard { copy, cut, delete });

style_struct!(Notice {
    filter,
    progress,
    search,
    search_loading,
});

style_struct!(Prompt {
    cursor,
    delete,
    goto_suggestion,
    input,
    label,
    selected,
});

style_struct!(Status { detail, label });

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
    bookmark: StyleConfig,
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
    style_getter!(bookmark);
    style_getter!(delete);
    style_getter!(header);
    style_getter!(header_sorted);
    style_getter!(marked);
    style_getter!(selected);
}

style_struct!(Help {
    base,
    actions,
    header,
    shortcuts,
});

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
        FileType::default()
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
        ft.name_styles.push((pattern.to_string(), red()));
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
    fn apply_ls_colors_skips_empty_colon_separated_entries() {
        let mut ft = make_file_type();
        ft.apply_ls_colors("::di=34::", false);
        assert_eq!(ft.directory().fg, Some(Color::Blue));
    }
}
