use super::{len_utf8, truncate_left_utf8_with_ellipsis, View};
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
enum Clipboard {
    Copy(HumanPath),
    Cut(HumanPath),
    #[default]
    None,
}

impl Clipboard {
    fn is_some(&self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Default)]
pub(super) struct StatusView {
    clipboard: Clipboard,
    directory: HumanPath,
    directory_len: usize,
    filter: String,
    rect: Rect,
    selected: Option<HumanPath>,
}

impl StatusView {
    fn clipboard_widget(&mut self, theme: &Theme) -> Paragraph<'_> {
        let (label, path) = match &self.clipboard {
            Clipboard::Copy(path) => ("Copy", path),
            Clipboard::Cut(path) => ("Cut", path),
            Clipboard::None => unreachable!(),
        };
        let bold_style = Style::default().add_modifier(Modifier::BOLD);
        let width = len_utf8(label) + 4; // 2 for spaces + 2 for quotation marks
        let width = self.rect.width.saturating_sub(width);
        let path = truncate_left_utf8_with_ellipsis(&path.path, width);
        let spans = vec![
            Span::raw(format!(" {label} \"")),
            Span::styled(path, bold_style),
            Span::raw("\""),
        ];
        Paragraph::new(Line::from(spans)).style(theme.status_clipboard())
    }

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

    fn set_clipboard_copy(&mut self, path: HumanPath) -> CommandResult {
        self.clipboard = Clipboard::Copy(path);
        CommandResult::none()
    }

    fn set_clipboard_cut(&mut self, path: HumanPath) -> CommandResult {
        self.clipboard = Clipboard::Cut(path);
        CommandResult::none()
    }

    fn set_directory(&mut self, directory: HumanPath, children: &Vec<HumanPath>) -> CommandResult {
        self.clipboard = Clipboard::None;
        self.directory = directory;
        self.directory_len = children.len();
        CommandResult::none()
    }

    fn set_filter(&mut self, filter: String) -> CommandResult {
        self.clipboard = Clipboard::None;
        self.filter = filter;
        CommandResult::none()
    }

    fn set_selected(&mut self, selected: Option<HumanPath>) -> CommandResult {
        self.clipboard = Clipboard::None;
        self.selected = selected;
        CommandResult::none()
    }
}

impl CommandHandler for StatusView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::ClipboardCopy(path) => self.set_clipboard_copy(path.clone()),
            Command::ClipboardCut(path) => self.set_clipboard_cut(path.clone()),
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
        self.rect = rect;

        let widget = if self.clipboard.is_some() {
            self.clipboard_widget(theme)
        } else if !self.filter.is_empty() {
            self.filter_widget(theme)
        } else {
            self.normal_widget(theme)
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
