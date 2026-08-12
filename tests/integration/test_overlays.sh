#!/usr/bin/env bash
# Integration tests for the help overlay (?) and the "open with" picker (o).

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

test_help_toggles_with_question_mark() {
    app_start
    send '?'
    assert_screen 'Help'
    assert_screen 'Normal Mode'
    assert_screen 'Select next, previous row'
    send '?'
    assert_gone 'Normal Mode'
    assert_screen 'documents/' # back to the table
}

test_help_esc_closes() {
    app_start
    send '?'
    assert_screen 'Normal Mode'
    send Escape
    assert_gone 'Normal Mode'
}

test_help_captures_keys_from_the_table() {
    app_start
    send '?'
    assert_screen 'Normal Mode'
    send j j # must scroll the help view, not move the table selection
    send Escape
    assert_gone 'Normal Mode'
    assert_selected "documents/" # table selection unchanged
}

test_open_with_lists_the_configured_opener() {
    app_start
    send G k k k # a.txt
    assert_selected "a.txt"
    send o
    assert_screen 'Open a\.txt with'
    assert_screen 'true %s +openers\.open_file'
}

test_open_with_on_a_directory_lists_the_directory_opener() {
    app_start
    send o # documents/ is selected on start
    assert_screen 'Open documents/ with'
    assert_screen 'true %s +openers\.open_directory'
}

test_open_with_esc_closes() {
    app_start
    send o
    assert_screen 'Open documents/ with'
    send Escape
    assert_gone 'Open documents/ with'
    send j
    assert_selected "executables/" # normal mode again
}

test_open_with_enter_runs_the_opener() {
    app_start
    send G k k k # a.txt (opener is a stub, so nothing visible happens)
    send o
    assert_screen 'Open a\.txt with'
    send Enter
    assert_gone 'Open a\.txt with'
    assert_running
    assert_breadcrumbs "$SANDBOX/fixtures"
}

run_tests
