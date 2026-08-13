# Integration test harness: drives the real filectrl binary inside a tmux pane.
#
# Sourced by test_*.sh suites. Provides:
#   app_start [dir]        launch filectrl in a fresh tmux session (returns
#                          once the screen has settled and a row is selected)
#   app_stop               kill the tmux session
#   send KEY...            send keys (tmux key names: j, Enter, Escape, Left, ...)
#   send_escape            Escape, separated from whatever is sent next (use
#                          this for every Escape, never `send Escape`)
#   type_text TEXT         send literal text (for prompts)
#   click X Y / double_click X Y / scroll_wheel up|down X Y
#   drag X Y1 Y2 / mouse_down X Y / mouse_drag X Y / mouse_up X Y
#                          SGR mouse events (1-based screen coordinates)
#   resize_window COLS ROWS
#   screen                 plain-text screen capture
#   selected_row           text of the highlighted table row (via marker theme)
#   marked_rows            text of marked rows other than the cursor row
#   alert_lines warn|error the alert lines of one kind (via marker theme)
#   breadcrumbs            the path line above the table header
#   wait_until COMMAND...  poll until COMMAND succeeds or TIMEOUT elapses
#   wait_settled [CMD...]  poll until the screen stops changing
#   wait_for_selection     poll until a row is selected (after a navigation,
#                          before a key that acts on the selection)
#   assert_screen REGEX / assert_not_screen REGEX / assert_gone REGEX
#   assert_selected TEXT / assert_selected_name NAME / assert_running
#   assert_breadcrumbs TEXT
#   assert_marked TEXT / assert_marked_count "N items"
#   assert_alert warn|error TEXT
#   assert_mode OCTAL PATH
#   write_desktop_entry NAME EXEC [LINE...]   an application for the picker
#   write_mime_globs "TYPE:GLOB" ...          a minimal MIME database
#   cleanup_later PATH     remove a path outside the sandbox along with it
#   run_filectrl ARG...    run the binary outside tmux (RUN_OUTPUT/RUN_STATUS)
#   assert_run_succeeded / assert_run_failed / assert_run_output REGEX
#
# Set COLORS_256=1 in a test before app_start to run the app with
# --no-truecolor; the marker theme covers that palette too. Set EXTRA_INCLUDES
# to a list of TOML paths to merge on top of the config.
#
# Each test is a shell function named test_*; run_tests discovers and runs
# them, giving every test a fresh sandbox (a copy of fixtures/, a fake $HOME,
# and its own copy of the config) and a fresh app.

set -u

# tmux refuses to start without a UTF-8 locale; C.UTF-8 is built into glibc so
# it works even in slim containers with no locale data installed.
export LC_ALL=C.UTF-8

# Git records only 644 and 755, so a working tree's fixture modes are whatever
# the checkout umask produced, and `cp` (which does not preserve modes) applies
# the runner's umask again. Fixing it here is what lets the chmod tests assert
# literal modes.
umask 022

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
FILECTRL_BIN="${FILECTRL_BIN:-$REPO_ROOT/target/release/filectrl}"
TMUX=(tmux -L "filectrl-it-$$")
SESSION="it"
COLS=120
ROWS=30
TIMEOUT="${FILECTRL_IT_TIMEOUT:-5}"

# Extra TOML files merged on top of the marker theme, highest precedence last.
# Set by a test before app_start to override config or keybindings.
EXTRA_INCLUDES=()

# Set to 1 by a test before app_start to run the app with --no-truecolor. The
# marker theme carries a [theme256] section too, so selected_row/marked_rows
# work in both modes; only the SGR fragment they look for differs.
COLORS_256=0

# The marker theme paints the selected table row with bg #010203 (256-color
# index 17) and marked rows with bg #040506 (index 53), which appear in
# `capture-pane -e` output as the unique SGR fragments below.
MARKER_TRUECOLOR='48;2;1;2;3'
MARKED_MARKER_TRUECOLOR='48;2;4;5;6'
MARKER_256='48;5;17'
MARKED_MARKER_256='48;5;53'
MARKER="$MARKER_TRUECOLOR"
MARKED_MARKER="$MARKED_MARKER_TRUECOLOR"

