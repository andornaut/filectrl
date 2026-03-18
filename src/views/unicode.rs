use textwrap::{wrap, Options, WordSplitter};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const ELLIPSIS: &str = "…";
const ELLIPSIS_WIDTH: usize = 1;

pub(super) fn split_with_ellipsis(line: &str, width: usize) -> Vec<String> {
    assert!(width > ELLIPSIS_WIDTH, "width > ELLIPSIS_WIDTH");

    let mut parts = split(line, width);
    let len = parts.len();
    if len > 1 {
        for part in &mut parts[..len - 1] {
            part.push_str(ELLIPSIS);
        }
    }
    parts
}

pub(super) fn truncate_left(line: &str, width: usize) -> String {
    assert!(width > ELLIPSIS_WIDTH, "width > ELLIPSIS_WIDTH");

    if line.width() <= width {
        return line.into();
    }

    let remaining_width = width.saturating_sub(ELLIPSIS_WIDTH);

    let mut total_width = 0;
    let mut end_index = line.len();

    for (idx, g) in line.grapheme_indices(true).rev() {
        let g_width = g.width();
        if total_width + g_width > remaining_width {
            break;
        }
        total_width += g_width;
        end_index = idx;
    }

    let mut result = String::with_capacity(width);
    result.push_str(ELLIPSIS);
    result.push_str(&line[end_index..]);
    result
}

fn split(line: &str, width: usize) -> Vec<String> {
    if line.width() <= width {
        return vec![line.into()];
    }

    let width = width.saturating_sub(ELLIPSIS_WIDTH);
    let options = Options::new(width)
        .word_splitter(WordSplitter::NoHyphenation)
        .break_words(true);

    wrap(line, options)
        .into_iter()
        .map(|s| s.into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    // ── split_with_ellipsis ───────────────────────────────────────────────────

    #[test_case(vec!["example"],              "example", 7; "fits unchanged at exact width")]
    #[test_case(vec!["examp…", "le"],         "example", 6; "two parts at width minus 1")]
    #[test_case(vec!["exa…", "mpl…", "e"],   "example", 4; "three parts")]
    fn split_with_ellipsis_ascii(expected: Vec<&str>, text: &str, width: usize) {
        assert_eq!(expected, split_with_ellipsis(text, width));
    }

    #[test]
    fn split_with_ellipsis_cjk_measures_display_width_not_bytes() {
        // "中文" has byte length 6 but display width 4; fits in one part at width 4
        assert_eq!(vec!["中文"], split_with_ellipsis("中文", 4));
    }

    #[test]
    #[should_panic(expected = "width > ELLIPSIS_WIDTH")]
    fn split_with_ellipsis_panics_when_width_equals_ellipsis_width() {
        split_with_ellipsis("example", 1);
    }

    // ── truncate_left ─────────────────────────────────────────────────────────

    #[test_case("example", "example", 7; "fits unchanged at exact width")]
    #[test_case("example", "example", 8; "fits unchanged when wider than needed")]
    #[test_case("…ample",  "example", 6; "truncates at width minus 1")]
    #[test_case("…e",      "example", 2; "truncates to minimum useful width")]
    fn truncate_left_ascii(expected: &str, text: &str, width: usize) {
        assert_eq!(expected, truncate_left(text, width));
    }

    // CJK characters have display width 2 each.
    #[test_case("中文",   "中文",   4; "fits unchanged when display width equals target")]
    #[test_case("…文字", "中文字", 5; "two wide chars fit in remaining width")]
    #[test_case("…字",   "中文字", 3; "wide char that would overflow is excluded")]
    fn truncate_left_cjk(expected: &str, text: &str, width: usize) {
        assert_eq!(expected, truncate_left(text, width));
    }

    // A base character followed by a combining accent forms one grapheme cluster
    // (display width 1) stored as two Unicode scalar values. Splitting between them
    // produces an orphaned combining character, which is visually broken. The
    // old char_indices() approach would produce "…\u{0301}f" here; the grapheme-
    // cluster approach correctly yields "…f".
    #[test_case("e\u{0301}f", "e\u{0301}f", 3; "combining char string fits unchanged")]
    #[test_case("…f",         "ae\u{0301}f", 2; "combining char mid-string: not split from base")]
    fn truncate_left_combining_chars(expected: &str, text: &str, width: usize) {
        assert_eq!(expected, truncate_left(text, width));
    }

    #[test]
    #[should_panic(expected = "width > ELLIPSIS_WIDTH")]
    fn truncate_left_panics_when_width_equals_ellipsis_width() {
        truncate_left("example", 1);
    }
}
