#!/usr/bin/env bash
# Integration tests for the go-to prompt's path completion and for prompt-mode
# text editing.

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

FX() { echo "$SANDBOX/fixtures"; }

test_partial_path_shows_suggestion_and_enter_navigates_to_it() {
    app_start
    send :
    type_text "$(FX)/do"
    assert_screen "Go to $(FX)/documents/" # suggestion completes inline
    send Enter # submits the suggestion, not the typed prefix
    assert_breadcrumbs "$(FX)/documents"
}

test_tab_accepts_suggestion_for_further_typing() {
    app_start
    send :
    type_text "$(FX)/do"
    assert_screen "Go to $(FX)/documents/"
    send Tab
    send Enter
    assert_breadcrumbs "$(FX)/documents"
}

test_arrows_cycle_suggestions() {
    app_start
    send :
    type_text "$(FX)/s" # scrolling, special_chars, special_files, symlinks
    assert_screen 'scrolling/ \(1 of 4\)'
    send Down
    assert_screen 'special_chars/ \(2 of 4\)'
    send Down
    assert_screen 'special_files/ \(3 of 4\)'
    send Up
    assert_screen 'special_chars/ \(2 of 4\)'
    send Enter # navigates to the currently shown suggestion
    assert_breadcrumbs "$(FX)/special_chars"
}

test_no_suggestion_for_unmatched_prefix() {
    app_start
    send :
    type_text "$(FX)/zz"
    assert_screen "Go to $(FX)/zz"
    assert_not_screen '\(1 of'
    send Enter
    assert_screen "Path does not exist"
}

test_a_leading_tilde_expands_to_home() {
    app_start
    send :
    type_text "~"
    send Enter
    assert_breadcrumbs "$SANDBOX/home"
    assert_screen 'home_marker\.txt'
}

# A path with no leading / or ~ is resolved against the directory being viewed,
# not the process's working directory.
test_a_relative_path_resolves_against_the_current_directory() {
    app_start
    send :
    type_text "documents"
    send Enter
    assert_breadcrumbs "$(FX)/documents"
    assert_screen 'notes\.md'
}

test_a_relative_path_with_no_match_is_rejected_against_that_directory() {
    app_start "$SANDBOX/home"
    send :
    type_text "documents" # exists under fixtures/, not here
    send Enter
    assert_alert warn "Path does not exist: $SANDBOX/home/documents"
    assert_breadcrumbs "$SANDBOX/home"
}

# The suggestion list is a ring: Up from the first wraps to the last.
test_the_suggestions_wrap_at_both_ends() {
    app_start
    send :
    type_text "$(FX)/s" # scrolling, special_chars, special_files, symlinks
    assert_screen 'scrolling/ \(1 of 4\)'
    send Up
    assert_screen 'symlinks/ \(4 of 4\)'
    send Down
    assert_screen 'scrolling/ \(1 of 4\)'
    send_escape
}

# A file is opened with its opener rather than navigated into, the same as
# pressing Enter on its row.
test_going_to_a_file_opens_it_without_navigating() {
    app_start
    send :
    type_text "$(FX)/readme.txt"
    send Enter
    assert_gone 'Go to '
    assert_breadcrumbs "$(FX)"
    assert_not_screen 'Alerts'
    assert_running
}

# Reset restores the value the prompt opened with, which for goto is empty.
test_prompt_reset_clears_a_goto_prompt() {
    app_start
    send :
    type_text "/some/garbage"
    assert_screen 'Go to /some/garbage'
    send C-u
    assert_gone '/some/garbage'
    send_escape
}

# Rename opens prefilled, so the same key restores the original name there.
test_prompt_reset_restores_a_prefilled_rename() {
    app_start
    send G k k k # a.txt
    send r
    assert_screen 'Rename a\.txt'
    send C-a
    type_text "clobbered"
    assert_screen 'Rename clobbered'
    send C-u
    assert_screen 'Rename a\.txt'
    send_escape
}

test_prompt_cursor_movement_and_insertion() {
    app_start
    send :
    type_text "abc"
    send Left Left
    type_text "X"
    assert_screen 'Go to aXbc'
    send C-a # select all
    type_text "z" # replaces the selection
    assert_screen 'Go to z'
    assert_not_screen 'aXbc'
    send_escape
}

test_backspace_deletes_before_cursor() {
    app_start
    send :
    type_text "abz"
    send BSpace
    type_text "c"
    assert_screen 'Go to abc'
    send_escape
}

run_tests
