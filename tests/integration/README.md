# Integration tests

End-to-end tests that build the real `filectrl` binary and drive its TUI from a
user's perspective: keyboard input, mouse clicks, and screen-content assertions,
all inside a tmux pane.

## Running

In Docker (builds the binary and runs every suite in a container):

```bash
tests/integration/run.sh
```

Directly on a host with `tmux` installed (uses `target/release/filectrl`, or
set `FILECTRL_BIN`):

```bash
cargo build --release
tests/integration/run_tests.sh       # all suites
tests/integration/test_navigation.sh # one suite
```

## How it works

- `harness.sh` starts `filectrl` in a detached tmux session with a per-test
  sandbox: a fresh copy of `fixtures/`, a fake `$HOME`, and its own copy of
  the hermetic config (`test_config.toml`; openers are stubbed with `true` so
  tests never launch real programs). The config lives in the sandbox so state
  stored beside it - the `bookmarks/` directory - is isolated per test.
- Keys are sent with `tmux send-keys`; mouse events (clicks, double-clicks,
  the scroll wheel) are injected as raw SGR escape sequences, and
  `resize_window` exercises the responsive layout.
- Assertions poll `tmux capture-pane` until they match or time out, so tests
  wait exactly as long as the app takes. `app_start` returns only once the
  screen has settled, so early keys cannot act on a partially loaded listing.
- `marker_theme.toml` paints the selected table row (`#010203`) and marked
  rows (`#040506`) with unique truecolor backgrounds, letting `selected_row`
  and `marked_rows` locate them in `capture-pane -e` output without guessing
  at screen coordinates.

## Adding a suite

Create `test_<capability>.sh` next to the others: source `harness.sh`, define
`test_*` functions using the assertion helpers, and end with `run_tests`.
`run_tests.sh` picks it up automatically.
