use super::{len_utf8, View};
use crate::{
    app::{config::Theme, focus::Focus},
    command::{handler::CommandHandler, result::CommandResult, Command},
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

#[derive(Default)]
pub(super) struct HeaderView {
    directory: HumanPath,
}

impl HeaderView {
    pub(super) fn height(&self, rect: Rect) -> u16 {
        // TODO: It's wasteful to do this twice per render(). Consider alternatives.
        let style = Style::default();
        spans(style, style, &self.directory.path, rect.width as u16).len() as u16
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

    fn is_focussed(&self, focus: &Focus) -> bool {
        *focus == Focus::Header
    }
}

impl<B: Backend> View<B> for HeaderView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _: &Focus, theme: &Theme) {
        let active_style = theme.header_active();
        let inactive_style = theme.header();
        let text: Vec<_> = spans(
            active_style,
            inactive_style,
            &self.directory.path,
            rect.width,
        )
        .into_iter()
        .map(|spans| Line::from(spans))
        .collect();
        let paragraph = Paragraph::new(text).style(theme.header());
        frame.render_widget(paragraph, rect);
    }
}

fn spans<'a>(
    active_style: Style,
    inactive_style: Style,
    path: &'a str,
    width: u16,
) -> Vec<Vec<Span<'a>>> {
    let mut container = vec![Vec::new()];
    let mut line_len = 0;
    let mut it = path.split('/').peekable();

    it.next(); // Skip empty string

    while let Some(name) = it.next() {
        let name = format!("/{}", name);
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
