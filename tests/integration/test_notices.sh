#!/usr/bin/env bash
# Integration tests for the notices strip: which notices are live at once, the
# order they stack in, and what dismisses them.
#
# The notices sit directly above the status bar, one per row, in a fixed order:
# progress, the operation, the search, marks, the clipboard, the filter. The
# search notices are covered by test_search.sh, which already has a walk slow
# enough to see them.

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

FX() { echo "$SANDBOX/fixtures"; }

# notice_lines N : the N rows directly above the status bar, in display order.
notice_lines() { screen | tail -n "$(($1 + 1))" | head -n "$1"; }

_notice_line_matches() { notice_lines "$1" | sed -n "$2p" | grep -Eq -- "$3"; }

# assert_notices REGEX... : one pattern per notice row, top to bottom. The
# count is part of the assertion: an extra notice shifts every row up, so the
# first pattern stops matching.
assert_notices() {
    local count=$# i=1 pattern
    for pattern in "$@"; do
        wait_until _notice_line_matches "$count" "$i" "$pattern" ||
            _fail "expected notice row $i of $count to match '$pattern' (rows: $(notice_lines "$count" | tr '\n' '|'))"
        i=$((i + 1))
    done
}

# The row a notice occupies on screen, counting the status bar as the last row.
notice_y() { echo $((ROWS - $1)); } # notice_y 1 = the bottom-most notice

# ------------------------------------------------------------------- stacking

test_the_marked_notice_sits_above_the_filter_notice() {
    app_start
    send f
    type_text "txt"
    send Enter
    assert_screen '\[Filtered\] txt'
    send g v j v
    assert_notices '\[Selected\] 2 items' '\[Filtered\] txt'
}

# Marks and the clipboard are mutually exclusive, so copying the marked entries
# swaps one notice for the other rather than adding a row.
test_the_clipboard_notice_replaces_the_marked_one() {
    app_start
    send f
    type_text "txt"
    send Enter
    assert_screen '\[Filtered\] txt'
    send g v j v
    assert_marked_count '2 items'
    send y
    assert_notices '\[Copy\] 2 items' '\[Filtered\] txt'
    assert_not_screen '\[Selected\]'
}

test_marking_again_drops_the_clipboard_notice() {
    app_start
    send g v
    assert_marked_count '1 item'
    send y
    assert_screen '\[Copy\]'
    send j v
    assert_notices '\[Selected\] 2 items'
    assert_not_screen '\[Copy\]'
}

# One entry is named, several are counted.
test_one_copied_entry_is_named_and_several_are_counted() {
    app_start
    send g v y
    assert_notices "\[Copy\] .*fixtures/documents"
    send j v
    send y
    assert_notices '\[Copy\] 2 items'
}

# The delete prompt says how many items it is about, so the marked notice would
# only repeat it; it comes back if the prompt is dismissed.
test_the_delete_prompt_hides_the_marked_notice() {
    app_start
    send g v
    assert_marked_count '1 item'
    send d
    assert_screen 'Delete 1 item\? \(y/n\)'
    assert_not_screen '\[Selected\]'
    send n
    assert_notices '\[Selected\] 1 item'
}

# ---------------------------------------------------------------- task notices

# A copy big enough to still be running when the test looks at it. The sparse
# source costs no disk, but every byte is written.
SPARSE_SIZE=2147483648

start_big_copy() {
    TIMEOUT=60
    truncate -s "$SPARSE_SIZE" "$(FX)/big.bin"
    app_start
    send G k k k # big.bin sorts between a.txt and hello.md
    assert_selected "big.bin"
    send y
    send g Enter # documents/
    assert_breadcrumbs "$(FX)/documents"
    wait_for_selection
    send p
    assert_screen 'Copying .*big\.bin'
}

test_a_running_copy_shows_progress_above_the_operation() {
    start_big_copy
    assert_notices '[0-9]+%$' 'Copying .*big\.bin to .*documents/'
}

# A click on a notice resets the view, but not on the task notices: clearing
# those is Ctrl+p, and a stray click must not look like it stopped the work.
test_a_click_on_a_task_notice_leaves_it_alone() {
    start_big_copy
    click 5 "$(notice_y 2)" # the progress row
    click 5 "$(notice_y 1)" # the operation row
    assert_screen 'Copying .*big\.bin'
    assert_not_screen 'Cancelled'
}

test_clearing_progress_leaves_the_filter_notice() {
    start_big_copy
    send f
    type_text "big"
    send Enter
    assert_notices '[0-9]+%$' 'Copying .*big\.bin to .*documents/' '\[Filtered\] big'
    send C-p
    assert_gone 'Copying '
    assert_notices '\[Filtered\] big'
}

# --------------------------------------------------------------- dismissing

test_a_click_on_the_filter_notice_clears_it() {
    app_start
    send f
    type_text "txt"
    send Enter
    assert_screen '\[Filtered\] txt'
    assert_not_screen 'documents/'
    click 5 "$(notice_y 1)"
    assert_gone '\[Filtered\]'
    assert_screen 'documents/' # the whole listing is back
}

# Search results are unfiltered, so a filter notice left up would describe a
# filter that is no longer applied.
test_starting_a_search_clears_the_filter_notice() {
    app_start
    send f
    type_text "txt"
    send Enter
    assert_screen '\[Filtered\] txt'
    send /
    type_text "photo"
    send Enter
    assert_breadcrumbs "[Search] $(FX)"
    assert_screen 'images/photo\.png'
    assert_not_screen '\[Filtered\]'
}

test_navigating_clears_the_filter_and_marked_notices() {
    app_start
    send f
    type_text "documents"
    send Enter
    assert_screen '\[Filtered\] documents'
    send g v
    assert_notices '\[Selected\] 1 item' '\[Filtered\] documents'
    send Enter # into documents/
    assert_breadcrumbs "$(FX)/documents"
    assert_not_screen '\[Filtered\]'
    assert_not_screen '\[Selected\]'
}

run_tests
