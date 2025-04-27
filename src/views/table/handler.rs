use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    prelude::Position,
};

use super::{columns::SortColumn, TableView};
use crate::command::{handler::CommandHandler, result::CommandResult, Command};

impl CommandHandler for TableView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::ClearClipboard => self.clear_clipboard(),
            Command::Copy(_, _) | Command::Move(_, _) => Command::ClearClipboard.into(),
            Command::SetDirectory(directory, children) => {
                self.set_directory(directory.clone(), children.to_vec())
            }
            // self.handle_key() and PromptView may emit SetFilter()
            Command::SetFilter(filter) => self.set_filter(filter.to_string()),

            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.copy_to_clipboard(),
            (KeyCode::Char('x'), KeyModifiers::CONTROL) => self.cut_to_clipboard(),
            (KeyCode::Char('v'), KeyModifiers::CONTROL) => self.paste_from_clipboard(),
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
                KeyCode::Char('c') => self.clear_clipboard(),
                KeyCode::Char('r') | KeyCode::F(2) => self.open_rename_prompt(),
                KeyCode::Char('z') => self.move_to_middle(),
                KeyCode::Char('n') | KeyCode::Char('N') => self.sort_by(SortColumn::Name),
                KeyCode::Char('m') | KeyCode::Char('M') => self.sort_by(SortColumn::Modified),
                KeyCode::Char('s') | KeyCode::Char('S') => self.sort_by(SortColumn::Size),
                _ => CommandResult::NotHandled,
            },
            (_, _) => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        let x = event.column.saturating_sub(self.table_area.x);
        let y = event.row.saturating_sub(self.table_area.y);

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Check for scrollbar click first
                if self.scrollbar_view.is_clicked(event.column, event.row) {
                    return self.handle_scroll(event);
                }

                // Then handle table clicks
                if y == 0 {
                    return self.click_header(x);
                }
                return self.click_table(y);
            }
            MouseEventKind::Up(MouseButton::Left) => self.handle_scroll(event),
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.scrollbar_view.is_dragging() {
                    return self.handle_scroll(event);
                }
                CommandResult::Handled
            }
            MouseEventKind::ScrollUp => self.previous(),
            MouseEventKind::ScrollDown => self.next(),
            _ => CommandResult::Handled,
        }
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        self.table_area.contains(Position { x, y }) || self.scrollbar_view.is_clicked(x, y)
    }
}
