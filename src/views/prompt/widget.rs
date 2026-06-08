use ratatui::widgets::Paragraph;

use crate::app::config::theme::Theme;

/// Full-width label paragraph for a Delete confirmation prompt.
pub(super) fn delete_label_widget(label: String, theme: &Theme) -> Paragraph<'static> {
    Paragraph::new(label).style(theme.prompt.delete())
}

/// Label paragraph shown to the left of the input for all other prompts.
pub(super) fn label_widget(label: String, theme: &Theme) -> Paragraph<'static> {
    Paragraph::new(label).style(theme.prompt.label())
}

/// The muted Goto type-ahead overlay text: the completion `suffix`, plus a
/// `(n of total)` match counter when more than one suggestion is available.
pub(super) fn suggestion_overlay_text(suffix: String, index: usize, total: usize) -> String {
    if total > 1 {
        format!("{suffix} ({} of {total})", index + 1)
    } else {
        suffix
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test_case("ple/",  0, 1 => "ple/"          ; "single suggestion shows only the suffix")]
    #[test_case("ple/",  0, 3 => "ple/ (1 of 3)" ; "multiple suggestions append a 1-based counter")]
    #[test_case("ricot", 1, 2 => "ricot (2 of 2)"; "counter reflects the active index")]
    fn suggestion_overlay_text_cases(suffix: &str, index: usize, total: usize) -> String {
        suggestion_overlay_text(suffix.to_string(), index, total)
    }
}
