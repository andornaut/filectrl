use std::collections::HashSet;

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

use super::serde::{deserialize_color, deserialize_modifier};

/// A triplet of style properties: foreground color, background color, and modifiers.
/// All fields are optional: omitted fields inherit defaults (no color, no modifiers).
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
        #[derive(Clone, Deserialize, Default)]
        pub struct FileType {
            $($field: StyleConfig,)+

            // `*` patterns, in `LS_COLORS` order (see `pattern_styles`).
            #[serde(skip)]
            suffix_styles: Vec<SuffixStyle>,
            // Keys in `FALLTHROUGH_KEYS` that `LS_COLORS` reset. `ls` treats
            // those as uncolored and classifies the entry by the next rule, so
            // a reset there is not "render plain".
            #[serde(skip)]
            uncolored: HashSet<&'static str>,
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
    normal_file => "no",
    pipe => "pi",
    regular_file => "fi",
    setgid => "sg",
    setuid => "su",
    socket => "so",
    symlink => "ln",
    symlink_broken => "or",
}

/// An `LS_COLORS` `*` pattern: the suffix after the `*`, and whether it
/// compares with regard to case.
#[derive(Clone)]
struct SuffixStyle {
    suffix: String,
    exact: bool,
    style: StyleConfig,
}

/// Keys `ls` consults only while they are colored: reset, the entry is
/// classified by the next rule (`ow` falls back to `di`, `ex` to the patterns
/// and `fi`, `or` to `ln`) rather than rendered plain.
const FALLTHROUGH_KEYS: [&str; 7] = ["ex", "or", "ow", "sg", "st", "su", "tw"];

impl FileType {
    /// This theme with `ls_colors` applied on top, as `ui.ls_colors_take_precedence`
    /// would apply it.
    #[cfg(test)]
    #[must_use]
    pub fn with_ls_colors(&self, ls_colors: &str) -> Self {
        let mut applied = self.clone();
        applied.apply_ls_colors(ls_colors, false);
        applied
    }

    /// Whether the `LS_COLORS` key `key` still colors its entries, which only a
    /// reset of one of `FALLTHROUGH_KEYS` undoes.
    pub fn is_colored(&self, key: &str) -> bool {
        !self.uncolored.contains(key)
    }

