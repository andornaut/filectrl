use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Position,
};

use super::OpenWithView;
use crate::{
    app::config::{
        Config,
        keybindings::{Action, hardcoded_normal_action},
    },
    command::{handler::CommandHandler, result::CommandResult},
};

impl CommandHandler for OpenWithView {
    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        let action = hardcoded_normal_action(code, modifiers)
            .or_else(|| Config::global().keybindings.normal_action(code, modifiers));
        match action {
            Some(Action::Open) => return self.launch_selected(),
            Some(Action::OpenWith) => {
                self.hide();
                return CommandResult::Handled;
            }
            Some(action) => {
                let result = self.handle_scroll_action(action);
                if result != CommandResult::NotHandled {
                    return result;
                }
            }
            None => {}
        }
        // Row shortcuts are checked last, so a digit bound to an action the
        // picker implements keeps that action. A digit bound to any other
        // action selects a row instead, which is what a picker should do.
        // Keys claimed higher in the handler tree (quit, cancel task, reset
        // view, toggle help) never reach this point.
        if *modifiers == KeyModifiers::NONE
            && let KeyCode::Char(digit @ '1'..='9') = code
        {
            return self.launch_row(*digit as usize - '1' as usize);
        }
        CommandResult::NotHandled
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::ScrollDown => self.handle_scroll_action(Action::SelectNext),
            MouseEventKind::ScrollUp => self.handle_scroll_action(Action::SelectPrevious),
            MouseEventKind::Down(MouseButton::Left)
            | MouseEventKind::Up(MouseButton::Left)
            | MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(offset) = self.scrollbar_view.handle_mouse(event, self.max_scroll()) {
                    self.scroll_offset = offset;
                    self.selected = super::clamp_selection(
                        self.inner_height,
                        self.candidates.len(),
                        offset,
                        self.selected,
                    );
                } else if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
                    && self.content_area.contains(Position {
                        x: event.column,
                        y: event.row,
                    })
                {
                    let row = event.row.saturating_sub(self.content_area.y) as usize;
                    let index = self.scroll_offset + row;
                    // Ignore a click on the blank space below the last row,
                    // which would otherwise silently move the selection.
                    if index < self.candidates.len() {
                        self.select(index);
                    }
                }
                CommandResult::Handled
            }
            // Claim everything else so a stray event cannot leak to the views
            // the picker is covering.
            _ => CommandResult::Handled,
        }
    }

    fn should_handle_mouse(&self, event: &MouseEvent) -> bool {
        matches!(
            event.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) || self.scrollbar_view.is_dragging()
            || self.area.contains(Position {
                x: event.column,
                y: event.row,
            })
    }
}
