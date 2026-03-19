mod handler;
mod notice;
mod view;
mod widgets;

use std::{collections::HashSet, rc::Rc};

use notice::Notice;
use ratatui::layout::Rect;

use crate::{
    app::AppState,
    command::{result::CommandResult, progress::Task},
    keybindings::KeyBindings,
};

pub(super) struct NoticesView {
    area: Rect,
    filter: String,
    keybindings: Rc<KeyBindings>,
    mark_count: usize,
    tasks: HashSet<Task>,
    /// Cached at render time so the mouse handler can map y-position → action
    notices: Vec<Notice>,
}

impl NoticesView {
    pub fn new(keybindings: Rc<KeyBindings>) -> Self {
        Self {
            area: Rect::default(),
            filter: String::new(),
            keybindings,
            mark_count: 0,
            tasks: HashSet::new(),
            notices: Vec::new(),
        }
    }
}

impl NoticesView {
    fn build_notices(&self, state: &AppState) -> Vec<Notice> {
        [
            (!self.tasks.is_empty()).then_some(Notice::Progress),
            (self.mark_count > 0).then_some(Notice::Marked(self.mark_count)),
            state.clipboard_entry.as_ref().map(|e| Notice::Clipboard(e.clone())),
            (!self.filter.is_empty()).then_some(Notice::Filter(self.filter.clone())),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    fn clear_progress(&mut self) -> CommandResult {
        self.tasks.clear();
        CommandResult::Handled
    }

    fn update_tasks(&mut self, task: Task) -> CommandResult {
        // If the task is not new and not in our set, it means we previously cleared it.
        // In this case, we should ignore the update to prevent resurrecting cleared tasks.
        if !task.is_new() && !self.tasks.contains(&task) {
            return CommandResult::Handled;
        }

        if task.is_done_or_error() {
            self.tasks.remove(&task);
        } else {
            self.tasks.replace(task); // upsert
        }
        CommandResult::Handled
    }
}
