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
    assert_gone '\[Copy\]'
}

test_cut_paste_moves_file() {
    app_start
    send G k k k # a.txt
    send x
    assert_screen '\[Cut\] .*/fixtures/a\.txt'
    send g Enter # documents/
    assert_breadcrumbs "$(FX)/documents"
    send p
    # A clean move clears the clipboard, so the [Cut] notice disappearing
    # signals completion
    assert_gone '\[Cut\]'
    wait_until [ -f "$(FX)/documents/a.txt" ] || _fail "file not moved to destination"
    wait_until [ ! -e "$(FX)/a.txt" ] || _fail "source file still exists after a move"
}

test_copy_esc_clears_clipboard() {
    app_start
    send y # copy documents/
    assert_screen '\[Copy\]'
    send Escape
    assert_gone '\[Copy\]'
    send p
    assert_screen 'Cannot paste: no system clipboard available'
}

test_paste_with_nothing_copied_alerts() {
    app_start
    send p
    assert_screen 'Cannot paste: no system clipboard available'
    assert_not_screen '\[Copy\]|\[Cut\]' # the failed read left nothing behind
    assert_screen '# Items:20'           # and nothing was pasted
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
    echo "from fixtures root" > "$(FX)/readme.txt"
    app_start
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
    echo "from fixtures root" > "$(FX)/readme.txt"
    app_start
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

# Copies notes.md and readme.txt from the fixtures root, both of which already
# exist in documents/, and pastes them there: two collisions in one paste, so
# the answer to the first is what decides whether the second is asked about.
two_collisions() {
    echo "from fixtures root" > "$(FX)/notes.md"
    echo "from fixtures root" > "$(FX)/readme.txt"
    app_start
    send G   # readme.txt (last row)
    send v k v # mark readme.txt and notes.md
    send y
    send g Enter # documents/
    assert_breadcrumbs "$(FX)/documents"
    send p
    assert_screen '"notes\.md" exists: \[s\]kip'
}

test_paste_conflict_skip_all_answers_the_rest_of_the_batch() {
    two_collisions
    send S
    assert_gone 'exists:' # the second collision is settled without asking
    wait_until grep -q "Hello" "$(FX)/documents/readme.txt" ||
        _fail "readme.txt was overwritten despite skip all"
    [ ! -s "$(FX)/documents/notes.md" ] || _fail "notes.md was overwritten despite skip all"
    # Nothing was pasted, so the clipboard is kept for a retry
    assert_screen '\[Copy\] 2 items'
}

test_paste_conflict_overwrite_all_answers_the_rest_of_the_batch() {
    two_collisions
    send O
    assert_gone 'exists:'
    wait_until grep -q "from fixtures root" "$(FX)/documents/notes.md" ||
        _fail "notes.md was not overwritten"
    wait_until grep -q "from fixtures root" "$(FX)/documents/readme.txt" ||
        _fail "readme.txt was not overwritten by the standing answer"
    assert_gone '\[Copy\]' # a clean paste clears the clipboard
}

# An unbound key abandons the paste rather than resolving the collision, and
# the clipboard is restored so the whole batch can be retried.
test_paste_conflict_unrecognized_key_abandons_the_paste() {
    two_collisions
    send q
    assert_gone 'exists:'
    assert_running # q is quit in normal mode, not at a conflict prompt
    [ ! -s "$(FX)/documents/notes.md" ] || _fail "notes.md was written by an abandoned paste"
    wait_until grep -q "Hello" "$(FX)/documents/readme.txt" ||
        _fail "readme.txt was written by an abandoned paste"
    assert_screen '\[Copy\] 2 items'
}

# Overwrite is not offered when the existing entry is a directory, so `o` is
# ignored instead of abandoning the batch.
test_paste_conflict_overwrite_key_is_ignored_for_a_directory() {
    mkdir "$(FX)/documents/images"
    app_start
    send j j j j # images/
    assert_selected "images/"
    send y
    send g Enter # documents/
    assert_breadcrumbs "$(FX)/documents"
    send p
    assert_screen '"images" exists as a directory: \[s\]kip, \[S\]kip all'
    send o
    # Still asking: an ignored key must not resolve or abandon the collision
    assert_screen '"images" exists as a directory'
    send s
    assert_gone 'exists as a directory'
    [ -z "$(ls -A "$(FX)/documents/images")" ] || _fail "the directory was merged"
    assert_screen '\[Copy\]'
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
