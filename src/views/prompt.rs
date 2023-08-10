use super::{len_utf8, View};
use crate::{
    app::{
        focus::Focus,
        style::{prompt_input_style, prompt_label_style},
    },
    command::{handler::CommandHandler, result::CommandResult, Command, PromptKind},
    file_system::human::HumanPath,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    backend::Backend,
    layout::Rect,
    prelude::{Constraint, Direction, Layout},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};
use tui_input::{backend::crossterm::EventHandler, Input};

#[derive(Default)]
pub(super) struct PromptView {
    input: Input,
    selected: Option<HumanPath>,
    kind: PromptKind,
}

impl PromptView {
    fn cancel(&mut self) -> CommandResult {
        Command::Focus(Focus::Content).into()
    }

    fn handle_input(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        let key_event = KeyEvent::new(code, modifiers);
        self.input.handle_event(&Event::Key(key_event));
        CommandResult::none()
    }

    fn label(&self) -> String {
        match self.kind {
            PromptKind::Filter => "Filter: ".to_string(),
            PromptKind::Rename => "Rename: ".to_string(),
        }
    }

    fn open(&mut self, kind: &PromptKind) -> CommandResult {
        match &self.selected {
            Some(selected) => {
                self.kind = kind.clone();
                self.input = Input::new(selected.basename.clone());
                Command::Focus(Focus::Prompt).into()
            }
            None => CommandResult::none(),
        }
    }

    fn set_selected(&mut self, selected: Option<HumanPath>) -> CommandResult {
        self.selected = selected;
        self.input.reset();
        CommandResult::none()
    }

    fn submit(&mut self) -> CommandResult {
        let value = self.input.value();
        match self.kind {
            PromptKind::Filter => todo!(),
            PromptKind::Rename => match &self.selected {
                Some(selected_path) => Command::RenamePath(selected_path.clone(), value.into()),
                None => Command::Focus(Focus::Content),
            },
        }
        .into()
    }
}

impl CommandHandler for PromptView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Key(code, modifiers) => {
                return match (*code, *modifiers) {
                    (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        self.cancel()
                    }
                    (KeyCode::Enter, _) => self.submit(),
                    (_, _) => self.handle_input(*code, *modifiers),
                };
            }
            Command::OpenPrompt(kind) => self.open(kind),
            Command::SetSelected(selected) => self.set_selected(selected.clone()),
            // Workaround for being unable to return 2 commands from this method:
            // self.submit() returns Command::RenamePath, and then this listens
            // for the same and returns Command::Focus
            Command::RenamePath(_, _) => Command::Focus(Focus::Content).into(),
            _ => CommandResult::NotHandled,
        }
    }

    fn is_focussed(&self, focus: &crate::app::focus::Focus) -> bool {
        *focus == Focus::Prompt
    }
}

impl<B: Backend> View<B> for PromptView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _: &Focus) {
        let label = self.label();
        let label_width = len_utf8(&label);
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(label_width as u16), Constraint::Min(1)].as_ref())
            .split(rect);

        let input_width = chunks[1].width;
        let x_offset = self.input.visual_scroll(input_width as usize) as u16;
        let x_offset_scroll = if self.input.visual_cursor() as u16 >= input_width {
            // Workaround: when there's scrolling, the cursor would otherwise
            // be positioned on the last char instead of after the last char
            // as is the case when there is no scrolling.
            x_offset + 1
        } else {
            x_offset
        };

        let input_widget = Paragraph::new(self.input.value())
            .scroll((0, x_offset_scroll))
            .style(prompt_input_style());

        let span = Span::from(label);
        let line = Line::from(span);
        let text = Text::from(line);
        let label_widget = Paragraph::new(text).style(prompt_label_style());

        frame.render_widget(label_widget, chunks[0]);
        frame.render_widget(input_widget, chunks[1]);

        frame.set_cursor(
            chunks[1].x + self.input.visual_cursor() as u16 - x_offset,
            chunks[1].y,
        );
    }
}
