#!/usr/bin/env bash
# Integration tests for file operations: create directory, rename, delete, and
# chmod, driven through the real TUI. Assertions check both the screen and the
# on-disk result, so they also exercise the filesystem watcher's auto-refresh.

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

FX() { echo "$SANDBOX/fixtures"; }

# --------------------------------------------------------------- create (c)

test_create_directory() {
    app_start
    send c
    assert_screen 'New directory'
    type_text "brand_new"
    send Enter
    assert_screen 'brand_new/'
    assert_screen '# Items:21'
    [ -d "$(FX)/brand_new" ] || _fail "directory was not created on disk"
}

test_create_directory_esc_cancels() {
    app_start
    send c
    assert_screen 'New directory'
    type_text "never_created"
    send Escape
    send j # normal mode again
    assert_selected "executables/"
    [ ! -e "$(FX)/never_created" ] || _fail "directory was created despite Esc"
}

test_create_directory_existing_name_shows_error() {
    app_start
    send c
    type_text "documents"
    send Enter
    assert_screen 'Failed to create directory "documents"'
    [ -f "$(FX)/documents/notes.md" ] || _fail "existing directory was clobbered"
}

test_create_directory_rejects_slash_in_name() {
    app_start
    send c
    type_text "nested/child"
    send Enter
    assert_screen "Directory name cannot contain '/'"
    [ ! -e "$(FX)/nested" ] || _fail "nested directory was created"
}

# --------------------------------------------------------------- rename (r)

test_rename_file() {
    app_start
    send G k k k # a.txt
    assert_selected "a.txt"
    send r
    assert_screen 'Rename a\.txt'
    send C-a # select all, so typing replaces the prefilled name
    type_text "renamed.txt"
    send Enter
    assert_screen 'renamed\.txt'
    [ -f "$(FX)/renamed.txt" ] || _fail "renamed file missing on disk"
    [ ! -e "$(FX)/a.txt" ] || _fail "old name still exists on disk"
}

test_rename_directory() {
    app_start
    send Enter # enter documents/ to prove the rename target keeps its contents
    assert_breadcrumbs "$(FX)/documents"
    send h
    assert_selected "documents/"
    send r
    assert_screen 'Rename documents'
    send C-a
    type_text "papers"
    send Enter
    assert_screen 'papers/'
    [ -f "$(FX)/papers/notes.md" ] || _fail "renamed directory lost its contents"
}

test_rename_esc_cancels() {
    app_start
    send G k k k # a.txt
    send r
    assert_screen 'Rename a\.txt'
    send C-a
    type_text "nope.txt"
    send Escape
    assert_selected "a.txt"
    [ -f "$(FX)/a.txt" ] || _fail "file renamed despite Esc"
    [ ! -e "$(FX)/nope.txt" ] || _fail "new name exists despite Esc"
}

test_rename_to_existing_name_is_refused() {
    app_start
    send G k k k # a.txt
    send r
    assert_screen 'Rename a\.txt'
    send C-a
    type_text "readme.txt"
    send Enter
    # The full "...already exists" message may wrap mid-word in the alert box
    assert_screen 'Failed to rename'
    [ -f "$(FX)/a.txt" ] || _fail "source file gone after refused rename"
    [ -f "$(FX)/readme.txt" ] || _fail "target file gone after refused rename"
}

# --------------------------------------------------------------- delete (d)

test_delete_file_after_confirm() {
    app_start
    send G k k k # a.txt
    assert_selected "a.txt"
    send d
    assert_screen 'Delete 1 item\? \(y/n\)'
    send y
    assert_screen '# Items:19'
    assert_not_screen 'a\.txt'
    [ ! -e "$(FX)/a.txt" ] || _fail "file still on disk after delete"
}

test_delete_any_other_key_cancels() {
    app_start
    send G k k k # a.txt
    send d
    assert_screen 'Delete 1 item\? \(y/n\)'
    send n
    assert_screen '# Items:20'
    [ -f "$(FX)/a.txt" ] || _fail "file deleted despite cancel"
}

test_delete_directory_recursively() {
    app_start
    send d # documents/ is selected on start
    assert_screen 'Delete 1 item\? \(y/n\)'
    send y
    assert_screen '# Items:19'
    [ ! -e "$(FX)/documents" ] || _fail "directory still on disk after delete"
}

test_delete_marked_files() {
    app_start
    send G k k k # a.txt
    send v j v   # mark a.txt, move down, mark hello.md
    send d
    assert_screen 'Delete 2 items\? \(y/n\)'
    send y
    assert_screen '# Items:18'
    [ ! -e "$(FX)/a.txt" ] || _fail "a.txt still on disk"
    [ ! -e "$(FX)/hello.md" ] || _fail "hello.md still on disk"
    [ -f "$(FX)/readme.txt" ] || _fail "unmarked file was deleted"
}

# ---------------------------------------------------------------- chmod (P)

test_chmod_changes_mode() {
    app_start
    send G k k k # a.txt
    assert_selected "a.txt"
    send P
    assert_screen 'Chmod 1 item \(octal\)'
    send C-a
    type_text "600"
    send Enter
    assert_screen '\-rw\-\-\-\-\-\-\-'
    wait_until [ "$(stat -c %a "$(FX)/a.txt")" = "600" ] ||
        _fail "mode on disk is $(stat -c %a "$(FX)/a.txt"), expected 600"
}

test_chmod_esc_cancels() {
    app_start
    send G k k k # a.txt
    send P
    assert_screen 'Chmod 1 item \(octal\)'
    send C-a
    type_text "600"
    send Escape
    send j
    assert_selected "hello.md" # normal mode again
    [ "$(stat -c %a "$(FX)/a.txt")" = "644" ] || _fail "mode changed despite Esc"
}

run_tests
