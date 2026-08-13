#!/usr/bin/env bash
# Integration tests for bookmarks: symlinks stored in a bookmarks/ directory
# beside the config file. Add (B), show (' or `), open, rename, and delete,
# with on-disk symlink assertions.

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

FX() { echo "$SANDBOX/fixtures"; }
BM() { echo "$SANDBOX/bookmarks"; }

# Adds a bookmark for the current directory under its default name and clears
# the confirmation alert.
add_bookmark() {
    send B
    assert_screen 'Add bookmark'
    send Enter
    assert_screen 'Bookmark .* added'
    send C-a
}

test_add_bookmark_with_default_name() {
    app_start
    send B
    assert_screen 'Add bookmark fixtures' # prompt prefilled with the dir name
    send Enter
    assert_screen 'Bookmark "fixtures" added'
    [ "$(readlink "$(BM)/fixtures")" = "$(FX)" ] ||
        _fail "bookmark symlink missing or wrong target: $(readlink "$(BM)/fixtures" 2>&1)"
}

test_add_bookmark_with_custom_name() {
    app_start
    send B
    assert_screen 'Add bookmark'
    send C-a
    type_text "favs"
    send Enter
    assert_screen 'Bookmark "favs" added'
    [ "$(readlink "$(BM)/favs")" = "$(FX)" ] || _fail "custom-named symlink missing"
}

test_add_duplicate_bookmark_name_is_refused() {
    app_start
    add_bookmark
    send B
    assert_screen 'Add bookmark'
    send Enter
    assert_screen 'A bookmark named "fixtures" already exists'
    # nullglob: without it an empty directory yields the unexpanded pattern and
    # counts as one entry, which is the case this is meant to catch.
    local entries
    shopt -s nullglob
    entries=("$(BM)"/*)
    shopt -u nullglob
    [ "${#entries[@]}" = "1" ] || _fail "expected exactly one bookmark, found: ${entries[*]}"
}

test_add_bookmark_with_empty_name_is_refused() {
    app_start
    send B
    assert_screen 'Add bookmark'
    send C-a BSpace Enter
    assert_screen 'Bookmark name cannot be empty'
    [ ! -d "$(BM)" ] || [ -z "$(ls "$(BM)")" ] || _fail "a bookmark was created"
}

test_show_bookmarks_and_open_navigates_to_target() {
    app_start
    send Enter # enter documents/ and bookmark it
    assert_breadcrumbs "$(FX)/documents"
    add_bookmark
    send h
    assert_breadcrumbs "$(FX)"
    send "'"
    assert_breadcrumbs "[Bookmarks] $(BM)"
    assert_selected "documents"
    send Enter
    assert_breadcrumbs "$(FX)/documents"
    assert_screen 'notes\.md'
}

test_backtick_also_shows_bookmarks() {
    app_start
    add_bookmark
    send '`'
    assert_breadcrumbs "[Bookmarks] $(BM)"
}

test_bookmarks_view_esc_returns_to_directory() {
    app_start
    add_bookmark
    send "'"
    assert_breadcrumbs "[Bookmarks] $(BM)"
    send_escape
    assert_breadcrumbs "$(FX)"
    assert_screen '# Items:20'
}

test_rename_bookmark() {
    app_start
    add_bookmark
    send "'"
    assert_breadcrumbs "[Bookmarks] $(BM)"
    wait_for_selection # rename acts on the selected bookmark
    send r
    assert_screen 'Rename fixtures'
    send C-a
    type_text "favs"
    send Enter
    assert_screen 'favs'
    wait_until [ -L "$(BM)/favs" ] || _fail "renamed symlink missing"
    [ ! -e "$(BM)/fixtures" ] || _fail "old symlink name still exists"
    [ "$(readlink "$(BM)/favs")" = "$(FX)" ] || _fail "rename changed the target"
}

test_delete_bookmark_keeps_target_directory() {
    app_start
    add_bookmark
    send "'"
    assert_breadcrumbs "[Bookmarks] $(BM)"
    wait_for_selection # delete acts on the selected bookmark
    send d
    assert_screen 'Delete 1 item\? \(y/n\)'
    send y
    wait_until [ ! -e "$(BM)/fixtures" ] || _fail "bookmark symlink still exists"
    [ -f "$(FX)/a.txt" ] || _fail "target directory was deleted with the bookmark"
    assert_breadcrumbs "[Bookmarks] $(BM)" # still in the (now empty) view
}

test_open_bookmark_with_missing_target_shows_error() {
    app_start
    send Enter # bookmark documents/, then remove it behind the app's back
    assert_breadcrumbs "$(FX)/documents"
    add_bookmark
    send h
    assert_breadcrumbs "$(FX)"
    rm -rf "$(FX)/documents"
    send "'"
    assert_breadcrumbs "[Bookmarks] $(BM)"
    wait_for_selection # Enter opens the selected bookmark
    send Enter
    # The error names the bookmark being opened, not just the raw io error
    assert_screen 'Failed to open .*documents'
    assert_running
}

run_tests
