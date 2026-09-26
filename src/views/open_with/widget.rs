use ratatui::{
    buffer::CellWidth,
    text::{Line, Span},
};

use super::MAX_SHORTCUT;
use crate::{app::config::theme::OpenWith, file_system::open_with::AppCandidate};

const DEFAULT_MARKER: &str = "(default)";
const NO_APPLICATIONS: &str = " No applications found";

/// One line per application: digit shortcut, name, and program.
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
    // A desktop file id or bundle identifier, which can hold any text.
    let program = crate::visible(&candidate.detail);
    let detail = match (candidate.is_default, program.is_empty()) {
        (false, _) => program.into_owned(),
        (true, true) => DEFAULT_MARKER.to_string(),
        (true, false) => format!("{program} {DEFAULT_MARKER}"),
    };
    let used = 1
        + shortcut.cell_width() as usize
        + candidate.name.cell_width() as usize
        + 2
        + detail.cell_width() as usize;
    let padding = " ".repeat((width as usize).saturating_sub(used));

    if is_selected {
        // One unstyled span, so the highlight line style applies uniformly.
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
    use ratatui::style::Style;
    use test_case::test_case;

    use super::{NO_APPLICATIONS, build_rows};
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
            setting: None,
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
        assert!(text(&rows[9]).starts_with("    App9"));
        assert!(text(&rows[10]).starts_with("    App10"));
    }

    #[test_case("prog" => " 2. Viewer  prog (default)" ; "after the program")]
    #[test_case("" => " 2. Viewer  (default)" ; "alone when there is no program")]
    fn the_default_application_is_marked(detail: &str) -> String {
        let mut viewer = candidate("Viewer", true);
        viewer.detail = detail.to_string();
        let rows = build_rows(theme(), 0, 0, &[candidate("App0", false), viewer]);
        text(&rows[1])
    }

    #[test_case(false ; "not the default")]
    #[test_case(true ; "the default")]
    fn the_program_is_shown_with_disguising_characters_spelled_out(is_default: bool) {
        let mut app = candidate("App", is_default);
        app.detail = "org.a\u{202e}pp".to_string();
        let rows = build_rows(theme(), 0, 0, &[app]);
        assert!(
            text(&rows[0]).starts_with(" 1. App  org.a\\u{202e}pp"),
            "{:?}",
            text(&rows[0])
        );
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
