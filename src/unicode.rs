use textwrap::{wrap, Options, WordSplitter};
use unicode_width::UnicodeWidthStr;

const ELLIPSIS: &str = "…";
// width() considers ellipsis to be two characters wide, but we know it's 1,
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

    // Calculate total width from the end until we exceed the remaining width
    let mut total_width = 0;
    let mut end_index = line.len();

    // Iterate through grapheme clusters from the end
    for (idx, c) in line.char_indices().rev() {
        let char_width = c.to_string().width();
        if total_width + char_width > remaining_width {
            break;
        }
        total_width += char_width;
        end_index = idx;
    }

    // Build the result string
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

    #[test_case(vec!["example"], "example", 7; "same width")]
    #[test_case(vec!["examp…", "le"], "example", 6; "width minus 1")]
    #[test_case(vec!["exa…", "mpl…", "e"], "example", 4; "three lines")]
    fn split_with_ellipsis_succeeds_on(expected: Vec<&str>, text: &str, width: usize) {
        let actual = split_with_ellipsis(text, width);
        assert_eq!(expected, actual);
    }

    #[test]
    #[should_panic(expected = "width > ELLIPSIS_WIDTH")]
    fn split_with_ellipsis_panics_on_width_equal_to_ellipsis() {
        split_with_ellipsis("example", 1);
    }

    #[test_case("example", "example", 7; "same width")]
    #[test_case("…ample", "example", 6; "width minus 1")]
    fn truncate_left_succeeds_on(expected: &str, text: &str, width: usize) {
        let actual = truncate_left(text, width);
        assert_eq!(expected, actual);
    }

    #[test]
    #[should_panic(expected = "width > ELLIPSIS_WIDTH")]
    fn truncate_left_panics_on_width_equal_to_ellipsis() {
        truncate_left("example", 1);
    }
}
