use ratatui::widgets::Paragraph;

use super::LabelLine;
use crate::{app::config::theme::Theme, views::unicode::fit_left};

/// Full-width label paragraph for a single-keypress confirmation prompt, which
/// has no input area. Shared by the delete and paste-conflict prompts: both ask
/// the user to approve something destructive, so they read the same. Each line
/// is fitted to `width`, one row per line: a path that does not fit loses its
/// start to an ellipsis, so the file name and the text around it (the question
/// and its choices) stay visible.
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

/// Label paragraph shown to the left of the input for all other prompts.
pub(super) fn label_widget(label: String, theme: &Theme) -> Paragraph<'static> {
    Paragraph::new(label).style(theme.prompt.label())
}

/// The muted Goto type-ahead overlay text: the completion `suffix`, plus a
/// `(n of total)` match counter when more than one suggestion is available.
/// The suffix is part of a file name, so it is shown through `crate::visible`;
/// accepting it inserts the real name.
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

    /// Left raw, U+202E would draw the rest of the overlay, the counter
    /// included, right to left.
    #[test]
    fn a_disguising_suffix_is_spelled_out() {
        assert_eq!(
            "a\\u{202e}txt (1 of 2)",
            suggestion_overlay_text("a\u{202e}txt", 0, 2)
        );
    }
}
