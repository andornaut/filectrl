use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    prelude::Position,
};

use super::{columns::SortColumn, TableView};
use crate::{
    command::{handler::CommandHandler, result::CommandResult, Command},
    keybindings::Action,
};

impl CommandHandler for TableView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Copy { .. } | Command::Move { .. } => {
                self.clear_marks();
                Command::ClearClipboard.into()
            }
            Command::Delete(_) => {
                self.clear_marks();
                CommandResult::Handled
            }
            Command::Reset => {
                self.clear_marks();
                self.set_filter(String::new());
                CommandResult::Handled
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
        // Hardcoded keys (arrow keys, Home/End, PageUp/PageDown)
        match (*code, *modifiers) {
            (KeyCode::Down, KeyModifiers::NONE) => return self.select_next(),
            (KeyCode::Up, KeyModifiers::NONE) => return self.select_previous(),
            (KeyCode::Left, KeyModifiers::NONE) => return Command::Back.into(),
            (KeyCode::Right, KeyModifiers::NONE) => return self.open_selected(),
            (KeyCode::Home, KeyModifiers::NONE) => return self.select_first(),
            (KeyCode::End, KeyModifiers::NONE) => return self.select_last(),
            (KeyCode::PageUp, KeyModifiers::NONE) => return self.previous_page(),
            (KeyCode::PageDown, KeyModifiers::NONE) => return self.next_page(),
            _ => {}
        }
        // Rebindable keys
        match self.keybindings.normal_action(code, modifiers) {
            // Clipboard
            Some(Action::Copy) => self.copy_to_clipboard(),
            Some(Action::Cut) => self.cut_to_clipboard(),
            Some(Action::Paste) => self.paste_from_clipboard(),
            // Navigation (page)
            Some(Action::PageUp) => self.previous_page(),
            Some(Action::PageDown) => self.next_page(),
            // Navigation (filesystem)
            Some(Action::Refresh) => Command::Refresh.into(),
            Some(Action::Back) => Command::Back.into(),
            Some(Action::Open) => self.open_selected(),
            Some(Action::OpenCustom) => self.open_selected_in_custom_program(),
            Some(Action::OpenNewWindow) => Command::OpenNewWindow.into(),
            Some(Action::OpenTerminal) => Command::OpenTerminal.into(),
            Some(Action::GoHome) => self.navigate_to_home_directory(),
            // Selection
            Some(Action::SelectNext) => self.select_next(),
            Some(Action::SelectPrevious) => self.select_previous(),
            Some(Action::SelectFirst) => self.select_first(),
            Some(Action::SelectLast) => self.select_last(),
            Some(Action::SelectMiddle) => self.select_middle_visible_item(),
            // Marks
            Some(Action::ToggleMark) => self.toggle_mark(),
            Some(Action::RangeMark) => self.enter_range_mode(),
            // File operations
            Some(Action::Delete) => self.delete(),
            Some(Action::Rename) => self.open_rename_prompt(),
            Some(Action::Filter) => self.open_filter_prompt(),
            // Sort
            Some(Action::SortByName) => self.sort_by(SortColumn::Name),
            Some(Action::SortByModified) => self.sort_by(SortColumn::Modified),
            Some(Action::SortBySize) => self.sort_by(SortColumn::Size),
            _ => CommandResult::NotHandled,
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
        is_scroll
            || self.table_area.contains(Position { x: event.column, y: event.row })
            || self.scrollbar_view.is_clicked(event.column, event.row)
    }
}
