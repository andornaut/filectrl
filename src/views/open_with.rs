mod handler;
mod view;
mod widget;

use ratatui::layout::Rect;

use super::ScrollbarView;
use crate::{
    app::config::{Config, keybindings::Action},
    command::{Command, result::CommandResult},
    file_system::{
        open_with::{AppCandidate, candidates_for},
        path_info::PathInfo,
    },
};

const MIN_HEIGHT: u16 = 3; // border + 1 row + border
/// Rows past this many have no digit shortcut and are reached by scrolling.
const MAX_SHORTCUT: usize = 9;

/// Lists the applications that can open one path, and resolves the chosen one
/// into a `Command::OpenWith`. Shown in place of the table, so that the
/// breadcrumbs above and the status bar below stay visible.
pub(super) struct OpenWithView {
    area: Rect,
    candidates: Vec<AppCandidate>,
    /// The rows' area, for hit testing a click on a row.
    content_area: Rect,
    /// Bordered header hint, cached at construction.
    hint: String,
    inner_height: usize,
    is_visible: bool,
    scroll_offset: usize,
    scrollbar_view: ScrollbarView,
    selected: usize,
    /// Bordered header title, rebuilt each time the picker is shown.
    title: String,
}

impl OpenWithView {
    pub(super) fn new() -> Self {
        let kb = &Config::global().keybindings;
        Self {
            area: Rect::default(),
            candidates: Vec::new(),
            content_area: Rect::default(),
            hint: format!(
                "(Press {} to close)",
                kb.hint_for(&[Action::OpenWith, Action::ResetView])
            ),
            inner_height: 0,
            is_visible: false,
            scroll_offset: 0,
            scrollbar_view: ScrollbarView::default(),
            selected: 0,
            title: String::new(),
        }
    }

    pub(super) fn is_visible(&self) -> bool {
        self.is_visible
    }

    /// Enumerate the applications for `path` and show the picker.
    pub(super) fn show(&mut self, path: &PathInfo) {
        self.candidates = candidates_for(path.as_path());
        self.inner_height = 0;
        self.is_visible = true;
        self.scroll_offset = 0;
        self.selected = 0;
        self.title = format!("Open {} with", path.name());
    }

    pub(super) fn hide(&mut self) {
        self.candidates = Vec::new();
        self.is_visible = false;
        self.title = String::new();
    }

    fn max_scroll(&self) -> usize {
        self.candidates.len().saturating_sub(self.inner_height)
    }

    fn select(&mut self, index: usize) -> CommandResult {
        if self.candidates.is_empty() {
            return CommandResult::Handled;
        }
        self.selected = index.min(self.candidates.len() - 1);
        self.scroll_offset = clamp_scroll(self.inner_height, self.selected, self.scroll_offset);
        CommandResult::Handled
    }

    fn handle_scroll_action(&mut self, action: Action) -> CommandResult {
        let last = self.candidates.len().saturating_sub(1);
        let page = self.inner_height.max(1);
        match action {
            Action::SelectNext => self.select(self.selected.saturating_add(1)),
            Action::SelectPrevious => self.select(self.selected.saturating_sub(1)),
            Action::PageDown => self.select(self.selected.saturating_add(page)),
            Action::PageUp => self.select(self.selected.saturating_sub(page)),
            Action::SelectFirst => self.select(0),
            Action::SelectLast => self.select(last),
            _ => CommandResult::NotHandled,
        }
    }

    fn launch_selected(&mut self) -> CommandResult {
        self.launch_row(self.selected)
    }

    /// Launch the application in `index`, or do nothing when the row does not
    /// exist (a digit beyond the end of a short list).
    fn launch_row(&mut self, index: usize) -> CommandResult {
        let Some(candidate) = self.candidates.get(index) else {
            return CommandResult::Handled;
        };
        let command = Command::OpenWith {
            argv: candidate.argv.clone(),
            label: candidate.name.clone(),
            working_dir: candidate.working_dir.clone(),
        };
        self.hide();
        command.into()
    }
}

/// Move `selected` into the viewport starting at `scroll`, moving as little as
/// possible. A scrollbar drag sets the offset directly, and the next render
/// would pull it straight back if the selection were left off screen.
fn clamp_selection(inner_height: usize, count: usize, scroll: usize, selected: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let last_visible = scroll + inner_height.saturating_sub(1);
    selected.max(scroll).min(last_visible).min(count - 1)
}

/// The scroll offset that keeps `selected` inside the viewport, moving as
/// little as possible.
fn clamp_scroll(inner_height: usize, selected: usize, scroll: usize) -> usize {
    if inner_height == 0 {
        return 0;
    }
    if selected < scroll {
        selected
    } else if selected >= scroll + inner_height {
        selected + 1 - inner_height
    } else {
        scroll
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::{clamp_scroll, clamp_selection};

    #[test_case(5, 20, 15, 0, 15 ; "a drag to the bottom carries the selection with it")]
    #[test_case(5, 20, 0, 19, 4 ; "a drag to the top carries the selection with it")]
    #[test_case(5, 20, 10, 12, 12 ; "a selection already in the viewport does not move")]
    #[test_case(5, 3, 0, 2, 2 ; "fewer candidates than rows")]
    #[test_case(0, 20, 7, 0, 7 ; "an unmeasured viewport still moves to the offset")]
    #[test_case(5, 0, 3, 0, 0 ; "no candidates")]
    fn clamp_selection_follows_the_scroll_offset(
        inner_height: usize,
        count: usize,
        scroll: usize,
        selected: usize,
        expected: usize,
    ) {
        assert_eq!(
            expected,
            clamp_selection(inner_height, count, scroll, selected)
        );
    }

    #[test_case(0, 5, 3, 0 ; "an unmeasured viewport pins the offset to the top")]
    #[test_case(5, 2, 0, 0 ; "already visible, no movement")]
    #[test_case(5, 4, 0, 0 ; "the last visible row does not scroll")]
    #[test_case(5, 5, 0, 1 ; "one row past the bottom scrolls by one")]
    #[test_case(5, 9, 0, 5 ; "a jump past the bottom scrolls just far enough")]
    #[test_case(5, 2, 4, 2 ; "above the viewport scrolls up to the row")]
    #[test_case(5, 6, 6, 6 ; "the first visible row does not scroll")]
    fn clamp_scroll_keeps_the_selection_visible(
        inner_height: usize,
        selected: usize,
        scroll: usize,
        expected: usize,
    ) {
        assert_eq!(expected, clamp_scroll(inner_height, selected, scroll));
    }
}
