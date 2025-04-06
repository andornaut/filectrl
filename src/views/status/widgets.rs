use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{app::config::theme::Theme, file_system::path_info::PathInfo};

pub(super) fn default_widget<'a>(
    directory: &'a PathInfo,
    directory_len: usize,
    selected: &Option<PathInfo>,
    theme: &Theme,
) -> Paragraph<'a> {
    let mut spans = Vec::new();
    add_directory(&mut spans, theme, directory.mode(), directory_len);

    if let Some(selected) = &selected {
        add_selected(&mut spans, theme, selected);
    }
    Paragraph::new(Line::from(spans)).style(theme.status_selected())
}

fn add_directory(spans: &mut Vec<Span>, theme: &Theme, mode: String, len: usize) {
    spans.push(Span::styled(" Directory ", theme.status_directory_label()));
    let fields = vec![(" Mode:", mode), (" #Items:", format!("{} ", len))];
    let default_style = theme.status_directory();
    let label_style = default_style.add_modifier(Modifier::BOLD);
    spans.extend(to_entries(fields, default_style, label_style));
}

fn add_selected(spans: &mut Vec<Span>, theme: &Theme, selected: &PathInfo) {
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

fn kind_field(selected: &PathInfo) -> String {
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
    if selected.is_pipe() {
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
