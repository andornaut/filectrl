use super::{len_utf8, View};
use crate::{
    app::theme::Theme,
    command::{
        handler::CommandHandler, mode::InputMode, result::CommandResult, Command, PromptKind,
    },
    file_system::human::HumanPath,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    backend::Backend,
    layout::Rect,
    prelude::{Constraint, Direction, Layout},
    widgets::Paragraph,
    Frame,
};
use tui_input::{backend::crossterm::EventHandler, Input};

#[derive(Default)]
pub(super) struct PromptView {
    filter: String,
    input: Input,
    kind: PromptKind,
    selected: Option<HumanPath>,
}

impl PromptView {
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

    fn set_selected(&mut self, selected: Option<HumanPath>) -> CommandResult {
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

    fn handle_input(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match *code {
            KeyCode::Esc => Command::ClosePrompt.into(),
            KeyCode::Enter => self.submit(),
            _ => self.handle_key(*code, *modifiers),
        }
    }

    fn should_receive_input(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Prompt)
    }
}

impl<B: Backend> View<B> for PromptView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, mode: &InputMode, theme: &Theme) {
        if !self.should_show(mode) {
            return;
        }

        let label = self.label();
        let label_width = len_utf8(&label);
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(label_width as u16), Constraint::Min(1)].as_ref())
            .split(rect);

        let input_width = chunks[1].width;
        let cursor_x_pos = self.input.visual_cursor() as u16;
        let x_offset = self.input.visual_scroll(input_width as usize) as u16;
        let x_offset_scroll = if cursor_x_pos >= input_width {
            // Workaround: when there's scrolling, the cursor would otherwise
            // be positioned on the last char instead of after the last char
            // as is the case when there is no scrolling.
            x_offset + 1
        } else {
            x_offset
        };

        let input_widget = Paragraph::new(self.input.value())
            .scroll((0, x_offset_scroll))
            .style(theme.prompt_input());
        let label_widget = Paragraph::new(label).style(theme.prompt_label());
        frame.render_widget(label_widget, chunks[0]);
        frame.render_widget(input_widget, chunks[1]);
        frame.set_cursor(chunks[1].x + cursor_x_pos - x_offset, chunks[1].y);
    }
}
