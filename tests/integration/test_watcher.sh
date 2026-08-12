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

test_manual_refresh_keeps_listing_and_selection() {
    app_start
    send C-r
    assert_screen '# Items:20'
    assert_selected "documents/"
    assert_running
}

run_tests
