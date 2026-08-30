---
name: verify
description: Build and drive FileCTRL in an isolated tmux to observe a change at the TUI surface. Use when verifying filectrl behaviour by running it, not by running tests.
---

# Verifying FileCTRL

FileCTRL is a TUI. The surface is the terminal, so drive it in tmux and capture panes.

## Handle

```bash
cargo build --release                      # target/release/filectrl
tmux -L fcverify kill-server 2>/dev/null   # own socket: do not share the host's
tmux -L fcverify new-session -d -x 120 -y 30 -s fc \
  "target/release/filectrl -c <config> <start-dir> 2>err.log"
tmux -L fcverify capture-pane -p -t fc
```

Pass `-c <path>` with an **empty** TOML file. User config merges onto the
built-in defaults, so an empty file pins the default keybindings regardless of
what is in `~/.config/filectrl/config.toml`. Without it a custom user config
silently changes which keys do what.

`--print-keybindings` prints the resolved bindings; read them rather than guessing.

## Driving

Navigate with the **Goto prompt** (`:`), not with `h`/`j`/`l`. Cursor position
after `h` is restored to the directory you came from, so counting `j` presses
gets it wrong, and a stray `l` on a regular file spawns the external opener.

```bash
tmux -L fcverify send-keys -t fc ':'
tmux -L fcverify send-keys -t fc -l "/abs/path/"   # -l for literal text
tmux -L fcverify send-keys -t fc Enter
```

Useful keys: `V`+`j` range-mark, `y` copy, `x` cut, `p` paste, `d` delete
(`y` confirms), `K` cancel operation, `c` new directory, `/` search, `Ctrl+a`
clear alerts, `Esc` clear clipboard/marks.

Pane rows on a 30-row session: 1-2 breadcrumbs, 3 header, 4+ listing,
26-28 notices (progress bar and operation), 29 prompt, 30 status.

## Mouse

`tmux -L fcverify set -g mouse off` first, or tmux eats the events. Then send
raw SGR sequences; `send-keys` mouse key names do not carry coordinates.

```bash
# press then release button 0 at col 3, row 2 (a breadcrumb segment)
tmux -L fcverify send-keys -t fc -H 1b 5b 3c 30 3b 33 3b 32 4d
tmux -L fcverify send-keys -t fc -H 1b 5b 3c 30 3b 33 3b 32 6d
```

Click *on* the text: clicking past the end of a breadcrumb segment is a no-op
and looks identical to the event not arriving.

## Making operations observable

Copies and deletes finish faster than you can capture on tmpfs or with a warm
page cache.

- Slow a copy with tiny buffers in the config: `buffer_max_bytes = 4096`,
  `buffer_min_bytes = 4096`. Even so, ~1 GB still completes in under a second.
- To catch a queue, send `p` and `K` back to back with no `sleep` between them.
  Operations run on one shared worker, so a batch queues and only the first
  runs; the cancel alert names which one it stopped.
- Delete progress sits at 0% during the pre-count scan before the bar moves.
  Sample with `sleep 0.15` over several seconds, not in a tight loop, or you
  only ever see the scan.

## Config

Validation failures print `Error: ...` and exit 1, so bad-config cases can be
checked without the TUI at all:

```bash
printf '[file_system]\nsearch_max_depth = 0\n' > bad.toml
target/release/filectrl -c bad.toml . ; echo $?   # -> Error, exit 1
```