    /// Applies `LS_COLORS` (passed in by the caller, not read from the
    /// environment here, so config parsing stays pure) on top of the
    /// configured colors.
    pub(super) fn apply_ls_colors(&mut self, ls_colors: &str, warn_on_rgb: bool) {
        let mut found_rgb = false;
        for entry in ls_colors.split(':') {
            let Some((key, value)) = entry.split_once('=') else {
                continue;
            };

            let (fg, bg, attrs) = super::ls_colors::parse(value);
            // `ls` counts a key as uncolored only when its value is empty,
            // `0` or `00` exactly. That renders the entry plain, except for
            // `FALLTHROUGH_KEYS`. Other values made only of reset codes
            // (`0;00`) count as colored and print as plain. Anything else that
            // parses to no style is unrecognized codes, which leave the
            // configured style alone.
            let is_reset = matches!(value, "" | "0" | "00");
            let is_plain = is_reset || value.split(';').all(|code| matches!(code, "0" | "00"));
            if fg.is_none() && bg.is_none() && attrs == Modifier::empty() && !is_plain {
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

            if let Some(&fallthrough) = FALLTHROUGH_KEYS.iter().find(|&&k| k == key) {
                if is_reset {
                    self.uncolored.insert(fallthrough);
                    continue;
                }
                self.uncolored.remove(fallthrough);
            }

            let style = StyleConfig::new(fg, bg, attrs);
            if self.set_ls_color(key, style) {
                // Recognized file-type key, handled by set_ls_color.
            } else if let Some(suffix) = key.strip_prefix('*') {
                self.suffix_styles.push(SuffixStyle {
                    suffix: suffix.to_string(),
                    exact: false,
                    style,
                });
            }
            // Otherwise unrecognized (e.g. "ca" capabilities), ignored.
        }
        // As in `ls`, a pattern compares with regard to case only when
        // another one differs from it in case alone.
        let suffixes: Vec<String> = self
            .suffix_styles
            .iter()
            .map(|pattern| pattern.suffix.clone())
            .collect();
        for pattern in &mut self.suffix_styles {
            pattern.exact = suffixes.iter().any(|other| {
                *other != pattern.suffix && other.eq_ignore_ascii_case(&pattern.suffix)
            });
        }
        if found_rgb {
            log::warn!(
                "$LS_COLORS contains truecolor (RGB) entries; these may not render correctly on a 256-color terminal"
            );
        }
    }

    /// The style of the last `*` pattern that `name` ends with, as `ls`
    /// matches them: a suffix of the whole name (so `*.gitignore` colors the
    /// dotfile `.gitignore`), without regard to ASCII case unless the pattern
    /// has a case variant listed too, the last listed match winning.
    pub fn pattern_styles(&self, name: &str) -> Option<Style> {
        let name = name.as_bytes();
        self.suffix_styles
            .iter()
            .rev()
            .find(|pattern| {
                let suffix = pattern.suffix.as_bytes();
                name.len() >= suffix.len() && {
                    let tail = &name[name.len() - suffix.len()..];
                    if pattern.exact {
                        tail == suffix
                    } else {
                        tail.eq_ignore_ascii_case(suffix)
                    }
                }
            })
            .map(|pattern| pattern.style.into())
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

style_struct!(Clipboard { copy, cut });

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

style_struct!(Table {
    body,
    bookmark,
    delete,
    header,
    header_sorted,
    marked,
    selected,
});

style_struct!(Help {
    base,
    actions,
    header,
    shortcuts,
});

style_struct!(OpenWith {
    base,
    detail,
    selected,
    shortcut,
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
    pub open_with: OpenWith,
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
    use test_case::test_case;

    use super::*;

    fn red() -> StyleConfig {
        StyleConfig::new(Some(Color::Red), None, Modifier::empty())
    }

    // --- pattern_styles: `*` patterns, matched as GNU `ls` matches them ---

    fn fg_of(ls_colors: &str, name: &str) -> Option<Color> {
        let mut ft = FileType::default();
        ft.apply_ls_colors(ls_colors, false);
        ft.pattern_styles(name).and_then(|style| style.fg)
    }

    #[test_case("*.rs=31", "foo.rs", true ; "an extension")]
    #[test_case("*.gz=31", "foo.tar.gz", true ; "the last extension of several")]
    #[test_case("*.rs=31", "Makefile", false ; "another name")]
    #[test_case("*.foo=31", "foo", false ; "a name shorter than the suffix")]
    #[test_case("*Makefile=31", "OldMakefile", true ; "a longer name ending with it")]
    #[test_case("*Makefile=31", "Makefile.bak", false ; "a name that only contains it")]
    #[test_case("*bashrc=31", ".bashrc", true ; "a dotfile by a name pattern")]
    #[test_case("*.gitignore=31", ".gitignore", true ; "a dotfile that is the whole pattern")]
    fn a_pattern_is_a_suffix_of_the_whole_name(ls_colors: &str, name: &str, matches: bool) {
        assert_eq!(matches, fg_of(ls_colors, name).is_some());
    }

    #[test_case("*.jpg=31", "a.JPG" ; "an upper-case extension")]
    #[test_case("*.jpg=31", "a.Jpg" ; "a mixed-case extension")]
    #[test_case("*README=31", "readme" ; "a name pattern")]
    fn a_pattern_matches_without_regard_to_case(ls_colors: &str, name: &str) {
        assert_eq!(Some(Color::Red), fg_of(ls_colors, name));
    }

    /// `ls` compares case only among patterns that differ in case alone, so
    /// each of them colors its own spelling and a third spelling gets neither.
    #[test_case("a.JPG" => Some(Color::Blue) ; "one spelling")]
    #[test_case("a.jpg" => Some(Color::Red) ; "the other")]
    #[test_case("a.Jpg" => None ; "a third")]
    fn case_variants_match_their_own_case(name: &str) -> Option<Color> {
        fg_of("*.JPG=34:*.jpg=31", name)
    }

    /// The last listed match wins, not the longest, as in `ls`: both orders,
    /// so neither the first nor the longest pattern can pass for the rule.
    #[test_case("*.tar.gz=34:*.gz=31" => Some(Color::Red) ; "the shorter listed last")]
    #[test_case("*.gz=31:*.tar.gz=34" => Some(Color::Blue) ; "the longer listed last")]
    fn the_last_listed_match_wins(ls_colors: &str) -> Option<Color> {
        fg_of(ls_colors, "foo.tar.gz")
    }

    /// An empty value, `0` or `00` renders the entry plain, as `ls` does, for
    /// a file-type key and a pattern alike, and so does a value of reset codes
    /// alone; codes it does not recognize leave the theme alone.
    #[test_case("di=00:*.txt=0:*README=00" => (Style::default(), Some(Style::default()), Some(Style::default())) ; "an explicit reset")]
    #[test_case("di=:*.txt=:*README=" => (Style::default(), Some(Style::default()), Some(Style::default())) ; "an empty value")]
    #[test_case("di=0;00:*.txt=00;0" => (Style::default(), Some(Style::default()), None) ; "reset codes alone")]
    #[test_case("di=xyz:*.txt=x:*README=99" => (Style::from(red()), None, None) ; "unrecognized codes")]
    fn an_explicit_reset_renders_plain(ls_colors: &str) -> (Style, Option<Style>, Option<Style>) {
        let mut ft = FileType {
            directory: red(),
            ..FileType::default()
        };
        ft.apply_ls_colors(ls_colors, false);
        (
            ft.directory(),
            ft.pattern_styles("notes.txt"),
            ft.pattern_styles("README"),
        )
    }

    #[test]
    fn a_style_config_carries_every_property_into_the_style() {
        let style = Style::from(StyleConfig::new(
            Some(Color::Red),
            Some(Color::Blue),
            Modifier::BOLD,
        ));
        assert_eq!(
            Style::default()
                .fg(Color::Red)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            style
        );
    }

    // --- apply_ls_colors round-trips ---

    #[test]
    fn apply_ls_colors_sets_directory_color() {
        let mut ft = FileType::default();
        ft.apply_ls_colors("di=34", false);
        assert_eq!(ft.directory().fg, Some(Color::Blue));
    }

    #[test]
    fn apply_ls_colors_sets_executable_color() {
        let mut ft = FileType::default();
        ft.apply_ls_colors("ex=32", false);
        assert_eq!(ft.executable().fg, Some(Color::Green));
    }

    #[test]
    fn apply_ls_colors_keeps_the_configured_style_for_an_entry_that_parses_to_nothing() {
        let mut ft = FileType::default();
        ft.apply_ls_colors("di=34", false);

        // "99" is not a code this understands, so the entry carries no color
        // and no modifier. Applying it would replace a color the user
        // configured with nothing at all.
        ft.apply_ls_colors("di=99", false);

        assert_eq!(ft.directory().fg, Some(Color::Blue));
    }

    #[test]
    fn apply_ls_colors_skips_empty_colon_separated_entries() {
        // A trailing or doubled colon leaves an empty entry, which GNU `ls`
        // accepts, so it must be skipped rather than end the parse.
        let mut ft = FileType::default();
        ft.apply_ls_colors("::di=34::", false);
        assert_eq!(ft.directory().fg, Some(Color::Blue));
    }
}