# The same technique for the alert kinds, which differ only in foreground.
ALERT_WARN_TRUECOLOR='38;2;10;11;12'
ALERT_ERROR_TRUECOLOR='38;2;13;14;15'
ALERT_WARN_256='38;5;55'
ALERT_ERROR_256='38;5;56'
ALERT_WARN="$ALERT_WARN_TRUECOLOR"
ALERT_ERROR="$ALERT_ERROR_TRUECOLOR"

# The config directory is resolved from the environment, so a host that sets
# XDG_CONFIG_HOME would otherwise pull the developer's own config into a run
# that reaches for the default path.
#
# The "open with" picker reads desktop entries from $XDG_DATA_HOME and
# $XDG_DATA_DIRS and MIME associations from $XDG_CONFIG_DIRS, all of which fall
# back to the machine's own directories when unset. Those are pointed at the
# sandbox rather than dropped: unset means /usr/share, so the picker would
# offer whatever the developer has installed, and Enter would launch it.
# Set per test, once the sandbox exists.
HERMETIC_ENV=()
_set_hermetic_env() {
    HERMETIC_ENV=(
        -u XDG_CONFIG_HOME
        "XDG_CONFIG_DIRS=$SANDBOX/xdg"
        "XDG_DATA_HOME=$SANDBOX/xdg"
        "XDG_DATA_DIRS=$SANDBOX/xdg"
    )
}

# Writes a desktop entry into the sandbox's applications directory, so a test
# can give the picker something to offer.
# write_desktop_entry NAME EXEC [EXTRA_LINE...]
write_desktop_entry() {
    local name="$1" exec_line="$2"
    shift 2
    mkdir -p "$SANDBOX/xdg/applications"
    {
        printf '[Desktop Entry]\nType=Application\nName=%s\nExec=%s\n' "$name" "$exec_line"
        printf '%s\n' "$@"
    } > "$SANDBOX/xdg/applications/$name.desktop"
}

# A minimal shared-MIME-info database. The real one lives under the data
# directories the sandbox replaces, so without this nothing resolves a name to
# a type and the picker has only the configured opener to offer.
# write_mime_globs "text/plain:*.txt" ...
write_mime_globs() {
    mkdir -p "$SANDBOX/xdg/mime"
    printf '%s\n' "$@" > "$SANDBOX/xdg/mime/globs"
}

PASS=0
FAIL=0
FAILED_TESTS=()

fatal() {
    echo "FATAL: $*" >&2
    exit 1
}

# ---------------------------------------------------------------- app control

app_start() {
    local dir="${1:-$SANDBOX/fixtures}"
    local color_args=()
    if ((COLORS_256)); then
        color_args=(--no-truecolor)
        MARKER="$MARKER_256"
        MARKED_MARKER="$MARKED_MARKER_256"
        ALERT_WARN="$ALERT_WARN_256"
        ALERT_ERROR="$ALERT_ERROR_256"
    else
        MARKER="$MARKER_TRUECOLOR"
        MARKED_MARKER="$MARKED_MARKER_TRUECOLOR"
        ALERT_WARN="$ALERT_WARN_TRUECOLOR"
        ALERT_ERROR="$ALERT_ERROR_TRUECOLOR"
    fi
    local include_args=(-i "$HERE/marker_theme.toml") extra
    for extra in "${EXTRA_INCLUDES[@]}"; do
        include_args+=(-i "$extra")
    done
    "${TMUX[@]}" kill-server 2>/dev/null || true
    # DISPLAY/WAYLAND_DISPLAY are dropped so the app never finds a system
    # clipboard: the copy/paste suite asserts the no-clipboard fallback, and a
    # test run must not read or overwrite the developer's real selection.
    # LS_COLORS is dropped because it recolors table rows, which is how the
    # marker theme locates the selected and marked ones.
    "${TMUX[@]}" new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS" \
        env -u DISPLAY -u WAYLAND_DISPLAY -u LS_COLORS "${HERMETIC_ENV[@]}" \
        COLORTERM=truecolor HOME="$SANDBOX/home" \
        "$FILECTRL_BIN" -c "$SANDBOX/config.toml" "${include_args[@]}" \
        "${color_args[@]}" -- "$dir" ||
        fatal "could not start tmux session"
    # The status bar renders once the initial directory listing is loaded.
    wait_for 'Directory +Mode:' || fatal "filectrl did not start; screen: $(screen)"
    # The listing streams in batches, so an early key could act on a partial
    # table (e.g. G selecting the last row loaded so far). Wait until a row is
    # selected and the screen has stopped changing before returning.
    wait_settled _has_selection ||
        fatal "screen did not settle after startup; screen: $(screen)"
}

