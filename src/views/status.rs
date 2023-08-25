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
    widgets::Paragraph,
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
        let bold_style = Style::default().add_modifier(Modifier::BOLD);
        let spans = vec![
            Span::raw(" Filtered by \""),
            Span::styled(&self.filter, bold_style),
            Span::raw("\". Press "),
            Span::styled("Esc", bold_style),
            Span::raw(" to exit filtered mode."),
        ];
        Paragraph::new(Line::from(spans)).style(theme.status_filter())
    }

    fn normal_widget(&mut self, theme: &Theme) -> Paragraph<'_> {
        let mut spans = Vec::new();
        add_directory(&mut spans, theme, self.directory.mode(), self.directory_len);

        if let Some(selected) = &self.selected {
            add_selected(&mut spans, theme, selected);
        }
        Paragraph::new(Line::from(spans)).style(theme.status_selected())
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

fn add_directory(spans: &mut Vec<Span>, theme: &Theme, mode: String, len: usize) {
    spans.push(Span::styled(" Directory ", theme.status_directory_label()));
    let fields = vec![(" Mode:", mode), (" #Items:", format!("{} ", len))];
    let default_style = theme.status_directory();
    let label_style = default_style.add_modifier(Modifier::BOLD);
    spans.extend(to_entries(fields, default_style, label_style));
}

fn add_selected(spans: &mut Vec<Span>, theme: &Theme, selected: &HumanPath) {
    spans.push(Span::styled(" Selected ", theme.status_selected_label()));
    let mut fields = Vec::new();
    if let Some(owner) = selected.owner() {
        fields.push((" Owner:", owner));
    }
    if let Some(group) = selected.group() {
        fields.push((" Group:", group));
    }
    fields.push((" Type:", kind_field(selected)));
    if let Some(accessed) = selected.accessed() {
        fields.push((" Accessed:", accessed));
    }
    if let Some(created) = selected.created() {
        fields.push((" Created:", created));
    }
    let default_style = theme.status_selected();
    let label_style = default_style.add_modifier(Modifier::BOLD);
    spans.extend(to_entries(fields, default_style, label_style));
}

fn kind_field(selected: &HumanPath) -> String {
    let mut kind = Vec::new();
    if selected.is_block_device() {
        kind.push("Block");
    }
    if selected.is_character_device() {
        kind.push("Character");
    }
    if selected.is_directory() {
        kind.push("Directory");
    }
    if selected.is_fifo() {
        kind.push("FIFO");
    }
    if selected.is_file() {
        kind.push("File");
    }
    if selected.is_setgid() {
        kind.push("SetGID");
    }
    if selected.is_setuid() {
        kind.push("SetUID");
    }
    if selected.is_socket() {
        kind.push("Socket");
    }
    if selected.is_sticky() {
        kind.push("Sticky");
    }
    if selected.is_symlink() {
        kind.push("Symlink");
    }
    kind.join(",")
}

fn to_entries(
    entries: Vec<(&str, String)>,
    default_style: Style,
    label_style: Style,
) -> Vec<Span<'_>> {
    entries
        .into_iter()
        .flat_map(|(label, value)| {
            [
                Span::styled(label, label_style),
                Span::styled(value, default_style),
            ]
        })
        .collect()
}
