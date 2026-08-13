#!/usr/bin/env bash
# Integration tests for the help overlay (?), the "open with" picker (o), and
# the keys that hand the current directory to an external program (t, w).
#
# The picker reads the application database from the XDG data directories,
# which the harness points at the sandbox. A test therefore gets exactly the
# applications it writes there, and none of the machine's own.

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

FX() { echo "$SANDBOX/fixtures"; }

# Two applications for text/plain, plus the type itself: the picker resolves a
# name to a type before it can offer anything.
two_text_applications() {
    write_mime_globs "text/plain:*.txt"
    write_desktop_entry "Alpha" "true --alpha %f" "MimeType=text/plain;"
    write_desktop_entry "Zebra" "true --zebra %f" "MimeType=text/plain;"
}

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
    send_escape
    assert_gone 'Normal Mode'
}

test_help_captures_keys_from_the_table() {
    app_start
    send '?'
    assert_screen 'Normal Mode'
    send j j # must scroll the help view, not move the table selection
    send_escape
    assert_gone 'Normal Mode'
    assert_selected "documents/" # table selection unchanged
}

# The keybinding list is taller than the pane, and opening the help resets the
# scroll, so it always starts at the first section.
test_help_scrolls_and_reopens_at_the_top() {
    app_start
    send '?'
    assert_screen 'Normal Mode'
    send PgDn
    assert_gone 'Normal Mode' # the first section scrolled off the top
    assert_screen 'Add bookmark'
    send g
    assert_screen 'Normal Mode'
    send PgDn
    assert_gone 'Normal Mode'
    send '?' # close
    assert_screen 'documents/'
    send '?' # and reopen
    assert_screen 'Normal Mode'
}

test_the_wheel_scrolls_the_help() {
    app_start
    send '?'
    assert_screen 'Normal Mode'
    scroll_wheel down 40 10
    scroll_wheel down 40 10
    assert_gone 'Normal Mode'
    scroll_wheel up 40 10
    scroll_wheel up 40 10
    assert_screen 'Normal Mode'
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
    send_escape
    assert_gone 'Open documents/ with'
    send j
    assert_selected "executables/" # normal mode again
}

test_open_with_enter_runs_the_opener() {
    trace_openers
    app_start
    send G k k k # a.txt
    send o
    assert_screen 'Open a\.txt with'
    send Enter
    assert_gone 'Open a\.txt with'
    assert_opener_ran open_file
    assert_running
    assert_breadcrumbs "$SANDBOX/fixtures"
}

# The configured opener is always offered; the applications come from the
# database, sorted, with the one that would be used marked.
test_the_picker_lists_the_applications_for_the_type() {
    two_text_applications
    app_start
    send G k k k # a.txt
    assert_selected "a.txt"
    send o
    assert_screen 'Open a\.txt with'
    assert_screen '1\. Alpha +Alpha \(default\)'
    assert_screen '2\. Zebra +Zebra'
    assert_screen '3\. true %s +openers\.open_file'
}

# The picker reports the default the desktop already resolves to rather than
# choosing one itself, so a mimeapps.list entry moves the marker.
test_a_default_from_mimeapps_list_is_offered_first() {
    two_text_applications
    printf '[Default Applications]\ntext/plain=Zebra.desktop\n' > "$SANDBOX/xdg/mimeapps.list"
    app_start
    send G k k k
    send o
    assert_screen 'Open a\.txt with'
    assert_screen '1\. Zebra +Zebra \(default\)'
    assert_screen '2\. Alpha +Alpha'
}

test_an_application_for_another_type_is_not_offered() {
    write_mime_globs "text/plain:*.txt" "image/png:*.png"
    write_desktop_entry "Alpha" "true --alpha %f" "MimeType=text/plain;"
    write_desktop_entry "Pixels" "true --pixels %f" "MimeType=image/png;"
    app_start
    send G k k k # a.txt
    send o
    assert_screen 'Open a\.txt with'
    assert_screen 'Alpha'
    assert_not_screen 'Pixels'
}

# Choosing the second entry has to run the second application. The Exec line
# leaves a file behind, which is the only way to tell from outside which one
# was launched.
test_choosing_an_application_runs_that_one() {
    write_mime_globs "text/plain:*.txt"
    write_desktop_entry "Alpha" "true --alpha %f" "MimeType=text/plain;"
    write_desktop_entry "Zebra" "touch $SANDBOX/zebra_ran %f" "MimeType=text/plain;"
    app_start
    send G k k k
    send o
    assert_screen '2\. Zebra'
    send j
    send Enter
    assert_gone 'Open a\.txt with'
    wait_until [ -f "$SANDBOX/zebra_ran" ] || _fail "the chosen application did not run"
}

# `run_in_terminal = ""` in the test config means an application that asks for
# a terminal has no way to be launched, so the picker leaves it out rather than
# offering something that cannot work.
test_an_application_needing_a_terminal_is_left_out() {
    write_mime_globs "text/plain:*.txt"
    write_desktop_entry "Alpha" "true --alpha %f" "MimeType=text/plain;"
    write_desktop_entry "Termy" "true --termy %f" "MimeType=text/plain;" "Terminal=true"
    app_start
    send G k k k
    send o
    assert_screen 'Open a\.txt with'
    assert_screen 'Alpha'
    assert_not_screen 'Termy'
}

# Setting run_in_terminal gives it a way to be launched, and then it is offered.
test_a_terminal_application_is_offered_when_one_is_configured() {
    write_mime_globs "text/plain:*.txt"
    write_desktop_entry "Termy" "true --termy %f" "MimeType=text/plain;" "Terminal=true"
    printf '[openers.linux]\nrun_in_terminal = "true %%s"\n' > "$SANDBOX/terminal.toml"
    EXTRA_INCLUDES=("$SANDBOX/terminal.toml")
    app_start
    send G k k k
    send o
    assert_screen 'Open a\.txt with'
    assert_screen 'Termy'
}

# t and w hand the current directory to a program and change nothing on screen,
# so each is traced: without the marker these would pass with the key doing
# nothing at all. The rest of each test is what must NOT have happened.
test_open_current_directory_runs_its_opener_without_navigating() {
    trace_openers
    app_start
    send t
    assert_opener_ran open_directory
    send j
    assert_selected "executables/"
    assert_not_screen 'Alerts'
    assert_breadcrumbs "$SANDBOX/fixtures"
}

test_open_new_window_leaves_this_one_alone() {
    trace_openers
    app_start
    send w
    assert_opener_ran open_filectrl_window
    send j
    assert_selected "executables/"
    assert_not_screen 'Alerts'
    assert_breadcrumbs "$SANDBOX/fixtures"
    assert_running
}

run_tests
