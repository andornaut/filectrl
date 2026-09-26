use ratatui::widgets::Paragraph;

use super::LabelLine;
use crate::{app::config::theme::Theme, views::unicode::fit_left};

/// Full-width label for a single-keypress confirmation prompt (delete, paste conflict).
/// Each line fits `width`; a path that does not fit loses its start to an ellipsis.
pub(super) fn confirmation_label_widget(
    lines: &[LabelLine],
    width: u16,
    theme: &Theme,
) -> Paragraph<'static> {
    let text: Vec<String> = lines
        .iter()
        .map(|line| fit_left(&line.before, &line.path, &line.after, usize::from(width)))
        .collect();
    Paragraph::new(text.join("\n")).style(theme.prompt.delete())
}

/// Label shown to the left of the input for other prompts.
pub(super) fn label_widget(label: String, theme: &Theme) -> Paragraph<'static> {
    Paragraph::new(label).style(theme.prompt.label())
}

/// The muted Goto overlay: the completion `suffix` (escaped) plus `(n of total)` when there are
/// several.
pub(super) fn suggestion_overlay_text(suffix: &str, index: usize, total: usize) -> String {
    let suffix = crate::visible(suffix);
    if total > 1 {
        format!("{suffix} ({} of {total})", index + 1)
    } else {
        suffix.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test_case("ple/",  0, 1 => "ple/"          ; "single suggestion shows only the suffix")]
    #[test_case("ple/",  0, 3 => "ple/ (1 of 3)" ; "multiple suggestions append a 1-based counter")]
    #[test_case("ricot", 1, 2 => "ricot (2 of 2)"; "counter reflects the active index")]
    fn suggestion_overlay_counts_only_multiple_suggestions(
        suffix: &str,
        index: usize,
        total: usize,
    ) -> String {
        suggestion_overlay_text(suffix, index, total)
    }

    #[test]
    fn a_disguising_suffix_is_spelled_out() {
        assert_eq!(
            "a\\u{202e}txt (1 of 2)",
            suggestion_overlay_text("a\u{202e}txt", 0, 2)
        );
    }
}
