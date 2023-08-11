use super::{bordered, View};
use crate::{
    app::{
        focus::Focus,
        style::{header_component_active_style, header_component_style},
    },
    command::{handler::CommandHandler, result::CommandResult, Command},
    file_system::human::HumanPath,
};
use ratatui::{
    backend::Backend,
    layout::Rect,
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};

#[derive(Default)]
pub(super) struct HeaderView {
    directory: HumanPath,
}

impl HeaderView {
    pub(super) fn height(&self, rect: Rect) -> u16 {
        self.spans(rect.width as u16).len() as u16
    }

    fn set_directory(&mut self, directory: HumanPath) -> CommandResult {
        self.directory = directory;
        CommandResult::none()
    }

    fn spans(&self, width: u16) -> Vec<Vec<Span<'_>>> {
        let active_style = header_component_active_style();
        let style = header_component_style();

        let path = self.directory.path.clone();
        let mut container = vec![Vec::new()];
        let mut line_len = 0;
        let mut it = path.split('/').peekable();

        it.next(); // Skip empty string

        while let Some(part) = it.next() {
            let part = format!("/{}", part);
            let is_last = it.peek().is_none();
            let style = if is_last { active_style } else { style };

            line_len += part.len();
            if line_len as u16 > width {
                // New line
                container.push(Vec::new());
                line_len = part.len();
            }

            let line = &mut container.last_mut().unwrap();
            line.push(Span::styled(part, style));
        }
        container
    }
}

impl CommandHandler for HeaderView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::SetDirectory(directory, _) => self.set_directory(directory.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn is_focussed(&self, focus: &Focus) -> bool {
        *focus == Focus::Header
    }
}

impl<B: Backend> View<B> for HeaderView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _: &Focus) {
        let mut text = Text::default();
        self.spans(rect.width)
            .into_iter()
            .for_each(|spans| text.extend(Text::from(Line::from(spans))));
        let paragraph = Paragraph::new(text);
        frame.render_widget(paragraph, rect);
    }
}
