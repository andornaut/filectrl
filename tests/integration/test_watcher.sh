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
