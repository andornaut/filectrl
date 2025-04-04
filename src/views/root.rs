use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Paragraph, Widget, Wrap},
    Frame,
};

use super::{
    errors::ErrorsView, header::HeaderView, help::HelpView, prompt::PromptView, status::StatusView,
    table::TableView, View,
};
use crate::{
    app::{config::theme::Theme, config::Config},
    command::{handler::CommandHandler, mode::InputMode},
};

const MIN_HEIGHT: u16 = 6;
const MIN_WIDTH: u16 = 10;
const RESIZE_WINDOW: &'static str = "Resize window";

#[derive(Default)]
pub struct RootView {
    errors: ErrorsView,
    header: HeaderView,
    help: HelpView,
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
}

impl CommandHandler for RootView {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        vec![
            &mut self.errors,
            &mut self.header,
            &mut self.help,
            &mut self.prompt,
            &mut self.status,
            &mut self.table,
        ]
    }
}

impl View for RootView {
    fn render(&mut self, buf: &mut Buffer, area: Rect, mode: &InputMode, theme: &Theme) {
        self.last_rendered_area = area;

        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            render_resize_message(buf, area, theme);
            return;
        }

        let constraints = vec![
            Constraint::Length(self.errors.height(area.width)),
            Constraint::Length(self.help.height()),
            Constraint::Length(self.header.height(area.width, theme)),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(self.prompt.height(mode)),
        ];
        let handlers: Vec<&mut dyn View> = vec![
            // The order is significant
            &mut self.errors,
            &mut self.help,
            &mut self.header,
            &mut self.table,
            &mut self.status,
            &mut self.prompt,
        ];
        Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area)
            .into_iter()
            .zip(handlers.into_iter())
            .for_each(|(chunk, handler): (&Rect, &mut dyn View)| {
                handler.render(buf, *chunk, mode, theme)
            });
    }
}

fn render_resize_message(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let widget = Paragraph::new(RESIZE_WINDOW)
        .style(theme.error())
        .wrap(Wrap { trim: true });
    widget.render(area, buf);
}
