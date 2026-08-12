#!/usr/bin/env bash
# Integration tests for filtering (f), recursive search (/), sorting (n/m/s,
# header clicks), and the hidden-files toggle (.).

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

FX() { echo "$SANDBOX/fixtures"; }

# ---------------------------------------------------------------- filter (f)

test_filter_narrows_table() {
    app_start
    send f
    assert_screen ' Filter'
    type_text "txt"
    send Enter
    assert_screen '\[Filtered\] txt'
    assert_screen 'a\.txt'
    assert_screen 'readme\.txt'
    assert_not_screen 'hello\.md'
    assert_not_screen 'documents/'
}

test_filter_esc_restores_full_listing() {
    app_start
    send f
    type_text "txt"
    send Enter
    assert_screen '\[Filtered\] txt'
    send Escape
    assert_gone '\[Filtered\]'
    assert_screen 'hello\.md'
    assert_screen 'documents/'
}

test_filter_with_no_matches_shows_empty_table() {
    app_start
    send f
    type_text "zzzznothing"
    send Enter
    assert_screen '\[Filtered\] zzzznothing'
    assert_not_screen 'a\.txt'
    send Escape
    assert_screen 'a\.txt'
}

test_filter_selection_moves_within_matches_only() {
    app_start
    send f
    type_text "txt"
    send Enter
    assert_screen '\[Filtered\] txt'
    send g
    assert_selected "a.txt"
    send j
    assert_selected "readme.txt"
}

test_filter_clears_when_navigating() {
    app_start
    send f
    type_text "doc"
    send Enter
    assert_screen '\[Filtered\] doc'
    send g Enter # documents/ is the only match
    assert_breadcrumbs "$(FX)/documents"
    assert_gone '\[Filtered\]'
    assert_screen 'notes\.md'
}

# ------------------------------------------------------------- sort (n/m/s)

test_sort_by_name_toggles_direction() {
    app_start
    assert_screen '\[N\]ame⌃' # ascending by default
    send n
    assert_screen '\[N\]ame⌄'
    send g
    assert_selected "various_extensions/"
    send n
    assert_screen '\[N\]ame⌃'
    send g
    assert_selected "documents/"
}

test_sort_by_size() {
    app_start
    send s
    assert_screen '\[S\]ize⌄' # size defaults to largest first: directories (4K)
    send g
    assert_selected "4K"
    send s
    assert_screen '\[S\]ize⌃' # smallest first: empty files
    send g
    assert_selected " 0 "
}

test_sort_by_modified() {
    touch -t 200001010000 "$(FX)/z_from_2000.txt"
    app_start
    send m
    assert_screen '\[M\]odified⌄' # modified defaults to newest first
    send G
    assert_selected "z_from_2000.txt"
    send m
    assert_screen '\[M\]odified⌃' # oldest first
    send g
    assert_selected "z_from_2000.txt"
}

test_sort_persists_across_navigation() {
    app_start
    send s
    assert_screen '\[S\]ize⌄'
    send : # navigate by path to keep the sort probe independent of row order
    type_text "$(FX)/documents"
    send Enter
    assert_breadcrumbs "$(FX)/documents"
    # The direction arrow, not the header, is the signal: every column header
    # renders at this width whatever the sort column is.
    assert_screen '\[S\]ize⌄'
    assert_not_screen '\[N\]ame⌃'
}

test_click_header_sorts_by_that_column() {
    app_start
    local offset
    offset=$(screen | sed -n '2p' | grep -bo '\[S\]ize' | head -1 | cut -d: -f1)
    click $((offset + 2)) 2
    assert_screen '\[S\]ize⌄' # size defaults to largest first
    click $((offset + 2)) 2
    assert_screen '\[S\]ize⌃' # clicking the same column toggles direction
    offset=$(screen | sed -n '2p' | grep -bo '\[N\]ame' | head -1 | cut -d: -f1)
    click $((offset + 2)) 2
    assert_screen '\[N\]ame⌃' # name defaults to ascending
}

# ------------------------------------------------- hidden-files toggle (.)

test_toggle_hidden_files() {
    app_start
    assert_screen '\.hidden_file'
    send .
    assert_gone '\.hidden_file'
    assert_not_screen '\.hidden_dir'
    send .
    assert_screen '\.hidden_file'
}

# ---------------------------------------------------------------- search (/)

test_search_lists_recursive_matches() {
    app_start
    send /
    assert_screen ' Search'
    type_text "photo"
    send Enter
    assert_breadcrumbs "[Search] $(FX)"
    assert_screen 'images/photo\.gif'
    assert_screen 'images/photo\.jpg'
    assert_screen 'images/photo\.png'
    assert_not_screen 'documents/'
}

test_search_esc_returns_to_directory() {
    app_start
    send /
    type_text "photo"
    send Enter
    assert_breadcrumbs "[Search] $(FX)"
    send Escape
    assert_breadcrumbs "$(FX)"
    assert_screen 'documents/'
    assert_screen '# Items:20'
}

test_search_with_no_matches_shows_empty_results() {
    app_start
    send /
    type_text "zzzznothing"
    send Enter
    assert_breadcrumbs "[Search] $(FX)"
    assert_not_screen 'documents/'
    send Escape
    assert_breadcrumbs "$(FX)"
}

test_search_open_result_does_not_crash() {
    app_start
    send /
    type_text "photo"
    send Enter
    assert_screen 'images/photo\.gif'
    send g Enter # open the first result (a file; opener is stubbed)
    assert_running
}

run_tests
