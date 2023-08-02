use super::View;
use crate::{
    app::focus::Focus,
    command::{handler::CommandHandler, result::CommandResult, Command},
    views::Renderable,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    backend::Backend,
    layout::Rect,
    prelude::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

#[derive(Default)]
pub(super) struct PromptView {
    focus: PromptFocus,
    input: Input,
    label: String,
}

impl PromptView {
    pub fn setup(&mut self, value: String) {
        self.label = value;
        self.input.reset();
        self.focus = PromptFocus::Input;
    }

    fn next_focus(&mut self) {
        self.focus.next()
    }

    fn previous_focus(&mut self) {
        self.focus.previous()
    }

    fn cancel(&mut self) -> CommandResult {
        eprintln!("PromptView.cancel()");
        self.input.reset();
        Command::CancelPrompt.into()
    }

    fn submit(&mut self) -> CommandResult {
        let value = self.input.value().into();
        eprintln!("PromptView.submit():{value}");
        self.input.reset();
        Command::SubmitPrompt(value).into()
    }
}

impl<B: Backend> View<B> for PromptView {}

impl CommandHandler for PromptView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match *command {
            Command::Key(code, modifiers) => {
                return match (code, modifiers) {
                    (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        self.cancel()
                    }
                    (KeyCode::Tab, _) => {
                        self.next_focus();
                        CommandResult::none()
                    }
                    (KeyCode::BackTab, _) => {
                        self.previous_focus();
                        CommandResult::none()
                    }
                    (KeyCode::Enter, _) => self.submit(),
                    (_, _) => {
                        let key_event = KeyEvent::new(code, modifiers);
                        self.input.handle_event(&Event::Key(key_event));
                        CommandResult::none()
                    }
                };
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn is_focussed(&self, focus: &crate::app::focus::Focus) -> bool {
        *focus == Focus::Prompt
    }
}

impl<B: Backend> Renderable<B> for PromptView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints(
                [
                    Constraint::Length(1),
                    Constraint::Length(3),
                    Constraint::Min(1),
                ]
                .as_ref(),
            )
            .split(rect);

        let msg = vec![
            Span::raw("Press "),
            Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" to cancel, or "),
            Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" to submit"),
        ];
        let text = Text::from(Line::from(msg));
        let help_message = Paragraph::new(text).style(Style::default());
        frame.render_widget(help_message, chunks[0]);

        let width = chunks[0].width.max(3) - 3; // keep 2 for borders and 1 for cursor
        let scroll = self.input.visual_scroll(width as usize);
        let input = Paragraph::new(self.input.value())
            .style(Style::default().fg(Color::Yellow))
            .scroll((0, scroll as u16))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(self.label.as_ref()),
            );
        frame.render_widget(input, chunks[1]);

        // Make the cursor visible and ask tui-rs to put it at the specified coordinates after rendering
        frame.set_cursor(
            // Put cursor past the end of the input text
            chunks[1].x + ((self.input.visual_cursor()).max(scroll) - scroll) as u16 + 1,
            // Move one line down, from the border to the input line
            chunks[1].y + 1,
        );
    }
}

#[derive(Default)]
enum PromptFocus {
    CancelButton,
    #[default]
    Input,
    OkButton,
}

impl PromptFocus {
    pub fn next(&mut self) {
        match self {
            Self::CancelButton => *self = Self::Input,
            Self::Input => *self = Self::OkButton,
            Self::OkButton => *self = Self::CancelButton,
        }
    }

    pub fn previous(&mut self) {
        match self {
            Self::Input => *self = Self::CancelButton,
            Self::OkButton => *self = Self::Input,
            Self::CancelButton => *self = Self::OkButton,
        }
    }
}
