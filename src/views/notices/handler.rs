use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::prelude::Rect;

use super::{NoticeKind, NoticesView};
use crate::clipboard::ClipboardCommand;
use crate::command::{handler::CommandHandler, result::CommandResult, Command};

impl CommandHandler for NoticesView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::CancelClipboard => self.clear_clipboard(),
            Command::CopiedToClipboard(_) | Command::CutToClipboard(_) => {
                let clipboard_command = ClipboardCommand::try_from(command)
                    .expect("We already checked that the command is a clipboard command");
                self.clipboard_command = Some(clipboard_command);
                CommandResult::Handled
            }
            Command::Copy(_, _) | Command::Move(_, _) => {
                // The clipboard was pasted
                self.clipboard_command = None;
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
                    Some(NoticeKind::Clipboard(_)) => Command::CancelClipboard.into(),
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
