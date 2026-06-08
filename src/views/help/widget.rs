use ratatui::buffer::CellWidth;
use ratatui::text::{Line, Span};

use crate::app::config::{
    keybindings::{Action, KeyBindings},
    theme::Help,
};

pub(super) fn max_label_width(normal: &[(String, String)], prompt: &[(String, String)]) -> usize {
    normal
        .iter()
        .chain(prompt.iter())
        .map(|(label, _)| label.cell_width() as usize)
        .max()
        .unwrap_or(0)
}

pub(super) fn add_section_header(
    lines: &mut Vec<Line<'_>>,
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

pub(super) fn add_keybinding_lines<'a>(
    lines: &mut Vec<Line<'a>>,
    keybindings: &'a [(String, String)],
    max_label_width: usize,
    help: &Help,
) {
    lines.extend(keybindings.iter().map(|(label, keys)| {
        let padding = " ".repeat(max_label_width.saturating_sub(label.cell_width() as usize));
        Line::from(vec![
            Span::styled(label.as_str(), help.actions()),
            Span::raw(": "),
            Span::raw(padding),
            Span::styled(keys.as_str(), help.shortcuts()),
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

/// Build the plain-text keybindings help content for the `--keybindings` CLI flag.
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
            out.push_str(&format!("{label}: {padding}{keys}\n"));
        }
    }

    let normal = build_normal_keybindings(kb);
    let prompt = build_prompt_keybindings(kb);
    let max_width = max_label_width(&normal, &prompt);

    let mut out = String::new();
    append_section(&mut out, "Normal Mode", &normal, max_width, bold);
    out.push('\n');
    append_section(&mut out, "Prompt Mode", &prompt, max_width, bold);
    out
}

fn kb_entry(label: &str, keys: String) -> (String, String) {
    (label.to_string(), keys)
}

/// Build normal mode keybinding display strings from KeyBindings.
pub(super) fn build_normal_keybindings(kb: &KeyBindings) -> Vec<(String, String)> {
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
        // Marking
        kb_entry("Mark/unmark item", s(Action::ToggleMark)),
        kb_entry("Range mark", s(Action::RangeMark)),
        // File operations
        kb_entry(
            "Copy, Cut, Paste",
            t(Action::Copy, Action::Cut, Action::Paste),
        ),
        kb_entry("Rename", s(Action::Rename)),
        kb_entry("Chmod", s(Action::Chmod)),
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
pub(super) fn build_prompt_keybindings(kb: &KeyBindings) -> Vec<(String, String)> {
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
        kb_entry("Move cursor by word", "Ctrl+←/→".into()),
        kb_entry(
            "Move cursor to start, end",
            "Ctrl+a/Home, Ctrl+e/End".into(),
        ),
        kb_entry("Select text", "Shift+←/→".into()),
        kb_entry("Select to line start, end", "Shift+Home, Shift+End".into()),
        kb_entry("Select by word", "Ctrl+Shift+←/→".into()),
        kb_entry("Delete before, after cursor", "Backspace, Delete".into()),
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

    use super::annotate_uppercase;

    #[test_case("End",     "End"                 ; "multi-char key is unchanged")]
    #[test_case("G",       "G (Uppercase)"       ; "single uppercase letter is annotated")]
    #[test_case("g",       "g"                   ; "single lowercase letter is unchanged")]
    #[test_case("1",       "1"                   ; "single digit is not annotated")]
    #[test_case("",        ""                    ; "empty string is unchanged")]
    #[test_case("G/End",   "G (Uppercase)/End"   ; "annotates only the single-letter half")]
    #[test_case("g/G",     "g/G (Uppercase)"     ; "annotates the uppercase half of a pair")]
    fn annotate_uppercase_cases(input: &str, expected: &str) {
        assert_eq!(annotate_uppercase(input), expected);
    }
}
