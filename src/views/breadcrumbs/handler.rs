use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::{BreadcrumbsView, widget::clicked_index};
use crate::{
    app::config::Config,
    command::{Command, handler::CommandHandler, result::CommandResult},
    views::{ListingMode, contains},
};

impl CommandHandler for BreadcrumbsView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        // Mode membership comes from the shared transition; the arms below
        // handle the breadcrumb data.
        if let Some(mode) = ListingMode::transition(command) {
            self.mode = mode;
        }
        match command {
            Command::NavigatedDirectory { directory, .. } => self.set_directory(&directory.clone()),
            Command::RefreshedDirectory { directory, .. } => {
                // In bookmarks mode the listing reloads via a follow-up
                // Bookmarks command; keep the bookmarks breadcrumbs meanwhile.
                if self.mode == ListingMode::Bookmarks {
                    self.directory.clone_from(&directory.path);
                    return CommandResult::Handled;
                }
                self.set_directory(&directory.clone())
            }
            // A search walks the directory behind the bookmarks, not the
            // bookmarks, and a reset lists it again.
            Command::StartSearch(_) | Command::ResetView => self.restore_directory(),
            Command::Bookmarks { .. } => {
                self.set_path(&Config::global().bookmarks_dir());
                self.positions.clear();
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let x = event.column.saturating_sub(self.area.x);
                let y = event.row.saturating_sub(self.area.y);
                // Positions are populated in render(); guard against a stale area or a
                // mouse event arriving before the first render.
                let Some(row) = self.positions.get(y as usize) else {
                    return CommandResult::Handled;
                };
                let has_tag = self.mode != ListingMode::Normal;
                let index = clicked_index(row, x, has_tag);
                if let Some(path) = index.and_then(|i| self.to_path(i)) {
                    Command::Open(path).into()
                } else {
                    CommandResult::Handled
                }
            }
            _ => CommandResult::Handled,
        }
    }

    fn should_handle_mouse(&self, event: MouseEvent) -> bool {
        contains(self.area, event)
    }
}
