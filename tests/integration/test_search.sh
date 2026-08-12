#!/usr/bin/env bash
# Integration tests for search interaction: where a walk is rooted, how its
# results are ordered when it ends, the depth and result limits, and cancelling
# one that is still running.
#
# The basic cases (a recursive match, Esc returning to the directory, opening a
# result) live in test_filter_search_sort.sh; this suite covers what the walk
# itself does.

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

FX() { echo "$SANDBOX/fixtures"; }

search_for() {
    send /
    assert_screen ' Search'
    type_text "$1"
    send Enter
}

_selected_name_is() { [ "$(selected_row | awk '{print $1}')" = "$1" ]; }

# The name column, compared exactly. A result's name carries its path relative
# to the search root, and assert_selected matches a substring, so it cannot
# tell "readme.txt" from "documents/readme.txt".
assert_selected_name() {
    wait_until _selected_name_is "$1" ||
        _fail "expected the selected row to be exactly '$1' (got: '$(selected_row | awk '{print $1}')')"
}

# Overrides one [file_system] key for the next app_start. The limits are
# configurable so that they can be reached without building a tree big enough
# to hit the shipped ones.
limit() { # limit KEY VALUE
    printf '[file_system]\n%s = %s\n' "$1" "$2" > "$SANDBOX/limits.toml"
    EXTRA_INCLUDES=("$SANDBOX/limits.toml")
}

# ---------------------------------------------------------------- the results

# Results append in walk order so partial ones are usable, and are ordered once
# the walk ends. The root is read first, so readme.txt is the first hit, and
# ending up in the middle is what says the listing was ordered afterwards.
test_results_are_ordered_by_their_rendered_path_when_the_walk_ends() {
    app_start
    search_for "readme"
    assert_screen 'scrolling/008_readme\.txt'
    send g
    assert_selected_name "documents/readme.txt"
    send j
    assert_selected_name "readme.txt"
    send j
    assert_selected_name "scrolling/008_readme.txt"
}

test_a_search_is_rooted_at_the_current_directory() {
    app_start
    send g Enter # documents/
    assert_breadcrumbs "$(FX)/documents"
    search_for "readme"
    assert_breadcrumbs "[Search] $(FX)/documents"
    assert_screen 'readme\.txt'
    # documents/ holds one of the three; the rest of the tree is out of scope
    assert_not_screen 'scrolling/'
    assert_not_screen 'documents/readme'
}

test_a_second_search_replaces_the_first_at_the_same_root() {
    app_start
    search_for "readme"
    assert_screen 'scrolling/008_readme\.txt'
    search_for "photo"
    assert_breadcrumbs "[Search] $(FX)"
    assert_screen 'images/photo\.png'
    assert_not_screen 'readme'
}

test_a_filter_narrows_the_results() {
    app_start
    search_for "readme"
    assert_screen 'scrolling/008_readme\.txt'
    send f
    type_text "documents"
    send Enter
    assert_screen '\[Filtered\] documents'
    assert_screen 'documents/readme\.txt'
    assert_not_screen 'scrolling/'
    # Esc resets the view rather than peeling the filter off the search: one
    # press drops both and returns to the directory, unlike a filter over a
    # plain listing, which Esc restores in place.
    send Escape
    assert_breadcrumbs "$(FX)"
    assert_gone '\[Filtered\]'
    assert_screen 'documents/'
}

# ----------------------------------------------------------------- the limits

test_without_a_depth_limit_the_deepest_match_is_found() {
    app_start
    search_for "rabbit"
    assert_screen 'plant/orange/rabbit'
    assert_screen 'orange/rabbit'
    assert_not_screen '┌Alerts'
}

# The limit turns away directories rather than entries, so a match at the
# boundary is still listed; it is what lies below it that goes unfound.
test_the_depth_limit_warns_and_leaves_the_deepest_match_unfound() {
    limit search_max_depth 1
    app_start
    search_for "rabbit"
    assert_screen 'orange/rabbit'
    assert_alert warn "Search reached maximum depth of 1 level;"
    assert_not_screen 'plant/orange/rabbit'
}

