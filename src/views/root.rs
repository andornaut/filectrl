use ratatui::{
    Frame,
    buffer::Buffer,
    crossterm::event::{KeyCode, KeyModifiers},
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Fill, Paragraph, Widget, Wrap},
};

use super::{
    ListingCount, View, alerts::AlertsView, breadcrumbs::BreadcrumbsView, help::HelpView,
    notices::NoticesView, open_with::OpenWithView, prompt::PromptView, status::StatusView,
    table::TableView,
};
use crate::app::config::theme::Theme;
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

    fn should_handle_key(&self, _: InputMode) -> bool {
        false
    }
}

/// Forwards everything but mouse events, for the views under the "Resize
/// window" message: nothing of them is drawn, so a click must not be tested
/// against the last layout that was.
struct NoMouse<'a>(&'a mut dyn CommandHandler);

impl CommandHandler for NoMouse<'_> {
    fn visit_command_handlers(&mut self, visitor: &mut dyn FnMut(&mut dyn CommandHandler)) {
        self.0
            .visit_command_handlers(&mut |child| visitor(&mut NoMouse(child)));
    }

    fn handle_command(&mut self, command: &Command) -> CommandResult {
        self.0.handle_command(command)
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        self.0.handle_key(code, modifiers)
    }

    fn handle_paste(&mut self, text: &str) -> CommandResult {
        self.0.handle_paste(text)
    }

    fn should_handle_key(&self, mode: InputMode) -> bool {
        self.0.should_handle_key(mode)
    }
}

pub struct RootView {
    alerts: AlertsView,
    breadcrumbs: BreadcrumbsView,
    help: HelpView,
    is_help_visible: bool,
    /// Set while the terminal is too small to draw anything but the "Resize
    /// window" message.
    is_too_small: bool,
    mode: InputMode,
    notices: NoticesView,
    open_with: OpenWithView,
    prompt: PromptView,
    status: StatusView,
    table: TableView,
}

impl RootView {
    /// Every view that needs a setting is given it here, so nothing below this
    /// point reaches for the config to build itself.
    pub fn new(config: &Config) -> Self {
        let keybindings = &config.keybindings;
        Self {
            alerts: AlertsView::new(keybindings),
            breadcrumbs: BreadcrumbsView::default(),
            help: HelpView::new(config),
            is_help_visible: false,
            is_too_small: false,
            mode: InputMode::default(),
            notices: NoticesView::new(keybindings),
            open_with: OpenWithView::new(keybindings),
            prompt: PromptView::default(),
            status: StatusView::default(),
            table: TableView::new(config.ui, &config.keybindings),
        }
    }

    pub fn mode(&self) -> InputMode {
        self.mode
    }

