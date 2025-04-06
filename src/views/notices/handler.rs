use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::prelude::Rect;

use crate::command::{handler::CommandHandler, result::CommandResult, Command};

use super::{ClipboardOperation, NoticeType, NoticesView};

impl CommandHandler for NoticesView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::CancelClipboard => self.clear_clipboard(),
            Command::ClipboardCopy(path) => {
                self.set_clipboard(path.clone(), ClipboardOperation::Copy)
            }
            Command::ClipboardCut(path) => {
                self.set_clipboard(path.clone(), ClipboardOperation::Cut)
            }
            Command::Copy(_, _) | Command::Move(_, _) => {
                // The clipboard was pasted
                self.clipboard = None;
                CommandResult::none()
            }
            Command::Progress(task) => self.update_tasks(task.clone()),
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('p'), KeyModifiers::NONE) => self.clear_progress(),
            (KeyCode::Char('c'), KeyModifiers::NONE) => Command::CancelClipboard.into(),
            (_, _) => CommandResult::NotHandled,
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
                    Some(NoticeType::Progress) => self.clear_progress(),
                    Some(NoticeType::Clipboard(_)) => Command::CancelClipboard.into(),
                    Some(NoticeType::Filter(_)) => self.clear_filter(),
                    None => CommandResult::none(),
                }
            }
            _ => CommandResult::none(),
        }
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        self.area.intersects(Rect::new(x, y, 1, 1))
    }
}
