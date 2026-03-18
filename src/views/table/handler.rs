use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    prelude::Position,
};

use super::{columns::SortColumn, TableView};
use crate::command::{handler::CommandHandler, result::CommandResult, Command};

impl CommandHandler for TableView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Copy { .. } | Command::Move { .. } => Command::ClearClipboard.into(),
            Command::ToggleHelp => {
                self.is_visible = !self.is_visible;
                CommandResult::NotHandled
            }
            Command::NavigateDirectory(directory, children) => {
                self.set_directory(directory.clone(), children.to_vec(), false)
            }
            Command::RefreshDirectory(directory, children) => {
                self.set_directory(directory.clone(), children.to_vec(), true)
            }
            // self.handle_key() and PromptView may emit SetFilter()
            Command::SetFilter(filter) => self.set_filter(filter.clone()),

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
            (KeyCode::Char('G'), KeyModifiers::SHIFT) => self.select_last(),
            (KeyCode::Char('d'), KeyModifiers::CONTROL)
            | (KeyCode::Char('f'), KeyModifiers::CONTROL)
            | (KeyCode::PageDown, KeyModifiers::NONE) => self.next_page(),
            (_, KeyModifiers::NONE) => match code {
                KeyCode::Delete => self.delete(),
                KeyCode::Enter
                | KeyCode::Right
                | KeyCode::Char('f')
                | KeyCode::Char('l')
                | KeyCode::Char(' ') => self.open_selected(),
                KeyCode::Esc => Command::SetFilter("".into()).into(),
                KeyCode::Char('~') => self.navigate_to_home_directory(),
                KeyCode::Char('o') => self.open_selected_in_custom_program(),
                KeyCode::Down | KeyCode::Char('j') => self.select_next(),
                KeyCode::Up | KeyCode::Char('k') => self.select_previous(),
                KeyCode::Char('^') | KeyCode::Home | KeyCode::Char('g') => self.select_first(),
                KeyCode::Char('$') | KeyCode::End => self.select_last(),
                KeyCode::Char('/') => self.open_filter_prompt(),
                KeyCode::Char('c') => Command::ClearClipboard.into(),
                KeyCode::Char('r') | KeyCode::F(2) => self.open_rename_prompt(),
                KeyCode::Char('z') => self.select_middle_visible_item(),
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
                self.click_table(y)
            }
            MouseEventKind::Up(MouseButton::Left) => self.handle_scroll(event),
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.scrollbar_view.is_dragging() {
                    return self.handle_scroll(event);
                }
                CommandResult::Handled
            }
            MouseEventKind::ScrollUp => self.select_previous(),
            MouseEventKind::ScrollDown => self.select_next(),
            _ => CommandResult::Handled,
        }
    }

    fn should_handle_mouse(&self, event: &MouseEvent) -> bool {
        let is_scroll = matches!(event.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown);
        (is_scroll && self.is_visible)
            || self.table_area.contains(Position { x: event.column, y: event.row })
            || self.scrollbar_view.is_clicked(event.column, event.row)
    }
}
