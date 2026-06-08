mod handler;
mod view;
mod widget;

use ratatui::layout::Rect;

use self::widget::{build_normal_keybindings, build_prompt_keybindings};
use super::ScrollbarView;
use crate::{
    app::config::{Config, keybindings::Action},
    command::result::CommandResult,
};

pub use widget::keybindings_help_text;

const MIN_HEIGHT: u16 = 5;

pub(super) struct HelpView {
    area: Rect,
    /// Bordered header hint, cached at construction.
    hint: String,
    inner_height: u16,
    max_scroll: u16,
    /// Keybinding display strings, cached at construction (keybindings never change).
    normal_keybindings: Vec<(String, String)>,
    prompt_keybindings: Vec<(String, String)>,
    scroll_offset: u16,
    scrollbar_view: ScrollbarView,
}

impl HelpView {
    pub fn new() -> Self {
        let kb = &Config::global().keybindings;
        let hint = format!(
            "(Press {} to close)",
            kb.hint_for(&[Action::ToggleHelp, Action::ResetView])
        );
        let normal_keybindings = build_normal_keybindings(kb);
        let prompt_keybindings = build_prompt_keybindings(kb);
        Self {
            area: Rect::default(),
            hint,
            inner_height: 0,
            max_scroll: 0,
            normal_keybindings,
            prompt_keybindings,
            scroll_offset: 0,
            scrollbar_view: ScrollbarView::default(),
        }
    }

    fn reset_scroll(&mut self) {
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
