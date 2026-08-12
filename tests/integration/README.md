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
- The environment is pinned so a run means the same thing everywhere: `umask
  022`, so the sandbox modes the chmod tests assert are the modes the copy
  creates whatever the checkout umask was; and `DISPLAY`, `WAYLAND_DISPLAY`
  and `LS_COLORS` are dropped, so the app finds no system clipboard (which is
  what the copy/paste suite asserts, and keeps a test run off the developer's
  real selection) and no external colors on the table.
- The suites are Linux-only, because `test_config.toml` stubs the openers per
  platform; `run_tests` refuses to run anywhere else. Tests that assert an
  operation is refused need the permission bits to apply, so the Docker image
  runs them as a normal user rather than root.
- Keys are sent with `tmux send-keys`; mouse events (clicks, double-clicks,
  the scroll wheel, and press/move/release drags) are injected as raw SGR
  escape sequences, and `resize_window` exercises the responsive layout.
- `run_filectrl` runs the binary to completion outside tmux and captures its
  output and exit status, for the flags that print or write something and exit
  (`--keybindings`, `--write-default-config`). `test_config_cli.sh` uses it to
  resolve the config chain and read back which layer won, without a terminal.
- Assertions poll `tmux capture-pane` until they match or time out, so tests
  wait exactly as long as the app takes. `app_start` returns only once the
  screen has settled, so early keys cannot act on a partially loaded listing.
- `marker_theme.toml` paints the selected table row (`#010203`, 256-color
  index 17) and marked rows (`#040506`, index 53) with backgrounds nothing
  else uses, letting `selected_row` and `marked_rows` locate them in
  `capture-pane -e` output without guessing at screen coordinates. The warn
  and error alert kinds get marker foregrounds for the same reason, since
  style is all that separates them, and `alert_lines` reads them back. It
  covers both palettes so a test can set `COLORS_256=1` before `app_start` and
  keep the same assertions.

## In CI

The `integration` job in `.github/workflows/release.yml` builds the release
binary and runs `run_tests.sh` on every push and pull request; the release
builds depend on it. That job does not use the Docker image, so build it
locally with `run.sh` after changing the `Dockerfile`.

## Adding a suite

Create `test_<capability>.sh` next to the others: source `harness.sh`, define
`test_*` functions using the assertion helpers, and end with `run_tests`.
`run_tests.sh` picks it up automatically.
