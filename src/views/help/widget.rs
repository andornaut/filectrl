use std::fmt::Write as _;

use ratatui::buffer::CellWidth;
use ratatui::text::{Line, Span};

use crate::app::config::{
    keybindings::{Action, KeyBindings},
    theme::Help,
};

/// One `(label, keys)` row.
type Row = (&'static str, String);

/// A titled group of rows.
pub(super) type Section = (&'static str, Vec<Row>);

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

/// The widest label in one section. Each section lines up its own key column,
/// so one long label does not push every other section's keys off a narrow
/// terminal.
pub(super) fn label_width(rows: &[Row]) -> usize {
    rows.iter()
        .map(|(label, _)| label.cell_width() as usize)
        .max()
        .unwrap_or(0)
}

/// Spaces after a section title, so "Keybindings" starts where the rows'
/// keys do: rows insert ": " (2 columns) between label and keys.
fn header_padding(title: &str, label_width: usize) -> String {
    " ".repeat((label_width + 2).saturating_sub(title.cell_width() as usize))
}

/// Spaces after a row's ": ", so its keys start in the section's key column.
fn row_padding(label: &str, label_width: usize) -> String {
    " ".repeat(label_width.saturating_sub(label.cell_width() as usize))
}

pub(super) fn add_section_header(
    lines: &mut Vec<Line<'static>>,
    title: &'static str,
    label_width: usize,
    help: &Help,
) {
    lines.push(Line::from(vec![
        Span::styled(title, help.header()),
        Span::raw(header_padding(title, label_width)),
        Span::styled("Keybindings", help.header()),
    ]));
}

pub(super) fn add_keybinding_lines(
    lines: &mut Vec<Line<'static>>,
    rows: &[Row],
    label_width: usize,
    help: &Help,
) {
    lines.extend(rows.iter().map(|(label, keys)| {
        Line::from(vec![
            Span::styled(*label, help.actions()),
            Span::raw(": "),
            Span::raw(row_padding(label, label_width)),
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

/// The keys bound to each of `actions`, joined by `separator`.
fn keys(kb: &KeyBindings, actions: &[Action], separator: &str) -> String {
    actions
        .iter()
        .map(|action| annotate_uppercase(kb.display_for(*action)))
        .collect::<Vec<_>>()
        .join(separator)
}

/// Build the plain-text keybindings help content for the `--print-keybindings` CLI flag.
/// Section headers are emitted with ANSI bold when `bold` is true (i.e. stdout is a terminal).
pub fn keybindings_help_text(kb: &KeyBindings, bold: bool) -> String {
    let (bold, reset) = if bold {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    let mut out = String::new();
    for (index, (title, rows)) in build_sections(kb).iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let width = label_width(rows);
        // Writing to a String is infallible, so the Result cannot be an error.
        let _ = writeln!(
            out,
            "{bold}{title}{}Keybindings{reset}",
            header_padding(title, width)
        );
        for (label, keys) in rows {
            let _ = writeln!(out, "{label}: {}{keys}", row_padding(label, width));
        }
    }
    out
}

/// The normal bindings as they act on a bookmark, which is a symlink: open
/// follows it, and rename and delete act on the link, not the folder.
fn build_bookmarks_keybindings(kb: &KeyBindings) -> Vec<Row> {
    let k = |actions: &[Action]| keys(kb, actions, ", ");
    vec![
        ("Go to the linked folder", k(&[Action::Open])),
        (
            "Rename, delete the bookmark",
            k(&[Action::Rename, Action::Delete]),
        ),
        ("Leave the bookmarks", k(&[Action::ResetView])),
    ]
}

/// The answers to a paste collision. They are fixed keys, read by the prompt
/// itself rather than bound.
fn build_conflict_keybindings() -> Vec<Row> {
    vec![
        ("Skip this entry", "s".into()),
        (
            "Skip every collision, also in sources already running",
            "S (Uppercase)".into(),
        ),
        ("Replace the existing entry", "o".into()),
        (
            "Replace every collision the paste meets",
            "O (Uppercase)".into(),
        ),
        ("Abandon the rest of the paste", "Esc".into()),
    ]
}

/// The "Open with" picker's keys: the normal bindings it reads, plus the row
/// numbers it takes itself.
fn build_open_with_keybindings(kb: &KeyBindings) -> Vec<Row> {
    let k = |actions: &[Action]| keys(kb, actions, ", ");
    vec![
        (
            "Select next, previous application",
            k(&[Action::SelectNext, Action::SelectPrevious]),
        ),
        (
            "Select first, last application",
            k(&[Action::SelectFirst, Action::SelectLast]),
        ),
        ("Page down, up", k(&[Action::PageDown, Action::PageUp])),
        ("Open with the selected application", k(&[Action::Open])),
        ("Open with a numbered application", "1-9".into()),
        ("Close the picker", k(&[Action::OpenWith])),
        (
            "Close the picker and reset the view",
            k(&[Action::ResetView]),
        ),
    ]
}

/// Build normal mode keybinding display strings from KeyBindings.
fn build_normal_keybindings(kb: &KeyBindings) -> Vec<Row> {
    let k = |actions: &[Action]| keys(kb, actions, ", ");

    vec![
        // Navigation
        (
            "Select next, previous row",
            k(&[Action::SelectNext, Action::SelectPrevious]),
        ),
        (
            "Select first, middle, last row",
            k(&[
                Action::SelectFirst,
                Action::SelectMiddle,
                Action::SelectLast,
            ]),
        ),
        (
            "Select top, middle, bottom row",
            k(&[
                Action::SelectFirstVisible,
                Action::SelectMiddleVisible,
                Action::SelectLastVisible,
            ]),
        ),
        ("Page down, up", k(&[Action::PageDown, Action::PageUp])),
        ("Go to parent dir", k(&[Action::GoToParentDirectory])),
        ("Go to previous dir", k(&[Action::GoToPreviousDirectory])),
        ("Go to home dir", k(&[Action::GoHome])),
        ("Go to path", k(&[Action::Goto])),
        // Opening
        ("Open", k(&[Action::Open])),
        ("Open current directory", k(&[Action::OpenCurrentDirectory])),
        ("Open new window", k(&[Action::OpenNewWindow])),
        ("Open with...", k(&[Action::OpenWith])),
        (
            "Edit ($EDITOR), page ($PAGER)",
            k(&[Action::Edit, Action::Page]),
        ),
        // Marking
        ("Mark/unmark item, end range", k(&[Action::ToggleMark])),
        ("Range mark", k(&[Action::RangeMark])),
        ("Mark all shown items", k(&[Action::SelectAll])),
        // File operations
        (
            "Copy, Cut, Paste",
            k(&[Action::Copy, Action::Cut, Action::Paste]),
        ),
        ("Rename", k(&[Action::Rename])),
        ("Chmod (octal)", k(&[Action::Chmod])),
        ("Create directory", k(&[Action::CreateDirectory])),
        ("Delete", k(&[Action::Delete])),
        // View
        ("Filter", k(&[Action::Filter])),
        ("Search", k(&[Action::Search])),
        ("Add bookmark", k(&[Action::AddBookmark])),
        ("Show bookmarks", k(&[Action::GetBookmarks])),
        ("Refresh", k(&[Action::Refresh])),
        (
            "Sort by name, modified, size",
            k(&[
                Action::SortByName,
                Action::SortByModified,
                Action::SortBySize,
            ]),
        ),
        ("Toggle show hidden files", k(&[Action::ToggleShowHidden])),
        // Application
        ("Cancel paste, delete or search", k(&[Action::CancelTask])),
        (
            "Clear alerts, progress",
            k(&[Action::ClearAlerts, Action::ClearProgress]),
        ),
        ("Reset view, leave bookmarks", k(&[Action::ResetView])),
        ("Toggle help", k(&[Action::ToggleHelp])),
        ("Quit", k(&[Action::Quit])),
    ]
}

/// Build prompt mode keybinding display strings from KeyBindings.
fn build_prompt_keybindings(kb: &KeyBindings) -> Vec<Row> {
    let k = |actions: &[Action]| keys(kb, actions, ", ");

    vec![
        ("Submit", k(&[Action::PromptSubmit])),
        ("Cancel", k(&[Action::PromptCancel])),
        ("Reset to initial value", k(&[Action::PromptReset])),
        ("Select all", k(&[Action::PromptSelectAll])),
        (
            "Copy, Cut, Paste text",
            k(&[Action::PromptCopy, Action::PromptCut, Action::PromptPaste]),
        ),
        ("Move cursor", "←/→".into()),
        ("Move cursor by word", "Ctrl+←/→, Alt+b/f".into()),
        ("Move cursor to start, end", "Home, Ctrl+e/End".into()),
        ("Select text", "Shift+←/→".into()),
        ("Select to line start, end", "Shift+Home, Shift+End".into()),
        ("Select by word", "Ctrl+Shift+←/→".into()),
        ("Delete before, after cursor", "Backspace, Delete".into()),
        (
            "Delete word before, after cursor",
            "Ctrl+w/Alt+Backspace, Alt+d/Alt+Delete".into(),
        ),
        ("Delete to start, end", "Ctrl+j, Ctrl+k".into()),
        (
            "Accept path suggestion",
            k(&[Action::PromptAcceptSuggestion]),
        ),
        (
            "Cycle path suggestions",
            keys(
                kb,
                &[
                    Action::PromptNextSuggestion,
                    Action::PromptPreviousSuggestion,
                ],
                "/",
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

    /// Every key column starts where its section's "Keybindings" header does,
    /// so each section reads as two columns.
    #[test]
    fn the_printed_keys_line_up_under_their_header() {
        let text = help_text(false);
        let mut header_column = None;

        for line in text.lines() {
            if line.ends_with("Keybindings") {
                header_column = line.find("Keybindings");
            } else if let Some(after_label) = line.find(": ").map(|at| at + 2) {
                let keys_column = after_label + line[after_label..].len()
                    - line[after_label..].trim_start().len();
                assert_eq!(header_column, Some(keys_column), "{line:?}");
            }
        }
    }

    /// Each section sizes its own label column, so on an 80-column terminal
    /// no key is cut off by one long label elsewhere.
    #[test]
    fn every_printed_line_fits_80_columns() {
        use ratatui::buffer::CellWidth;
        let text = help_text(false);

        for line in text.lines() {
            assert!(
                line.cell_width() <= 78,
                "{} columns: {line:?}",
                line.cell_width()
            );
        }
    }

    /// The keys only a paste collision or the picker reads, which neither
    /// mode section lists, and the bookmarks view's use of the normal ones.
    #[test_case("Bookmarks View", "Go to the linked folder", "\u{2192}/l/Enter" ; "following a bookmark")]
    #[test_case("Bookmarks View", "Rename, delete the bookmark", "r/F2, d/Delete" ; "renaming and deleting a bookmark")]
    #[test_case("Paste Conflict", "Skip every collision, also in sources already running", "S (Uppercase)" ; "a conflict answer")]
    #[test_case("Paste Conflict", "Replace every collision the paste meets", "O (Uppercase)" ; "overwrite all")]
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
