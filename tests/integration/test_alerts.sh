#!/usr/bin/env bash
# Integration tests for the alerts panel: raising alerts, the cap, dismissal by
# key and by click, and the panel taking its height out of the table.
#
# The panel renders above the breadcrumbs, newest alert first, inside a border
# titled "Alerts" whose right-hand hint names the clear key. Nothing outside a
# debug build raises an Info alert, so only Warn and Error appear here.

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

FX() { echo "$SANDBOX/fixtures"; }

# Submitting a path that cannot exist is the cheapest warning: the go-to prompt
# validates before navigating, and the message names the path, so each call
# raises an alert that is distinguishable from the last. A rejected path leaves
# the prompt open for a correction, hence the Escape.
warn_alert() { # warn_alert SUFFIX
    send :
    type_text "$(FX)/zz$1"
    send Enter
    assert_alert warn "Path does not exist: $(FX)/zz$1"
    send Escape
}

# Unlinking needs write permission on the containing directory, so a delete
# there fails in the worker and is reported as an error.
error_alert() {
    chmod 555 "$(FX)/no_delete"
    app_start "$(FX)/no_delete"
    send d
    assert_screen 'Delete 1 item\? \(y/n\)'
    send y
}

# ---------------------------------------------------------------- raising them

test_a_rejected_path_raises_a_warning() {
    app_start
    warn_alert 1
    assert_screen '┌Alerts'
    assert_screen '\(Press "Ctrl\+a" to clear\)'
    assert_alert warn " • Path does not exist: $(FX)/zz1"
}

test_a_failed_delete_raises_an_error() {
    error_alert
    assert_alert error "Permission denied"
    assert_alert error "another_protected.txt"
}

# The kinds share the panel and differ only in style, so a run that mislabels
# one would still print the right text.
test_a_warning_and_an_error_are_styled_apart() {
    error_alert
    assert_alert error "another_protected.txt"
    warn_alert 1
    assert_alert warn "zz1"
    [ "$(alert_lines warn | wc -l)" = 1 ] ||
        _fail "expected exactly one warning (got: $(alert_lines warn | tr '\n' '|'))"
    [ "$(alert_lines error | wc -l)" = 1 ] ||
        _fail "expected exactly one error (got: $(alert_lines error | tr '\n' '|'))"
}

test_the_newest_alert_is_listed_first() {
    app_start
    warn_alert 1
    warn_alert 2
    [ "$(alert_lines warn | head -1)" = " • Path does not exist: $(FX)/zz2" ] ||
        _fail "expected zz2 on top (alerts: $(alert_lines warn | tr '\n' '|'))"
}

# Five is the cap; a sixth drops the oldest rather than growing the panel.
test_a_sixth_alert_drops_the_oldest() {
    app_start
    local i
    for i in 1 2 3 4 5; do warn_alert "$i"; done
    assert_alert warn "zz1"
    warn_alert 6
    for i in 2 3 4 5 6; do
        assert_alert warn "$(FX)/zz$i"
    done
    if alert_lines warn | grep -Fq -- "$(FX)/zz1"; then
        _fail "the oldest alert should have been dropped"
    fi
    [ "$(alert_lines warn | wc -l)" = 5 ] ||
        _fail "expected five alerts (got: $(alert_lines warn | wc -l))"
}

# A message wider than the panel is split, each line but the last gaining an
# ellipsis, and the continuation lines are indented under the bullet.
test_a_long_alert_wraps_with_an_ellipsis() {
    app_start
    local long
    long="$(printf 'a%.0s' {1..120})"
    send :
    type_text "$(FX)/zz$long"
    send Enter
    assert_alert warn "Path does not exist"
    [ "$(alert_lines warn | wc -l)" = 2 ] ||
        _fail "expected the message to wrap onto two lines (got: $(alert_lines warn | wc -l))"
    assert_screen 'aaa…'
    [ "$(alert_lines warn | tail -1 | cut -c1-4)" = "   a" ] ||
        _fail "expected the continuation line indented under the bullet (got: '$(alert_lines warn | tail -1)')"
}

# ------------------------------------------------------------------ dismissing

test_the_clear_key_dismisses_the_panel() {
    app_start
    warn_alert 1
    send C-a
    assert_gone '┌Alerts'
    assert_selected "documents/"
}

test_a_click_in_the_panel_dismisses_it() {
    app_start
    warn_alert 1
    click 5 2 # the alert line itself
    assert_gone '┌Alerts'
    # The click landed on the panel, not on the row that moved up into its place
    assert_selected "documents/"
}

# The hit-test area covers the border, not just the text.
test_a_click_on_the_border_dismisses_it_too() {
    app_start
    warn_alert 1
    click 5 1
    assert_gone '┌Alerts'
}

test_a_click_below_the_panel_leaves_it_alone() {
    app_start
    warn_alert 1
    assert_selected "documents/"
    click 5 8 # a table row, three rows below the panel
    assert_selected "file_types/"
    assert_screen '┌Alerts'
}

# Mouse dispatch is deliberately not short-circuited, because the table accepts
# the wheel wherever the pointer is. A wheel over the panel therefore scrolls
# the table, and is not a click, so it does not dismiss anything.
test_the_wheel_over_the_panel_scrolls_the_table() {
    app_start
    warn_alert 1
    assert_selected "documents/"
    scroll_wheel down 5 2
    assert_selected "executables/"
    assert_screen '┌Alerts'
}

# ------------------------------------------------------------------- lifecycle

test_alerts_survive_navigation() {
    app_start
    warn_alert 1
    send g Enter # documents/
    assert_breadcrumbs "$(FX)/documents"
    assert_alert warn "zz1"
}

# Help replaces the whole layout, so the panel goes with it and comes back.
test_help_hides_the_panel_and_closing_it_brings_the_alerts_back() {
    app_start
    warn_alert 1
    send '?'
    assert_screen '┌Help'
    assert_not_screen '┌Alerts'
    send '?'
    assert_alert warn "zz1"
}

# The panel takes its height out of the table rather than overlaying it, so the
# viewport shrinks by one row per alert plus two for the border. Measured with
# L, which selects the last visible row.
test_the_panel_shrinks_the_table() {
    app_start "$(FX)/scrolling"
    send L
    assert_selected "026_continuing_on.txt"
    send g
    warn_alert 1 # one alert: three rows
    send L
    assert_selected "023_short_three.txt"
    send g
    local i
    for i in 2 3 4 5; do warn_alert "$i"; done # four more rows
    send L
    assert_selected "019_normal.go"
}

run_tests
