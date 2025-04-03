use ratatui::{
    layout::{Constraint, Direction, Layout},
    prelude::Rect,
    widgets::{Paragraph, Wrap},
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
    last_rendered_rect: Rect,
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
    fn render(&mut self, frame: &mut Frame, rect: Rect, mode: &InputMode, theme: &Theme) {
        self.last_rendered_rect = rect;

        if rect.width < MIN_WIDTH || rect.height < MIN_HEIGHT {
            render_resize_message(frame, rect, theme);
            return;
        }

        let constraints = vec![
            Constraint::Length(self.errors.height(rect.width)),
            Constraint::Length(self.help.height()),
            Constraint::Length(self.header.height(rect.width, theme)),
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
            .split(rect)
            .into_iter()
            .zip(handlers.into_iter())
            .for_each(|(chunk, handler): (&Rect, &mut dyn View)| {
                handler.render(frame, *chunk, mode, theme)
            });
    }
}

fn render_resize_message(frame: &mut Frame<'_>, rect: Rect, theme: &Theme) {
    frame.render_widget(
        Paragraph::new(RESIZE_WINDOW)
            .style(theme.error())
            .wrap(Wrap { trim: true }),
        rect,
    );
}
