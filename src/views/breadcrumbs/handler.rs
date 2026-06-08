use std::path::Path;

use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::BreadcrumbsView;
use crate::{
    app::config::Config,
    command::{Command, handler::CommandHandler, result::CommandResult},
};

fn path_breadcrumbs(path: &Path) -> Vec<String> {
    let mut parts: Vec<_> = path
        .ancestors()
        .map(|p| {
            p.file_name()
                .map_or(String::new(), |n| n.to_string_lossy().into_owned())
        })
        .collect();
    parts.reverse();
    parts
}

impl CommandHandler for BreadcrumbsView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::NavigatedDirectory { directory, .. } => {
                self.set_directory(directory.clone());
                self.is_searching = false;
                CommandResult::Handled
            }
            Command::RefreshedDirectory { directory, .. } => self.set_directory(directory.clone()),
            Command::StartSearch(_) => {
                self.is_searching = true;
                CommandResult::Handled
            }
            Command::ResetView => {
                self.is_searching = false;
                CommandResult::Handled
            }
            Command::Bookmarks { .. } => {
                let dir = Config::global().bookmarks_dir();
                self.breadcrumbs = path_breadcrumbs(&dir);
                self.is_bookmarks = true;
                self.positions.clear();
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let x = event.column.saturating_sub(self.area.x);
                let y = event.row.saturating_sub(self.area.y);
                // Positions are populated in render(); guard against a stale area or a
                // mouse event arriving before the first render.
                let Some(row) = self.positions.get(y as usize) else {
                    return CommandResult::Handled;
                };
                let has_tag = self.is_bookmarks || self.is_searching;
                let clicked_index = row.iter().find_map(|p| {
                    if p.intersects(x) {
                        let i = p.index();
                        if has_tag {
                            if i == 0 { None } else { Some(i - 1) }
                        } else {
                            Some(i)
                        }
                    } else {
                        None
                    }
                });
                if let Some(path) = clicked_index.and_then(|i| self.to_path(i)) {
                    Command::Open(path).into()
                } else {
                    CommandResult::Handled
                }
            }
            _ => CommandResult::Handled,
        }
    }

    fn should_handle_mouse(&self, event: &MouseEvent) -> bool {
        self.area.contains(ratatui::layout::Position {
            x: event.column,
            y: event.row,
        })
    }
}
