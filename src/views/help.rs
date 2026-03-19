use std::rc::Rc;

use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::{Constraint, Direction, Layout, Position, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::{ScrollbarView, View, bordered};
use crate::{
    app::{AppState, config::theme::Theme},
    command::{Command, handler::CommandHandler, result::CommandResult},
    keybindings::{Action, KeyBindings},
};

const MIN_HEIGHT: u16 = 5;

/// Labels for normal mode keybindings. The order matches the help view display.
/// Convention: "/" separates alternatives for one action, ", " separates different actions.
const NORMAL_MODE_LABELS: [&str; 24] = [
    "Quit: ",
    "Go to parent dir: ",
    "Open: ",
    "Open custom: ",
    "Open new window: ",
    "Open terminal: ",
    "Go to home dir: ",
    "Select next, previous row: ",
    "Select first, last row: ",
    "Jump to middle row: ",
    "Page down, up: ",
    "Mark/unmark item: ",
    "Range mark: ",
    "Copy: ",
    "Cut: ",
    "Paste: ",
    "Delete: ",
    "Rename: ",
    "Filter: ",
    "Sort by name, modified, size: ",
    "Refresh: ",
    "Clear clipboard/filter/marks: ",
    "Clear alerts, progress: ",
    "Toggle help: ",
];

/// Labels for prompt mode keybindings that are rebindable.
const PROMPT_REBINDABLE_LABELS: [&str; 5] = [
    "Submit: ",
    "Cancel: ",
    "Reset to initial value: ",
    "Select all: ",
    "Copy, Cut, Paste text: ",
];

/// Labels for prompt mode keybindings that are hardcoded (cursor/selection keys).
const PROMPT_HARDCODED_KEYBINDINGS: [(&str, &str); 7] = [
    ("Move cursor: ", "←/→"),
    ("Move cursor by word: ", "Ctrl+←/→"),
    ("Jump to line start, end: ", "Ctrl+a/Home, Ctrl+e/End"),
    ("Select text: ", "Shift+←/→"),
    ("Select to line start, end: ", "Shift+Home, Shift+End"),
    ("Select by word: ", "Ctrl+Shift+←/→"),
    ("Delete before, after cursor: ", "Backspace, Delete"),
];

use std::sync::LazyLock;

static MAX_LABEL_WIDTH: LazyLock<usize> = LazyLock::new(|| {
    NORMAL_MODE_LABELS
        .iter()
        .chain(PROMPT_REBINDABLE_LABELS.iter())
        .chain(PROMPT_HARDCODED_KEYBINDINGS.iter().map(|(label, _)| label))
        .map(|label| label.width())
        .max()
        .unwrap_or(0)
});

pub(super) struct HelpView {
    area: Rect,
    /// Bordered header hint, cached at construction.
    hint: String,
    inner_height: u16,
    keybindings: Rc<KeyBindings>,
    max_scroll: u16,
    /// Keybinding display strings, cached at construction (keybindings never change).
    normal_keybindings: Vec<(String, String)>,
    prompt_keybindings: Vec<(String, String)>,
    scroll_offset: u16,
    scrollbar_view: ScrollbarView,
}

impl HelpView {
    pub fn new(keybindings: Rc<KeyBindings>) -> Self {
        let hint = format!(
            "(Press \"{}\" or Esc to close)",
            keybindings.display_for(Action::ToggleHelp)
        );
        let normal_keybindings = build_normal_keybindings(&keybindings);
        let prompt_keybindings = build_prompt_keybindings(&keybindings);
        Self {
            area: Rect::default(),
            hint,
            inner_height: 0,
            keybindings,
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
        // Hardcoded keys (arrow keys, Home/End, PageUp/PageDown)
        match (*code, *modifiers) {
            (KeyCode::Down, KeyModifiers::NONE) => {
                self.scroll_down(1);
                return CommandResult::Handled;
            }
            (KeyCode::Up, KeyModifiers::NONE) => {
                self.scroll_up(1);
                return CommandResult::Handled;
            }
            (KeyCode::PageDown, KeyModifiers::NONE) => {
                self.scroll_down(self.inner_height);
                return CommandResult::Handled;
            }
            (KeyCode::PageUp, KeyModifiers::NONE) => {
                self.scroll_up(self.inner_height);
                return CommandResult::Handled;
            }
            (KeyCode::Home, KeyModifiers::NONE) => {
                self.reset_scroll();
                return CommandResult::Handled;
            }
            (KeyCode::End, KeyModifiers::NONE) => {
                self.scroll_offset = self.max_scroll;
                return CommandResult::Handled;
            }
            _ => {}
        }
        // Rebindable keys (mirrors table navigation)
        match self.keybindings.normal_action(code, modifiers) {
            Some(Action::SelectNext) => {
                self.scroll_down(1);
                CommandResult::Handled
            }
            Some(Action::SelectPrevious) => {
                self.scroll_up(1);
                CommandResult::Handled
            }
            Some(Action::PageDown) => {
                self.scroll_down(self.inner_height);
                CommandResult::Handled
            }
            Some(Action::PageUp) => {
                self.scroll_up(self.inner_height);
                CommandResult::Handled
            }
            Some(Action::SelectFirst) => {
                self.reset_scroll();
                CommandResult::Handled
            }
            Some(Action::SelectLast) => {
                self.scroll_offset = self.max_scroll;
                CommandResult::Handled
            }
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

use crate::app::config::theme::Help;

fn add_section_header(lines: &mut Vec<Line<'_>>, title: &str, help: &Help) {
    let header_padding = " ".repeat(MAX_LABEL_WIDTH.saturating_sub(title.width()));
    lines.push(Line::from(vec![
        Span::styled(title.to_string(), help.header()),
        Span::raw(header_padding),
        Span::styled("Keybindings", help.header()),
    ]));
}

fn add_keybinding_lines<'a>(
    lines: &mut Vec<Line<'a>>,
    keybindings: &'a [(String, String)],
    help: &Help,
) {
    lines.extend(keybindings.iter().map(|(label, keys)| {
        let padding = " ".repeat(MAX_LABEL_WIDTH.saturating_sub(label.width()));
        Line::from(vec![
            Span::styled(label.as_str(), help.actions()),
            Span::raw(padding),
            Span::styled(keys.as_str(), help.shortcuts()),
        ])
    }));
}

/// Build normal mode keybinding display strings from KeyBindings.
fn build_normal_keybindings(kb: &KeyBindings) -> Vec<(String, String)> {
    let d = |a: Action| kb.display_for(a).to_string();

    let keys: Vec<String> = vec![
        d(Action::Quit),
        d(Action::Back),
        d(Action::Open),
        d(Action::OpenCustom),
        d(Action::OpenNewWindow),
        d(Action::OpenTerminal),
        d(Action::GoHome),
        format!("{}, {}", d(Action::SelectNext), d(Action::SelectPrevious)),
        format!("{}, {}", d(Action::SelectFirst), d(Action::SelectLast)),
        d(Action::SelectMiddle),
        format!("{}, {}", d(Action::PageDown), d(Action::PageUp)),
        d(Action::ToggleMark),
        d(Action::RangeMark),
        d(Action::Copy),
        d(Action::Cut),
        d(Action::Paste),
        d(Action::Delete),
        d(Action::Rename),
        d(Action::Filter),
        format!(
            "{}, {}, {}",
            d(Action::SortByName),
            d(Action::SortByModified),
            d(Action::SortBySize)
        ),
        d(Action::Refresh),
        d(Action::Reset),
        format!(
            "{}, {}",
            d(Action::ClearAlerts),
            d(Action::ClearProgress)
        ),
        d(Action::ToggleHelp),
    ];

    NORMAL_MODE_LABELS
        .iter()
        .zip(keys)
        .map(|(label, keys)| (label.to_string(), keys))
        .collect()
}

/// Build prompt mode keybinding display strings from KeyBindings.
fn build_prompt_keybindings(kb: &KeyBindings) -> Vec<(String, String)> {
    let d = |a: Action| kb.display_for(a).to_string();

    let rebindable_keys: Vec<String> = vec![
        d(Action::PromptSubmit),
        d(Action::PromptCancel),
        d(Action::PromptReset),
        d(Action::PromptSelectAll),
        format!(
            "{}, {}, {}",
            d(Action::PromptCopy),
            d(Action::PromptCut),
            d(Action::PromptPaste)
        ),
    ];

    let mut result: Vec<(String, String)> = PROMPT_REBINDABLE_LABELS
        .iter()
        .zip(rebindable_keys)
        .map(|(label, keys)| (label.to_string(), keys))
        .collect();

    result.extend(
        PROMPT_HARDCODED_KEYBINDINGS
            .iter()
            .map(|(label, keys)| (label.to_string(), keys.to_string())),
    );

    result
}

impl View for HelpView {
    fn constraint(&self, _: Rect, _: &AppState) -> Constraint {
        Constraint::Min(MIN_HEIGHT)
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, _state: &AppState, theme: &Theme) {
        self.area = area;
        if area.height < MIN_HEIGHT {
            return;
        }

        let style = theme.help.base();
        let bordered_area = bordered(area, frame.buffer_mut(), style, "Help", &self.hint);

        let mut lines: Vec<Line> = Vec::new();
        add_section_header(&mut lines, "Normal Mode", &theme.help);
        add_keybinding_lines(&mut lines, &self.normal_keybindings, &theme.help);
        lines.push(Line::raw(""));
        add_section_header(&mut lines, "Prompt Mode", &theme.help);
        add_keybinding_lines(&mut lines, &self.prompt_keybindings, &theme.help);

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
                theme,
                scroll as usize,
                self.max_scroll as usize,
                self.inner_height as usize,
            );
        } else {
            self.scrollbar_view
                .render(Rect::default(), frame.buffer_mut(), theme, 0, 0, 0);
            Paragraph::new(lines)
                .style(style)
                .render(bordered_area, frame.buffer_mut());
        }
    }
}
