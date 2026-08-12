#!/usr/bin/env bash
# Integration tests for multi-select: v mark toggle, V range mode (keyboard
# and mouse), click-to-mark while marks exist, Esc clearing, and the
# documented interactions (marks clear the clipboard; sorting clears marks).
#
# The cursor row renders with the selected style even when marked, so
# assertions use assert_marked_count (the "[Selected] N items" notice) for
# counts and assert_marked (the marked style) for rows other than the cursor.

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

FIRST_ROW_Y=3

test_v_toggles_a_mark() {
    app_start
    send G k k k # a.txt
    send v
    assert_marked_count '1 item'
    send v
    assert_gone '\[Selected\]'
}

test_space_also_toggles_a_mark() {
    app_start
    send Space
    assert_marked_count '1 item'
}

test_marks_accumulate_and_esc_clears() {
    app_start
    send G k k k v # a.txt
    send j v       # hello.md
    assert_marked_count '2 items'
    assert_marked "a.txt"
    send Escape
    assert_gone '\[Selected\]'
    [ -z "$(marked_rows)" ] || _fail "marked rows remain after Esc"
}

test_range_mode_extends_from_anchor() {
    app_start
    send V # anchor on documents/
    send j j
    assert_marked_count '3 items'
    assert_marked "documents/"
    assert_marked "executables/"
}

test_range_mode_v_exits_and_keeps_marks() {
    app_start
    send V j j
    assert_marked_count '3 items'
    send V # exit range mode
    send j # moving no longer extends the range
    assert_marked_count '3 items'
}

test_range_mode_shrinks_when_cursor_moves_back() {
    app_start
    send V j j
    assert_marked_count '3 items'
    send k
    assert_marked_count '2 items'
}

# Outside range mode a click only moves the cursor; existing marks are kept.
test_click_moves_cursor_and_keeps_marks() {
    app_start
    send v # mark documents/ (row 1)
    assert_marked_count '1 item'
    click 5 $((FIRST_ROW_Y + 2)) # file_types/
    assert_selected "file_types/"
    assert_marked_count '1 item'
    assert_marked "documents/"
}

test_click_in_range_mode_extends_range_to_clicked_row() {
    app_start
    send V # anchor on documents/
    click 5 $((FIRST_ROW_Y + 4)) # images/ (row 5)
    assert_marked_count '5 items'
    assert_marked "documents/"
    assert_marked ".hidden_dir/"
}

test_marking_clears_the_clipboard() {
    app_start
    send y # copy documents/
    assert_screen '\[Copy\]'
    send v
    assert_gone '\[Copy\]'
    assert_marked_count '1 item'
}

test_sorting_clears_marks() {
    app_start
    send v j v
    assert_marked_count '2 items'
    send n # re-sort: marks track row positions, so they are cleared
    assert_gone '\[Selected\]'
}

test_filtering_clears_marks() {
    app_start
    send v
    assert_marked_count '1 item'
    send f
    type_text "txt"
    send Enter
    assert_screen '\[Filtered\] txt'
    assert_gone '\[Selected\]'
}

test_marked_chmod_applies_to_all_marked() {
    app_start
    send G k k k v j v # mark a.txt and hello.md
    assert_marked_count '2 items'
    send P
    assert_screen 'Chmod 2 items \(octal\)'
    send C-a
    type_text "600"
    send Enter
    assert_mode 600 "$SANDBOX/fixtures/a.txt"
    assert_mode 600 "$SANDBOX/fixtures/hello.md"
    assert_mode 644 "$SANDBOX/fixtures/readme.txt" # unmarked
}

run_tests
