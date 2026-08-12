#!/usr/bin/env bash
# Integration tests for the scrollbar: clicking the track, dragging the thumb,
# and the column staying inert when the listing fits.
#
# The scrollbar is the rightmost column, and the table takes the rest of the
# width. Its track starts one row below the table's top, so it lines up with
# the first data row and leaves a 1x1 block under the header.
#
# fixtures/scrolling has 48 entries and 27 rows are visible, so a drag to the
# bottom of the track lands on the bottom-most window, which starts at 023.

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

SCROLLBAR_X=120     # equal to COLS: the last column
SCROLLBAR_TOP=3     # first data row
SCROLLBAR_BOTTOM=29 # last row above the status bar

scrolling_start() { app_start "$SANDBOX/fixtures/scrolling"; }

# The drawn part of the scrollbar column, blanks removed, so an empty result
# means nothing was rendered there. Sampled well above the last row: a notice
# shrinks the table, and its own "(Press ... to clear)" hint reaches this
# column, which would otherwise read as a scrollbar.
SCROLLBAR_SAMPLE_BOTTOM=20
scrollbar_column() {
    screen | sed -n "$SCROLLBAR_TOP,${SCROLLBAR_SAMPLE_BOTTOM}p" |
        cut -c"$SCROLLBAR_X" | tr -d ' \n'
}

# ------------------------------------------------------------------ rendering

test_scrollbar_is_drawn_when_the_listing_overflows() {
    scrolling_start
    [ -n "$(scrollbar_column)" ] || _fail "nothing drawn in the scrollbar column"
}

test_scrollbar_is_not_drawn_when_the_listing_fits() {
    app_start "$SANDBOX/fixtures/documents"
    assert_screen 'report\.pdf' # all three entries fit
    [ -z "$(scrollbar_column)" ] ||
        _fail "scrollbar drawn for a listing that fits: '$(scrollbar_column)'"
}

test_a_click_in_the_column_does_nothing_when_the_scrollbar_is_not_drawn() {
    app_start "$SANDBOX/fixtures/documents"
    assert_selected "notes.md"
    click "$SCROLLBAR_X" 5 # level with the third row
    send j                 # a key whose effect is visible, so the click was processed first
    assert_selected "readme.txt"
}

# Not drawing the scrollbar also clears its hit-test area, so the press above
# cannot start a drag. Nothing moves while the listing fits either way, so the
# difference only shows once it overflows again: a stale drag would let the
# next motion event scroll, without any press to begin it.
test_a_press_while_the_scrollbar_is_hidden_does_not_arm_a_drag() {
    scrolling_start
    send f
    type_text "001_normal"
    send Enter
    assert_screen '\[Filtered\] 001_normal'
    [ -z "$(scrollbar_column)" ] || _fail "one match should not overflow the viewport"
    mouse_down "$SCROLLBAR_X" 3
    mouse_up "$SCROLLBAR_X" 3
    send Escape # back to all 48 entries, so the scrollbar is drawn again
    assert_gone '\[Filtered\]'
    mouse_drag "$SCROLLBAR_X" "$SCROLLBAR_BOTTOM"
    send j # processed after the motion, so the selection below is settled
    assert_selected "002_another_normal_file.md"
    assert_not_screen '053_bonus_file_three'
}

# ---------------------------------------------------------- clicking the track

test_clicking_the_top_of_the_track_scrolls_to_the_start() {
    scrolling_start
    send G
    assert_selected "053_bonus_file_three.txt"
    click "$SCROLLBAR_X" "$SCROLLBAR_TOP"
    assert_selected "001_normal_file.txt"
}

test_clicking_the_bottom_of_the_track_scrolls_to_the_end() {
    scrolling_start
    click "$SCROLLBAR_X" "$SCROLLBAR_BOTTOM"
    assert_selected "023_short_three.txt" # the bottom-most window's first row
    assert_screen '053_bonus_file_three'
}

test_clicking_the_middle_of_the_track_scrolls_proportionally() {
    scrolling_start
    click "$SCROLLBAR_X" 16
    assert_selected "013_another_short.md"
    assert_not_screen '001_normal_file' # scrolled past the top
    assert_not_screen '053_bonus_file_three'
}

# --------------------------------------------------------- dragging the thumb

test_dragging_the_thumb_to_the_bottom_scrolls_to_the_end() {
    scrolling_start
    drag "$SCROLLBAR_X" "$SCROLLBAR_TOP" "$SCROLLBAR_BOTTOM"
    assert_selected "023_short_three.txt"
    assert_screen '053_bonus_file_three'
}

test_dragging_the_thumb_back_to_the_top_returns_to_the_start() {
    scrolling_start
    drag "$SCROLLBAR_X" "$SCROLLBAR_TOP" "$SCROLLBAR_BOTTOM"
    assert_selected "023_short_three.txt"
    drag "$SCROLLBAR_X" "$SCROLLBAR_BOTTOM" "$SCROLLBAR_TOP"
    assert_selected "001_normal_file.txt"
}

# Releasing ends the drag, so later motion over the column is not a drag.
test_motion_after_the_release_does_not_scroll() {
    scrolling_start
    drag "$SCROLLBAR_X" "$SCROLLBAR_TOP" "$SCROLLBAR_BOTTOM"
    assert_selected "023_short_three.txt"
    mouse_drag "$SCROLLBAR_X" "$SCROLLBAR_TOP"
    send j # processed after the motion, so the selection below is settled
    assert_selected "024_medium_filename_with_some_words_in_it.txt"
}

# A drag has to start on the scrollbar; one that starts on a row must not
# capture the thumb when it crosses the column.
test_a_drag_that_starts_off_the_scrollbar_does_not_scroll_it() {
    scrolling_start
    mouse_down 5 "$SCROLLBAR_TOP"
    mouse_drag "$SCROLLBAR_X" "$SCROLLBAR_BOTTOM"
    mouse_up "$SCROLLBAR_X" "$SCROLLBAR_BOTTOM"
    assert_screen '001_normal_file'
    assert_not_screen '053_bonus_file_three'
}

run_tests
