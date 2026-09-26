mod handler;
mod view;
mod widget;

use std::path::PathBuf;

use ratatui::layout::Rect;

use super::{ScrollbarView, scroll_to_show};
use crate::{
    app::config::keybindings::{Action, KeyBindings},
    command::{Command, result::CommandResult},
    file_system::{
        open_with::{AppCandidate, candidates_for},
        path_info::PathInfo,
    },
};

const MIN_HEIGHT: u16 = 3; // border + 1 row + border
/// Rows past this many have no digit shortcut.
const MAX_SHORTCUT: usize = 9;

/// Lists the applications that can open a path and resolves the choice to `Command::OpenWith`.
/// Shown in place of the table.
pub(super) struct OpenWithView {
    area: Rect,
    candidates: Vec<AppCandidate>,
    /// What the candidates open, named in a launch failure.
    path: PathBuf,
    content_area: Rect,
    hint: String,
    inner_height: usize,
    is_visible: bool,
    scroll_offset: usize,
    scrollbar_view: ScrollbarView,
    selected: usize,
    title: String,
}

impl OpenWithView {
    pub(super) fn new(kb: &KeyBindings) -> Self {
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
            path: PathBuf::new(),
            scroll_offset: 0,
            scrollbar_view: ScrollbarView::default(),
            selected: 0,
            title: String::new(),
        }
    }

    pub(super) fn is_visible(&self) -> bool {
        self.is_visible
    }

    /// Shows the picker for `path`, returning an alert when the application
    /// lookup could not run.
    pub(super) fn show(&mut self, path: &PathInfo) -> CommandResult {
        let (candidates, error) = candidates_for(path.as_path());
        self.candidates = candidates;
        self.path.clone_from(&path.path);
        self.inner_height = 0;
        self.is_visible = true;
        self.scroll_offset = 0;
        self.selected = 0;
        self.title = format!("Open {} with", path.name());
        error.map_or(CommandResult::Handled, Into::into)
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
        self.scroll_offset = scroll_to_show(self.inner_height, self.scroll_offset, self.selected);
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

    /// Launches the application in row `index`, if it exists.
    fn launch_row(&mut self, index: usize) -> CommandResult {
        let Some(candidate) = self.candidates.get(index) else {
            return CommandResult::Handled;
        };
        let command = Command::OpenWith {
            argv: candidate.argv.clone(),
            label: candidate.failure_name(),
            path: self.path.clone(),
        };
        self.hide();
        command.into()
    }
}

/// Moves `selected` into the viewport at `scroll`, so a scrollbar drag is not undone by the next
/// render.
fn clamp_selection(inner_height: usize, count: usize, scroll: usize, selected: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let last_visible = scroll + inner_height.saturating_sub(1);
    selected.max(scroll).min(last_visible).min(count - 1)
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::{OpenWithView, clamp_selection};
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    use crate::{
        app::config::{Config, keybindings::Action},
        command::{Command, handler::CommandHandler, result::CommandResult},
        file_system::open_with::AppCandidate,
    };

    fn picker(count: usize) -> OpenWithView {
        Config::init_test();
        let mut view = OpenWithView::new(&Config::global().keybindings);
        view.candidates = (0..count)
            .map(|index| AppCandidate {
                argv: vec!["prog".into()],
                detail: "prog".to_string(),
                is_default: false,
                name: format!("App{index}"),
                setting: None,
            })
            .collect();
        view.inner_height = 5;
        view
    }

    #[test]
    fn selecting_past_the_end_lands_on_the_last_candidate() {
        let mut view = picker(3);

        view.select(9);

        assert_eq!(2, view.selected);
    }

    #[test]
    fn selecting_in_an_empty_list_selects_nothing() {
        let mut view = picker(0);

        // `len() - 1` would underflow.
        assert_eq!(CommandResult::Handled, view.select(0));
        assert_eq!(0, view.selected);
    }

    #[test]
    fn a_digit_launches_its_row() {
        Config::init_test();
        let mut view = picker(20);

        let result = view.handle_key(KeyCode::Char('3'), KeyModifiers::NONE);

        let Ok(Command::OpenWith { label, .. }) = Command::try_from(result) else {
            panic!("expected the third row to launch");
        };
        assert_eq!("\"App2\"", label);
    }

    /// A failure to run the configured opener names the setting.
    #[test]
    fn the_configured_opener_launches_under_its_setting() {
        let mut view = picker(1);
        view.candidates[0].setting = Some("open_file");

        let result = view.handle_key(KeyCode::Char('1'), KeyModifiers::NONE);

        let Ok(Command::OpenWith { label, .. }) = Command::try_from(result) else {
            panic!("expected the row to launch");
        };
        assert_eq!("openers.open_file", label);
    }

    #[test]
    fn a_digit_with_a_modifier_is_not_a_row_shortcut() {
        Config::init_test();
        let mut view = picker(20);

        // A chord belongs to whatever binds it.
        let result = view.handle_key(KeyCode::Char('3'), KeyModifiers::CONTROL);

        assert_eq!(CommandResult::NotHandled, result);
    }

    #[test]
    fn a_scroll_action_is_claimed_rather_than_falling_through_to_the_row_shortcuts() {
        Config::init_test();
        let mut view = picker(20);

        let result = view.handle_key(KeyCode::Down, KeyModifiers::NONE);

        assert_eq!(CommandResult::Handled, result);
        assert_eq!(1, view.selected);
    }

    #[test]
    fn a_click_selects_the_row_under_it_but_not_the_blank_space_below() {
        use ratatui::{
            crossterm::event::{MouseButton, MouseEvent, MouseEventKind},
            layout::Rect,
        };

        let mut view = picker(3);
        view.content_area = Rect::new(0, 1, 40, 5);
        let mut click = |row| {
            view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row,
                modifiers: KeyModifiers::NONE,
            });
            view.selected
        };

        assert_eq!(1, click(2));
        // Row 4 is below the last candidate.
        assert_eq!(1, click(5));
    }

    // 20 candidates in a 5-row viewport.
    #[test_case(Action::SelectNext, 0, 1       ; "next moves one row down")]
    #[test_case(Action::SelectPrevious, 3, 2   ; "previous moves one row up")]
    #[test_case(Action::SelectPrevious, 0, 0   ; "previous holds at the first row")]
    #[test_case(Action::PageDown, 0, 5         ; "a page down moves by the viewport height")]
    #[test_case(Action::PageUp, 12, 7          ; "a page up moves by the viewport height")]
    #[test_case(Action::PageUp, 2, 0           ; "a page up holds at the first row")]
    #[test_case(Action::SelectFirst, 12, 0     ; "first jumps to the top")]
    #[test_case(Action::SelectLast, 0, 19      ; "last jumps to the bottom")]
    fn a_scroll_action_moves_the_selection(action: Action, from: usize, expected: usize) {
        let mut view = picker(20);
        view.select(from);

        view.handle_scroll_action(action);

        assert_eq!(expected, view.selected);
    }

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
}
