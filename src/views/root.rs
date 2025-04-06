use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Paragraph, Widget, Wrap},
    Frame,
};

use super::{
    errors::ErrorsView, header::HeaderView, help::HelpView, notices::NoticesView,
    prompt::PromptView, status::StatusView, table::TableView, View,
};
use crate::{
    app::{config::theme::Theme, config::Config},
    command::{handler::CommandHandler, mode::InputMode},
};

const MIN_WIDTH: u16 = 10;
const MIN_HEIGHT: u16 = 8;
const RESIZE_WINDOW: &'static str = "Resize window";

#[derive(Default)]
pub struct RootView {
    errors: ErrorsView,
    header: HeaderView,
    help: HelpView,
    notices: NoticesView,
    prompt: PromptView,
    last_rendered_area: Rect,
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

    pub fn update_cursor(&mut self, frame: &mut Frame<'_>, mode: &InputMode) {
        let cursor_position = self.prompt.cursor_position(&mode);
        if let Some(position) = cursor_position {
            frame.set_cursor_position(position);
        }
    }

    fn views(&mut self) -> Vec<&mut dyn View> {
        let views: Vec<&mut dyn View> = vec![
            &mut self.errors,
            &mut self.help,
            &mut self.header,
            &mut self.table,
            &mut self.notices,
            &mut self.prompt,
            &mut self.status,
        ];
        views
    }
}

impl CommandHandler for RootView {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        vec![
            // The order is NOT significant
            &mut self.errors,
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
    fn constraint(&self, _: Rect, _: &InputMode) -> Constraint {
        unreachable!("RootView is the top-level view that always receives the full terminal area directly from App, so its constraint should never be called")
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, mode: &InputMode, theme: &Theme) {
        self.last_rendered_area = area;

        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            render_resize_message(buf, area, theme);
            return;
        }

        let views = self.views();
        let constraints: Vec<Constraint> = views
            .iter()
            .map(|view| view.constraint(area, mode))
            .collect();
        Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area)
            .into_iter()
            .zip(views)
            .for_each(|(area, handler)| handler.render(*area, buf, mode, theme));
    }
}

fn render_resize_message(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let widget = Paragraph::new(RESIZE_WINDOW)
        .style(theme.error())
        .wrap(Wrap { trim: true });
    widget.render(area, buf);
}
