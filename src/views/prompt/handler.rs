use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use tui_input::backend::crossterm::EventHandler;

use super::PromptView;
use crate::command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command};

impl CommandHandler for PromptView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::SetDirectory(directory, _) => {
                if let Some(previous_directory) = &self.directory {
                    if previous_directory.path != directory.path {
                        self.directory = Some(directory.clone());
                        return Command::SetFilter("".into()).into();
                    }
                }
                CommandResult::none()
            }
            Command::OpenPrompt(kind) => self.open(kind),
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            Command::SetSelected(selected) => self.set_selected(selected.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match *code {
            KeyCode::Esc => Command::ClosePrompt.into(),
            KeyCode::Enter => self.submit(),
            _ => self.handle_key(*code, *modifiers),
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Calculate cursor position from mouse click
                if let Some(new_position) = self.calculate_cursor_position(event.column) {
                    // Start selection on mouse down
                    self.selection.active = true;
                    self.selection.start = new_position;
                    self.selection.end = new_position;

                    // Use the input handler to set the cursor position
                    // Move cursor to the beginning
                    while self.input.cursor() > 0 {
                        self.input.handle_event(&Event::Key(KeyEvent::new(
                            KeyCode::Left,
                            KeyModifiers::CONTROL,
                        )));
                    }
                    // Move cursor to the calculated position
                    for _ in 0..new_position {
                        self.input.handle_event(&Event::Key(KeyEvent::new(
                            KeyCode::Right,
                            KeyModifiers::NONE,
                        )));
                    }
                }
                CommandResult::none()
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                // Update selection end point when dragging
                if self.selection.active {
                    if let Some(new_position) = self.calculate_cursor_position(event.column) {
                        self.selection.end = new_position;

                        // Update cursor position to match the end of selection
                        // Move cursor to the beginning
                        while self.input.cursor() > 0 {
                            self.input.handle_event(&Event::Key(KeyEvent::new(
                                KeyCode::Left,
                                KeyModifiers::CONTROL,
                            )));
                        }
                        // Move cursor to the end of selection
                        for _ in 0..new_position {
                            self.input.handle_event(&Event::Key(KeyEvent::new(
                                KeyCode::Right,
                                KeyModifiers::NONE,
                            )));
                        }
                    }
                }
                CommandResult::none()
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // Finalize selection on mouse up
                if self.selection.active && self.selection.start == self.selection.end {
                    // If start and end are the same, clear selection
                    self.selection.active = false;
                }
                CommandResult::none()
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn should_receive_key(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Prompt)
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        // Check if the point (x, y) is inside the input area rectangle
        x >= self.area.x
            && x < self.area.x + self.area.width
            && y >= self.area.y
            && y < self.area.y + self.area.height
    }
}

impl PromptView {
    fn calculate_cursor_position(&self, click_x: u16) -> Option<usize> {
        // Check if the click is inside the input area
        if click_x < self.area.x || click_x >= self.area.x + self.area.width {
            return None;
        }

        // Calculate the relative x position within the input field
        let relative_x = click_x.saturating_sub(self.area.x);

        // We need to convert from screen position to character position
        // For this simplified version, we're assuming 1:1 mapping between screen position and character
        // A more sophisticated implementation would consider text scrolling and unicode width
        let visible_text = self.input.value();
        let scroll_offset = self.input.visual_scroll(self.area.width as usize);

        // Start from the scroll offset and count characters until we reach the click position
        let mut position = scroll_offset;
        let mut current_width = 0;

        for (idx, _) in visible_text.chars().enumerate().skip(scroll_offset) {
            if current_width >= relative_x as usize {
                return Some(idx);
            }

            // Simple approximation - using 1 for ASCII, could be improved with unicode_width
            current_width += 1;
        }

        // If click was after the end of text, position at the end
        Some(visible_text.len())
    }

    // Add a method to get normalized selection bounds (start <= end)
    pub(crate) fn get_selection_bounds(&self) -> Option<(usize, usize)> {
        if !self.selection.active || self.selection.start == self.selection.end {
            return None;
        }

        if self.selection.start <= self.selection.end {
            Some((self.selection.start, self.selection.end))
        } else {
            Some((self.selection.end, self.selection.start))
        }
    }

    // Add a method to clear the selection
    pub(crate) fn clear_selection(&mut self) {
        self.selection.active = false;
        self.selection.start = 0;
        self.selection.end = 0;
    }
}