    pub fn is_help_visible(&self) -> bool {
        self.is_help_visible
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

impl RootView {
    /// Returns to Normal mode, yielding `CancelPrompt` when that closed a
    /// prompt which was still open.
    ///
    /// Mouse events are not mode-gated, so a click on the breadcrumbs or a
    /// notice reaches the view under the prompt and closes it from beneath
    /// whoever is holding state for it. `CancelPrompt` is how they are told to
    /// drop it: without it a paste waiting on a conflict answer would stall
    /// with no clipboard follow-up, and its stale answer would arrive at
    /// whatever prompt the user opened next.
    fn close_prompt(&mut self) -> Option<Command> {
        let was_open = matches!(self.mode, InputMode::Prompt);
        self.mode = InputMode::Normal;
        was_open.then_some(Command::CancelPrompt)
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
            | Command::StartSearch(_) => self
                .close_prompt()
                .map_or(CommandResult::NotHandled, Into::into),
            // The conflict prompt's own answer. The paste is waiting on it and
            // may reopen the prompt for the next collision, so this must not be
            // announced as the prompt being abandoned.
            Command::ResolveConflict(_) => {
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
                self.is_help_visible = false;
                self.open_with.hide();
                self.close_prompt()
                    .map_or(CommandResult::Handled, Into::into)
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
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
            Some(Action::ResetView) if self.is_help_visible => {
                self.is_help_visible = false;
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn visit_command_handlers(&mut self, visitor: &mut dyn FnMut(&mut dyn CommandHandler)) {
        if self.is_too_small {
            self.visit_views(&mut |child| visitor(&mut NoMouse(child)));
        } else {
            self.visit_views(visitor);
        }
    }
}

impl RootView {
    fn visit_views(&mut self, visitor: &mut dyn FnMut(&mut dyn CommandHandler)) {
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

    fn render(&mut self, theme: &Theme, area: Rect, frame: &mut Frame<'_>) {
        self.is_too_small = area.width < MIN_WIDTH || area.height < MIN_HEIGHT;
        if self.is_too_small {
            self.table.hide();
            render_resize_message(theme, frame.buffer_mut(), area);
            return;
        }

        // Fill the entire frame with the base background color so that uncovered areas
        // (e.g. continuation lines of wrapped filenames, empty space below the last row)
        // show the correct color rather than the terminal default.
        Fill::new(" ")
            .style(theme.base())
            .render(area, frame.buffer_mut());

        // RootView owns both, so the count is handed over directly rather
        // than broadcast after every change that could alter it.
        let listing_count = self.table.listing_count();
        self.status.set_listing_count(listing_count);
        self.notices.set_result_count(match listing_count {
            ListingCount::Results { total, .. } => Some(total),
            _ => None,
        });
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
            .for_each(|(area, handler)| handler.render(theme, *area, frame));
    }
}

fn render_resize_message(theme: &Theme, buf: &mut Buffer, area: Rect) {
    let widget = Paragraph::new(RESIZE_WINDOW)
        .style(theme.alert.error())
        .wrap(Wrap { trim: true });
    widget.render(area, buf);
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;
    use crate::{
        command::{ConflictChoice, PromptAction},
        file_system::path_info::PathInfo,
    };

    fn view() -> RootView {
        Config::init_test();
        RootView::new(Config::global())
    }

    /// Whether any handler the root dispatches to takes a left click at
    /// `(column, row)`.
    fn takes_click(root: &mut RootView, column: u16, row: u16) -> bool {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let mut taken = false;
        root.visit_command_handlers(&mut |handler| {
            taken |= handler.should_handle_mouse(event);
        });
        taken
    }

    fn render(root: &mut RootView, width: u16, height: u16) {
        use ratatui::{Terminal, backend::TestBackend};

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| root.render(Config::global().theme(), frame.area(), frame))
            .unwrap();
    }

    /// Under the "Resize window" message nothing is drawn, so a click on it
    /// must not reach a view laid out by an earlier frame. Keys still do.
    #[test]
    fn a_click_reaches_no_view_while_the_terminal_is_too_small() {
        let mut root = view();
        render(&mut root, 80, 24);
        assert!(takes_click(&mut root, 1, 5), "the table takes a click");

        render(&mut root, 10, 4);
        assert!(!takes_click(&mut root, 1, 5));
        let mut takes_keys = false;
        root.visit_command_handlers(&mut |handler| {
            takes_keys |= handler.should_handle_key(InputMode::Normal);
        });
        assert!(takes_keys);

        render(&mut root, 80, 24);
        assert!(takes_click(&mut root, 1, 5));
    }

    /// The resize message stands in for every view, so a release arriving
    /// under it never reaches the scrollbar. Were the drag to survive, the
    /// table would claim every later click, wherever it landed.
    #[test]
    fn a_terminal_too_small_to_draw_the_table_ends_a_scrollbar_drag() {
        let mut root = view();
        render(&mut root, 80, 24);
        root.table.scrollbar_mut().begin_drag();

        render(&mut root, 10, 4);

        assert!(!root.table.scrollbar_mut().is_dragging());
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

    /// Opening a prompt is what routes keys to `PromptView`, and every prompt
    /// resolves through a command that has to bring the mode back. The paste
    /// conflict prompt is the only one opened by a handler that is not a view,
    /// so `FileSystem` depends on both halves of this holding for a command it
    /// cannot observe.
    #[test_case(&Command::OpenPrompt(PromptAction::Conflict {
        name: "a.txt".to_string(),
        can_overwrite: true,
    }), InputMode::Prompt ; "opening the conflict prompt takes keys")]
    #[test_case(&Command::ResolveConflict(ConflictChoice::Skip), InputMode::Normal ; "answering it gives them back")]
    #[test_case(&Command::CancelPrompt, InputMode::Normal ; "dismissing it gives them back")]
    fn a_command_leaves_the_root_in_mode(command: &Command, expected: InputMode) {
        let mut root = view();
        // Start from the opposite mode so a no-op arm cannot pass by accident.
        root.mode = match expected {
            InputMode::Prompt => InputMode::Normal,
            InputMode::Normal => InputMode::Prompt,
        };

        root.handle_command(command);

        assert_eq!(expected, root.mode());
    }

    /// A click that closes a prompt from beneath it must still tell whoever
    /// holds state for that prompt; see `close_prompt`.
    #[test_case(&Command::Open(PathInfo::try_from("/tmp").unwrap()) ; "a breadcrumb click")]
    #[test_case(&Command::ResetView ; "a notice click")]
    fn closing_an_open_prompt_from_underneath_announces_it(command: &Command) {
        let mut root = view();
        root.mode = InputMode::Prompt;

        let result = root.handle_command(command);

        assert_eq!(Some(Command::CancelPrompt), Command::try_from(result).ok());
        assert_eq!(InputMode::Normal, root.mode());
    }

    #[test_case(&Command::Open(PathInfo::try_from("/tmp").unwrap()) ; "a breadcrumb click")]
    #[test_case(&Command::ResetView ; "a notice click")]
    fn closing_nothing_announces_nothing(command: &Command) {
        let mut root = view();

        assert_eq!(None, Command::try_from(root.handle_command(command)).ok());
    }

    #[test]
    fn answering_the_conflict_prompt_is_not_a_dismissal() {
        let mut root = view();
        root.mode = InputMode::Prompt;

        // The paste is waiting on this answer and may reopen the prompt for the
        // next collision. Announcing a dismissal would cancel the very paste
        // being answered.
        let result = root.handle_command(&Command::ResolveConflict(ConflictChoice::Overwrite));

        assert_eq!(None, Command::try_from(result).ok());
    }

    /// What every handler that takes keys in `mode` makes of one keypress.
    /// A count alone cannot say *which* handler that is, so each caller
    /// presses a key whose result only the expected view produces.
    fn press(
        root: &mut RootView,
        mode: InputMode,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Vec<CommandResult> {
        let mut results = Vec::new();
        root.visit_command_handlers(&mut |handler| {
            if handler.should_handle_key(mode) {
                results.push(handler.handle_key(code, modifiers));
            }
        });
        results
    }

    #[test]
    fn only_the_prompt_view_takes_keys_in_prompt_mode() {
        let mut root = view();
        // Prompt mode both adds PromptView to the visited views and makes it
        // the only key handler. That pairing is the other half of the routing:
        // it is what carries the conflict prompt's keypress to PromptView
        // rather than to the table.
        root.mode = InputMode::Prompt;

        // Esc dismisses a prompt, which is PromptView's answer and no other's.
        assert_eq!(
            vec![CommandResult::from(Command::CancelPrompt)],
            press(
                &mut root,
                InputMode::Prompt,
                KeyCode::Esc,
                KeyModifiers::NONE
            )
        );
    }

    #[test]
    fn keys_reach_only_the_help_view_while_help_is_visible() {
        let mut root = view();
        root.is_help_visible = true;

        // The delete key: HelpView implements only the scroll actions and
        // declines it, where the table beneath would open the delete prompt.
        assert_eq!(
            vec![CommandResult::NotHandled],
            press(
                &mut root,
                InputMode::Normal,
                KeyCode::Char('d'),
                KeyModifiers::NONE
            )
        );
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

        // The open-with key closes the picker, which only the picker does.
        assert_eq!(
            vec![CommandResult::Handled],
            press(
                &mut root,
                InputMode::Normal,
                KeyCode::Char('o'),
                KeyModifiers::NONE
            )
        );
        assert!(!root.open_with.is_visible());
    }

    #[test]
    fn open_with_replaces_the_table_slot_rather_than_adding_one() {
        let mut root = view();
        let without_picker = root.views().len();

        root.open_with.show(&PathInfo::try_from("/tmp").unwrap());

        assert_eq!(without_picker, root.views().len());
    }

    #[test]
    fn the_help_key_toggles_help() {
        let mut root = view();

        root.handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(root.is_help_visible);
        root.handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(!root.is_help_visible);
    }

    #[test]
    fn the_reset_key_closes_help_without_resetting_the_view() {
        let mut root = view();
        root.is_help_visible = true;

        assert_eq!(
            CommandResult::Handled,
            root.handle_key(KeyCode::Esc, KeyModifiers::NONE)
        );

        assert!(!root.is_help_visible);
    }

    #[test]
    fn reset_view_closes_help() {
        let mut root = view();
        root.is_help_visible = true;

        root.handle_command(&Command::ResetView);

        assert!(!root.is_help_visible);
    }

    #[test]
    fn the_prompt_takes_a_row_only_while_one_is_open() {
        let mut root = view();
        let normal = root.views().len();

        root.mode = InputMode::Prompt;

        assert_eq!(normal + 1, root.views().len());
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
