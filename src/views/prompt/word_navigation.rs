use unicode_segmentation::UnicodeSegmentation;

/// Finds the byte offset of the next word boundary
pub(super) fn find_next_word_boundary(text: &str, current_byte_offset: usize) -> usize {
    let text_len = text.len();
    if current_byte_offset >= text_len {
        return text_len;
    }

    // Iterator starting from the current offset
    let mut grapheme_iter = text
        .grapheme_indices(true)
        .filter(|(idx, _)| *idx >= current_byte_offset)
        .peekable();

    // Determine if starting grapheme is a separator by peeking at the first element
    let initial_is_sep = grapheme_iter.peek().map_or(true, |&(_, g)| is_separator(g));
    if initial_is_sep {
        // Started ON separator:
        grapheme_iter
            .skip_while(|&(_, g)| is_separator(g)) // Skip separators
            .skip_while(|&(_, g)| !is_separator(g)) // Skip word
            .next() // Get next separator
            .map_or(text_len, |(idx, _)| idx) // Get index or text_len if none
    } else {
        // Started IN word:
        grapheme_iter
            .find(|&(_, g)| is_separator(g)) // Find the next separator
            .map_or(text_len, |(idx, _)| idx) // Get index or end_len if none
    }
}

/// Finds the byte offset of the previous word boundary
pub(super) fn find_prev_word_boundary(text: &str, current_byte_offset: usize) -> usize {
    if current_byte_offset == 0 {
        return 0;
    }

    // Iterate graphemes backward, ending *before* the current offset
    text.grapheme_indices(true)
        .rev()
        .filter(|(idx, _)| *idx < current_byte_offset)
        // Skip initial separators
        .skip_while(|&(_, g)| is_separator(g))
        // Skip word characters
        .skip_while(|&(_, g)| !is_separator(g))
        // The next item is the separator before the word (or None if we hit the start)
        .next()
        .map(|(idx, grapheme)| idx + grapheme.len())
        // If no separator found before the word, we reached the beginning
        .unwrap_or(0)
}

fn is_separator(grapheme: &str) -> bool {
    grapheme
        .chars()
        .next()
        .map_or(true, |c| c.is_whitespace() || c.is_ascii_punctuation())
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("word1 word2 word3", 8, 11, "from_middle")]
    #[test_case("word1 word2 word3", 6, 11, "from_start_of_word")]
    #[test_case("word1 word2 word3", 5, 11, "from_space")]
    #[test_case("word1/word2.word3", 11, 17, "with_punctuation")]
    #[test_case("word1   word2 ", 8, 13, "with_multiple_spaces_in_word")]
    #[test_case("word1   word2 ", 5, 13, "with_multiple_spaces_on_space")]
    #[test_case("word1 word2 ", 0, 5, "from_start_of_doc")]
    #[test_case("word1 word2 ", 12, 12, "at_end_of_doc")]
    fn test_next_word_boundary(
        text: &str,
        start_offset: usize,
        expected_offset: usize,
        description: &str,
    ) {
        let result = find_next_word_boundary(text, start_offset);
        assert_eq!(result, expected_offset, "next - {}", description);
    }

    #[test_case("word1 word2 word3", 8, 6, "from_middle")]
    #[test_case("word1 word2 word3", 6, 0, "from_start_of_word")]
    #[test_case("word1 word2 word3", 5, 0, "from_space")]
    #[test_case("word1/word2.word3", 11, 6, "with_punctuation")]
    #[test_case("word1   word2 ", 8, 0, "with_multiple_spaces")]
    #[test_case("word1 word2 ", 0, 0, "at_start_of_doc")]
    fn test_prev_word_boundary(
        text: &str,
        start_offset: usize,
        expected_offset: usize,
        description: &str,
    ) {
        let result = find_prev_word_boundary(text, start_offset);
        assert_eq!(result, expected_offset, "prev - {}", description);
    }
}
