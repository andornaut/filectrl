use super::{
    errors::ErrorsView, header::HeaderView, help::HelpView, prompt::PromptView, status::StatusView,
    table::TableView, View,
};
use crate::{
    app::{config::theme::Theme, config::Config},
    command::{handler::CommandHandler, mode::InputMode},
};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    prelude::{Backend, Rect},
    widgets::{Paragraph, Wrap},
    Frame,
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
        let errors: &mut dyn CommandHandler = &mut self.errors;
        let header: &mut dyn CommandHandler = &mut self.header;
        let help: &mut dyn CommandHandler = &mut self.help;
        let prompt: &mut dyn CommandHandler = &mut self.prompt;
        let status: &mut dyn CommandHandler = &mut self.status;
        let table: &mut dyn CommandHandler = &mut self.table;
        vec![errors, header, help, prompt, status, table]
    }
}

impl<B: Backend> View<B> for RootView {
    fn render(&mut self, frame: &mut Frame, rect: Rect, mode: &InputMode, theme: &Theme) {
        self.last_rendered_rect = rect;

        if rect.width < MIN_WIDTH || rect.height < MIN_HEIGHT {
            frame.render_widget(
                Paragraph::new(RESIZE_WINDOW)
                    .style(theme.error())
                    .wrap(Wrap { trim: true }),
                rect,
            );
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
        let handlers: Vec<&mut dyn View<_>> = vec![
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
            .for_each(|(chunk, handler): (&Rect, &mut dyn View<B>)| {
                handler.render(frame, *chunk, mode, theme)
            });
    }
}
