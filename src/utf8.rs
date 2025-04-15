use textwrap::{wrap, Options, WordSplitter};
use unicode_width::UnicodeWidthStr;

const ELLIPSIS: &str = "…";
// width() considers ellipsis to be two characters wide, but we know it's 1,
const ELLIPSIS_WIDTH: u16 = 1;

pub(super) fn split_with_ellipsis(line: &str, width: u16) -> Vec<String> {
    assert!(width > ELLIPSIS_WIDTH, "width > ELLIPSIS_WIDTH");

    let mut parts = split_utf8(line, width);
    let len = parts.len();
    if len > 1 {
        for part in &mut parts[..len - 1] {
            part.push_str(ELLIPSIS);
        }
    }
    parts
}

fn split_utf8(line: &str, width: u16) -> Vec<String> {
    if line.len() <= width as usize {
        return vec![line.into()];
    }

    let width = width.saturating_sub(ELLIPSIS_WIDTH);
    let options = Options::new(width as usize)
        .word_splitter(WordSplitter::NoHyphenation)
        .break_words(true);

    wrap(line, options)
        .into_iter()
        .map(|s| s.into_owned())
        .collect()
}

pub(super) fn truncate_left_utf8(line: &str, width: u16) -> String {
    assert!(width > ELLIPSIS_WIDTH, "width > ELLIPSIS_WIDTH");

    let line_width = line.width();
    if line_width <= width as usize {
        return line.into();
    }

    let remaining_width = width.saturating_sub(ELLIPSIS_WIDTH);
    let chars: Vec<char> = line.chars().collect();

    // Calculate total width from the end until we exceed the remaining width
    let mut total_width = 0;
    let mut chars_to_include = Vec::new();

    for c in chars.iter().rev() {
        let char_width = c.to_string().width();
        if total_width + char_width > remaining_width as usize {
            break;
        }
        chars_to_include.push(*c);
        total_width += char_width;
    }

    // Build the result string
    let mut result = String::with_capacity(width as usize);
    result.push_str(ELLIPSIS);

    // Add the characters in reverse order (to get them back in the right order)
    for c in chars_to_include.iter().rev() {
        result.push(*c);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(vec!["example"], "example", 7; "same width")]
    #[test_case(vec!["examp…", "le"], "example", 6; "width minus 1")]
    #[test_case(vec!["exa…", "mpl…", "e"], "example", 4; "three lines")]
    fn split_with_ellipsis_succeeds_on(expected: Vec<&str>, text: &str, width: u16) {
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
    fn truncate_left_utf8_succeeds_on(expected: &str, text: &str, width: u16) {
        let actual = truncate_left_utf8(text, width);
        assert_eq!(expected, actual);
    }

    #[test]
    #[should_panic(expected = "width > ELLIPSIS_WIDTH")]
    fn truncate_left_utf8_panics_on_width_equal_to_ellipsis() {
        truncate_left_utf8("example", 1);
    }
}
