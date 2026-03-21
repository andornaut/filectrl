use ratatui::{
    Frame,
    buffer::Buffer,
    crossterm::event::{KeyCode, KeyModifiers},
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, Paragraph, Widget, Wrap},
};

use super::{
    View, alerts::AlertsView, breadcrumbs::BreadcrumbsView, help::HelpView, notices::NoticesView,
    prompt::PromptView, status::StatusView, table::TableView,
};
use crate::{
    app::config::{Config, keybindings::Action},
    command::{Command, InputMode, handler::CommandHandler, result::CommandResult},
};

const MIN_WIDTH: u16 = 14;
const MIN_HEIGHT: u16 = 5;
const RESIZE_WINDOW: &str = "Resize window";

pub struct RootView {
    alerts: AlertsView,
    breadcrumbs: BreadcrumbsView,
    help: HelpView,
    is_help_visible: bool,
    mode: InputMode,
    notices: NoticesView,
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
            vec![&mut self.help]
        } else {
            let mut views: Vec<&mut dyn View> = vec![
                &mut self.alerts,
                &mut self.breadcrumbs,
                &mut self.table,
                &mut self.notices,
            ];
            if matches!(self.mode, InputMode::Prompt) {
                views.push(&mut self.prompt);
            }
            views.push(&mut self.status);
            views
        }
    }
}

impl CommandHandler for RootView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::CancelPrompt
            | Command::ConfirmDelete
            | Command::RenamePath(_, _)
            | Command::SetFilter(_) => {
                self.mode = InputMode::Normal;
                CommandResult::Handled
            }
            Command::OpenPrompt(_) => {
                self.mode = InputMode::Prompt;
                CommandResult::Handled
            }
            Command::Reset => {
                self.mode = InputMode::Normal;
                self.is_help_visible = false;
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
                    Command::ResetHelpScroll.into()
                } else {
                    CommandResult::Handled
                }
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn visit_command_handlers(&mut self, visitor: &mut dyn FnMut(&mut dyn CommandHandler)) {
        for view in self.views() {
            visitor(view);
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
        Block::default()
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
