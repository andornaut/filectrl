mod handler;
mod view;
mod widget;

use std::path::{MAIN_SEPARATOR, MAIN_SEPARATOR_STR};

use ratatui::{layout::Rect, style::Style};

use self::widget::{Position, spans};
use crate::{command::result::CommandResult, file_system::path_info::PathInfo};

#[derive(Default)]
pub(super) struct BreadcrumbsView {
    breadcrumbs: Vec<String>,
    is_bookmarks: bool,
    is_searching: bool,
    area: Rect,
    positions: Vec<Vec<Position>>,
}

impl BreadcrumbsView {
    fn display_breadcrumbs(&self) -> Vec<String> {
        if self.is_bookmarks {
            let mut display = vec!["[Bookmarks] ".to_string()];
            display.extend(self.breadcrumbs.iter().cloned());
            display
        } else if self.is_searching {
            let mut display = vec!["[Search] ".to_string()];
            display.extend(self.breadcrumbs.iter().cloned());
            display
        } else {
            self.breadcrumbs.clone()
        }
    }

    fn height(&self, width: u16) -> u16 {
        // Calculate height based on content length and width, without theme styling
        let (container, _) = spans(
            &self.display_breadcrumbs(),
            width,
            None,
            Style::default(),
            Style::default(),
            Style::default(),
        );
        container.len() as u16
    }

    fn set_directory(&mut self, directory: PathInfo) -> CommandResult {
        self.breadcrumbs = directory.breadcrumbs();
        self.is_bookmarks = false;
        CommandResult::Handled
    }

    fn to_path(&self, end_index: usize) -> Option<PathInfo> {
        if let Some(components) = self.breadcrumbs.get(0..=end_index) {
            let path = if components.len() == 1 {
                // Clicked on the root element, which is empty string
                MAIN_SEPARATOR.to_string()
            } else {
                components.join(MAIN_SEPARATOR_STR)
            };
            PathInfo::try_from(path).ok()
        } else {
            None
        }
    }
}
