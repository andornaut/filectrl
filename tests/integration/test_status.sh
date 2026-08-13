#!/usr/bin/env bash
# Integration tests for the status bar: the Directory summary (mode and item
# count) and the Selected details (owner, group, and the composed type).
#
# The Directory section describes the directory the user is in, not the listing
# on screen, which is what several of these pin: a filter, a search and the
# bookmarks overlay all change the table without changing the summary.
#
# Created: is deliberately not asserted. It is the last field on the line and
# a long owner or group name pushes it past the right edge.

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

FX() { echo "$SANDBOX/fixtures"; }

# The status bar is the last row of the screen.
status_line() { screen | tail -1; }

item_count() { status_line | sed -n 's/.*# Items:\([0-9]*\).*/\1/p'; }
directory_mode() { status_line | sed -n 's/.*Mode:\([^ ]*\).*/\1/p'; }

# The Type field of the selected entry, which is absent when nothing is
# selected. Accessed is the field that follows it.
selected_kind() {
    status_line | sed -n 's/.*Type:\([^:]*\) Accessed.*/\1/p'
}

_items_are() { [ "$(item_count)" = "$1" ]; }
_kind_is() { [ "$(selected_kind)" = "$1" ]; }
_status_matches() { status_line | grep -Eq -- "$1"; }

assert_items() {
    wait_until _items_are "$1" ||
        _fail "expected # Items:$1 (status: $(status_line))"
}

assert_kind() {
    wait_until _kind_is "$1" ||
        _fail "expected Type:$1 (status: $(status_line))"
}

assert_status() {
    wait_until _status_matches "$1" ||
        _fail "expected the status bar to match: $1 (status: $(status_line))"
}

assert_not_status() {
    if _status_matches "$1"; then
        _fail "expected the status bar NOT to match: $1 (status: $(status_line))"
    fi
}

# Filtering is how a test lands on a named entry without counting rows. The
# filter stays up while the assertions run: clearing it moves the cursor back.
pick() {
    # Only when there is one to drop, and wait for it to go: an escape sent
    # into a view with nothing to reset has no effect to wait for, and one that
    # is missed would leave the next key typed into the filter prompt.
    if screen | grep -q '\[Filtered\]'; then
        send_escape
        assert_gone '\[Filtered\]'
    fi
    send f
    type_text "$1"
    send Enter
    assert_screen "\[Filtered\] $1"
    send g
    assert_selected "$1"
}

# ------------------------------------------------------- the Directory summary

test_the_directory_summary_shows_the_mode_and_the_item_count() {
    app_start
    [ "$(directory_mode)" = "drwxr-xr-x" ] ||
        _fail "expected the fixtures directory's mode (got: '$(directory_mode)')"
    assert_items 20
}

test_the_summary_follows_navigation() {
    app_start
    assert_items 20
    send g Enter # documents/
    assert_breadcrumbs "$(FX)/documents"
    assert_items 3
    send h
    assert_breadcrumbs "$(FX)"
    assert_items 20
}

test_the_mode_is_the_directorys_own() {
    chmod 555 "$(FX)/no_delete"
    app_start "$(FX)/no_delete"
    [ "$(directory_mode)" = "dr-xr-xr-x" ] ||
        _fail "expected the read-only mode (got: '$(directory_mode)')"
}

test_creating_a_directory_raises_the_item_count() {
    app_start "$(FX)/documents"
    assert_items 3
    send c
    type_text "brand_new"
    send Enter
    assert_screen 'brand_new/' # the row, not the prompt text still on screen
    assert_items 4
}

# The summary counts what the directory holds, so hiding the dot entries
# changes the table without changing the count.
test_hidden_entries_stay_in_the_count_when_the_table_hides_them() {
    app_start
    assert_screen '\.hidden_file'
    assert_items 20
    send .
    assert_gone '\.hidden_file'
    assert_items 20
}

test_an_empty_directory_reports_no_items_and_no_selection() {
    app_start
    send c
    type_text "empty_dir"
    send Enter
    assert_screen 'empty_dir/' # the row, not the prompt text still on screen
    send :
    type_text "$(FX)/empty_dir"
    send Enter
    assert_breadcrumbs "$(FX)/empty_dir"
    assert_items 0
    assert_not_status 'Selected'
}

# Results stream under their own load generation, which is what keeps a search
# over the whole tree from being counted as the directory's contents.
test_search_results_do_not_change_the_item_count() {
    app_start
    assert_items 20
    send /
    type_text "txt"
    send Enter
    assert_screen 'documents/readme\.txt' # a result from a subdirectory
    assert_items 20
}

test_the_bookmarks_listing_does_not_change_the_summary() {
    app_start
    assert_items 20
    send "'"
    assert_breadcrumbs "[Bookmarks] $SANDBOX/bookmarks"
    assert_items 20
}

# --------------------------------------------------------- the Selected detail

test_the_selected_detail_names_the_owner_and_the_group() {
    app_start
    assert_status "Owner:$(id -un) "
    assert_status "Group:$(id -gn) "
    assert_status 'Accessed:'
}

test_the_selected_detail_follows_the_cursor() {
    app_start
    assert_selected "documents/"
    assert_kind "Directory,Executable"
    send G # readme.txt, the last entry
    assert_selected "readme.txt"
    assert_kind "File"
}

test_a_filter_with_no_matches_clears_the_selected_detail() {
    app_start
    assert_status 'Selected'
    send f
    type_text "zzzznothing"
    send Enter
    assert_screen '\[Filtered\] zzzznothing'
    assert_not_status 'Selected'
    assert_items 20 # the directory is still the same one
}

# ------------------------------------------------------------------- the kinds

# The test makes the pipe rather than reading one out of the fixtures: git
# stores regular files, symlinks and gitlinks, so a fixture pipe exists only in
# the working tree of whoever made it and is absent from a fresh checkout.
test_an_executable_and_a_fifo_are_typed() {
    mkfifo "$(FX)/special_files/a_pipe"
    app_start "$(FX)/special_files"
    pick "executable-file"
    assert_kind "File,Executable"
    pick "a_pipe"
    assert_kind "FIFO"
}

# Git records neither the setuid, setgid nor sticky bit, so the fixtures named
# for them carry the bit only once a test sets it.
test_the_setuid_setgid_and_sticky_bits_are_typed() {
    chmod u+s "$(FX)/special_files/setuid-u+s"
    chmod g+s "$(FX)/special_files/setgid-g+s"
    chmod +t "$(FX)/special_files/file2"
    app_start "$(FX)/special_files"
    pick "setuid"
    assert_kind "File,SetUID"
    pick "setgid"
    assert_kind "File,SetGID"
    pick "file2"
    assert_kind "File,Sticky"
}

test_a_world_writable_file_is_typed() {
    chmod o+w "$(FX)/special_files/file1.ogg"
    app_start "$(FX)/special_files"
    pick "file1.ogg"
    assert_kind "File,Other Writable"
}

# A symlink is described by the link itself, never by its target, so the type
# carries the link's own 0777 mode and says nothing about what it points at
# beyond whether that still exists.
#
# Matched as a prefix rather than with assert_kind: the line is one row and is
# truncated at the screen's width, and these are the longest types there are.
# "Type:Symlink," is still what tells the two apart, since the broken one reads
# "Type:Broken Symlink,".
test_a_symlink_is_typed_and_a_broken_one_says_so() {
    app_start "$(FX)/symlinks"
    assert_selected "broken_link.txt"
    assert_status 'Type:Broken Symlink,Other Writable'
    send j
    assert_selected "valid_link.txt"
    assert_status 'Type:Symlink,Other Writable'
}

run_tests