_has_selection() { [ -n "$(selected_row)" ]; }

app_stop() {
    "${TMUX[@]}" kill-server 2>/dev/null || true
}

app_is_running() {
    "${TMUX[@]}" has-session -t "$SESSION" 2>/dev/null
}

# ---------------------------------------------------------------------- input

send() {
    "${TMUX[@]}" send-keys -t "$SESSION" -- "$@"
}

type_text() {
    "${TMUX[@]}" send-keys -t "$SESSION" -l -- "$1"
}

# Escape has to reach the app on its own. Whatever is sent next can land in the
# same read, and an escape byte followed by another byte is an Alt+key sequence
# rather than Escape and then that key, so the next key is lost; under load that
# happened in a fifth of attempts. The pause puts the two in separate reads.
#
# Use this for every Escape a test sends. An assertion in between looks like it
# separates them, but only if it actually waits: one that is already true
# returns before the app has read the escape at all. A pause rather than
# wait_settled, because the screen does not settle while a search spinner is
# animating.
#
# The pause makes the loss unlikely, not impossible: a machine busy enough not
# to read for that long can still take the two together. Where the next key
# depends on the escape having landed, follow it with an assertion of what the
# escape closed, so the wait is on the effect rather than on a duration.
send_escape() {
    send Escape
    sleep 0.2
}

# SGR mouse events at 1-based screen coordinates. Button 0 is the left button;
# 32 adds the motion bit, which is what makes a move a drag rather than a
# hover. `M` is press, `m` is release.
mouse_down() { type_text "$(printf '\033[<0;%d;%dM' "$1" "$2")"; }
mouse_drag() { type_text "$(printf '\033[<32;%d;%dM' "$1" "$2")"; }
mouse_up() { type_text "$(printf '\033[<0;%d;%dm' "$1" "$2")"; }

click() {
    mouse_down "$1" "$2"
    mouse_up "$1" "$2"
}

double_click() {
    click "$1" "$2"
    click "$1" "$2"
}

# drag X Y1 Y2 : press at Y1, move down or up the column to Y2, release there.
drag() {
    local x="$1" from="$2" to="$3" step y
    step=$((to > from ? 1 : -1))
    mouse_down "$x" "$from"
    for ((y = from + step; y != to + step; y += step)); do
        mouse_drag "$x" "$y"
    done
    mouse_up "$x" "$to"
}

scroll_wheel() { # scroll_wheel up|down X Y
    local code=65
    [ "$1" = up ] && code=64
    type_text "$(printf '\033[<%d;%d;%dM' "$code" "$2" "$3")"
}

resize_window() { # resize_window COLS ROWS
    "${TMUX[@]}" resize-window -t "$SESSION" -x "$1" -y "$2"
}

# -------------------------------------------------------------------- capture

screen() {
    "${TMUX[@]}" capture-pane -t "$SESSION" -p
}

screen_ansi() {
    "${TMUX[@]}" capture-pane -t "$SESSION" -p -e
}

