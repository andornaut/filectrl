mod handler;
mod view;
mod widget;

use ratatui::{layout::Rect, text::Line};

use self::widget::{
    add_keybinding_lines, add_section_header, build_normal_keybindings, build_prompt_keybindings,
    max_label_width,
};
use super::ScrollbarView;
use crate::{
    app::config::{Config, keybindings::Action, theme::Help},
    command::result::CommandResult,
};

pub use widget::keybindings_help_text;

const MIN_HEIGHT: u16 = 5;

pub(super) struct HelpView {
    area: Rect,
    /// Bordered header hint, built once from the keybindings.
    hint: String,
    inner_height: u16,
    /// The label and key columns, resolved once from the keybindings. The
    /// styled lines are built per frame from the theme the render is handed,
    /// so the body and the border it sits in cannot come from two themes.
    normal_keybindings: Vec<(String, String)>,
    prompt_keybindings: Vec<(String, String)>,
    /// The width the two columns are laid out to, which the line count below
    /// depends on and the theme does not.
    label_width: usize,
    max_scroll: u16,
    scroll_offset: u16,
    scrollbar_view: ScrollbarView,
}

impl HelpView {
    pub fn new(config: &Config) -> Self {
        let kb = &config.keybindings;
        let hint = format!(
            "(Press {} to close)",
            kb.hint_for(&[Action::ToggleHelp, Action::ResetView])
        );
        let normal_keybindings = build_normal_keybindings(kb);
        let prompt_keybindings = build_prompt_keybindings(kb);
        let label_width = max_label_width(&normal_keybindings, &prompt_keybindings);
        Self {
            area: Rect::default(),
            hint,
            inner_height: 0,
            normal_keybindings,
            prompt_keybindings,
            label_width,
            max_scroll: 0,
            scroll_offset: 0,
            scrollbar_view: ScrollbarView::default(),
        }
    }

    /// The help text, styled with `theme`. Built per frame rather than cached,
    /// so every part of the view is drawn with the theme its render was given.
    fn lines(&self, theme: &Help) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        add_section_header(&mut lines, "Normal Mode", self.label_width, theme);
        add_keybinding_lines(
            &mut lines,
            &self.normal_keybindings,
            self.label_width,
            theme,
        );
        lines.push(Line::raw(""));
        add_section_header(&mut lines, "Prompt Mode", self.label_width, theme);
        add_keybinding_lines(
            &mut lines,
            &self.prompt_keybindings,
            self.label_width,
            theme,
        );
        lines
    }

    pub(super) fn reset_scroll(&mut self) {
        self.scroll_offset = 0;
    }

    fn scroll_down(&mut self, lines: u16) {
        self.scroll_offset = self
            .scroll_offset
            .saturating_add(lines)
            .min(self.max_scroll);
    }

    fn scroll_up(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    fn handle_scroll_action(&mut self, action: Action) -> CommandResult {
        match action {
            Action::SelectNext => self.scroll_down(1),
            Action::SelectPrevious => self.scroll_up(1),
            Action::PageDown => self.scroll_down(self.inner_height),
            Action::PageUp => self.scroll_up(self.inner_height),
            Action::SelectFirst => self.reset_scroll(),
            Action::SelectLast => self.scroll_offset = self.max_scroll,
            _ => return CommandResult::NotHandled,
        }
        CommandResult::Handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A help view scrolled to the top of a longer document, with a viewport
    /// of 4 lines. The two fields are set by the render pass, which no unit
    /// test runs.
    fn help() -> HelpView {
        Config::init_test();
        let mut view = HelpView::new(Config::global());
        view.inner_height = 4;
        view.max_scroll = 10;
        view
    }

    #[test]
    fn scrolling_stops_at_both_ends_of_the_document() {
        let mut view = help();

        view.handle_scroll_action(Action::SelectPrevious);
        assert_eq!(0, view.scroll_offset, "scrolled above the first line");

        view.handle_scroll_action(Action::SelectLast);
        view.handle_scroll_action(Action::SelectNext);
        assert_eq!(
            view.max_scroll, view.scroll_offset,
            "scrolled past the last line"
        );
    }

    #[test]
    fn a_page_moves_by_the_viewport_and_clamps() {
        let mut view = help();

        view.handle_scroll_action(Action::PageDown);
        assert_eq!(4, view.scroll_offset);

        // Two more pages would reach 12, past the end of a 10-line scroll.
        view.handle_scroll_action(Action::PageDown);
        view.handle_scroll_action(Action::PageDown);
        assert_eq!(view.max_scroll, view.scroll_offset);

        view.handle_scroll_action(Action::PageUp);
        assert_eq!(6, view.scroll_offset);
    }

    #[test]
    fn an_action_the_help_does_not_scroll_by_is_declined_and_changes_nothing() {
        let mut view = help();
        view.handle_scroll_action(Action::PageDown);
        let before = view.scroll_offset;

        // Returning NotHandled without mutating is what lets a key the help
        // ignores leave the screen alone: `changed_nothing_visible` skips the
        // redraw for a batch nothing claimed.
        assert_eq!(
            CommandResult::NotHandled,
            view.handle_scroll_action(Action::Quit)
        );
        assert_eq!(before, view.scroll_offset);
    }

    #[test]
    fn toggling_help_open_starts_at_the_top() {
        let mut view = help();
        view.handle_scroll_action(Action::SelectLast);

        // `RootView` calls this when help is shown, so reopening it does not
        // resume half way down where the last visit left off.
        view.reset_scroll();

        assert_eq!(0, view.scroll_offset);
    }
}
