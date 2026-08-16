mod handler;
mod view;
mod widget;

use ratatui::{layout::Rect, text::Line};

use self::widget::{
    add_keybinding_lines, add_section_header, build_normal_keybindings, build_prompt_keybindings,
    max_label_width,
};
use super::{ScrollbarView, as_dimension};
use crate::{
    app::config::{Config, keybindings::Action},
    command::result::CommandResult,
};

pub use widget::keybindings_help_text;

const MIN_HEIGHT: u16 = 5;

pub(super) struct HelpView {
    area: Rect,
    /// Rendered content height in lines, cached at construction.
    content_height: u16,
    /// Bordered header hint, cached at construction.
    hint: String,
    inner_height: u16,
    /// Help content lines, cached at construction (keybindings and theme
    /// never change after startup).
    lines: Vec<Line<'static>>,
    max_scroll: u16,
    scroll_offset: u16,
    scrollbar_view: ScrollbarView,
}

impl HelpView {
    pub fn new() -> Self {
        let config = Config::global();
        let kb = &config.keybindings;
        let hint = format!(
            "(Press {} to close)",
            kb.hint_for(&[Action::ToggleHelp, Action::ResetView])
        );
        let normal_keybindings = build_normal_keybindings(kb);
        let prompt_keybindings = build_prompt_keybindings(kb);
        let help_theme = &config.theme().help;
        let max_width = max_label_width(&normal_keybindings, &prompt_keybindings);
        let mut lines: Vec<Line<'static>> = Vec::new();
        add_section_header(&mut lines, "Normal Mode", max_width, help_theme);
        add_keybinding_lines(&mut lines, &normal_keybindings, max_width, help_theme);
        lines.push(Line::raw(""));
        add_section_header(&mut lines, "Prompt Mode", max_width, help_theme);
        add_keybinding_lines(&mut lines, &prompt_keybindings, max_width, help_theme);
        let content_height = as_dimension(lines.len());
        Self {
            area: Rect::default(),
            content_height,
            hint,
            inner_height: 0,
            lines,
            max_scroll: 0,
            scroll_offset: 0,
            scrollbar_view: ScrollbarView::default(),
        }
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
