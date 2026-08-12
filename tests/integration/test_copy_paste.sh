#!/usr/bin/env bash
# Integration tests for copy/cut/paste. These run without a system clipboard
# (headless), exercising the in-process fallback: copy/paste works within one
# window, and paste alerts when there is nothing to paste and no system
# clipboard to read (an entry from another window would be unreachable).

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

FX() { echo "$SANDBOX/fixtures"; }

test_copy_paste_file_into_another_directory() {
    app_start
    send G k k k # a.txt
    assert_selected "a.txt"
    send y
    assert_screen '\[Copy\] .*/fixtures/a\.txt'
    send g Enter # documents/
    assert_breadcrumbs "$(FX)/documents"
    send p
    assert_screen 'a\.txt'
    assert_screen '# Items:4'
    [ -f "$(FX)/documents/a.txt" ] || _fail "file not pasted on disk"
    [ -f "$(FX)/a.txt" ] || _fail "source file removed by a copy"
    # A clean paste clears the clipboard, so the notice disappears
    assert_not_screen '\[Copy\]'
}

test_cut_paste_moves_file() {
    app_start
    send G k k k # a.txt
    send x
    assert_screen '\[Cut\] .*/fixtures/a\.txt'
    send g Enter # documents/
    assert_breadcrumbs "$(FX)/documents"
    send p
    assert_screen 'a\.txt'
    [ -f "$(FX)/documents/a.txt" ] || _fail "file not moved to destination"
    wait_until [ ! -e "$(FX)/a.txt" ] || _fail "source file still exists after a move"
}

test_copy_esc_clears_clipboard() {
    app_start
    send y # copy documents/
    assert_screen '\[Copy\]'
    send Escape
    assert_not_screen '\[Copy\]'
    send p
    assert_screen 'Cannot paste: no system clipboard available'
}

test_paste_with_nothing_copied_alerts() {
    app_start
    send p
    assert_screen 'Cannot paste: no system clipboard available'
    assert_screen '# Items:20' # nothing changed
    assert_running
}

test_copy_marked_files_pastes_all() {
    app_start
    send G k k k # a.txt
    send v j v   # mark a.txt and hello.md
    send y
    assert_screen '\[Copy\]'
    send g Enter # documents/
    assert_breadcrumbs "$(FX)/documents"
    send p
    assert_screen '# Items:5'
    [ -f "$(FX)/documents/a.txt" ] || _fail "a.txt not pasted"
    [ -f "$(FX)/documents/hello.md" ] || _fail "hello.md not pasted"
}

test_copy_directory_pastes_recursively() {
    app_start
    send y # documents/ is selected on start
    assert_screen '\[Copy\] .*/fixtures/documents'
    send j j j j Enter # images/
    assert_breadcrumbs "$(FX)/images"
    send p
    assert_screen 'documents/'
    [ -f "$(FX)/images/documents/notes.md" ] ||
        _fail "directory not pasted recursively"
    [ -f "$(FX)/documents/notes.md" ] || _fail "source directory was moved, not copied"
}

test_paste_conflict_skip_keeps_destination() {
    app_start
    echo "from fixtures root" > "$(FX)/readme.txt"
    send G # readme.txt, which also exists in documents/
    assert_selected "readme.txt"
    send y
    assert_screen '\[Copy\]'
    send g Enter # documents/
    assert_breadcrumbs "$(FX)/documents"
    send p
    assert_screen '"readme\.txt" exists: \[s\]kip, \[S\]kip all, \[o\]verwrite, \[O\]verwrite all'
    send s
    wait_until grep -q "Hello" "$(FX)/documents/readme.txt" ||
        _fail "destination was modified despite skip"
    # Nothing was pasted, so the clipboard is kept for a retry
    assert_screen '\[Copy\]'
}

test_paste_conflict_overwrite_replaces_destination() {
    app_start
    echo "from fixtures root" > "$(FX)/readme.txt"
    send G
    assert_selected "readme.txt"
    send y
    send g Enter # documents/
    assert_breadcrumbs "$(FX)/documents"
    send p
    assert_screen '"readme\.txt" exists'
    send o
    wait_until grep -q "from fixtures root" "$(FX)/documents/readme.txt" ||
        _fail "destination was not overwritten"
}

test_second_paste_after_clean_paste_alerts() {
    app_start
    send G k k k y # copy a.txt
    send g Enter   # documents/
    assert_breadcrumbs "$(FX)/documents"
    send p
    wait_until [ -f "$(FX)/documents/a.txt" ] || _fail "first paste failed"
    # The clean paste cleared the clipboard; without a system clipboard to
    # fall back to, a second paste has nothing to read and says so.
    send p
    assert_screen 'Cannot paste: no system clipboard available'
}

run_tests
