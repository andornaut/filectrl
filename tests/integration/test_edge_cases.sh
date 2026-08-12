#!/usr/bin/env bash
# Integration tests for edge cases: unicode and special-character filenames,
# long filenames, symlinks (valid and broken), chmod on a directory, and the
# responsive layout at narrow terminal widths.

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

FX() { echo "$SANDBOX/fixtures"; }

# ------------------------------------------------------------------ filenames

test_unicode_filenames_render() {
    app_start "$(FX)/unicode"
    assert_screen 'café\.txt'
    assert_screen 'emoji_🎉_party\.txt'
    assert_screen '日本語ファイル\.txt'
    assert_screen 'ひらがな\.txt'
    assert_screen '# Items:5'
}

test_unicode_filename_can_be_renamed() {
    app_start "$(FX)/unicode"
    send f # filter down to the one file so selection is deterministic
    type_text "café"
    send Enter
    assert_screen '\[Filtered\] café'
    send g r
    assert_screen 'Rename café\.txt'
    send C-a
    type_text "café_renamed.txt"
    send Enter
    wait_until [ -f "$(FX)/unicode/café_renamed.txt" ] || _fail "renamed unicode file missing"
    [ ! -e "$(FX)/unicode/café.txt" ] || _fail "old unicode name still exists"
}

test_filename_with_spaces_can_be_deleted() {
    app_start "$(FX)/special_chars"
    assert_screen 'file with spaces\.txt'
    send f
    type_text "spaces"
    send Enter
    assert_screen '\[Filtered\] spaces'
    send g d
    assert_screen 'Delete 1 item\? \(y/n\)'
    send y
    wait_until [ ! -e "$(FX)/special_chars/file with spaces.txt" ] ||
        _fail "file with spaces still on disk"
}

test_parentheses_and_brackets_filename_renders() {
    app_start "$(FX)/special_chars"
    assert_screen '\(parentheses\) and \[brackets\]\.txt'
}

test_long_filenames_display_without_breaking_navigation() {
    app_start "$(FX)/long_names"
    assert_screen '# Items:4'
    send G
    send g
    assert_selected "a file name with spaces"
    assert_running
}

# ------------------------------------------------------------------- symlinks

test_valid_symlink_shows_symlink_type() {
    app_start "$(FX)/symlinks"
    send G # valid_link.txt sorts after broken_link.txt
    assert_selected "valid_link.txt"
    assert_screen 'Type:[A-Za-z,]*Symlink'
    send Enter # opens the target file via the stubbed opener
    assert_running
    assert_breadcrumbs "$(FX)/symlinks"
}

test_broken_symlink_shows_type_and_open_fails_cleanly() {
    app_start "$(FX)/symlinks"
    send g
    assert_selected "broken_link.txt"
    assert_screen 'Broken Symlink'
    send Enter
    assert_screen 'Failed to open .*broken_link\.txt'
    assert_running
}

# ---------------------------------------------------------------------- chmod

test_chmod_on_a_directory() {
    app_start
    send P # documents/ is selected on start
    assert_screen 'Chmod 1 item \(octal\) 755'
    send C-a
    type_text "700"
    send Enter
    assert_screen 'drwx------'
    wait_until [ "$(stat -c %a "$(FX)/documents")" = "700" ] ||
        _fail "directory mode is $(stat -c %a "$(FX)/documents"), expected 700"
}

# ---------------------------------------------------------- responsive layout

test_narrow_window_drops_columns() {
    app_start
    resize_window 50 30
    assert_screen '\[M\]odified'
    assert_not_screen '\[S\]ize'
    resize_window 30 30
    assert_gone '\[M\]odified' # name column only
    assert_screen 'documents/'
    send j
    assert_selected "executables/" # still fully usable
}

test_tiny_window_shows_resize_message_and_recovers() {
    app_start
    resize_window 12 30
    # The message wraps at this width, so match its words separately
    assert_screen 'Resize'
    assert_screen 'window'
    resize_window 120 30
    assert_gone 'Resize'
    assert_screen '\[N\]ame⌃'
    send j
    assert_selected "executables/"
    assert_running
}

run_tests
