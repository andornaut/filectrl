mod handler;
mod view;
mod widget;

use std::path::{MAIN_SEPARATOR, MAIN_SEPARATOR_STR};

use ratatui::{layout::Rect, style::Style};

use self::widget::{Position, spans};
use super::ListingMode;
use crate::{command::result::CommandResult, file_system::path_info::PathInfo};

#[derive(Default)]
pub(super) struct BreadcrumbsView {
    breadcrumbs: Vec<String>,
    /// Which listing the header describes; transitions come solely from
    /// `ListingMode::transition`.
    mode: ListingMode,
    area: Rect,
    positions: Vec<Vec<Position>>,
}

impl BreadcrumbsView {
    fn tag(&self) -> Option<&'static str> {
        match self.mode {
            ListingMode::Normal => None,
            ListingMode::Search => Some("[Search] "),
            ListingMode::Bookmarks => Some("[Bookmarks] "),
        }
    }

    fn display_breadcrumbs(&self) -> Vec<String> {
        match self.tag() {
            Some(tag) => {
                let mut display = vec![tag.to_string()];
                display.extend(self.breadcrumbs.iter().cloned());
                display
            }
            None => self.breadcrumbs.clone(),
        }
    }

    fn height(&self, width: u16) -> u16 {
        // Calculate height based on content length and width, without theme
        // styling. The tag placeholder must match render(): a tag entry has no
        // trailing separator, so measuring without one would wrap a column early.
        let tag_style = self.tag().map(|_| Style::default());
        let (container, _) = spans(
            &self.display_breadcrumbs(),
            width,
            tag_style,
            Style::default(),
            Style::default(),
            Style::default(),
        );
        container.len() as u16
    }

    fn set_directory(&mut self, directory: PathInfo) -> CommandResult {
        self.breadcrumbs = directory.breadcrumbs();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::config::{Config, RuntimeEnv},
        command::{Command, handler::CommandHandler},
    };

    fn ensure_config_initialized() {
        let config = Config::load(RuntimeEnv::default(), None, vec![]).unwrap();
        Config::init(config);
    }

    fn view(parts: &[&str], mode: ListingMode) -> BreadcrumbsView {
        BreadcrumbsView {
            breadcrumbs: parts.iter().map(|s| s.to_string()).collect(),
            mode,
            ..Default::default()
        }
    }

    #[test]
    fn height_with_tag_does_not_wrap_at_the_exact_width() {
        // "[Search] "(9) + ""(0+1 sep) + "home"(4+1 sep) + "abcde"(5, last) fills
        // exactly 20 columns when the tag has no trailing separator, as in render().
        let v = view(&["", "home", "abcde"], ListingMode::Search);
        assert_eq!(1, v.height(20));
        assert_eq!(2, v.height(19));
    }

    #[test]
    fn height_without_tag_is_unchanged() {
        // ""(0+1 sep) + "home"(4+1 sep) + "abcde"(5, last) = 11 columns.
        let v = view(&["", "home", "abcde"], ListingMode::Normal);
        assert_eq!(1, v.height(11));
        assert_eq!(2, v.height(10));
    }

    #[test]
    fn search_after_bookmarks_shows_the_search_tag() {
        ensure_config_initialized();
        let mut v = BreadcrumbsView::default();
        v.handle_command(&Command::Bookmarks { bookmarks: vec![] });
        assert_eq!(v.display_breadcrumbs()[0], "[Bookmarks] ");

        v.handle_command(&Command::StartSearch("q".into()));
        assert_eq!(v.display_breadcrumbs()[0], "[Search] ");
    }

    #[test]
    fn bookmarks_after_search_shows_the_bookmarks_tag() {
        ensure_config_initialized();
        let mut v = BreadcrumbsView::default();
        v.handle_command(&Command::StartSearch("q".into()));
        assert_eq!(v.display_breadcrumbs()[0], "[Search] ");

        v.handle_command(&Command::Bookmarks { bookmarks: vec![] });
        assert_eq!(v.display_breadcrumbs()[0], "[Bookmarks] ");
    }
}
