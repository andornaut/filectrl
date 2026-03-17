use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, Paragraph, Widget, Wrap},
    Frame,
};

use super::{
    alerts::AlertsView, header::HeaderView, help::HelpView, notices::NoticesView,
    prompt::PromptView, status::StatusView, table::TableView, View,
};
use crate::{
    app::{config::theme::Theme, config::Config, state::AppState},
    command::handler::CommandHandler,
};

const MIN_WIDTH: u16 = 14; // Must be 11 or larger to prevent clipboard notices from causing a panic
const MIN_HEIGHT: u16 = 5;
const RESIZE_WINDOW: &str = "Resize window";

#[derive(Default)]
pub struct RootView {
    alerts: AlertsView,
    header: HeaderView,
    help: HelpView,
    notices: NoticesView,
    prompt: PromptView,
    status: StatusView,
    table: TableView,
}

impl RootView {
    pub fn new(config: &Config) -> Self {
        Self {
            table: TableView::new(config),
            ..Self::default()
        }
    }

    fn views(&mut self) -> Vec<&mut dyn View> {
        vec![
            // The order is significant
            &mut self.alerts,
            &mut self.help,
            &mut self.header,
            &mut self.table,
            &mut self.notices,
            &mut self.status,
            &mut self.prompt,
        ]
    }
}

impl CommandHandler for RootView {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        vec![
            // The order is NOT significant
            &mut self.alerts,
            &mut self.header,
            &mut self.help,
            &mut self.notices,
            &mut self.prompt,
            &mut self.status,
            &mut self.table,
        ]
    }
}

impl View for RootView {
    fn constraint(&self, _: Rect, _: &AppState) -> Constraint {
        unreachable!("RootView is the top-level view that always receives the full terminal area directly from App, so its constraint should never be called")
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, state: &AppState, theme: &Theme) {
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            render_resize_message(frame.buffer_mut(), area, theme);
            return;
        }

        // Fill the entire frame with the base background color so that uncovered areas
        // (e.g. continuation lines of wrapped filenames, empty space below the last row)
        // show the correct color rather than the terminal default.
        Block::default().style(theme.background()).render(area, frame.buffer_mut());

        let views = self.views();
        let constraints: Vec<Constraint> = views
            .iter()
            .map(|view| view.constraint(area, state))
            .collect();
        Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area)
            .iter()
            .zip(views)
            .for_each(|(area, handler)| handler.render(*area, frame, state, theme));
    }
}

fn render_resize_message(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let widget = Paragraph::new(RESIZE_WINDOW)
        .style(theme.alert_error())
        .wrap(Wrap { trim: true });
    widget.render(area, buf);
}
