mod handler;
mod view;
mod widget;

use ratatui::{layout::Rect, text::Line};

use self::widget::{
    Section, add_keybinding_lines, add_section_header, build_sections, label_width,
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
    hint: String,
    inner_height: u16,
    /// Label and key columns, resolved once; styled lines are built per frame from the render's
    /// theme.
    sections: Vec<Section>,
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
        let sections = build_sections(kb);
        Self {
            area: Rect::default(),
            hint,
            inner_height: 0,
            sections,
            max_scroll: 0,
            scroll_offset: 0,
            scrollbar_view: ScrollbarView::default(),
        }
    }

    /// The help text, styled with `theme` per frame.
    fn lines(&self, theme: &Help) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        for (index, (title, rows)) in self.sections.iter().enumerate() {
            if index > 0 {
                lines.push(Line::raw(""));
            }
            let width = label_width(rows);
            add_section_header(&mut lines, title, width, theme);
            add_keybinding_lines(&mut lines, rows, width, theme);
        }
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

    /// Uses the drawn offset: after a resize the stored one can exceed `max_scroll`.
    fn scroll_up(&mut self, lines: u16) {
        self.scroll_offset = self
            .scroll_offset
            .min(self.max_scroll)
            .saturating_sub(lines);
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

    #[test]
    fn every_row_fits_an_80_column_terminal() {
        Config::init_test();
        let view = HelpView::new(Config::global());

        for line in view.lines(&Config::global().theme.help) {
            assert!(line.width() <= 78, "{} columns: {line}", line.width());
        }
    }

    /// Scrolled to the top of a longer document with a 4-line viewport (fields the render pass
    /// sets).
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
    fn scrolling_up_after_the_document_got_shorter_moves_from_what_is_drawn() {
        let mut view = help();
        view.handle_scroll_action(Action::SelectLast);
        // The render pass lowers the maximum after a resize.
        view.max_scroll = 5;

        view.handle_scroll_action(Action::SelectPrevious);

        assert_eq!(4, view.scroll_offset);
    }

    #[test]
    fn the_wheel_scrolls_one_line_at_a_time() {
        use ratatui::crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};

        use crate::command::handler::CommandHandler;

        let mut view = help();
        let mut wheel = |kind| {
            view.handle_mouse(MouseEvent {
                kind,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            });
        };
        wheel(MouseEventKind::ScrollDown);
        wheel(MouseEventKind::ScrollDown);
        wheel(MouseEventKind::ScrollUp);

        assert_eq!(1, view.scroll_offset);
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

        // Returning NotHandled without mutating lets `changed_nothing_visible` skip the redraw.
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

        view.reset_scroll();

        assert_eq!(0, view.scroll_offset);
    }
}
