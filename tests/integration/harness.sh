# Integration test harness: drives the real filectrl binary inside a tmux pane.
#
# Sourced by test_*.sh suites. Provides:
#   app_start [dir]        launch filectrl in a fresh tmux session
#   app_stop               kill the tmux session
#   send KEY...            send keys (tmux key names: j, Enter, Escape, Left, ...)
#   type_text TEXT         send literal text (for prompts)
#   click X Y / double_click X Y   SGR mouse events (1-based screen coordinates)
#   screen                 plain-text screen capture
#   selected_row           text of the highlighted table row (via marker theme)
#   assert_screen REGEX / assert_not_screen REGEX / assert_selected TEXT
#   assert_breadcrumbs TEXT / assert_running
#
# Each test is a shell function named test_*; run_tests discovers and runs them,
# giving every test a fresh sandbox (a copy of fixtures/) and a fresh app.

set -u

# tmux refuses to start without a UTF-8 locale; C.UTF-8 is built into glibc so
# it works even in slim containers with no locale data installed.
export LC_ALL=C.UTF-8

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
FILECTRL_BIN="${FILECTRL_BIN:-$REPO_ROOT/target/release/filectrl}"
TMUX=(tmux -L "filectrl-it-$$")
SESSION="it"
COLS=120
ROWS=30
TIMEOUT="${FILECTRL_IT_TIMEOUT:-5}"

# The marker theme paints the selected table row with bg #010203, which appears
# in `capture-pane -e` output as the unique SGR fragment "48;2;1;2;3".
MARKER='48;2;1;2;3'

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
    "${TMUX[@]}" kill-server 2>/dev/null || true
    "${TMUX[@]}" new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS" \
        env COLORTERM=truecolor HOME="$SANDBOX/home" \
        "$FILECTRL_BIN" -c "$HERE/test_config.toml" -i "$HERE/marker_theme.toml" "$dir" ||
        fatal "could not start tmux session"
    # The status bar renders once the initial directory listing is loaded.
    wait_for 'Directory +Mode:' || fatal "filectrl did not start; screen: $(screen)"
}

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

# SGR mouse press+release at 1-based screen coordinates.
click() {
    local x="$1" y="$2"
    type_text "$(printf '\033[<0;%d;%dM\033[<0;%d;%dm' "$x" "$y" "$x" "$y")"
}

double_click() {
    click "$1" "$2"
    click "$1" "$2"
}

scroll_wheel() { # scroll_wheel up|down X Y
    local code=65
    [ "$1" = up ] && code=64
    type_text "$(printf '\033[<%d;%d;%dM' "$code" "$2" "$3")"
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

# The breadcrumbs render on the line directly above the table header. Locating
# the header (rather than assuming line 1) keeps this correct when the alerts
# panel is open above and pushes the layout down.
breadcrumbs() {
    screen | awk '/\[N\]ame|\[M\]odified|\[S\]ize/ { print prev; exit } { prev = $0 }'
}

# ------------------------------------------------------- waiting + assertions

# wait_until COMMAND... : poll until COMMAND succeeds or TIMEOUT elapses.
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

# No waiting: asserts about the current, settled screen.
assert_not_screen() {
    if _screen_matches "$1"; then
        _fail "expected screen NOT to match: $1"
    fi
}

assert_selected() {
    wait_until _selected_contains "$1" ||
        _fail "expected selected row to contain '$1' (selected: '$(selected_row)')"
}

assert_breadcrumbs() {
    wait_until _breadcrumbs_equal "$1" ||
        _fail "expected breadcrumbs '$1' (got: '$(breadcrumbs)')"
}

assert_running() {
    app_is_running || _fail "expected filectrl to still be running"
}

# --------------------------------------------------------------------- runner

run_tests() {
    local tests
    tests=$(declare -F | awk '$3 ~ /^test_/ {print $3}')
    [ -n "$tests" ] || fatal "no test_* functions defined"
    [ -x "$FILECTRL_BIN" ] || fatal "filectrl binary not found at $FILECTRL_BIN (set FILECTRL_BIN)"

    local t
    for t in $tests; do
        SANDBOX="$(mktemp -d)"
        cp -r "$REPO_ROOT/fixtures" "$SANDBOX/fixtures"
        mkdir -p "$SANDBOX/home"
        touch "$SANDBOX/home/home_marker.txt"

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
        rm -rf "$SANDBOX"
    done

    echo
    echo "$PASS passed, $FAIL failed"
    if ((FAIL > 0)); then
        printf 'failed: %s\n' "${FAILED_TESTS[@]}"
        exit 1
    fi
}