# One warning however many directories are turned away, so a wide tree cannot
# bury the listing under repeats. Two are turned away here: plant/orange and
# special_files/common.
test_the_depth_limit_warns_only_once() {
    limit search_max_depth 1
    app_start
    search_for "a"
    assert_alert warn "Search reached maximum depth of 1 level;"
    [ "$(alert_lines warn | wc -l)" = 1 ] ||
        _fail "expected one warning (got: $(alert_lines warn | tr '\n' '|'))"
}

# Truncating is a normal finish, not a cancellation: the results that did
# arrive stay listed and no cancelled notice appears.
test_the_result_limit_warns_and_truncates() {
    limit search_max_results 1
    app_start
    search_for "readme"
    assert_alert warn "Search stopped at 1 result"
    assert_screen 'readme\.txt'
    assert_not_screen 'documents/readme'
    assert_not_screen '008_readme'
    assert_not_screen 'Cancelled'
}

# ------------------------------------------------------------- cancelling one

# A tree big enough that the walk is still running when the test acts on it.
# Every file matches, and a match costs a stat where a miss costs a name
# comparison, which is what stretches the walk to roughly a second. The result
# limit is raised so that truncation cannot end it first.
SEARCH_DIRS=100
SEARCH_FILES=1000

# The name column of every result row on screen. The rows the big tree
# produces are the only ones shaped like this, so a notice or a header cannot
# be mistaken for one.
visible_result_names() { screen | awk '$1 ~ /^d[0-9]+\/hit_/ { print $1 }'; }

_results_are_ordered() {
    local names
    names="$(visible_result_names)"
    [ -n "$names" ] && [ "$names" = "$(printf '%s\n' "$names" | LC_ALL=C sort)" ]
}

# Polls, because the walk orders its results when it announces its exit, which
# is a moment later than the notice that says it was cancelled.
assert_results_ordered() {
    wait_until _results_are_ordered ||
        _fail "expected the visible results to be ordered (got: $(visible_result_names | tr '\n' ' '))"
}

# Starts a search that is still walking when this returns.
start_slow_search() {
    TIMEOUT=60
    limit search_max_results 5000000
    local d
    for d in $(seq 1 "$SEARCH_DIRS"); do
        mkdir -p "$SANDBOX/big/d$d"
        seq 1 "$SEARCH_FILES" | sed "s|^|$SANDBOX/big/d$d/hit_|" | xargs -d '\n' touch
    done
    app_start "$SANDBOX/big"
    search_for "hit_"
    assert_screen '\[Searching\.\.\.\]'
}

# Only a running search can be cancelled, so a walk that somehow finished first
# fails this rather than passing silently: the notice it asserts is one that a
# completed search never shows.
test_cancelling_a_running_search_keeps_its_results_and_its_marks() {
    start_slow_search
    send g
    send v
    assert_marked_count '1 item'
    local marked
    marked="$(selected_row | awk '{print $1}')"
    [ -n "$marked" ] || _fail "no row was selected to mark"
    send K
    assert_screen "Cancelled: \[Searching\] hit_"
    # The walk announces its exit whether it finished or was stopped, and that
    # is what orders the results it had. Leaving them in walk order would leave
    # the header advertising an order the listing never takes.
    assert_results_ordered
    # The marked entry has moved in that reorder, and both the mark and the
    # cursor moved with it rather than staying on a row index.
    assert_marked_count '1 item'
    assert_selected_name "$marked"
    send Escape
    assert_gone 'Cancelled'
    assert_breadcrumbs "$SANDBOX/big"
    assert_running
}

# The walk self-cancels before announcing its exit, so a cancel that arrives
# afterwards finds nothing to cancel and cannot relabel a finished search.
test_the_cancel_key_does_nothing_once_the_walk_has_finished() {
    app_start
    search_for "readme"
    assert_screen 'scrolling/008_readme\.txt'
    send K
    send j # a key whose effect is visible, so the K above was processed first
    assert_selected "readme.txt"
    assert_not_screen 'Cancelled'
    assert_screen 'scrolling/008_readme\.txt'
}

run_tests
