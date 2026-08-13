#!/usr/bin/env bash
# Integration tests for the filesystem watcher: changes made outside the app
# (another process creating, deleting, or renaming entries in the current
# directory) appear in the listing without any user action.

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

FX() { echo "$SANDBOX/fixtures"; }

test_externally_created_file_appears() {
    app_start
    touch "$(FX)/appeared_outside.txt"
    assert_screen 'appeared_outside\.txt'
    assert_screen '# Items:21'
}

test_externally_created_directory_appears() {
    app_start
    mkdir "$(FX)/new_dir_outside"
    assert_screen 'new_dir_outside/'
}

test_externally_deleted_file_disappears() {
    app_start
    rm "$(FX)/a.txt"
    assert_gone ' a\.txt' # leading context keeps this off the notice area
    assert_screen '# Items:19'
}

test_externally_renamed_file_updates() {
    app_start
    mv "$(FX)/readme.txt" "$(FX)/renamed_outside.txt"
    assert_screen 'renamed_outside\.txt'
    assert_gone 'readme\.txt'
}

test_an_external_chmod_updates_the_mode_column() {
    app_start
    assert_selected "drwxr-xr-x" # documents/
    chmod 700 "$(FX)/documents"
    assert_selected "drwx------"
}

# Updates are debounced, so a burst arrives as one refresh rather than thirty.
# The count is what says none of them were dropped on the way.
test_a_burst_of_creations_all_arrive() {
    app_start
    local i
    for i in $(seq 1 30); do touch "$(FX)/burst_$i.txt"; done
    assert_screen '# Items:50'
}

# A reload keeps the cursor where it is rather than on the entry it was on: the
# entry is gone, and the row it occupied is now the next one along.
test_deleting_the_selected_entry_holds_the_cursor_position() {
    app_start
    send j j
    assert_selected "file_types/"
    rm -rf "$(FX)/file_types"
    assert_selected ".hidden_dir/" # what moved up into that row
    assert_screen '# Items:19'
}

# Marks are row indices, and a reload can renumber every row, so it drops them.
test_an_external_change_clears_the_marks() {
    app_start
    send v j v
    assert_marked_count '2 items'
    assert_marked "documents/"
    touch "$(FX)/zz_appeared.txt"
    assert_screen 'zz_appeared\.txt'
    assert_gone '\[Selected\]'
    [ -z "$(marked_rows)" ] || _fail "marked rows survived the reload: $(marked_rows | tr '\n' '|')"
}

# The clipboard holds paths rather than positions, so a reload leaves it alone
# and the copy can still be pasted.
test_an_external_change_keeps_the_clipboard() {
    app_start
    send y # documents/
    assert_screen '\[Copy\] .*fixtures/documents'
    touch "$(FX)/zz_appeared.txt"
    assert_screen 'zz_appeared\.txt'
    assert_screen '\[Copy\] .*fixtures/documents'
}

# The filter is a view over whatever the listing holds, so a refresh reapplies
# it rather than dropping it, and an arriving entry that matches shows up.
test_an_external_change_keeps_the_filter_applied() {
    app_start
    send f
    type_text "txt"
    send Enter
    assert_screen '\[Filtered\] txt'
    assert_not_screen 'documents/'
    touch "$(FX)/zz_appeared.txt"
    assert_screen 'zz_appeared\.txt'
    assert_screen '\[Filtered\] txt'
    assert_not_screen 'documents/'
}

test_a_change_in_another_directory_is_ignored() {
    app_start "$(FX)/documents"
    assert_screen '# Items:3'
    touch "$(FX)/elsewhere.txt"
    mkdir "$(FX)/elsewhere_dir"
    wait_settled
    assert_screen '# Items:3'
    assert_not_screen 'elsewhere'
}

# The watcher follows the directory rather than the name, so renaming the
# directory being viewed is not a change to its contents. The listing stays as
# it was, under the path it was opened with.
test_renaming_the_viewed_directory_leaves_the_listing_alone() {
    app_start "$(FX)/documents"
    assert_screen 'notes\.md'
    mv "$(FX)/documents" "$(FX)/renamed_dir"
    wait_settled
    assert_breadcrumbs "$(FX)/documents"
    assert_screen 'notes\.md'
    assert_screen '# Items:3'
    assert_running
}

# The watcher has already applied any external change by the time a manual
# refresh could, so a refresh has no effect of its own to observe. What is
# observable is what it must not disturb: the cursor is moved off the first row
# first, because asserting the default position would hold whether the refresh
# reselected it or reset the listing.
test_manual_refresh_keeps_listing_and_selection() {
    app_start
    send j j
    assert_selected "file_types/"
    send C-r
    wait_settled # give the reload time to land, since nothing else marks it
    assert_screen '# Items:20'
    assert_selected "file_types/"
    assert_running
}

run_tests
