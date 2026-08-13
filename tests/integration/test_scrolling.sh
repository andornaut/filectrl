#!/usr/bin/env bash
# Integration tests for scrolling a listing taller than the viewport:
# page keys, visible-row selection (H/M/L), the mouse wheel, and the
# viewport following the selection.
#
# fixtures/scrolling has 48 files; in a 30-row pane 26 rows are visible
# (001..026), so paging and viewport movement are all observable.

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

scrolling_start() { app_start "$SANDBOX/fixtures/scrolling"; }

test_page_down_selects_last_visible_then_pages() {
    scrolling_start
    send C-d
    assert_selected "026_continuing_on.txt" # first press: jump to last visible
    send C-d
    assert_selected "053_bonus_file_three.txt" # second press: a full page (clamped to end)
    assert_screen '023_short_three' # viewport scrolled down
    send C-d
    assert_selected "053_bonus_file_three.txt" # already at the end
}

test_page_up_mirrors_page_down() {
    scrolling_start
    send C-d C-d
    assert_selected "053_bonus_file_three.txt"
    send C-u
    assert_selected "023_short_three.txt" # first press: jump to first visible
    send C-u
    assert_selected "001_normal_file.txt"
}

test_pgdn_pgup_keys_page_too() {
    scrolling_start
    send PgDn
    assert_selected "026_continuing_on.txt"
    send PgUp
    assert_selected "001_normal_file.txt"
}

test_select_first_middle_last_visible_row() {
    scrolling_start
    send M
    assert_selected "014_medium_length_filename"
    send L
    assert_selected "026_continuing_on.txt"
    send H
    assert_selected "001_normal_file.txt"
}

test_mouse_wheel_moves_selection() {
    scrolling_start
    scroll_wheel down 40 10
    assert_selected "002_another_normal_file.md"
    scroll_wheel down 40 10
    assert_selected "003_config.toml"
    scroll_wheel up 40 10
    assert_selected "002_another_normal_file.md"
}

test_the_wheel_clamps_at_both_ends() {
    scrolling_start
    local i
    for i in $(seq 1 8); do scroll_wheel up 40 10; done
    assert_selected "001_normal_file.txt" # already at the top
    send G
    assert_selected "053_bonus_file_three.txt"
    for i in $(seq 1 8); do scroll_wheel down 40 10; done
    assert_selected "053_bonus_file_three.txt" # already at the end
}

# H, M and L are about the window rather than the listing, so they follow it
# when it scrolls.
test_visible_row_keys_follow_the_scrolled_window() {
    scrolling_start
    send C-d C-d
    assert_selected "053_bonus_file_three.txt"
    send H
    assert_selected "023_short_three.txt"
    send M
    assert_selected "036_data_export"
    send L
    assert_selected "053_bonus_file_three.txt"
}

# A name too long for the column wraps onto a second screen row, which is one
# entry and not two: the cursor steps over it in a single press.
test_a_wrapped_name_is_still_one_entry() {
    scrolling_start
    send g
    send j j j j j j j j j # nine rows down, over the gap where 007 would be
    assert_selected "011_this_filename_is_quite_long"
    assert_screen 'rrow_terminal_column\.txt' # the tail, on its own row
    send j
    assert_selected "012_short.txt"
}

# The selection is what the viewport follows, so a window too small to hold the
# rows it held before keeps showing it.
test_shrinking_the_window_keeps_the_selection_visible() {
    scrolling_start
    send G
    assert_selected "053_bonus_file_three.txt"
    assert_screen '023_short_three' # the top of the bottom-most window at 30 rows
    resize_window 120 15
    wait_settled
    assert_selected "053_bonus_file_three.txt"
    assert_not_screen '023_short_three' # too far back to fit now
    resize_window 120 30
    wait_settled
    assert_selected "053_bonus_file_three.txt"
}

# assert_selected only matches visible rows (the highlight marker), so these
# also prove the viewport scrolled to keep the selection on screen.
test_viewport_follows_selection_to_the_ends() {
    scrolling_start
    send G
    assert_selected "053_bonus_file_three.txt"
    assert_not_screen '001_normal_file' # the top scrolled out of view
    send g
    assert_selected "001_normal_file.txt"
    assert_not_screen '053_bonus_file_three'
}

run_tests
