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
    app::config::keybindings::{Action, KeyBindings},
};

const MIN_HEIGHT: u16 = 5;

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

fn max_label_width(
    normal: &[(String, String)],
    prompt: &[(String, String)],
) -> usize {
    normal
        .iter()
        .chain(prompt.iter())
        .map(|(label, _)| label.width())
        .max()
        .unwrap_or(0)
}

fn add_section_header(lines: &mut Vec<Line<'_>>, title: &str, max_label_width: usize, help: &Help) {
    let header_padding = " ".repeat(max_label_width.saturating_sub(title.width()));
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
        let padding = " ".repeat(max_label_width.saturating_sub(label.width()));
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

    vec![
        ("Quit: ".into(), d(Action::Quit)),
        ("Go to parent dir: ".into(), d(Action::Back)),
        ("Open: ".into(), d(Action::Open)),
        ("Open custom: ".into(), d(Action::OpenCustom)),
        ("Open new window: ".into(), d(Action::OpenNewWindow)),
        ("Open terminal: ".into(), d(Action::OpenTerminal)),
        ("Go to home dir: ".into(), d(Action::GoHome)),
        ("Select next, previous row: ".into(), format!("{}, {}", d(Action::SelectNext), d(Action::SelectPrevious))),
        ("Select first, last row: ".into(), format!("{}, {}", d(Action::SelectFirst), d(Action::SelectLast))),
        ("Jump to middle row: ".into(), d(Action::SelectMiddle)),
        ("Page down, up: ".into(), format!("{}, {}", d(Action::PageDown), d(Action::PageUp))),
        ("Mark/unmark item: ".into(), d(Action::ToggleMark)),
        ("Range mark: ".into(), d(Action::RangeMark)),
        ("Copy: ".into(), d(Action::Copy)),
        ("Cut: ".into(), d(Action::Cut)),
        ("Paste: ".into(), d(Action::Paste)),
        ("Delete: ".into(), d(Action::Delete)),
        ("Rename: ".into(), d(Action::Rename)),
        ("Filter: ".into(), d(Action::Filter)),
        ("Sort by name, modified, size: ".into(), format!("{}, {}, {}", d(Action::SortByName), d(Action::SortByModified), d(Action::SortBySize))),
        ("Refresh: ".into(), d(Action::Refresh)),
        ("Clear clipboard/filter/marks: ".into(), d(Action::Reset)),
        ("Clear alerts, progress: ".into(), format!("{}, {}", d(Action::ClearAlerts), d(Action::ClearProgress))),
        ("Toggle help: ".into(), d(Action::ToggleHelp)),
    ]
}

/// Build prompt mode keybinding display strings from KeyBindings.
fn build_prompt_keybindings(kb: &KeyBindings) -> Vec<(String, String)> {
    let d = |a: Action| kb.display_for(a).to_string();

    vec![
        ("Submit: ".into(), d(Action::PromptSubmit)),
        ("Cancel: ".into(), d(Action::PromptCancel)),
        ("Reset to initial value: ".into(), d(Action::PromptReset)),
        ("Select all: ".into(), d(Action::PromptSelectAll)),
        ("Copy, Cut, Paste text: ".into(), format!("{}, {}, {}", d(Action::PromptCopy), d(Action::PromptCut), d(Action::PromptPaste))),
        ("Move cursor: ".into(), "←/→".into()),
        ("Move cursor by word: ".into(), "Ctrl+←/→".into()),
        ("Jump to line start, end: ".into(), "Ctrl+a/Home, Ctrl+e/End".into()),
        ("Select text: ".into(), "Shift+←/→".into()),
        ("Select to line start, end: ".into(), "Shift+Home, Shift+End".into()),
        ("Select by word: ".into(), "Ctrl+Shift+←/→".into()),
        ("Delete before, after cursor: ".into(), "Backspace, Delete".into()),
    ]
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
