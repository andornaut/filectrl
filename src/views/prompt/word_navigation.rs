use unicode_segmentation::UnicodeSegmentation;

fn is_separator(grapheme: &str) -> bool {
    grapheme
        .chars()
        .next()
        .map_or(true, |c| c.is_whitespace() || c.is_ascii_punctuation())
}

/// Finds the byte offset of the previous word boundary (simplified for single line).
pub(super) fn find_prev_word_boundary(text: &str, current_byte_offset: usize) -> usize {
    if current_byte_offset == 0 {
        return 0;
    }

    // Iterate graphemes backward, ending *before* the current offset
    let iter = text
        .grapheme_indices(true)
        .rev()
        .filter(|(idx, _)| *idx < current_byte_offset);

    // Skip initial separators (going backward)
    let iter = iter.skip_while(|&(_, g)| is_separator(g));

    // Skip word characters (going backward)
    let mut iter = iter.skip_while(|&(_, g)| !is_separator(g));

    // The next item is the separator before the word (or None if we hit the start)
    match iter.next() {
        // Boundary is the position *after* this separator
        Some((idx, grapheme)) => idx + grapheme.len(),
        // If no separator found before the word, we reached the beginning
        None => 0,
    }
}

/// Finds the byte offset of the next word boundary (simplified for single line).
pub(super) fn find_next_word_boundary(text: &str, current_byte_offset: usize) -> usize {
    let text_len = text.len();
    if current_byte_offset >= text_len {
        return text_len;
    }

    // Iterator starting from the current offset
    let grapheme_iter = text
        .grapheme_indices(true)
        .filter(|(idx, _)| *idx >= current_byte_offset);

    // Determine if starting grapheme is a separator
    // Need to clone the iterator as next() consumes the first item
    let initial_is_sep = grapheme_iter
        .clone()
        .next()
        .map_or(true, |(_, g)| is_separator(g));

    if !initial_is_sep {
        // Started IN word: Find the next separator
        let mut grapheme_iter = grapheme_iter;
        grapheme_iter
            .find(|&(_, g)| is_separator(g))
            .map_or(text_len, |(idx, _)| idx) // Get index or end_len if none
    } else {
        // Started ON separator: Skip separators, then skip word
        let iter_after_separators = grapheme_iter.skip_while(|&(_, g)| is_separator(g));
        let mut iter_after_word = iter_after_separators.skip_while(|&(_, g)| !is_separator(g));

        // The next item is the separator after the word (or None if end)
        iter_after_word.next().map_or(text_len, |(idx, _)| idx) // Get index or end_len if none
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("word1 word2 word3", 8, 6, "simple")]
    #[test_case("word1 word2 word3", 6, 0, "start_of_word")]
    #[test_case("word1 word2 word3", 5, 0, "on_space")]
    #[test_case("word1/word2.word3", 11, 6, "with_punctuation")]
    #[test_case("word1   word2 ", 8, 0, "multiple_spaces")]
    #[test_case("word1 word2 ", 0, 0, "start_of_doc")]
    fn test_prev_word_boundary(
        text: &str,
        start_offset: usize,
        expected_offset: usize,
        description: &str,
    ) {
        let result = find_prev_word_boundary(text, start_offset);
        assert_eq!(result, expected_offset, "prev - {}", description);
    }

    #[test_case("word1 word2 word3", 8, 11, "next_from_simple_start")]
    #[test_case("word1 word2 word3", 6, 11, "next_from_start_of_word")]
    #[test_case("word1 word2 word3", 5, 11, "next_from_on_space")]
    #[test_case("word1/word2.word3", 11, 17, "next_from_with_punctuation")]
    #[test_case("word1   word2 ", 8, 13, "next_from_multiple_spaces_in_word")]
    #[test_case("word1   word2 ", 5, 13, "next_from_multiple_spaces_on_space")]
    #[test_case("word1 word2 ", 0, 5, "next_from_start_of_doc")]
    #[test_case("word1 word2 ", 12, 12, "next_at_end_of_doc")]
    fn test_next_word_boundary(
        text: &str,
        start_offset: usize,
        expected_offset: usize,
        description: &str,
    ) {
        let result = find_next_word_boundary(text, start_offset);
        assert_eq!(result, expected_offset, "next - {}", description);
    }
}
