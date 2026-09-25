use std::fmt::Write as _;

use ratatui::buffer::CellWidth;
use ratatui::text::{Line, Span};

use crate::app::config::{
    keybindings::{Action, KeyBindings},
    theme::Help,
};

/// A titled group of `(label, keys)` rows.
pub(super) type Section = (&'static str, Vec<(String, String)>);

/// Every section of the help, in the order shown.
pub(super) fn build_sections(kb: &KeyBindings) -> Vec<Section> {
    vec![
        ("Normal Mode", build_normal_keybindings(kb)),
        ("Prompt Mode", build_prompt_keybindings(kb)),
        ("Bookmarks View", build_bookmarks_keybindings(kb)),
        ("Paste Conflict", build_conflict_keybindings()),
        ("Open With", build_open_with_keybindings(kb)),
    ]
}

/// The widest label across every section, so their key columns line up.
pub(super) fn max_label_width(sections: &[Section]) -> usize {
    sections
        .iter()
        .flat_map(|(_, rows)| rows)
        .map(|(label, _)| label.cell_width() as usize)
        .max()
        .unwrap_or(0)
}

pub(super) fn add_section_header(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    max_label_width: usize,
    help: &Help,
) {
    // Body rows insert ": " (2 cols) between label and keys; match that here.
    let header_padding =
        " ".repeat((max_label_width + 2).saturating_sub(title.cell_width() as usize));
    lines.push(Line::from(vec![
        Span::styled(title.to_string(), help.header()),
        Span::raw(header_padding),
        Span::styled("Keybindings", help.header()),
    ]));
}

pub(super) fn add_keybinding_lines(
    lines: &mut Vec<Line<'static>>,
    keybindings: &[(String, String)],
    max_label_width: usize,
    help: &Help,
) {
    lines.extend(keybindings.iter().map(|(label, keys)| {
        let padding = " ".repeat(max_label_width.saturating_sub(label.cell_width() as usize));
        Line::from(vec![
            Span::styled(label.clone(), help.actions()),
            Span::raw(": "),
            Span::raw(padding),
            Span::styled(keys.clone(), help.shortcuts()),
        ])
    }));
}

/// Annotate single uppercase-letter keys with "(Uppercase)" in a "/"-joined
/// display string, e.g. "G/End" -> "G (Uppercase)/End".
fn annotate_uppercase(display: &str) -> String {
    display
        .split('/')
        .map(|key| {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_uppercase() => format!("{c} (Uppercase)"),
                _ => key.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Build the plain-text keybindings help content for the `--print-keybindings` CLI flag.
/// Section headers are emitted with ANSI bold when `bold` is true (i.e. stdout is a terminal).
pub fn keybindings_help_text(kb: &KeyBindings, bold: bool) -> String {
    const BOLD: &str = "\x1b[1m";
    const RESET: &str = "\x1b[0m";

    fn append_section(
        out: &mut String,
        title: &str,
        bindings: &[(String, String)],
        max_width: usize,
        bold: bool,
    ) {
        let header_padding =
            " ".repeat((max_width + 2).saturating_sub(title.cell_width() as usize));
        if bold {
            out.push_str(BOLD);
        }
        out.push_str(title);
        out.push_str(&header_padding);
        out.push_str("Keybindings");
        if bold {
            out.push_str(RESET);
        }
        out.push('\n');
        for (label, keys) in bindings {
            let padding = " ".repeat(max_width.saturating_sub(label.cell_width() as usize));
            // Writing to a String is infallible, so the Result cannot be an error.
            let _ = writeln!(out, "{label}: {padding}{keys}");
        }
    }

    let sections = build_sections(kb);
    let max_width = max_label_width(&sections);

    let mut out = String::new();
    for (index, (title, rows)) in sections.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        append_section(&mut out, title, rows, max_width, bold);
    }
    out
}

fn kb_entry(label: &str, keys: String) -> (String, String) {
    (label.to_string(), keys)
}

/// The normal bindings as they act on a bookmark, which is a symlink: open
/// follows it, and rename and delete act on the link, not the folder.
fn build_bookmarks_keybindings(kb: &KeyBindings) -> Vec<(String, String)> {
    let d = |a: Action| annotate_uppercase(kb.display_for(a));
    vec![
        kb_entry("Go to the linked folder", d(Action::Open)),
        kb_entry(
            "Rename, delete the bookmark",
            format!("{}, {}", d(Action::Rename), d(Action::Delete)),
        ),
        kb_entry("Leave the bookmarks", d(Action::ResetView)),
    ]
}

/// The answers to a paste collision. They are fixed keys, read by the prompt
/// itself rather than bound.
fn build_conflict_keybindings() -> Vec<(String, String)> {
    vec![
        kb_entry("Skip this entry", "s".into()),
        kb_entry(
            "Skip this and every later collision, also in sources already running",
            "S (Uppercase)".into(),
        ),
        kb_entry("Replace the existing entry", "o".into()),
        kb_entry(
            "Replace this and every later collision the paste meets",
            "O (Uppercase)".into(),
        ),
        kb_entry("Abandon the rest of the paste", "Esc".into()),
    ]
}

/// The "Open with" picker's keys: the normal bindings it reads, plus the row
/// numbers it takes itself.
fn build_open_with_keybindings(kb: &KeyBindings) -> Vec<(String, String)> {
    let d = |a: Action| annotate_uppercase(kb.display_for(a));
    vec![
        kb_entry(
            "Select next, previous application",
            format!("{}, {}", d(Action::SelectNext), d(Action::SelectPrevious)),
        ),
        kb_entry(
            "Select first, last application",
            format!("{}, {}", d(Action::SelectFirst), d(Action::SelectLast)),
        ),
        kb_entry(
            "Page down, up",
            format!("{}, {}", d(Action::PageDown), d(Action::PageUp)),
        ),
        kb_entry("Open with the selected application", d(Action::Open)),
        kb_entry("Open with a numbered application", "1-9".into()),
        kb_entry("Close the picker", d(Action::OpenWith)),
        kb_entry("Close the picker and reset the view", d(Action::ResetView)),
    ]
}

/// Build normal mode keybinding display strings from KeyBindings.
fn build_normal_keybindings(kb: &KeyBindings) -> Vec<(String, String)> {
    let d = |a: Action| annotate_uppercase(kb.display_for(a));
    let s = |a| d(a);
    let p = |a, b| format!("{}, {}", d(a), d(b));
    let t = |a, b, c| format!("{}, {}, {}", d(a), d(b), d(c));

    vec![
        // Navigation
        kb_entry(
            "Select next, previous row",
            p(Action::SelectNext, Action::SelectPrevious),
        ),
        kb_entry(
            "Select first, middle, last row",
            t(
                Action::SelectFirst,
                Action::SelectMiddle,
                Action::SelectLast,
            ),
        ),
        kb_entry(
            "Select top, middle, bottom visible row",
            t(
                Action::SelectFirstVisible,
                Action::SelectMiddleVisible,
                Action::SelectLastVisible,
            ),
        ),
        kb_entry("Page down, up", p(Action::PageDown, Action::PageUp)),
        kb_entry("Go to parent dir", s(Action::GoToParentDirectory)),
        kb_entry("Go to previous dir", s(Action::GoToPreviousDirectory)),
        kb_entry("Go to home dir", s(Action::GoHome)),
        kb_entry("Go to path", s(Action::Goto)),
        // Opening
        kb_entry("Open", s(Action::Open)),
        kb_entry("Open current directory", s(Action::OpenCurrentDirectory)),
        kb_entry("Open new window", s(Action::OpenNewWindow)),
        kb_entry("Open with...", s(Action::OpenWith)),
        kb_entry(
            "Edit ($VISUAL/$EDITOR), page ($PAGER)",
            p(Action::Edit, Action::Page),
        ),
        // Marking
        kb_entry("Mark/unmark item, end range", s(Action::ToggleMark)),
        kb_entry("Range mark", s(Action::RangeMark)),
        kb_entry("Mark all shown items", s(Action::SelectAll)),
        // File operations
        kb_entry(
            "Copy, Cut, Paste",
            t(Action::Copy, Action::Cut, Action::Paste),
        ),
        kb_entry("Rename", s(Action::Rename)),
        kb_entry("Chmod (octal)", s(Action::Chmod)),
        kb_entry("Create directory", s(Action::CreateDirectory)),
        kb_entry("Delete", s(Action::Delete)),
        // View
        kb_entry("Filter", s(Action::Filter)),
        kb_entry("Search", s(Action::Search)),
        kb_entry("Add bookmark", s(Action::AddBookmark)),
        kb_entry("Show bookmarks", s(Action::GetBookmarks)),
        kb_entry("Refresh", s(Action::Refresh)),
        kb_entry(
            "Sort by name, modified, size",
            t(
                Action::SortByName,
                Action::SortByModified,
                Action::SortBySize,
            ),
        ),
        kb_entry("Toggle show hidden files", s(Action::ToggleShowHidden)),
        // Application
        kb_entry("Cancel file or search operations", s(Action::CancelTask)),
        kb_entry(
            "Clear alerts, progress",
            p(Action::ClearAlerts, Action::ClearProgress),
        ),
        kb_entry(
            "Clear clipboard/filter/marks/search, exit bookmarks",
            s(Action::ResetView),
        ),
        kb_entry("Toggle help", s(Action::ToggleHelp)),
        kb_entry("Quit", s(Action::Quit)),
    ]
}

/// Build prompt mode keybinding display strings from KeyBindings.
fn build_prompt_keybindings(kb: &KeyBindings) -> Vec<(String, String)> {
    let d = |a: Action| annotate_uppercase(kb.display_for(a));
    let s = |a| d(a);
    let t = |a, b, c| format!("{}, {}, {}", d(a), d(b), d(c));
    let pair = |a, b| format!("{}/{}", d(a), d(b));

    vec![
        kb_entry("Submit", s(Action::PromptSubmit)),
        kb_entry("Cancel", s(Action::PromptCancel)),
        kb_entry("Reset to initial value", s(Action::PromptReset)),
        kb_entry("Select all", s(Action::PromptSelectAll)),
        kb_entry(
            "Copy, Cut, Paste text",
            t(Action::PromptCopy, Action::PromptCut, Action::PromptPaste),
        ),
        kb_entry("Move cursor", "←/→".into()),
        kb_entry("Move cursor by word", "Ctrl+←/→, Alt+b/f".into()),
        kb_entry("Move cursor to start, end", "Home, Ctrl+e/End".into()),
        kb_entry("Select text", "Shift+←/→".into()),
        kb_entry("Select to line start, end", "Shift+Home, Shift+End".into()),
        kb_entry("Select by word", "Ctrl+Shift+←/→".into()),
        kb_entry("Delete before, after cursor", "Backspace, Delete".into()),
        kb_entry(
            "Delete word before, after cursor",
            "Ctrl+w/Alt+Backspace, Alt+d/Alt+Delete".into(),
        ),
        kb_entry("Delete to start, end", "Ctrl+j, Ctrl+k".into()),
        kb_entry("Accept path suggestion", s(Action::PromptAcceptSuggestion)),
        kb_entry(
            "Cycle path suggestions",
            pair(
                Action::PromptNextSuggestion,
                Action::PromptPreviousSuggestion,
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::{annotate_uppercase, keybindings_help_text};
    use crate::app::config::Config;

    #[test_case("End",     "End"                 ; "multi-char key is unchanged")]
    #[test_case("G",       "G (Uppercase)"       ; "single uppercase letter is annotated")]
    #[test_case("g",       "g"                   ; "single lowercase letter is unchanged")]
    #[test_case("1",       "1"                   ; "single digit is not annotated")]
    #[test_case("",        ""                    ; "empty string is unchanged")]
    #[test_case("G/End",   "G (Uppercase)/End"   ; "annotates only the single-letter half")]
    #[test_case("g/G",     "g/G (Uppercase)"     ; "annotates the uppercase half of a pair")]
    fn annotate_uppercase_marks_single_uppercase_letters(input: &str, expected: &str) {
        assert_eq!(annotate_uppercase(input), expected);
    }

    /// What `--print-keybindings` writes. The flag exists to be read, so the
    /// two mode sections and the resolved keys have to reach the output.
    fn help_text(bold: bool) -> String {
        keybindings_help_text(&Config::builtin().keybindings, bold)
    }

    #[test]
    fn keybindings_help_lists_both_modes_and_their_resolved_keys() {
        let text = help_text(false);

        assert!(text.contains("Normal Mode"), "{text}");
        assert!(text.contains("Prompt Mode"), "{text}");
        // A configurable binding, and one that merges a hardcoded key with a
        // configurable one; both are what the printed list is for.
        assert!(text.contains("Quit:"), "{text}");
        assert!(text.contains("Select next, previous row:"), "{text}");
        assert!(text.contains("\u{2193}/j"), "{text}");
    }

    /// Every key column starts where the header's "Keybindings" does, in both
    /// sections, so the printed list reads as two columns.
    #[test]
    fn the_printed_keys_line_up_under_the_header() {
        let text = help_text(false);
        let header_column = text.find("Keybindings").expect("a header");

        for line in text.lines().filter(|line| line.contains(": ")) {
            let after_label = line.find(": ").unwrap() + 2;
            let keys_column =
                after_label + line[after_label..].len() - line[after_label..].trim_start().len();
            assert_eq!(header_column, keys_column, "{line:?}");
        }
        for header in text.lines().filter(|line| line.ends_with("Keybindings")) {
            assert_eq!(
                header_column,
                header.find("Keybindings").unwrap(),
                "{header:?}"
            );
        }
    }

    /// The keys only a paste collision or the picker reads, which neither
    /// mode section lists, and the bookmarks view's use of the normal ones.
    #[test_case("Bookmarks View", "Go to the linked folder", "\u{2192}/l/Enter" ; "following a bookmark")]
    #[test_case("Bookmarks View", "Rename, delete the bookmark", "r/F2, d/Delete" ; "renaming and deleting a bookmark")]
    #[test_case("Paste Conflict", "Skip this and every later collision, also in sources already running", "S (Uppercase)" ; "a conflict answer")]
    #[test_case("Paste Conflict", "Replace this and every later collision the paste meets", "O (Uppercase)" ; "overwrite all")]
    #[test_case("Paste Conflict", "Abandon the rest of the paste", "Esc" ; "abandoning a paste")]
    #[test_case("Open With", "Open with a numbered application", "1-9" ; "the row numbers")]
    #[test_case("Open With", "Close the picker", "o" ; "closing the picker")]
    #[test_case("Open With", "Select first, last application", "Home/g/^, End/G (Uppercase)/$" ; "the first and last rows")]
    #[test_case("Open With", "Page down, up", "PgDn/Ctrl+d/Ctrl+f, PgUp/Ctrl+u/Ctrl+b" ; "paging")]
    fn the_help_lists_the_conflict_and_picker_keys(section: &str, label: &str, keys: &str) {
        let text = help_text(false);
        let rest = &text[text.find(section).expect("the section")..];
        let line = rest
            .lines()
            .find(|line| line.starts_with(&format!("{label}:")))
            .unwrap_or_else(|| panic!("no {label:?} line under {section}:\n{text}"));
        assert_eq!(keys, line.rsplit(": ").next().unwrap().trim(), "{line}");
    }

    #[test]
    fn only_the_bold_flag_adds_escape_codes() {
        // Bold is used when stdout is a terminal; a redirected run must stay
        // plain, or the codes end up in whatever the output was piped into.
        assert!(!help_text(false).contains('\u{1b}'));

        let bold = help_text(true);
        assert!(bold.contains("\u{1b}[1mNormal Mode"), "{bold}");
        assert!(bold.contains("\u{1b}[1mPrompt Mode"), "{bold}");
        // Only the headers are bold: a binding line carries no codes.
        let line = bold
            .lines()
            .find(|line| line.starts_with("Quit:"))
            .expect("a Quit line");
        assert!(!line.contains('\u{1b}'), "{line}");
    }
}
