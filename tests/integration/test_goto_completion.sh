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

# Reset restores the value the prompt opened with, which for goto is empty.
test_prompt_reset_clears_a_goto_prompt() {
    app_start
    send :
    type_text "/some/garbage"
    assert_screen 'Go to /some/garbage'
    send C-u
    assert_gone '/some/garbage'
    send Escape
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
    send Escape
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
    send Escape
}

test_backspace_deletes_before_cursor() {
    app_start
    send :
    type_text "abz"
    send BSpace
    type_text "c"
    assert_screen 'Go to abc'
    send Escape
}

run_tests
