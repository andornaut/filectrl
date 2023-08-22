use super::{len_utf8, View};
use crate::{
    app::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command},
    file_system::human::HumanPath,
};
use ratatui::{
    backend::Backend,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::path::MAIN_SEPARATOR;

#[derive(Default)]
pub(super) struct HeaderView {
    directory: HumanPath,
    last_rendered_rect: Rect,
}

impl HeaderView {
    pub(super) fn height(&self, parent_rect: Rect) -> u16 {
        // If the `rect.width` hasn't changed, then use the cached height to avoid some work.
        let width = parent_rect.width as u16;
        if self.last_rendered_rect.width == width {
            self.last_rendered_rect.height
        } else {
            let style = Style::default();
            spans(&self.directory.breadcrumbs(), width, style, style).len() as u16
        }
    }

    fn set_directory(&mut self, directory: HumanPath) -> CommandResult {
        self.directory = directory;
        CommandResult::none()
    }
}

impl CommandHandler for HeaderView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::SetDirectory(directory, _) => self.set_directory(directory.clone()),
            _ => CommandResult::NotHandled,
        }
    }
}

impl<B: Backend> View<B> for HeaderView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _: &InputMode, theme: &Theme) {
        self.last_rendered_rect = rect;

        let active_style = theme.header_active();
        let inactive_style = theme.header();
        let breadcrumbs = self.directory.breadcrumbs();
        let text: Vec<_> = spans(
            &breadcrumbs,
            self.last_rendered_rect.width,
            active_style,
            inactive_style,
        )
        .into_iter()
        .map(|spans| Line::from(spans))
        .collect();

        let paragraph = Paragraph::new(text).style(theme.header());
        frame.render_widget(paragraph, self.last_rendered_rect);
    }
}

fn spans<'a>(
    breadcrumbs: &[String],
    width: u16,
    active_style: Style,
    inactive_style: Style,
) -> Vec<Vec<Span<'a>>> {
    let mut container = vec![Vec::new()];
    let mut line_len = 0;
    let mut it = breadcrumbs.into_iter().peekable();

    while let Some(name) = it.next() {
        let name = format!("{}{MAIN_SEPARATOR}", name);
        let is_last = it.peek().is_none();
        let style = if is_last {
            active_style
        } else {
            inactive_style
        };

        let name_len = len_utf8(&name);
        line_len += name_len;
        if line_len > width {
            // New line
            container.push(Vec::new());
            line_len = name_len;
        }

        let line = &mut container.last_mut().unwrap();
        line.push(Span::styled(name, style));
    }
    container
}
