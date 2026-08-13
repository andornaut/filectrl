# Integration tests

End-to-end tests that drive the real `filectrl` binary in a tmux pane:
keyboard input, mouse clicks, and screen-content assertions.

## Running

Needs `tmux` and a release build (`target/release/filectrl`, or set
`FILECTRL_BIN`):

```bash
cargo build --release
tests/integration/run_tests.sh             # all suites
tests/integration/test_navigation.sh       # one suite
tests/integration/run_tests.sh --committed # against `git archive HEAD`
```

`--committed` takes the fixtures from the commit and the suites from the
working tree. Git stores only regular files, symlinks and gitlinks, so a fixture
that is a named pipe, carries a setuid bit, or is covered by an ignore rule does
not survive a checkout; a suite that needs one makes it itself, and this mode
catches the ones that forgot.

`run_tests` runs on Linux only, because `test_config.toml` stubs the openers per
platform. It also refuses to run as root, which would bypass the permission bits
the refusal tests need and pass them without the refusal happening.

## How it works

Piece | Role
--- | ---
`harness.sh` | Starts `filectrl` in a detached tmux session with a per-test sandbox: a fresh copy of `fixtures/`, a fake `$HOME`, and its own copy of `test_config.toml` (openers stubbed with `true`, so tests never launch real programs). The config lives in the sandbox, so state stored beside it (`bookmarks/`) is isolated per test
`run_filectrl` | Runs the binary to completion outside tmux, capturing output and exit status, for the flags that print or write something and exit. `test_config_cli.sh` uses it to resolve the config chain and read back which layer won, without a terminal
`marker_theme.toml` | Paints rows and alerts with colors nothing else uses, so helpers can find them in `capture-pane -e` output instead of guessing at screen coordinates

The environment is pinned so a run means the same thing everywhere:

Pinned | Why
--- | ---
`umask 022` | The sandbox modes the chmod tests assert are the modes the copy creates, whatever the checkout umask was
`DISPLAY`, `WAYLAND_DISPLAY` unset | The app finds no system clipboard, which the copy/paste suite asserts, and a run stays off the developer's real selection
`LS_COLORS` unset | No external colors on the table

Input and assertions:

- Keys go through `tmux send-keys`. Mouse events (clicks, double-clicks, the
  scroll wheel, and press/move/release drags) are injected as raw SGR escape
  sequences. `resize_window` exercises the responsive layout.
- Every assertion polls `capture-pane` until it holds or the timeout elapses,
  absence assertions included: a single read can catch the frame before the app
  has finished responding.
- Helpers return on an effect, never a duration. `app_start` waits for a settled
  screen; `start_big_copy` waits for bytes at the destination.

Markers cover both palettes, so a test can set `COLORS_256=1` before
`app_start` and keep the same assertions. Alerts are marked on the foreground
because style is all that distinguishes their kinds; info is absent, because
nothing outside the debug build raises one.

Marker | Truecolor | 256-color | Read by
--- | --- | --- | ---
Selected row background | `#010203` | 17 | `selected_row`
Marked row background | `#040506` | 53 | `marked_rows`
Warn alert foreground | `#0A0B0C` | 55 | `alert_lines`
Error alert foreground | `#0D0E0F` | 56 | `alert_lines`

## In CI

The `integration` job in `.github/workflows/release.yml` builds the release
binary and runs `run_tests.sh` on every push and pull request; the release
builds depend on it. It runs from a checkout, which is what `--committed`
reproduces locally.

## Adding a suite

Create `test_<capability>.sh` next to the others: source `harness.sh`, define
`test_*` functions using the assertion helpers, and end with `run_tests`.
`run_tests.sh` picks it up automatically.
