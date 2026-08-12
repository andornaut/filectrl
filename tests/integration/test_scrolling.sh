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
