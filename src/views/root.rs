use ratatui::{
    Frame,
    buffer::Buffer,
    crossterm::event::{KeyCode, KeyModifiers},
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Fill, Paragraph, Widget, Wrap},
};

use super::{
    View, alerts::AlertsView, breadcrumbs::BreadcrumbsView, help::HelpView, notices::NoticesView,
    open_with::OpenWithView, prompt::PromptView, status::StatusView, table::TableView,
};
use crate::{
    app::config::{Config, keybindings::Action},
    command::{Command, InputMode, handler::CommandHandler, result::CommandResult},
};

const MIN_WIDTH: u16 = 14;
const MIN_HEIGHT: u16 = 5;
const RESIZE_WINDOW: &str = "Resize window";

/// Forwards broadcast commands to a view covered by an overlay (help or the
/// "open with" picker) while declining key and mouse dispatch, which must
/// reach only the overlay.
struct CommandOnly<'a>(&'a mut dyn CommandHandler);

impl CommandHandler for CommandOnly<'_> {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        self.0.handle_command(command)
    }

    fn visit_command_handlers(&mut self, visitor: &mut dyn FnMut(&mut dyn CommandHandler)) {
        // Wrap children too, so a view that gains child handlers keeps
        // receiving commands (but not input) while an overlay is open.
        self.0
            .visit_command_handlers(&mut |child| visitor(&mut CommandOnly(child)));
    }

    fn should_handle_key(&self, _: &InputMode) -> bool {
        false
    }
}

pub struct RootView {
    alerts: AlertsView,
    breadcrumbs: BreadcrumbsView,
    help: HelpView,
    is_help_visible: bool,
    mode: InputMode,
    notices: NoticesView,
    open_with: OpenWithView,
    prompt: PromptView,
    status: StatusView,
    table: TableView,
}

impl RootView {
    pub fn new() -> Self {
        Self {
            alerts: AlertsView::new(),
            breadcrumbs: BreadcrumbsView::default(),
            help: HelpView::new(),
            is_help_visible: false,
            mode: InputMode::default(),
            notices: NoticesView::new(),
            open_with: OpenWithView::new(),
            prompt: PromptView::default(),
            status: StatusView::default(),
            table: TableView::default(),
        }
    }

    pub fn mode(&self) -> InputMode {
        self.mode
    }

    fn views(&mut self) -> Vec<&mut dyn View> {
        // The order is significant for layout
        if self.is_help_visible {
            return vec![&mut self.help];
        }
        // Read before the mutable borrows below.
        let is_open_with_visible = self.open_with.is_visible();
        let mut views: Vec<&mut dyn View> = vec![&mut self.alerts, &mut self.breadcrumbs];
        // The picker takes the table's slot, and has the same constraint, so
        // what is above and below it stays exactly where it was.
        if is_open_with_visible {
            views.push(&mut self.open_with);
        } else {
            views.push(&mut self.table);
        }
        views.push(&mut self.notices);
        if matches!(self.mode, InputMode::Prompt) {
            views.push(&mut self.prompt);
        }
        views.push(&mut self.status);
        views
    }
}

