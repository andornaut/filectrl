use super::View;
use crate::{
    app::focus::Focus,
    command::{handler::CommandHandler, result::CommandResult, Command},
    views::Renderable,
};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{backend::Backend, layout::Rect, widgets::Block, Frame};

#[derive(Default)]
pub(super) struct PromptView {
    focus: PromptFocus,
    input: String,
    label: String,
}

impl PromptView {
    pub fn new(label: String, default_input: Option<String>) -> Self {
        Self {
            label,
            input: default_input.unwrap_or(String::from("")),
            ..Self::default()
        }
    }

    fn next_focus(&mut self) {
        self.focus.next()
    }

    fn previous_focus(&mut self) {
        self.focus.previous()
    }
}

impl<B: Backend> View<B> for PromptView {}

impl CommandHandler for PromptView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match *command {
            Command::Key(code, modifiers) => {
                return match (code, modifiers) {
                    (KeyCode::Esc, _)
                    | (KeyCode::Char('c'), KeyModifiers::CONTROL)
                    | (KeyCode::Char('q'), _)
                    | (KeyCode::Char('Q'), _) => CommandResult::some(Command::Quit),
                    (KeyCode::Tab, _) => {
                        self.next_focus();
                        CommandResult::none()
                    }
                    (KeyCode::BackTab, _) => {
                        self.previous_focus();
                        CommandResult::none()
                    }
                    (KeyCode::Backspace, _) | (KeyCode::Left, _) | (KeyCode::Char('h'), _) => {
                        CommandResult::some(Command::BackDir)
                    }
                    (code, _) => {
                        todo!();
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
        let block = Block::default().title("Prompt");
        frame.render_widget(block, rect);
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
