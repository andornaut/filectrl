mod navigate;
mod render;
mod sort;
mod style;

use self::{
    navigate::navigate,
    render::{constraints, header, row},
    sort::{SortColumn, SortDirection},
};
use super::View;
use crate::{
    app::theme::Theme,
    command::{
        handler::CommandHandler, mode::InputMode, result::CommandResult, Command, PromptKind,
    },
    file_system::human::HumanPath,
};
use crossterm::event::{KeyCode, KeyModifiers, MouseEvent};
use ratatui::{
    backend::Backend,
    layout::Rect,
    widgets::{Table, TableState},
    Frame,
};

#[derive(Default)]
pub(super) struct TableView {
    directory_items: Vec<HumanPath>,
    directory: HumanPath,
    directory_items_sorted: Vec<HumanPath>,
    filter: String,
    last_rendered_rect: Rect,
    sort_column: SortColumn,
    sort_direction: SortDirection,
    state: TableState,
}

impl TableView {
    fn delete(&self) -> CommandResult {
        match self.selected() {
            Some(path) => Command::DeletePath(path.clone()).into(),
            None => CommandResult::none(),
        }
    }

    fn navigate(&mut self, delta: i8) -> CommandResult {
        if self.directory_items_sorted.is_empty() {
            return CommandResult::none();
        }
        let len = self.directory_items_sorted.len();
        let i = self.state.selected().map_or(0, |i| navigate(len, i, delta));
        self.state.select(Some(i));
        Command::SetSelected(Some(self.selected().unwrap().clone())).into()
    }

    fn next(&mut self) -> CommandResult {
        self.navigate(1)
    }

    fn previous(&mut self) -> CommandResult {
        self.navigate(-1)
    }

    fn open_filter_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptKind::Filter).into()
    }

    fn open_rename_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptKind::Rename).into()
    }

    fn open_selected(&mut self) -> CommandResult {
        match self.selected() {
            Some(path) => Command::Open(path.clone()).into(),
            None => CommandResult::none(),
        }
    }

    fn selected(&self) -> Option<&HumanPath> {
        self.state
            .selected()
            .map(|i| &self.directory_items_sorted[i])
    }

    fn set_directory(&mut self, directory: HumanPath, children: Vec<HumanPath>) -> CommandResult {
        self.directory = directory;
        self.directory_items = children;
        self.sort()
    }

    fn set_filter(&mut self, filter: String) -> CommandResult {
        // Avoid performing an extra SetFilter(None)
        // set_directory() -> sort() -> SetFilter(None) -> set_filter() -> sort() -> SetFilter(None)
        if self.filter.is_empty() && filter.is_empty() {
            return CommandResult::none();
        }
        self.filter = filter;
        self.sort()
    }

    fn sort(&mut self) -> CommandResult {
        let mut items = self.directory_items.clone();
        match self.sort_column {
            SortColumn::Name => items.sort_by_cached_key(|path| path.name_comparator()),
            SortColumn::Modified => items.sort_by_cached_key(|path| path.modified),
            SortColumn::Size => items.sort_by_cached_key(|path| path.size),
        };
        if self.sort_direction == SortDirection::Descending {
            items.reverse();
        }

        if !self.filter.is_empty() {
            let filter_lowercase = self.filter.to_ascii_lowercase();
            items = items
                .into_iter()
                .filter(|path| path.name().to_ascii_lowercase().contains(&filter_lowercase))
                .collect();
        }
        self.directory_items_sorted = items;
        self.unselect()
    }

    fn sort_by(&mut self, column: SortColumn) -> CommandResult {
        if self.sort_column == column {
            self.sort_direction.toggle();
        } else {
            self.sort_column = column;
        }
        self.sort()
    }

    fn unselect(&mut self) -> CommandResult {
        self.state.select(None);
        Command::SetSelected(None).into()
    }
}

impl CommandHandler for TableView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::SetDirectory(directory, children) => {
                self.set_directory(directory.clone(), children.clone())
            }
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.set_filter("".into()).into()
            }
            (_, _) => match code {
                KeyCode::Delete => self.delete(),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('f') | KeyCode::Char('l') => {
                    self.open_selected()
                }
                KeyCode::Down | KeyCode::Char('j') => self.next(),
                KeyCode::Up | KeyCode::Char('k') => self.previous(),
                KeyCode::Char('/') => self.open_filter_prompt(),
                KeyCode::Char('r') | KeyCode::F(2) => self.open_rename_prompt(),
                KeyCode::Char('n') | KeyCode::Char('N') => self.sort_by(SortColumn::Name),
                KeyCode::Char('m') | KeyCode::Char('M') => self.sort_by(SortColumn::Modified),
                KeyCode::Char('s') | KeyCode::Char('S') => self.sort_by(SortColumn::Size),
                KeyCode::Char(' ') => self.unselect(),
                _ => CommandResult::NotHandled,
            },
        }
    }

    fn handle_mouse(&mut self, _mouse: &MouseEvent) -> CommandResult {
        // Only invoked if self.should_receive_mouse() is true
        CommandResult::NotHandled
    }

    fn should_receive_mouse(&self, _column: u16, _row: u16) -> bool {
        false
    }
}

impl<B: Backend> View<B> for TableView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _: &InputMode, theme: &Theme) {
        self.last_rendered_rect = rect;

        let (constraints, name_width) = constraints(self.last_rendered_rect.width);
        let header = header(theme, &self.sort_column, &self.sort_direction);
        let rows = self
            .directory_items_sorted
            .iter()
            .map(|item| row(item, name_width, theme));
        let table = Table::new(rows)
            .header(header)
            .highlight_style(theme.table_selected())
            .widths(&constraints);
        frame.render_stateful_widget(table, self.last_rendered_rect, &mut self.state);
    }
}
