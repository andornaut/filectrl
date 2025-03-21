use super::{Clipboard, StatusView};
use crate::{
    app::theme::Theme,
    command::{
        mode::InputMode,
        task::{Progress, Task},
    },
    file_system::human::HumanPath,
    views::{len_utf8, truncate_left_utf8_with_ellipsis, View},
};
use ratatui::{
    prelude::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::collections::HashSet;

impl View for StatusView {
    fn render(&mut self, frame: &mut Frame, rect: Rect, _: &InputMode, theme: &Theme) {
        self.rect = rect;

        let widget = if !self.tasks.is_empty() {
            progress_paragraph(&self.tasks, theme, rect.width)
        } else if self.clipboard.is_some() {
            clipboard_paragraph(&self.clipboard, self.rect.width, theme)
        } else if !self.filter.is_empty() {
            filter_paragraph(&self.filter, theme)
        } else {
            default_paragraph(&self.directory, self.directory_len, &self.selected, theme)
        };
        frame.render_widget(widget, rect);
    }
}

fn clipboard_paragraph<'a>(clipboard: &'a Clipboard, width: u16, theme: &Theme) -> Paragraph<'a> {
    let (label, path) = match clipboard {
        Clipboard::Copy(path) => ("Copied", path),
        Clipboard::Cut(path) => ("Cut", path),
        Clipboard::None => unreachable!(),
    };
    let bold_style = Style::default().add_modifier(Modifier::BOLD);
    let width = width.saturating_sub(len_utf8(label) + 4); // 2for spaces + 2 for quotation marks
    let path = truncate_left_utf8_with_ellipsis(&path.path, width);
    let spans = vec![
        Span::raw(format!(" {label} \"")),
        Span::styled(path, bold_style),
        Span::raw("\""),
    ];
    Paragraph::new(Line::from(spans)).style(theme.status_clipboard())
}

fn filter_paragraph<'a>(filter: &'a str, theme: &Theme) -> Paragraph<'a> {
    let bold_style = Style::default().add_modifier(Modifier::BOLD);
    let spans = vec![
        Span::raw(" Filtered by \""),
        Span::styled(filter, bold_style),
        Span::raw("\". Press "),
        Span::styled("Esc", bold_style),
        Span::raw(" to exit filtered mode."),
    ];
    Paragraph::new(Line::from(spans)).style(theme.status_filter())
}

fn default_paragraph<'a>(
    directory: &'a HumanPath,
    directory_len: usize,
    selected: &Option<HumanPath>,
    theme: &Theme,
) -> Paragraph<'a> {
    let mut spans = Vec::new();
    add_directory(&mut spans, theme, directory.mode(), directory_len);

    if let Some(selected) = &selected {
        add_selected(&mut spans, theme, selected);
    }
    Paragraph::new(Line::from(spans)).style(theme.status_selected())
}

fn progress_paragraph<'a>(tasks: &'a HashSet<Task>, theme: &Theme, width: u16) -> Paragraph<'a> {
    let mut progress = Progress(0, 0);
    let mut has_error = false;
    for task in tasks {
        progress = task.combine_progress(&progress);
        if task.is_error() {
            has_error = true;
        }
    }
    let current = progress.scaled(width);
    let text = "█".repeat(current as usize);
    let style = if has_error {
        theme.status_progress_error()
    } else if progress.is_done() {
        theme.status_progress_done()
    } else {
        theme.status_progress()
    };
    Paragraph::new(text).style(style)
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
