use super::View;
use crate::{
    app::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command},
    file_system::human::HumanPath,
};
use ratatui::{
    backend::Backend,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
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
    fn filter_widget(&mut self, theme: &Theme) -> Paragraph<'_> {
        let spans = vec![
            Span::raw(" Filtered by \""),
            Span::styled(&self.filter, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("\". Press "),
            Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" to exit filtered mode."),
        ];
        Paragraph::new(Line::from(spans)).style(theme.status_filtered_mode())
    }

    fn normal_widget(&mut self, theme: &Theme) -> Paragraph<'_> {
        let directory_style = theme.status_directory();
        let directory_value_style = directory_style.add_modifier(Modifier::BOLD);
        let selected_style = theme.status_selected();
        let selected_value_style = selected_style.add_modifier(Modifier::BOLD);

        let mut spans = vec![
            Span::styled(" Directory ", theme.status_directory_label()),
            Span::styled(" Mode:", directory_style),
            Span::styled(self.directory.mode(), directory_value_style),
            Span::styled(" #Items:", directory_style),
            Span::styled(self.directory_len.to_string() + " ", directory_value_style),
        ];

        if let Some(selected) = &self.selected {
            spans.push(Span::styled(" Selected ", theme.status_selected_label()));
            spans.push(Span::styled(" Type:", selected_style));

            if selected.is_block_device() {
                spans.push(Span::styled("Block", selected_value_style));
            }
            if selected.is_character_device() {
                spans.push(Span::styled("Character", selected_value_style));
            }
            if selected.is_directory() {
                spans.push(Span::styled("Directory", selected_value_style));
            }
            if selected.is_fifo() {
                spans.push(Span::styled("FIFO", selected_value_style));
            }
            if selected.is_file() {
                spans.push(Span::styled("File", selected_value_style));
            }
            if selected.is_setgid() {
                spans.push(Span::styled("SetGID", selected_value_style));
            }
            if selected.is_setuid() {
                spans.push(Span::styled("SetUID", selected_value_style));
            }
            if selected.is_socket() {
                spans.push(Span::styled("Socket", selected_value_style));
            }
            if selected.is_sticky() {
                spans.push(Span::styled("Sticky", selected_value_style));
            }
            if selected.is_symlink() {
                spans.push(Span::styled("Symlink", selected_value_style));
            }
            spans.push(Span::styled(" Accessed:", selected_style));
            spans.push(Span::styled(selected.accessed(), selected_value_style));
            spans.push(Span::styled(" Created:", selected_style));
            spans.push(Span::styled(selected.created(), selected_value_style));
        }
        Paragraph::new(Line::from(spans))
            .style(theme.status_normal_mode())
            .wrap(Wrap { trim: false })
    }

    fn set_directory(&mut self, directory: HumanPath, children: &Vec<HumanPath>) -> CommandResult {
        self.directory = directory;
        self.directory_len = children.len();
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
                self.set_directory(directory.clone(), children)
            }
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            Command::SetSelected(selected) => self.set_selected(selected.clone()),
            _ => CommandResult::NotHandled,
        }
    }
}

impl<B: Backend> View<B> for StatusView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _: &InputMode, theme: &Theme) {
        let widget = if self.filter.is_empty() {
            self.normal_widget(theme)
        } else {
            self.filter_widget(theme)
        };
        frame.render_widget(widget, rect);
    }
}
