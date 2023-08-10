use super::View;
use crate::{
    app::{
        focus::Focus,
        style::{
            status_directory_style, status_filter_mode_style, status_normal_mode_style,
            status_selected_style,
        },
    },
    command::{handler::CommandHandler, result::CommandResult, Command},
    file_system::human::HumanPath,
};
use ratatui::{
    backend::Backend,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
    Frame,
};

#[derive(Default)]
pub(super) struct StatusView {
    directory: HumanPath,
    directory_len: usize,
    filter: String,
    selected: Option<HumanPath>,
}

impl StatusView {
    fn set_directory(&mut self, directory: HumanPath, directory_len: usize) -> CommandResult {
        self.directory = directory;
        self.directory_len = directory_len;
        CommandResult::none()
    }

    fn set_filter(&mut self, filter: String) -> CommandResult {
        self.filter = filter;
        CommandResult::none()
    }

    fn set_selected(&mut self, selected: Option<HumanPath>) -> CommandResult {
        self.selected = selected;
        CommandResult::none()
    }
}

impl CommandHandler for StatusView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::SetDirectory(directory, children) => {
                self.set_directory(directory.clone(), children.len())
            }
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            Command::SetSelected(selected) => self.set_selected(selected.clone()),
            _ => CommandResult::NotHandled,
        }
    }
}

impl<B: Backend> View<B> for StatusView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _: &Focus) {
        if !self.filter.is_empty() {
            let spans = vec![
                Span::raw("Filtered by \""),
                Span::styled(&self.filter, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("\". Press "),
                Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" to exit filtered mode."),
            ];
            let paragraph = Paragraph::new(Line::from(spans)).style(status_filter_mode_style());
            frame.render_widget(paragraph, rect);
            return;
        }

        let directory_style = status_directory_style();
        let directory_value_style = directory_style.add_modifier(Modifier::BOLD);
        let selected_style = status_selected_style();
        let selected_value_style = selected_style.add_modifier(Modifier::BOLD);

        let mut spans = vec![
            Span::styled("Directory mode:", directory_style),
            Span::styled(self.directory.mode(), directory_value_style),
            Span::styled(" #items:", directory_style),
            Span::styled(self.directory_len.to_string(), directory_value_style),
        ];

        if let Some(selected) = &self.selected {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("Selected type:", selected_style));

            if selected.is_block_device() {
                spans.push(Span::styled("block", selected_value_style));
            }
            if selected.is_char_device() {
                spans.push(Span::styled("character", selected_value_style));
            }
            if selected.is_dir() {
                spans.push(Span::styled("directory", selected_value_style));
            }
            if selected.is_fifo() {
                spans.push(Span::styled("FIFO", selected_value_style));
            }
            if selected.is_file() {
                spans.push(Span::styled("file", selected_value_style));
            }
            if selected.is_setgid() {
                spans.push(Span::styled("SetGID", selected_value_style));
            }
            if selected.is_setuid() {
                spans.push(Span::styled("SetUID", selected_value_style));
            }
            if selected.is_socket() {
                spans.push(Span::styled("socket", selected_value_style));
            }
            if selected.is_sticky() {
                spans.push(Span::styled("sticky", selected_value_style));
            }
            if selected.is_symlink() {
                spans.push(Span::styled("symlink", selected_value_style));
            }
            spans.push(Span::styled(" Accessed:", selected_style));
            spans.push(Span::styled(selected.accessed(), selected_value_style));
            spans.push(Span::styled(" Created:", selected_style));
            spans.push(Span::styled(selected.created(), selected_value_style));
        }
        let text = Text::from(Line::from(spans));
        let paragraph = Paragraph::new(text)
            .style(status_normal_mode_style())
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, rect);
    }
}