impl CommandHandler for RootView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::AddBookmark { .. }
            | Command::Chmod { .. }
            | Command::ConfirmDelete
            | Command::CreateDirectory(_)
            | Command::Open(_)
            | Command::Rename { .. }
            | Command::FilterChanged(_)
            | Command::ResolveConflict(_)
            | Command::StartSearch(_) => {
                self.mode = InputMode::Normal;
                CommandResult::NotHandled
            }
            Command::CancelPrompt => {
                self.mode = InputMode::Normal;
                CommandResult::Handled
            }
            Command::OpenPrompt(_) => {
                self.mode = InputMode::Prompt;
                CommandResult::Handled
            }
            Command::OpenWithPrompt(path) => {
                // RootView owns the picker, so showing it is a direct call
                // rather than a broadcast.
                self.open_with.show(path);
                CommandResult::Handled
            }
            Command::ResetView => {
                self.mode = InputMode::Normal;
                self.is_help_visible = false;
                self.open_with.hide();
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        // Rebindable keys
        match Config::global().keybindings.normal_action(code, modifiers) {
            Some(Action::ToggleHelp) => {
                self.is_help_visible = !self.is_help_visible;
                if self.is_help_visible {
                    // RootView owns the help view, so the scroll reset is a
                    // direct call rather than a broadcast command.
                    self.help.reset_scroll();
                }
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn visit_command_handlers(&mut self, visitor: &mut dyn FnMut(&mut dyn CommandHandler)) {
        if !self.is_help_visible && !self.open_with.is_visible() {
            for view in self.views() {
                visitor(view);
            }
            return;
        }
        // An overlay is the only key and mouse handler while it is shown, but
        // async commands (task progress, watcher refreshes, streamed listings)
        // keep arriving, so every view it covers must still receive them.
        let overlay: &mut dyn CommandHandler = if self.is_help_visible {
            &mut self.help
        } else {
            &mut self.open_with
        };
        visitor(overlay);
        let covered: [&mut dyn CommandHandler; 6] = [
            &mut self.alerts,
            &mut self.breadcrumbs,
            &mut self.notices,
            &mut self.prompt,
            &mut self.status,
            &mut self.table,
        ];
        for view in covered {
            visitor(&mut CommandOnly(view));
        }
    }
}

impl View for RootView {
    fn constraint(&self, _: Rect) -> Constraint {
        unreachable!(
            "RootView is the top-level view, which always receives the full terminal area directly from App, so constraint() should never be called"
        )
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>) {
        let theme = Config::global().theme();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            render_resize_message(frame.buffer_mut(), area);
            return;
        }

        // Fill the entire frame with the base background color so that uncovered areas
        // (e.g. continuation lines of wrapped filenames, empty space below the last row)
        // show the correct color rather than the terminal default.
        Fill::new(" ")
            .style(theme.base())
            .render(area, frame.buffer_mut());

        let views = self.views();
        Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                views
                    .iter()
                    .map(|view| view.constraint(area))
                    .collect::<Vec<_>>(),
            )
            .split(area)
            .iter()
            .zip(views)
            .for_each(|(area, handler)| handler.render(*area, frame));
    }
}

fn render_resize_message(buf: &mut Buffer, area: Rect) {
    let theme = Config::global().theme();
    let widget = Paragraph::new(RESIZE_WINDOW)
        .style(theme.alert.error())
        .wrap(Wrap { trim: true });
    widget.render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_system::path_info::PathInfo;

    fn view() -> RootView {
        Config::init_test();
        RootView::new()
    }

    #[test]
    fn commands_reach_hidden_views_while_help_is_visible() {
        let mut root = view();
        root.is_help_visible = true;

        // AlertError is only handled by the (hidden) AlertsView.
        let mut handled = false;
        root.visit_command_handlers(&mut |handler| {
            if handler.handle_command(&Command::AlertError("boom".into()))
                != CommandResult::NotHandled
            {
                handled = true;
            }
        });
        assert!(handled);
    }

    #[test]
    fn keys_reach_only_the_help_view_while_help_is_visible() {
        let mut root = view();
        root.is_help_visible = true;

        let mut key_handlers = 0;
        root.visit_command_handlers(&mut |handler| {
            if handler.should_handle_key(&InputMode::Normal) {
                key_handlers += 1;
            }
        });
        assert_eq!(1, key_handlers); // HelpView only
    }

    fn showing_open_with() -> RootView {
        let mut root = view();
        root.open_with.show(&PathInfo::try_from("/tmp").unwrap());
        assert!(root.open_with.is_visible());
        root
    }

    #[test]
    fn commands_reach_hidden_views_while_open_with_is_visible() {
        let mut root = showing_open_with();

        // AlertError is only handled by the (hidden) AlertsView.
        let mut handled = false;
        root.visit_command_handlers(&mut |handler| {
            if handler.handle_command(&Command::AlertError("boom".into()))
                != CommandResult::NotHandled
            {
                handled = true;
            }
        });
        assert!(handled);
    }

    #[test]
    fn keys_reach_only_the_open_with_view_while_open_with_is_visible() {
        let mut root = showing_open_with();

        let mut key_handlers = 0;
        root.visit_command_handlers(&mut |handler| {
            if handler.should_handle_key(&InputMode::Normal) {
                key_handlers += 1;
            }
        });
        assert_eq!(1, key_handlers); // OpenWithView only
    }

    #[test]
    fn open_with_replaces_the_table_slot_rather_than_adding_one() {
        let mut root = view();
        let without_picker = root.views().len();

        root.open_with.show(&PathInfo::try_from("/tmp").unwrap());

        assert_eq!(without_picker, root.views().len());
    }

    #[test]
    fn reset_view_closes_the_open_with_picker() {
        let mut root = showing_open_with();

        assert_eq!(
            CommandResult::Handled,
            root.handle_command(&Command::ResetView)
        );

        assert!(!root.open_with.is_visible());
    }
}
