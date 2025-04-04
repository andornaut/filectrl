use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Paragraph, Widget},
};
use tui_input::{backend::crossterm::EventHandler, Input};
use unicode_width::UnicodeWidthStr;

use super::View;
use crate::{
    app::config::theme::Theme,
    command::{
        handler::CommandHandler, mode::InputMode, result::CommandResult, Command, PromptKind,
    },
    file_system::path_info::PathInfo,
};

#[derive(Default)]
pub(super) struct CursorPosition {
    x: u16,
    y: u16,
}

#[derive(Default)]
pub(super) struct PromptView {
    cursor_position: CursorPosition,
    filter: String,
    input: Input,
    kind: PromptKind,
    selected: Option<PathInfo>,
}

impl PromptView {
    pub(super) fn cursor_position(&self, mode: &InputMode) -> Option<(u16, u16)> {
        if self.should_show(mode) {
            Some((self.cursor_position.x, self.cursor_position.y))
        } else {
            None
        }
    }

    pub(super) fn height(&self, mode: &InputMode) -> u16 {
        if self.should_show(mode) {
            1
        } else {
            0
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        let key_event = KeyEvent::new(code, modifiers);
        self.input.handle_event(&Event::Key(key_event));
        CommandResult::none()
    }

    fn label(&self) -> String {
        match self.kind {
            PromptKind::Filter => " Filter ".into(),
            PromptKind::Rename => " Rename ".into(),
        }
    }

    fn open(&mut self, kind: &PromptKind) -> CommandResult {
        self.kind = kind.clone();

        match &self.kind {
            PromptKind::Filter => self.input = Input::new(self.filter.clone()),
            PromptKind::Rename => match &self.selected {
                Some(selected) => self.input = Input::new(selected.basename.clone()),
                None => (),
            },
        }
        CommandResult::none()
    }

    fn set_filter(&mut self, filter: String) -> CommandResult {
        self.filter = filter;
        CommandResult::none()
    }

    fn set_selected(&mut self, selected: Option<PathInfo>) -> CommandResult {
        self.selected = selected;
        CommandResult::none()
    }

    fn should_show(&self, mode: &InputMode) -> bool {
        *mode == InputMode::Prompt
    }

    fn submit(&mut self) -> CommandResult {
        let value = self.input.value().to_string();
        match self.kind {
            PromptKind::Filter => Command::SetFilter(value).into(),
            PromptKind::Rename => match &self.selected {
                Some(selected_path) => Command::RenamePath(selected_path.clone(), value).into(),
                None => CommandResult::none(),
            },
        }
    }
}

impl CommandHandler for PromptView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::OpenPrompt(kind) => self.open(kind),
            Command::SetDirectory(_, _) => Command::SetFilter("".into()).into(),
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            Command::SetSelected(selected) => self.set_selected(selected.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match *code {
            KeyCode::Esc => Command::ClosePrompt.into(),
            KeyCode::Enter => self.submit(),
            _ => self.handle_key(*code, *modifiers),
        }
    }

    fn should_receive_key(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Prompt)
    }
}

impl View for PromptView {
    fn render(&mut self, buf: &mut Buffer, area: Rect, mode: &InputMode, theme: &Theme) {
        if !self.should_show(mode) {
            return;
        }

        let label = self.label();
        let label_width = label.width_cjk() as u16;
        let areas = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(label_width), Constraint::Min(1)].as_ref())
            .split(area);
        let prompt_area = areas[0];
        let input_area = areas[1];

        let (cursor_x_pos, cursor_x_scroll) = cursor_position(&self.input, input_area);

        let prompt_widget = prompt_widget(theme, label);
        prompt_widget.render(prompt_area, buf);

        let input_widget = input_widget(&self.input, theme, cursor_x_scroll);
        input_widget.render(input_area, buf);

        self.cursor_position.x = input_area.x + (cursor_x_pos - cursor_x_scroll) as u16;
        self.cursor_position.y = input_area.y;
    }
}

fn cursor_position(input: &Input, input_area: Rect) -> (usize, usize) {
    let input_width = input_area.width as usize;
    let cursor_x_pos = input.visual_cursor();
    let cursor_x_scroll = input.visual_scroll(input_width);
    let cursor_x_scroll = if cursor_x_pos >= input_width {
        // When there's horizontal scrolling in the input field, the cursor
        // would otherwise be positioned on the last char instead of after
        // the last char as is the case when there is no scrolling.
        cursor_x_scroll + 1
    } else {
        cursor_x_scroll
    };
    (cursor_x_pos, cursor_x_scroll)
}

fn prompt_widget<'a>(theme: &'a Theme, label: String) -> Paragraph<'a> {
    Paragraph::new(label).style(theme.prompt_label())
}

fn input_widget<'a>(input: &'a Input, theme: &'a Theme, x_offset_scroll: usize) -> Paragraph<'a> {
    Paragraph::new(input.value())
        .scroll((0, x_offset_scroll as u16))
        .style(theme.prompt_input())
}
