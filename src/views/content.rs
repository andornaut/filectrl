use super::{errors::ErrorsView, table::TableView, View};
use crate::{
    app::focus::Focus,
    command::{handler::CommandHandler, result::CommandResult, Command},
    file_system::path::HumanPath,
    views::prompt::PromptView,
};
use crossterm::event::KeyCode;
use ratatui::{
    backend::Backend,
    layout::{Constraint, Rect},
    prelude::{Direction, Layout},
    Frame,
};

#[derive(Default)]
pub(super) struct ContentView {
    errors: ErrorsView,
    mode: Mode,
    prompt: PromptView,
    table: TableView,
}

impl ContentView {
    fn cancel_prompt(&mut self) -> CommandResult {
        self.mode = Mode::Table;
        Command::Focus(Focus::Content).into()
    }

    fn prompt_rename(&mut self) -> CommandResult {
        match self.table.selected() {
            Some(selected_path) => {
                let label = format!("Rename \"{}\" to...", selected_path.basename);
                self.mode = Mode::PromptRename(selected_path.clone());
                self.prompt.setup(label);
                Command::Focus(Focus::Prompt).into()
            }
            None => CommandResult::none(),
        }
    }

    fn submit_prompt(&mut self, value: String) -> CommandResult {
        match self.mode.clone() {
            Mode::PromptRename(selected_path) => {
                self.mode = Mode::Table;
                Command::RenamePath(selected_path, value).into()
            }
            _ => panic!("Invalid ContentView.mode:{:?}", self.mode),
        }
    }
}

impl CommandHandler for ContentView {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        let errors: &mut dyn CommandHandler = &mut self.errors;
        let prompt: &mut dyn CommandHandler = &mut self.prompt;
        let table: &mut dyn CommandHandler = &mut self.table;
        vec![errors, prompt, table]
    }

    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Key(code, _) => match code {
                KeyCode::F(2) => self.prompt_rename(),
                _ => CommandResult::NotHandled,
            },
            Command::CancelPrompt => self.cancel_prompt(),
            Command::SubmitPrompt(value) => self.submit_prompt(value.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn is_focussed(&self, focus: &Focus) -> bool {
        *focus == Focus::Content
    }
}

impl<B: Backend> View<B> for ContentView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(self.errors.height()), Constraint::Min(0)].as_ref());
        let chunks = layout.split(rect);
        let errors_rect = chunks[0];
        let content_rect = chunks[1];

        self.errors.render(frame, errors_rect);

        match self.mode {
            Mode::PromptRename(_) => {
                self.prompt.render(frame, content_rect);
            }
            Mode::Table => {
                self.table.render(frame, content_rect);
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
enum Mode {
    PromptRename(HumanPath),
    #[default]
    Table,
}
