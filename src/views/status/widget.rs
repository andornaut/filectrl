use chrono::Local;
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{app::config::theme::Theme, file_system::path_info::PathInfo};

pub(super) fn default_widget<'a>(
    directory: &'a PathInfo,
    directory_len: usize,
    selected: Option<&PathInfo>,
    theme: &Theme,
) -> Paragraph<'a> {
    let mut spans = Vec::new();
    add_directory(&mut spans, theme, directory.unix_mode(), directory_len);

    if let Some(selected) = &selected {
        add_selected(&mut spans, theme, selected);
    }
    Paragraph::new(Line::from(spans)).style(theme.status.detail())
}

fn add_directory(spans: &mut Vec<Span>, theme: &Theme, mode: String, len: usize) {
    spans.push(Span::styled(" Directory ", theme.status.label()));
    let fields = vec![(" Mode:", mode), (" # Items:", format!("{len} "))];
    let default_style = theme.status.detail();
    let label_style = default_style.add_modifier(Modifier::BOLD);
    spans.extend(to_entries(fields, default_style, label_style));
}

fn add_selected(spans: &mut Vec<Span>, theme: &Theme, selected: &PathInfo) {
    let now = Local::now();
    spans.push(Span::styled(" Selected ", theme.status.label()));
    let mut fields = Vec::new();
    if let Some(owner) = selected.owner() {
        fields.push((" Owner:", owner));
    }
    if let Some(group) = selected.group() {
        fields.push((" Group:", group));
    }
    fields.push((" Type:", kind_field(selected)));
    if let Some(accessed) = selected.accessed(now) {
        fields.push((" Accessed:", accessed));
    }
    if let Some(created) = selected.created(now) {
        fields.push((" Created:", created));
    }
    let default_style = theme.status.detail();
    let label_style = default_style.add_modifier(Modifier::BOLD);
    spans.extend(to_entries(fields, default_style, label_style));
}

fn kind_field(selected: &PathInfo) -> String {
    let mut kind = Vec::with_capacity(5); // Pre-allocate with reasonable capacity

    // File type flags (mutually exclusive)
    if selected.is_block_device() {
        kind.push("Block");
    } else if selected.is_character_device() {
        kind.push("Character");
    } else if selected.is_directory() {
        kind.push("Directory");
    } else if selected.is_pipe() {
        kind.push("FIFO");
    } else if selected.is_file() {
        kind.push("File");
    } else if selected.is_socket() {
        kind.push("Socket");
    }

    // A symlink's permission bits are 0777 by convention and the kernel
    // ignores them, so the flags below would put two words on every link that
    // describe nothing. The link's own type is the whole answer.
    if selected.is_symlink() {
        kind.push(if selected.is_symlink_broken() {
            "Broken Symlink"
        } else {
            "Symlink"
        });
        return kind.join(",");
    }

    // Special flags (can be combined)
    if selected.is_setgid() {
        kind.push("SetGID");
    }
    if selected.is_setuid() {
        kind.push("SetUID");
    }
    if selected.is_sticky() {
        kind.push("Sticky");
    }
    if selected.is_other_writable() {
        kind.push("Other Writable");
    }
    if selected.is_executable() {
        kind.push("Executable");
    }

    // Note: is_door() is not included as it's a Solaris-specific IPC mechanism
    // and would only be relevant on Solaris systems
    kind.join(",") // No space after comma, intentional to save status bar width
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

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::kind_field;
    use crate::file_system::path_info::PathInfo;

    // The mode bits each case stands for. A door is Solaris-only and cannot be
    // built here, which is also why `kind_field` leaves it out.
    const REGULAR: u32 = 0o100_644;
    const EXECUTABLE: u32 = 0o100_755;
    const SETUID_SETGID: u32 = 0o106_755;
    const DIRECTORY: u32 = 0o040_755;
    const DIRECTORY_STICKY_OTHER_WRITABLE: u32 = 0o041_757;
    const SYMLINK: u32 = 0o120_777;
    const FIFO: u32 = 0o010_644;
    const SOCKET: u32 = 0o140_644;
    const BLOCK_DEVICE: u32 = 0o060_644;
    const CHARACTER_DEVICE: u32 = 0o020_644;

    /// One base type, then whichever flags also apply, comma-joined with no
    /// space: the status bar is one line and this field competes with the rest
    /// of it for width.
    #[test_case(REGULAR => "File" ; "a plain file")]
    #[test_case(DIRECTORY => "Directory,Executable" ; "a directory carries its execute bit")]
    #[test_case(EXECUTABLE => "File,Executable" ; "an executable file")]
    #[test_case(FIFO => "FIFO" ; "a fifo")]
    #[test_case(SOCKET => "Socket" ; "a socket")]
    #[test_case(BLOCK_DEVICE => "Block" ; "a block device")]
    #[test_case(CHARACTER_DEVICE => "Character" ; "a character device")]
    // The flags accumulate where the base types do not: unlike `name_style`,
    // this field reports every property rather than the highest-ranked one.
    #[test_case(SETUID_SETGID => "File,SetGID,SetUID,Executable" ; "both special bits and the execute bit they imply")]
    #[test_case(DIRECTORY_STICKY_OTHER_WRITABLE => "Directory,Sticky,Other Writable,Executable" ; "a sticky, other-writable directory")]
    fn kind_field_reports(mode: u32) -> String {
        kind_field(&PathInfo::with_mode(mode))
    }

    #[test]
    fn a_symlink_reports_its_type_and_nothing_else() {
        // The fixture mode is 0777, which is what a link carries on disk. The
        // kernel ignores those bits, so none of them reach the field: the
        // status bar is one line and "Other Writable,Executable" on every link
        // would spend it saying nothing.
        assert_eq!("Symlink", kind_field(&PathInfo::with_mode(SYMLINK)));
        assert_eq!(
            "Broken Symlink",
            kind_field(&PathInfo::with_mode(SYMLINK).broken())
        );
    }
}
