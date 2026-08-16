use ratatui::buffer::CellWidth;
use unicode_segmentation::UnicodeSegmentation;

const ELLIPSIS: &str = "…";
const ELLIPSIS_WIDTH: usize = 1;

pub(super) fn pluralize_items(count: usize) -> String {
    if count == 1 {
        "1 item".into()
    } else {
        format!("{count} items")
    }
}

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

    if line.cell_width() as usize <= width {
        return line.into();
    }

    let remaining_width = width.saturating_sub(ELLIPSIS_WIDTH);

    let mut total_width = 0;
    let mut end_index = line.len();

    for (idx, g) in line.grapheme_indices(true).rev() {
        let g_width = g.cell_width() as usize;
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
    if line.cell_width() as usize <= width {
        return vec![line.into()];
    }

    let chunk_width = width.saturating_sub(ELLIPSIS_WIDTH);
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    for g in line.graphemes(true) {
        let g_width = g.cell_width() as usize;
        // Break before this grapheme would overflow, but never emit an empty
        // line: a single grapheme wider than chunk_width still gets its own line.
        if current_width + g_width > chunk_width && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(g);
        current_width += g_width;
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    // ── pluralize_items ───────────────────────────────────────────────────────

    /// Reaches the user in the delete confirmation and the chmod prompt, where
    /// the count is what says how much the keypress is about to affect.
    #[test_case(0 => "0 items" ; "none")]
    #[test_case(1 => "1 item" ; "exactly one is the only singular case")]
    #[test_case(2 => "2 items" ; "more than one")]
    fn pluralize_items_agrees_with_the_count(count: usize) -> String {
        pluralize_items(count)
    }

    // ── split_with_ellipsis ───────────────────────────────────────────────────

    #[test_case(&["example"],              "example", 7; "fits unchanged at exact width")]
    #[test_case(&["examp…", "le"],         "example", 6; "two parts at width minus 1")]
    #[test_case(&["exa…", "mpl…", "e"],   "example", 4; "three parts")]
    fn split_with_ellipsis_ascii(expected: &[&str], text: &str, width: usize) {
        assert_eq!(expected, split_with_ellipsis(text, width));
    }

    #[test]
    fn split_with_ellipsis_cjk_measures_display_width_not_bytes() {
        // "中文" has byte length 6 but display width 4; fits in one part at width 4
        assert_eq!(vec!["中文"], split_with_ellipsis("中文", 4));
    }

    #[test]
    fn split_with_ellipsis_breaks_at_grapheme_boundary_not_word() {
        // Wrapping is character-based, not word-based: spaces are not treated as
        // preferred break points, so each line is filled to the available width.
        assert_eq!(
            vec!["ab …", "cd …", "ef"],
            split_with_ellipsis("ab cd ef", 4)
        );
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
    // (display width 1) stored as two Unicode scalar values. Truncating by scalar
    // value would cut between them and leave an orphaned combining character.
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

    /// Both functions cut at grapheme-cluster boundaries, so pin it at every
    /// width rather than at the handful a table would list.
    ///
    /// The fixture is a Devanagari consonant followed by its spacing vowel
    /// sign, which extended clustering keeps together and legacy clustering
    /// splits. A combining accent cannot tell the two apart: it is one cluster
    /// under either rule.
    #[test]
    fn neither_cut_splits_a_grapheme_cluster() {
        let text = "ab\u{0915}\u{093F}cd";
        let boundaries: Vec<usize> = text
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .chain(std::iter::once(text.len()))
            .collect();

        for width in 2..=text.cell_width() as usize + 2 {
            // What survives a left truncation is a suffix, so its start offset
            // is what has to land on a boundary.
            let truncated = truncate_left(text, width);
            let tail = truncated
                .strip_prefix(ELLIPSIS)
                .unwrap_or(truncated.as_str());
            assert!(
                boundaries.contains(&(text.len() - tail.len())),
                "truncate_left at width {width} cut inside a cluster: {truncated:?}"
            );

            // Each wrapped line starts where the previous one ended, so
            // walking the offsets checks every cut and that none is lost.
            let mut offset = 0;
            for part in split_with_ellipsis(text, width) {
                assert!(
                    boundaries.contains(&offset),
                    "split_with_ellipsis at width {width} cut inside a cluster"
                );
                offset += part.strip_suffix(ELLIPSIS).unwrap_or(&part).len();
            }
            assert_eq!(
                text.len(),
                offset,
                "split_with_ellipsis at {width} lost text"
            );
        }
    }
}
