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
    assert_gone 'New directory'
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
    assert_not_screen ' a\.txt' # leading context keeps this off the notice area
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

# Unlinking needs write permission on the containing directory, not on the
# file. The Docker image runs the suites as a normal user for this reason.
test_delete_from_an_unwritable_directory_is_reported() {
    chmod 555 "$(FX)/no_delete"
    app_start "$(FX)/no_delete"
    assert_selected "another_protected.txt"
    send d
    assert_screen 'Delete 1 item\? \(y/n\)'
    send y
    assert_screen 'Failed to delete .*another_protected\.txt'
    [ -f "$(FX)/no_delete/another_protected.txt" ] ||
        _fail "the file was deleted from an unwritable directory"
    assert_running
}

# ------------------------------------------------- tasks (K, Ctrl+p)

# Bytes in the sparse file the copy tests use as a source. Creating it costs no
# disk, but the copy writes every byte, which is what keeps the task in flight
# long enough to act on: roughly two seconds at 1 GB/s. Both tests assert an
# effect that only their key produces, so a copy that somehow finished first
# fails them rather than passing silently.
SPARSE_SIZE=2147483648

# Starts copying the sparse file into documents/ and returns once the operation
# notice is up.
start_big_copy() {
    # Long enough for a slow disk to finish 2 GiB; the assertions themselves
    # settle much sooner than this.
    TIMEOUT=60
    truncate -s "$SPARSE_SIZE" "$(FX)/big.bin"
    app_start
    send G k k k # big.bin sorts between a.txt and hello.md
    assert_selected "big.bin"
    send y
    send g Enter # documents/
    assert_breadcrumbs "$(FX)/documents"
    send p
    assert_screen 'Copying .*big\.bin'
}

_copy_is_complete() { [ "$(stat -c %s "$(FX)/documents/big.bin" 2>/dev/null)" = "$SPARSE_SIZE" ]; }

# The size once the worker has stopped writing, whether it was cancelled or ran
# out of bytes.
settled_copy_size() {
    local size prev=-1 deadline=$((SECONDS + TIMEOUT))
    while :; do
        size=$(stat -c %s "$(FX)/documents/big.bin" 2>/dev/null || echo 0)
        [ "$size" = "$prev" ] && break
        ((SECONDS >= deadline)) && break
        prev=$size
        sleep 0.3
    done
    echo "$size"
}

test_cancel_stops_a_running_copy() {
    start_big_copy
    send K
    assert_screen 'Cancelled: Copying .*big\.bin'
    local size
    size=$(settled_copy_size)
    [ "$size" -lt "$SPARSE_SIZE" ] ||
        _fail "the copy ran to completion ($size bytes) despite being cancelled"
    # Documented: a cancelled copy leaves its partial file under the final name
    [ -f "$(FX)/documents/big.bin" ] || _fail "the partial copy was removed"
}

# Ctrl+p clears the progress notice; unlike K it does not touch the work.
test_clear_progress_hides_a_running_copy_without_stopping_it() {
    start_big_copy
    send C-p
    assert_gone 'Copying '
    # The notice has to be gone while there is still copying left to do, or
    # this shows nothing that simply finishing would not also show.
    ! _copy_is_complete || _fail "the copy finished before Ctrl+p could be observed"
    assert_not_screen 'Cancelled' # unlike K, it leaves the work alone
    wait_until _copy_is_complete ||
        _fail "the copy stopped when its progress was cleared ($(settled_copy_size) of $SPARSE_SIZE bytes)"
}

test_cancel_with_no_running_task_is_reported() {
    app_start
    send K
    assert_screen 'No active task to cancel'
    send C-a
    assert_gone 'No active task to cancel'
    assert_running
}

test_clear_progress_with_no_progress_changes_nothing() {
    app_start
    send C-p
    send j # a key whose effect is visible, so Ctrl+p was processed first
    assert_selected "executables/"
    assert_not_screen 'Alerts'
    assert_screen '# Items:20'
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
    assert_mode 600 "$(FX)/a.txt"
}

test_chmod_esc_cancels() {
    app_start
    send G k k k # a.txt
    send P
    assert_screen 'Chmod 1 item \(octal\)'
    send C-a
    type_text "600"
    send Escape
    assert_gone 'Chmod 1 item'
    send j
    assert_selected "hello.md" # normal mode again
    [ "$(stat -c %a "$(FX)/a.txt")" = "644" ] || _fail "mode changed despite Esc"
}

run_tests
