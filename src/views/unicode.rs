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

/// The number of lines `split_with_ellipsis` returns, counted without building
/// them. Walks the graphemes with the same break rule as `split`.
pub(super) fn split_line_count(line: &str, width: usize) -> usize {
    assert!(width > ELLIPSIS_WIDTH, "width > ELLIPSIS_WIDTH");

    if line.cell_width() as usize <= width {
        return 1;
    }

    let chunk_width = width.saturating_sub(ELLIPSIS_WIDTH);
    let mut count = 0;
    let mut current_width = 0;
    let mut current_is_empty = true;
    for g in line.graphemes(true) {
        let g_width = g.cell_width() as usize;
        if current_width + g_width > chunk_width && !current_is_empty {
            count += 1;
            current_width = 0;
        }
        current_is_empty = false;
        current_width += g_width;
    }
    if !current_is_empty {
        count += 1;
    }
    count
}

/// `before`, `text` and `after` as one line of `width` columns. `before` and
/// `after` are kept whole, and `text` loses its start to an ellipsis when it
/// does not fit, which keeps the tail of a path (the part that identifies it)
/// visible. With no room for any of `text` beside the ellipsis, only the
/// ellipsis shows. When `before` and `after` alone are wider than `width`, the
/// line is too, and the caller's widget clips it.
pub(super) fn fit_left(before: &str, text: &str, after: &str, width: usize) -> String {
    let around = before.cell_width() as usize + after.cell_width() as usize;
    let room = width.saturating_sub(around);
    let text = if text.cell_width() as usize <= room {
        text.to_string()
    } else if room <= ELLIPSIS_WIDTH {
        ELLIPSIS.to_string()
    } else {
        truncate_left(text, room)
    };
    format!("{before}{text}{after}")
}

fn truncate_left(line: &str, width: usize) -> String {
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

    #[test]
    fn split_line_count_agrees_with_split_with_ellipsis() {
        let texts = [
            "",
            "example",
            "a_very_long_file_name_that_must_wrap_across_several_lines.txt",
            "ab cd ef",
            "中文文件名称非常长非常长非常长.txt",
            "a中b文c字d",
            "e\u{0301}e\u{0301}e\u{0301}e\u{0301}e\u{0301}",
            "ab\u{0915}\u{093F}cd\u{0915}\u{093F}ef",
            "\u{200B}\u{200B}abc",
        ];
        for text in texts {
            for width in 2..=text.cell_width() as usize + 2 {
                assert_eq!(
                    split_with_ellipsis(text, width).len(),
                    split_line_count(text, width),
                    "{text:?} at width {width}"
                );
            }
        }
    }

    // ── fit_left ──────────────────────────────────────────────────────────────

    #[test_case("[", "abc", "]", 5, "[abc]"; "fits unchanged at exact width")]
    #[test_case("[", "abc", "]", 9, "[abc]"; "fits unchanged when wider than needed")]
    #[test_case("[", "a", "]", 3, "[a]"; "fits unchanged in a single column")]
    #[test_case("", "", "", 0, ""; "empty text stays empty with no room")]
    #[test_case("[", "abcdef", "]", 5, "[…ef]"; "trimmed from the left keeping before and after")]
    #[test_case("Copying ", "/tmp/a to /home/developer/Downloads/", "", 24, "Copying …oper/Downloads/"; "a path keeps its tail")]
    #[test_case("[", "abcdef", "]", 4, "[…f]"; "one column of text beside the ellipsis")]
    #[test_case("[", "abcdef", "]", 3, "[…]"; "only an ellipsis in one column")]
    #[test_case("[", "abcdef", "]", 2, "[…]"; "only an ellipsis with no room")]
    #[test_case("[", "abcdef", "]", 0, "[…]"; "before and after kept whole past the width")]
    #[test_case("", "中文字", "", 5, "…文字"; "wide graphemes that fit are kept")]
    #[test_case("", "中文字", "", 4, "…字"; "a wide grapheme that would overflow is dropped whole")]
    #[test_case("> ", "中文字", "", 5, "> …字"; "wide graphemes measured after before")]
    #[test_case("", "ae\u{0301}f", "", 2, "…f"; "a combining accent is not split from its base")]
    fn fit_left_cases(before: &str, text: &str, after: &str, width: usize, expected: &str) {
        assert_eq!(expected, fit_left(before, text, after, width));
    }

    // ── truncate_left ─────────────────────────────────────────────────────────

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