# Text of the highlighted table row, ANSI codes stripped, trimmed.
selected_row() {
    screen_ansi | grep -F "$MARKER" | head -1 |
        sed -e $'s/\x1b\\[[0-9;]*m//g' -e 's/^ *//' -e 's/ *$//'
}

# Text of every marked table row, one per line, ANSI codes stripped, trimmed.
# The cursor row renders with the selected style even when marked, so it is
# not listed here; the "[Selected] N items" notice shares the marked style and
# is filtered out.
marked_rows() {
    screen_ansi | grep -F "$MARKED_MARKER" |
        sed -e $'s/\x1b\\[[0-9;]*m//g' -e 's/^ *//' -e 's/ *$//' |
        grep -v '^\[Selected\]'
}

# The alert kinds differ only in foreground color, so the marker theme is what
# tells them apart on screen.
_alert_marker() {
    case "$1" in
    warn) echo "$ALERT_WARN" ;;
    error) echo "$ALERT_ERROR" ;;
    *) fatal "unknown alert kind: $1 (expected warn or error)" ;;
    esac
}

# alert_lines warn|error : the alerts of that kind, one per line, with the
# border and the trailing padding removed. The " • " prefix is kept, because a
# wrapped alert's continuation lines carry "   " instead.
alert_lines() {
    screen_ansi | grep -F "$(_alert_marker "$1")" |
        sed -e $'s/\x1b\\[[0-9;]*m//g' -e 's/^│//' -e 's/│$//' -e 's/ *$//'
}

# The breadcrumbs render on the line directly above the table header. Locating
# the header (rather than assuming line 1) keeps this correct when the alerts
# panel is open above and pushes the layout down.
breadcrumbs() {
    screen | awk '/\[N\]ame|\[M\]odified|\[S\]ize/ { print prev; exit } { prev = $0 }'
}

# ------------------------------------------------------- waiting + assertions

# wait_until COMMAND... : poll until COMMAND succeeds or TIMEOUT elapses.
#
# COMMAND is re-run each round but its arguments are expanded once, so a check
# that must re-read state belongs in a function: `wait_until [ "$(stat ...)" =
# x ]` polls one stale substitution forever. `wait_until [ -f "$path" ]` is
# fine, since it is the `[` builtin that re-reads the path.
wait_until() {
    local deadline=$((SECONDS + TIMEOUT))
    until "$@"; do
        ((SECONDS >= deadline)) && return 1
        sleep 0.05
    done
}

_screen_matches() { screen | grep -Eq -- "$1"; }
_selected_contains() { selected_row | grep -Fq -- "$1"; }
# Exact match (after trimming): a substring match would let assertions pass
# early while the app is still mid-navigation (e.g. /a matching inside /a/b).
_breadcrumbs_equal() { [ "$(breadcrumbs | sed -e 's/^ *//' -e 's/ *$//')" = "$1" ]; }

