use ratatui::buffer::CellWidth;
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::{Constraint, Direction, Layout, Position, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::{ScrollbarView, View, bordered};
use crate::{
    app::config::Config,
    app::config::keybindings::{Action, hardcoded_action},
    command::{Command, handler::CommandHandler, result::CommandResult},
};

const MIN_HEIGHT: u16 = 5;

pub(super) struct HelpView {
    area: Rect,
    /// Bordered header hint, cached at construction.
    hint: String,
    inner_height: u16,
    max_scroll: u16,
    /// Keybinding display strings, cached at construction (keybindings never change).
    normal_keybindings: Vec<(String, String)>,
    prompt_keybindings: Vec<(String, String)>,
    scroll_offset: u16,
    scrollbar_view: ScrollbarView,
}

impl HelpView {
    pub fn new() -> Self {
        let kb = &Config::global().keybindings;
        let hint = format!(
            "(Press {} to close)",
            kb.hint_for(&[Action::ToggleHelp, Action::ResetView])
        );
        let normal_keybindings = build_normal_keybindings(kb);
        let prompt_keybindings = build_prompt_keybindings(kb);
        Self {
            area: Rect::default(),
            hint,
            inner_height: 0,
            max_scroll: 0,
            normal_keybindings,
            prompt_keybindings,
            scroll_offset: 0,
            scrollbar_view: ScrollbarView::default(),
        }
    }
}

impl HelpView {
    fn reset_scroll(&mut self) {
        self.scroll_offset = 0;
    }

    fn scroll_down(&mut self, lines: u16) {
        self.scroll_offset = self
            .scroll_offset
            .saturating_add(lines)
            .min(self.max_scroll);
    }

    fn scroll_up(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    fn handle_scroll_action(&mut self, action: Action) -> CommandResult {
        match action {
            Action::SelectNext => self.scroll_down(1),
            Action::SelectPrevious => self.scroll_up(1),
            Action::PageDown => self.scroll_down(self.inner_height),
            Action::PageUp => self.scroll_up(self.inner_height),
            Action::SelectFirst => self.reset_scroll(),
            Action::SelectLast => self.scroll_offset = self.max_scroll,
            _ => return CommandResult::NotHandled,
        }
        CommandResult::Handled
    }
}

impl CommandHandler for HelpView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::ResetHelpScroll => {
                self.reset_scroll();
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        let action = hardcoded_action(code, modifiers)
            .or_else(|| Config::global().keybindings.normal_action(code, modifiers));
        match action {
            Some(action) => self.handle_scroll_action(action),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::ScrollDown => {
                self.scroll_down(1);
                CommandResult::Handled
            }
            MouseEventKind::ScrollUp => {
                self.scroll_up(1);
                CommandResult::Handled
            }
            MouseEventKind::Down(MouseButton::Left)
            | MouseEventKind::Up(MouseButton::Left)
            | MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(pos) = self
                    .scrollbar_view
                    .handle_mouse(event, self.max_scroll as usize)
                {
                    self.scroll_offset = pos as u16;
                }
                CommandResult::Handled
            }
            _ => CommandResult::Handled,
        }
    }

    fn should_handle_mouse(&self, event: &MouseEvent) -> bool {
        matches!(
            event.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) || self.scrollbar_view.is_dragging()
            || self.area.contains(Position {
                x: event.column,
                y: event.row,
            })
    }
}

use crate::app::config::{keybindings::KeyBindings, theme::Help};

fn max_label_width(normal: &[(String, String)], prompt: &[(String, String)]) -> usize {
    normal
        .iter()
        .chain(prompt.iter())
        .map(|(label, _)| label.cell_width() as usize)
        .max()
        .unwrap_or(0)
}

fn add_section_header(lines: &mut Vec<Line<'_>>, title: &str, max_label_width: usize, help: &Help) {
    // Body rows insert ": " (2 cols) between label and keys; match that here.
    let header_padding =
        " ".repeat((max_label_width + 2).saturating_sub(title.cell_width() as usize));
    lines.push(Line::from(vec![
        Span::styled(title.to_string(), help.header()),
        Span::raw(header_padding),
        Span::styled("Keybindings", help.header()),
    ]));
}

fn add_keybinding_lines<'a>(
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

impl View for HelpView {
    fn constraint(&self, _: Rect) -> Constraint {
        Constraint::Min(MIN_HEIGHT)
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>) {
        self.area = area;
        if area.height < MIN_HEIGHT {
            return;
        }

        let theme = Config::global().theme();
        let style = theme.help.base();
        let bordered_area = bordered(area, frame.buffer_mut(), style, "Help", &self.hint);

        let max_width = max_label_width(&self.normal_keybindings, &self.prompt_keybindings);
        let mut lines: Vec<Line> = Vec::new();
        add_section_header(&mut lines, "Normal Mode", max_width, &theme.help);
        add_keybinding_lines(&mut lines, &self.normal_keybindings, max_width, &theme.help);
        lines.push(Line::raw(""));
        add_section_header(&mut lines, "Prompt Mode", max_width, &theme.help);
        add_keybinding_lines(&mut lines, &self.prompt_keybindings, max_width, &theme.help);

        let content_height = lines.len() as u16;
        self.inner_height = bordered_area.height;
        self.max_scroll = content_height.saturating_sub(self.inner_height);
        let scroll = self.scroll_offset.min(self.max_scroll);

        if self.max_scroll > 0 {
            let [content_area, scrollbar_area] = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .areas(bordered_area);

            Paragraph::new(lines)
                .style(style)
                .scroll((scroll, 0))
                .render(content_area, frame.buffer_mut());

            self.scrollbar_view.render(
                scrollbar_area,
                frame.buffer_mut(),
                scroll as usize,
                self.max_scroll as usize,
                self.inner_height as usize,
            );
        } else {
            self.scrollbar_view
                .render(Rect::default(), frame.buffer_mut(), 0, 0, 0);
            Paragraph::new(lines)
                .style(style)
                .render(bordered_area, frame.buffer_mut());
        }
    }
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
