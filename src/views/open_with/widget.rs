use ratatui::{
    buffer::CellWidth,
    text::{Line, Span},
};

use super::MAX_SHORTCUT;
use crate::{app::config::theme::OpenWith, file_system::open_with::AppCandidate};

const DEFAULT_MARKER: &str = "(default)";
const NO_APPLICATIONS: &str = " No applications found";

/// One line per application: a digit shortcut, the application name, and the
/// program behind it.
pub(super) fn build_rows(
    theme: &OpenWith,
    selected: usize,
    width: u16,
    candidates: &[AppCandidate],
) -> Vec<Line<'static>> {
    if candidates.is_empty() {
        return vec![Line::styled(NO_APPLICATIONS, theme.detail())];
    }
    candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| build_row(theme, index == selected, width, index, candidate))
        .collect()
}

fn build_row(
    theme: &OpenWith,
    is_selected: bool,
    width: u16,
    index: usize,
    candidate: &AppCandidate,
) -> Line<'static> {
    let shortcut = if index < MAX_SHORTCUT {
        format!("{}. ", index + 1)
    } else {
        " ".repeat(3)
    };
    let detail = match (candidate.is_default, candidate.detail.is_empty()) {
        (false, _) => candidate.detail.clone(),
        (true, true) => DEFAULT_MARKER.to_string(),
        (true, false) => format!("{} {DEFAULT_MARKER}", candidate.detail),
    };
    let used = 1
        + shortcut.cell_width() as usize
        + candidate.name.cell_width() as usize
        + 2
        + detail.cell_width() as usize;
    let padding = " ".repeat((width as usize).saturating_sub(used));

    if is_selected {
        // The whole row is highlighted, so it is one unstyled span that
        // inherits the line style rather than several competing ones.
        return Line::styled(
            format!(" {shortcut}{}  {detail}{padding}", candidate.name),
            theme.selected(),
        );
    }
    Line::from(vec![
        Span::raw(" "),
        Span::styled(shortcut, theme.shortcut()),
        Span::raw(candidate.name.clone()),
        Span::raw("  "),
        Span::styled(detail, theme.detail()),
        Span::raw(padding),
    ])
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::style::Style;

    use super::{DEFAULT_MARKER, NO_APPLICATIONS, build_rows};
    use crate::{
        app::config::{Config, theme::OpenWith},
        file_system::open_with::AppCandidate,
    };

    fn theme() -> &'static OpenWith {
        Config::init_test();
        &Config::global().theme().open_with
    }

    fn candidate(name: &str, is_default: bool) -> AppCandidate {
        AppCandidate {
            argv: vec!["prog".into()],
            detail: "prog".to_string(),
            is_default,
            name: name.to_string(),
            working_dir: None::<PathBuf>,
        }
    }

    fn candidates(count: usize) -> Vec<AppCandidate> {
        (0..count)
            .map(|index| candidate(&format!("App{index}"), false))
            .collect()
    }

    fn text(line: &ratatui::text::Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn an_empty_list_says_so() {
        let rows = build_rows(theme(), 0, 40, &[]);
        assert_eq!(1, rows.len());
        assert_eq!(NO_APPLICATIONS, text(&rows[0]));
    }

    #[test]
    fn only_the_first_nine_rows_get_a_digit_shortcut() {
        let rows = build_rows(theme(), 0, 40, &candidates(11));
        assert!(text(&rows[0]).starts_with(" 1. App0"));
        assert!(text(&rows[8]).starts_with(" 9. App8"));
        // Rows 10 and up keep the gutter but have no digit to press.
        assert!(text(&rows[9]).starts_with("    App9"));
        assert!(text(&rows[10]).starts_with("    App10"));
    }

    #[test]
    fn the_default_application_is_marked() {
        let rows = build_rows(theme(), 0, 40, &[candidate("Viewer", true)]);
        assert!(text(&rows[0]).contains(DEFAULT_MARKER));
    }

    #[test]
    fn only_the_selected_row_carries_the_selected_style() {
        let selected = theme().selected();
        let rows = build_rows(theme(), 1, 40, &candidates(3));
        assert_eq!(Style::default(), rows[0].style);
        assert_eq!(selected, rows[1].style);
        assert_eq!(Style::default(), rows[2].style);
    }

    #[test]
    fn the_selected_row_is_padded_so_the_highlight_spans_the_width() {
        let rows = build_rows(theme(), 0, 40, &candidates(1));
        assert_eq!(40, text(&rows[0]).chars().count());
    }

    #[test]
    fn a_row_wider_than_the_area_is_not_padded() {
        let rows = build_rows(theme(), 0, 4, &candidates(1));
        assert_eq!(" 1. App0  prog", text(&rows[0]));
    }
}
