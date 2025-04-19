use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    prelude::Rect,
};

use super::{NoticeKind, NoticesView};
use crate::{
    clipboard::ClipboardCommand,
    command::{handler::CommandHandler, result::CommandResult, Command},
};

impl CommandHandler for NoticesView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::ClearedClipboard => self.clear_clipboard(),
            Command::CopiedToClipboard(path) => {
                self.clipboard_command = Some(ClipboardCommand::Copy(path.clone()));
                CommandResult::Handled
            }
            Command::CutToClipboard(path) => {
                self.clipboard_command = Some(ClipboardCommand::Move(path.clone()));
                CommandResult::Handled
            }
            Command::Progress(task) => self.update_tasks(task.clone()),
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('p'), KeyModifiers::NONE) => self.clear_progress(),
            (KeyCode::Char('c'), KeyModifiers::NONE) => Command::ClearClipboard.into(),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Clear the notice that was clicked based on its position
                let y = event.row.saturating_sub(self.area.y);

                let notices: Vec<_> = self.active_notices().collect();

                // Find which notice was clicked based on y position
                match notices.get(y as usize) {
                    Some(NoticeKind::Clipboard(_)) => Command::ClearClipboard.into(),
                    Some(NoticeKind::Filter(_)) => Command::SetFilter("".into()).into(),
                    Some(NoticeKind::Progress) => self.clear_progress(),
                    None => CommandResult::Handled,
                }
            }
            _ => CommandResult::Handled,
        }
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        self.area.intersects(Rect::new(x, y, 1, 1))
    }
}