# wait_settled [PREDICATE...] : poll until the screen has stopped changing and
# the predicate, if given, succeeds; or until TIMEOUT elapses.
#
# For the points where a test must not act on a screen that is still moving: a
# directory listing streams in batches, so a cursor key sent while it arrives
# acts on a partial table.
wait_settled() {
    local prev="" cur deadline=$((SECONDS + TIMEOUT))
    while :; do
        cur="$(screen)"
        if [ -n "$cur" ] && [ "$cur" = "$prev" ] && { [ $# -eq 0 ] || "$@"; }; then
            return 0
        fi
        prev="$cur"
        ((SECONDS >= deadline)) && return 1
        sleep 0.15
    done
}

wait_for() { wait_until _screen_matches "$1"; }
wait_gone() { wait_until assert_not_now "$1"; }
assert_not_now() { ! _screen_matches "$1"; }

_fail() {
    echo "    ASSERTION FAILED: $*"
    echo "    --- screen ---"
    screen 2>/dev/null | sed 's/^/    | /'
    echo "    --------------"
    return 1
}

assert_screen() {
    wait_for "$1" || _fail "expected screen to match: $1"
}

# No waiting: asserts about the current, settled screen. Use assert_gone
# instead when the disappearance is the effect being waited for.
assert_not_screen() {
    if _screen_matches "$1"; then
        _fail "expected screen NOT to match: $1"
    fi
}

# Polls until the pattern disappears from the screen.
assert_gone() {
    wait_gone "$1" || _fail "expected screen to stop matching: $1"
}

assert_selected() {
    wait_until _selected_contains "$1" ||
        _fail "expected selected row to contain '$1' (selected: '$(selected_row)')"
}

_selected_name() { selected_row | awk '{print $1}'; }
_selected_name_is() { [ "$(_selected_name)" = "$1" ]; }

# The name column, compared exactly. Use this wherever one row's name can
# contain another's: a search result carries its path relative to the search
# root, so assert_selected cannot tell "readme.txt" from "documents/readme.txt".
assert_selected_name() {
    wait_until _selected_name_is "$1" ||
        _fail "expected the selected row to be exactly '$1' (got: '$(_selected_name)')"
}

assert_breadcrumbs() {
    wait_until _breadcrumbs_equal "$1" ||
        _fail "expected breadcrumbs '$1' (got: '$(breadcrumbs)')"
}

assert_running() {
    app_is_running || _fail "expected filectrl to still be running"
}

_marked_contains() { marked_rows | grep -Fq -- "$1"; }

# The cursor row renders with the selected style even when marked, so this sees
# every marked row except that one.
assert_marked() {
    wait_until _marked_contains "$1" ||
        _fail "expected a marked row containing '$1' (marked: $(marked_rows | tr '\n' '|'))"
}

# The "[Selected] N items" notice, which counts the cursor row too.
assert_marked_count() {
    assert_screen "\[Selected\] $1"
}

_alert_contains() { alert_lines "$1" | grep -Fq -- "$2"; }

# assert_alert warn|error TEXT
assert_alert() {
    wait_until _alert_contains "$1" "$2" ||
        _fail "expected a $1 alert containing '$2' (alerts: $(alert_lines "$1" | tr '\n' '|'))"
}

# A listing streams in batches, so the path line updates before the rows have
# arrived and the cursor has been restored. Use this between a navigation and
# the next key that acts on the selection: a cursor move sent early runs off a
# partial table, and Enter or an operation lands on no row at all.
#
# Fails the test on timeout rather than returning a status, like the other
# assertions: nothing selected at one of these points is never expected.
wait_for_selection() {
    wait_until _has_selection || _fail "expected a row to be selected"
}

_mode_is() { [ "$(stat -c %a "$2" 2>/dev/null)" = "$1" ]; }

# assert_mode OCTAL PATH: polls, because chmod is applied off the UI thread.
assert_mode() {
    wait_until _mode_is "$1" "$2" ||
        _fail "expected mode $1 on $2 (got: $(stat -c %a "$2" 2>/dev/null))"
}

# ------------------------------------------------------ non-interactive runs

# run_filectrl ARG... : run the binary to completion outside tmux, capturing
# stdout+stderr in RUN_OUTPUT and the exit status in RUN_STATUS. For the flags
# that print or write something and exit; anything that opens the TUI needs
# app_start. $HOME is the sandbox, so the default config path a test writes to
# or reads from is the sandbox's.
RUN_OUTPUT=""
RUN_STATUS=0
run_filectrl() {
    RUN_OUTPUT=$(env -u LS_COLORS "${HERMETIC_ENV[@]}" HOME="$SANDBOX/home" \
        "$FILECTRL_BIN" "$@" 2>&1) && RUN_STATUS=0 || RUN_STATUS=$?
}

_fail_run() {
    echo "    ASSERTION FAILED: $*"
    echo "    --- exit $RUN_STATUS, output ---"
    printf '%s\n' "$RUN_OUTPUT" | sed 's/^/    | /'
    echo "    --------------"
    return 1
}

assert_run_succeeded() {
    [ "$RUN_STATUS" = 0 ] || _fail_run "expected a zero exit status"
}

assert_run_failed() {
    [ "$RUN_STATUS" != 0 ] || _fail_run "expected a non-zero exit status"
}

assert_run_output() {
    printf '%s\n' "$RUN_OUTPUT" | grep -Eq -- "$1" ||
        _fail_run "expected the output to match: $1"
}

# --------------------------------------------------------------------- runner

# cleanup_later PATH : have a path outside the sandbox removed along with it,
# whether the test passes or fails. Recorded in a file rather than a variable,
# because each test runs in a subshell and anything it assigns is gone by the
# time the sandbox is removed.
cleanup_later() {
    printf '%s\n' "$1" >> "$SANDBOX/.cleanup_later"
}

# A test may leave a directory unwritable (the permission-denied cases), which
# `rm -rf` cannot descend into, so restore the modes we need first.
remove_sandbox() {
    [ -n "${SANDBOX:-}" ] && [ -d "$SANDBOX" ] || return 0
    if [ -f "$SANDBOX/.cleanup_later" ]; then
        local path
        while IFS= read -r path; do
            [ -n "$path" ] && rm -rf "$path"
        done < "$SANDBOX/.cleanup_later"
    fi
    chmod -R u+rwX "$SANDBOX" 2>/dev/null
    rm -rf "$SANDBOX"
}

# Kill the app and drop the sandbox even when the run is interrupted; otherwise
# a Ctrl-C leaves a tmux server and a mktemp directory behind. Both halves are
# no-ops the second time, so running it again from the EXIT trap is harmless.
cleanup() {
    app_stop
    remove_sandbox
}

run_tests() {
    local tests
    tests=$(declare -F | awk '$3 ~ /^test_/ {print $3}')
    [ -n "$tests" ] || fatal "no test_* functions defined"
    [ -x "$FILECTRL_BIN" ] || fatal "filectrl binary not found at $FILECTRL_BIN (set FILECTRL_BIN)"
    # The hermetic config stubs `[openers.linux]` only, so anywhere else a test
    # that opens an entry would launch the real program for its type.
    [ "$(uname -s)" = "Linux" ] || fatal "the integration suites are Linux-only (openers are stubbed per platform)"
    # Root bypasses the permission bits, so the tests that asserted an
    # operation was refused would pass with the refusal never happening.
    [ "$(id -u)" != 0 ] || fatal "the integration suites must not run as root (a refusal test cannot fail as root)"

    # A subshell does not inherit the EXIT trap, so a `fatal` inside a test
    # ends only that test and the loop below does the cleanup.
    trap cleanup EXIT
    trap 'cleanup; exit 130' INT TERM

    local t
    for t in $tests; do
        SANDBOX="$(mktemp -d)" || fatal "could not create a sandbox directory"
        cp -r "$REPO_ROOT/fixtures" "$SANDBOX/fixtures" ||
            fatal "could not copy $REPO_ROOT/fixtures into the sandbox"
        # The config lives in the sandbox so side effects beside it (the
        # bookmarks/ directory) are isolated per test.
        cp "$HERE/test_config.toml" "$SANDBOX/config.toml" ||
            fatal "could not copy the test config into the sandbox"
        mkdir -p "$SANDBOX/home"
        touch "$SANDBOX/home/home_marker.txt"
        _set_hermetic_env

        # Plain command (not an `if` condition) so `set -e` stays active
        # inside the subshell and the first failed assertion ends the test.
        (set -e; "$t")
        if [ $? -eq 0 ]; then
            echo "PASS $t"
            PASS=$((PASS + 1))
        else
            echo "FAIL $t"
            FAIL=$((FAIL + 1))
            FAILED_TESTS+=("$t")
        fi
        app_stop
        remove_sandbox
    done

    trap - EXIT INT TERM

    echo

    echo "$PASS passed, $FAIL failed"
    if ((FAIL > 0)); then
        printf 'failed: %s\n' "${FAILED_TESTS[@]}"
        exit 1
    fi
}
