#!/usr/bin/env bash
# Integration tests for directory navigation, driven from the user's
# perspective: keyboard and mouse against the real filectrl binary in tmux.
#
# Sandbox layout per test (created by the harness):
#   $SANDBOX/fixtures/...   copy of the repo's fixtures directory
#   $SANDBOX/home/          fake $HOME containing home_marker.txt

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

# In a 30-row pane every fixture entry is visible; rows start on screen line 3
# (line 1 = breadcrumbs, line 2 = table header).
FIRST_ROW_Y=3

test_starts_in_given_directory_with_first_row_selected() {
    app_start
    assert_breadcrumbs "$SANDBOX/fixtures"
    assert_screen '# Items:20'
    assert_selected "documents/"
}

test_j_and_k_move_selection() {
    app_start
    send j
    assert_selected "executables/"
    send j
    assert_selected "file_types/"
    send k
    assert_selected "executables/"
}

test_arrow_keys_move_selection() {
    app_start
    send Down Down
    assert_selected "file_types/"
    send Up
    assert_selected "executables/"
}

test_selection_stops_at_first_and_last_row() {
    app_start
    send k # already on the first row
    assert_selected "documents/"
    send G k j j # past the last row
    assert_selected "readme.txt"
}

test_first_last_middle_keys() {
    app_start
    send G
    assert_selected "readme.txt"
    send g
    assert_selected "documents/"
    send z
    assert_selected "rabbit/"
    send End
    assert_selected "readme.txt"
    send Home
    assert_selected "documents/"
}

test_enter_opens_selected_directory() {
    app_start
    send Enter # documents/ is selected on start
    assert_breadcrumbs "$SANDBOX/fixtures/documents"
    assert_screen 'notes\.md'
    assert_screen '# Items:3'
}

test_l_and_right_arrow_open_selected_directory() {
    app_start
    send l
    assert_breadcrumbs "$SANDBOX/fixtures/documents"
    send h
    assert_breadcrumbs "$SANDBOX/fixtures"
    send Right
    assert_breadcrumbs "$SANDBOX/fixtures/documents"
}

test_h_goes_to_parent_and_reselects_child() {
    app_start
    send j Enter # enter executables/
    assert_breadcrumbs "$SANDBOX/fixtures/executables"
    send h
    assert_breadcrumbs "$SANDBOX/fixtures"
    # Coming back up, the directory we just left should be selected
    assert_selected "executables/"
}

test_backspace_and_left_arrow_go_to_parent() {
    app_start
    send Enter
    assert_breadcrumbs "$SANDBOX/fixtures/documents"
    send BSpace
    assert_breadcrumbs "$SANDBOX/fixtures"
    send Enter
    assert_breadcrumbs "$SANDBOX/fixtures/documents"
    send Left
    assert_breadcrumbs "$SANDBOX/fixtures"
}

test_dash_toggles_previous_directory() {
    app_start
    send Enter # -> documents
    assert_breadcrumbs "$SANDBOX/fixtures/documents"
    send - # back to fixtures
    assert_breadcrumbs "$SANDBOX/fixtures"
    assert_not_screen 'notes\.md'
    send - # forward to documents again
    assert_breadcrumbs "$SANDBOX/fixtures/documents"
}

test_tilde_goes_to_home_directory() {
    app_start
    send '~'
    assert_breadcrumbs "$SANDBOX/home"
    assert_screen 'home_marker\.txt'
}

test_navigate_nested_directories_and_back_to_start() {
    app_start
    send Enter # documents
    send h     # fixtures
    send G     # readme.txt (last)
    send g     # documents (first)
    send j j j j # images/  (after .hidden_dir)
    assert_selected "images/"
    send Enter
    assert_breadcrumbs "$SANDBOX/fixtures/images"
    assert_screen 'photo\.png'
    send h h
    assert_breadcrumbs "$SANDBOX"
    assert_selected "fixtures/"
}

test_goto_prompt_navigates_to_typed_path() {
    app_start
    send :
    type_text "$SANDBOX/fixtures/unicode"
    send Enter
    assert_breadcrumbs "$SANDBOX/fixtures/unicode"
}

test_goto_prompt_rejects_nonexistent_path() {
    app_start
    send :
    type_text "/nonexistent_path_xyz"
    send Enter
    # An error alert appears and the prompt stays open for correction
    assert_screen 'Path does not exist: /nonexistent_path_xyz'
    assert_screen 'Go to /nonexistent_path_xyz'
    send Escape # close the prompt
    send C-a    # clear the alert
    assert_breadcrumbs "$SANDBOX/fixtures"
    assert_running
}

test_goto_prompt_esc_cancels() {
    app_start
    send :
    type_text "/etc"
    send Escape
    assert_gone 'Go to /etc'
    send j
    assert_selected "executables/" # normal mode again; j moves selection
    assert_breadcrumbs "$SANDBOX/fixtures"
}

test_opening_a_file_does_not_navigate() {
    app_start
    send G k k k # a.txt (files sort after directories)
    assert_selected "a.txt"
    send Enter
    assert_breadcrumbs "$SANDBOX/fixtures"
    assert_running
}

test_parent_of_filesystem_root_is_root() {
    app_start
    send :
    type_text "/"
    send Enter
    assert_screen '# Items:'
    send h h
    assert_running
    assert_breadcrumbs "/"
}

test_click_selects_row() {
    app_start
    click 5 $((FIRST_ROW_Y + 4)) # 5th row: images/
    assert_selected "images/"
}

test_double_click_opens_directory() {
    app_start
    double_click 5 "$FIRST_ROW_Y" # first row: documents/
    assert_breadcrumbs "$SANDBOX/fixtures/documents"
    assert_screen 'notes\.md'
}

test_click_breadcrumb_ancestor_navigates_to_it() {
    app_start
    send Enter # -> documents
    assert_breadcrumbs "$SANDBOX/fixtures/documents"
    # Click in the middle of the "fixtures" breadcrumb segment
    local offset
    offset=$(breadcrumbs | grep -bo 'fixtures' | head -1 | cut -d: -f1)
    click $((offset + 4)) 1
    assert_breadcrumbs "$SANDBOX/fixtures"
    assert_not_screen 'notes\.md'
}

test_quit() {
    app_start
    send q
    local deadline=$((SECONDS + TIMEOUT))
    while app_is_running; do
        ((SECONDS >= deadline)) && { _fail "app did not quit"; return 1; }
        sleep 0.05
    done
}

run_tests
