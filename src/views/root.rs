use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, Paragraph, Widget, Wrap},
};

use super::{
    View, alerts::AlertsView, breadcrumbs::BreadcrumbsView, help::HelpView, notices::NoticesView,
    prompt::PromptView, status::StatusView, table::TableView,
};
use crate::{
    app::{clipboard::Clipboard, config::Config, config::theme::Theme, state::AppState},
    command::handler::CommandHandler,
};

const MIN_WIDTH: u16 = 14;
const MIN_HEIGHT: u16 = 5;
const RESIZE_WINDOW: &str = "Resize window";

pub struct RootView {
    alerts: AlertsView,
    breadcrumbs: BreadcrumbsView,
    help: HelpView,
    notices: NoticesView,
    prompt: PromptView,
    status: StatusView,
    table: TableView,
}

impl RootView {
    pub fn new(config: &Config, clipboard: Clipboard) -> Self {
        Self {
            alerts: AlertsView::default(),
            breadcrumbs: BreadcrumbsView::default(),
            help: HelpView::default(),
            notices: NoticesView::default(),
            prompt: PromptView::new(clipboard),
            status: StatusView::default(),
            table: TableView::new(config),
        }
    }

    fn views(&mut self) -> [&mut dyn View; 7] {
        [
            // The order is significant
            &mut self.alerts,
            &mut self.breadcrumbs,
            &mut self.help,
            &mut self.table,
            &mut self.notices,
            &mut self.status,
            &mut self.prompt,
        ]
    }
}

impl CommandHandler for RootView {
    fn visit_command_handlers(&mut self, visitor: &mut dyn FnMut(&mut dyn CommandHandler)) {
        for view in self.views() {
            visitor(view);
        }
    }
}

impl View for RootView {
    fn constraint(&self, _: Rect, _: &AppState) -> Constraint {
        unreachable!(
            "RootView is the top-level view, which always receives the full terminal area directly from App, so constraint() should never be called"
        )
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, state: &AppState, theme: &Theme) {
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            render_resize_message(frame.buffer_mut(), area, theme);
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
            .constraints(views.iter().map(|view| view.constraint(area, state)).collect::<Vec<_>>())
            .split(area)
            .iter()
            .zip(views)
            .for_each(|(area, handler)| handler.render(*area, frame, state, theme));
    }
}

fn render_resize_message(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let widget = Paragraph::new(RESIZE_WINDOW)
        .style(theme.alert.error())
        .wrap(Wrap { trim: true });
    widget.render(area, buf);
}
