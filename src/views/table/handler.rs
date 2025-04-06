use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::prelude::Rect;

use super::{columns::SortColumn, TableView};
use crate::command::{handler::CommandHandler, result::CommandResult, Command};

impl CommandHandler for TableView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::SetDirectory(directory, children) => {
                self.set_directory(directory.clone(), children.clone())
            }
            // self.handle_key() and PromptView may emit SetFilter()
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.copy(),
            (KeyCode::Char('x'), KeyModifiers::CONTROL) => self.cut(),
            (KeyCode::Char('v'), KeyModifiers::CONTROL) => self.paste(),
            (KeyCode::Char('u'), KeyModifiers::CONTROL)
            | (KeyCode::Char('b'), KeyModifiers::CONTROL)
            | (KeyCode::PageUp, KeyModifiers::NONE) => self.previous_page(),
            (KeyCode::Char('G'), KeyModifiers::SHIFT) => self.last(),
            (KeyCode::Char('d'), KeyModifiers::CONTROL)
            | (KeyCode::Char('f'), KeyModifiers::CONTROL)
            | (KeyCode::PageDown, KeyModifiers::NONE) => self.next_page(),
            (_, KeyModifiers::NONE) => match code {
                KeyCode::Delete => self.delete(),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('f') | KeyCode::Char('l') => {
                    self.open_selected()
                }
                KeyCode::Esc => Command::SetFilter("".into()).into(),
                KeyCode::Char('o') => self.open_selected_in_custom_program(),
                KeyCode::Down | KeyCode::Char('j') => self.next(),
                KeyCode::Up | KeyCode::Char('k') => self.previous(),
                KeyCode::Char('^') | KeyCode::Home | KeyCode::Char('g') => self.first(),
                KeyCode::Char('$') | KeyCode::End => self.last(),
                KeyCode::Char('/') => self.open_filter_prompt(),
                KeyCode::Char('r') | KeyCode::F(2) => self.open_rename_prompt(),
                KeyCode::Char('n') | KeyCode::Char('N') => self.sort_by(SortColumn::Name),
                KeyCode::Char('m') | KeyCode::Char('M') => self.sort_by(SortColumn::Modified),
                KeyCode::Char('s') | KeyCode::Char('S') => self.sort_by(SortColumn::Size),
                KeyCode::Char(' ') => self.reset_selection(),
                _ => CommandResult::NotHandled,
            },
            (_, _) => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let x = event.column.saturating_sub(self.table_area.x);
                let y = event.row.saturating_sub(self.table_area.y);
                if y == 0 {
                    self.click_header(x)
                } else {
                    self.click_table(y)
                }
            }
            MouseEventKind::ScrollUp => self.previous(),
            MouseEventKind::ScrollDown => self.next(),
            _ => CommandResult::none(),
        }
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        self.table_area.intersects(Rect::new(x, y, 1, 1))
    }
}
