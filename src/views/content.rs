use super::{errors::ErrorsView, View};
use crate::{
    app::focus::Focus,
    command::{handler::CommandHandler, result::CommandResult, Command},
    file_system::path::HumanPath,
    views::{prompt::PromptView, Renderable},
};
use crossterm::event::KeyCode;
use ratatui::{
    backend::Backend,
    layout::{Constraint, Rect},
    prelude::{Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Frame,
};

#[derive(Default)]
pub(super) struct ContentView {
    errors: ErrorsView,
    directory_contents: Vec<HumanPath>,
    directory: HumanPath,
    mode: Mode,
    prompt: PromptView,
    state: TableState,
}

impl ContentView {
    fn open(&mut self) -> CommandResult {
        match self.selected() {
            Some(path) => {
                let path = path.clone();
                // TODO: handle symlinks
                (if path.is_dir {
                    Command::ChangeDir(path)
                } else {
                    Command::OpenFile(path)
                })
                .into()
            }
            None => CommandResult::none(),
        }
    }

    fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.directory_contents.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.directory_contents.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn selected(&self) -> Option<&HumanPath> {
        match self.state.selected() {
            Some(i) => Some(&self.directory_contents[i]),
            None => None,
        }
    }

    fn cancel_prompt(&mut self) -> CommandResult {
        self.mode = Mode::Table;
        Command::Focus(Focus::Content).into()
    }

    fn prompt_rename(&mut self) -> CommandResult {
        match self.selected() {
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
                Command::RenameDir(selected_path, value).into()
            }
            _ => panic!("Invalid ContentView.mode:{:?}", self.mode),
        }
    }
}

impl CommandHandler for ContentView {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        let errors: &mut dyn CommandHandler = &mut self.errors;
        let prompt: &mut dyn CommandHandler = &mut self.prompt;
        vec![errors, prompt]
    }

    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Key(code, _) => match code {
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open(),
                KeyCode::Up | KeyCode::Char('k') => {
                    self.previous();
                    CommandResult::none()
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.next();
                    CommandResult::none()
                }
                KeyCode::F(2) => self.prompt_rename(),
                _ => CommandResult::NotHandled,
            },
            Command::CancelPrompt => self.cancel_prompt(),
            Command::SubmitPrompt(value) => self.submit_prompt(value.clone()),
            Command::UpdateCurrentDir(directory, children) => {
                self.directory = directory.clone();
                self.directory_contents = children.clone();
                self.state.select(None);
                CommandResult::none()
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn is_focussed(&self, focus: &Focus) -> bool {
        *focus == Focus::Content
    }
}

impl<B: Backend> View<B> for ContentView {}

impl<B: Backend> Renderable<B> for ContentView {
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
                let table = create_table(&self.directory_contents);
                frame.render_stateful_widget(table, content_rect, &mut self.state);
            }
        }
    }
}

fn create_table(children: &[HumanPath]) -> Table {
    let selected_style = Style::default().add_modifier(Modifier::REVERSED);
    let normal_style = Style::default().bg(Color::Blue);
    let header_cells = ["Name", "Mode", "Size", "Modified"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::Red)));
    let header = Row::new(header_cells)
        .style(normal_style)
        .height(1)
        .bottom_margin(1);
    let rows = children.iter().map(|item| {
        let height = 1;
        let cells = vec![
            Cell::from(item.basename.clone()),
            Cell::from(item.mode.to_string()),
            Cell::from(item.human_size()),
            Cell::from(item.human_modified()),
        ];
        Row::new(cells).height(height as u16).bottom_margin(1)
    });
    Table::new(rows)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Table"))
        .highlight_style(selected_style)
        .widths(&[
            Constraint::Percentage(55),
            Constraint::Length(5),
            Constraint::Length(10),
            Constraint::Min(35),
        ])
}

#[derive(Clone, Debug, Default)]
enum Mode {
    PromptRename(HumanPath),
    #[default]
    Table,
}
