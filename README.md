# FileCTRL

[![Release](https://github.com/andornaut/filectrl/actions/workflows/release.yml/badge.svg)](https://github.com/andornaut/filectrl/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/license/MIT)

FileCTRL is a light, opinionated, responsive, theme-able, and simple Text User Interface (TUI) file manager for Linux and macOS

[![42KM theme](./screenshots/42KM.png)](./screenshots/42KM.png)

## Features

- Simple interface with [sensible defaults](#configuration)
- [Bookmarks](#bookmarks): save and return to frequently-used folders
- [Customizable colors](#theming): truecolor and 256 color themes, with `LS_COLORS` integration
- [Rebindable keys](#customizing-keybindings) via TOML config
- [Vim-like navigation](#default-keybindings) and [multi-select](#multi-select): hjkl movement, marks, range mode
- File operations: chmod, create directory, and [copy/cut/paste across windows](#copy--paste)
- "Go to" with path completion
- [Filtering](#filtering), [searching](#searching), and [sorting](#sorting)
- Responsive layout: adapts columns and content to the terminal size

## Installation

[Download a pre-built binary](https://github.com/andornaut/filectrl/releases). Each release publishes `filectrl_{system}_{arch}.tar.gz` and a `.sha256` checksum for `linux_x86_64`, `linux_arm64`, and `darwin_arm64` (macOS is Apple Silicon only).

```bash
curl -sL https://github.com/andornaut/filectrl/releases/latest/download/filectrl_linux_x86_64.tar.gz | tar -xz filectrl
sudo mv filectrl /usr/local/bin/
```

The archives also contain `LICENSE` and `README.md`; `tar -xz filectrl` extracts only the binary.

On macOS, a binary downloaded with a browser is quarantined; allow the _unsigned_ binary to run (`curl` sets no quarantine attribute, so this step is not needed after the command above):

```bash
xattr -d com.apple.quarantine filectrl
```

## Building

Requires Rust 1.97 or later.

```bash
cargo build --release && sudo cp target/release/filectrl /usr/local/bin/
```

## Usage

```bash
filectrl [OPTIONS] [DIRECTORY]
```

Option | Description
--- | ---
`-c`, `--config <PATH>` | Read the config from `PATH`, or write it there when combined with a `--write-default-*` flag
`-i`, `--include <PATH>` | Merge a TOML file on top of the config. Repeatable; later files take precedence
`--no-truecolor` | Use the 256-color theme instead of detecting truecolor support
`--force` | Replace an existing file when writing defaults, which fails without it. A symlink is refused even with `--force`, so a config linked into a dotfiles repository is left alone
`--print-keybindings` | Print the keybindings, then exit
`--write-default-config` | Write the default config, then exit
`--write-default-themes` | Write the default theme as `theme.toml` beside the config, then exit
`-V`, `--version` | Print the version, then exit
`-h`, `--help` | Print usage, then exit

`DIRECTORY` defaults to the current working directory. If given, it must exist, be a directory, and be listable, or FileCTRL exits with an error, like `ls`.

The four flags below act and exit. They are mutually exclusive, and each accepts only the arguments that change what it does:

Flag | Also accepts
--- | ---
`--print-keybindings` | `--config`, `--include`
`--write-default-config` | `--config`, `--force`
`--write-default-themes` | `--config`, `--force`
`--version` | nothing

Anything else is reported rather than ignored. Both write flags print the path they wrote, which follows `$XDG_CONFIG_HOME` on Linux and so is not always under `~/.config`.

SIGTERM, SIGINT, SIGHUP, SIGQUIT, SIGUSR1, SIGUSR2 and SIGALRM restore the terminal and exit with status 128 plus the signal number (143 for SIGTERM), as a shell reports a process the signal killed. A terminal that closes is answered as SIGHUP (status 129) even when no SIGHUP arrives, which happens when the shell ignores it (`trap "" HUP`). On macOS, which cannot poll a terminal device, only the signal counts. SIGTSTP is ignored, since a stopped process would leave the terminal in raw mode. While the editor or pager runs, the terminal is back in the shell's modes, so <kbd>Ctrl</kbd>+<kbd>z</kbd> stops FileCTRL together with the program and `fg` resumes both, as does a program that suspends itself. A quit signal is passed to the program so both exit: SIGHUP as itself, the others as SIGTERM. SIGINT and SIGQUIT are the program's alone meanwhile, like `system(3)`: FileCTRL does not act on them, even when sent with `kill`, since they cannot be told apart from <kbd>Ctrl</kbd>+<kbd>c</kbd> and <kbd>Ctrl</kbd>+<kbd>&#92;</kbd>.

### Bookmarks

Bookmarks are symlinks to folders, stored in a `bookmarks/` directory beside the config file (e.g. `~/.config/filectrl/bookmarks/` on Linux).

Key | Action
--- | ---
<kbd>B</kbd> | Bookmark the current directory. The prompt defaults to the directory's name
<kbd>'</kbd> or <kbd>&#96;</kbd> | Show all bookmarks in the table
<kbd>Enter</kbd> | Navigate to the linked folder
<kbd>r</kbd>, <kbd>d</kbd> | Rename or delete the bookmark

Adding a bookmark, pasting, and creating a directory are refused while the bookmarks are shown, since each would act on the directory hidden behind them.

Names must be unique, cannot be empty, and cannot contain a path separator. A name is used as typed, surrounding whitespace included, the same as for rename and create.

### Copy / paste

Copying or cutting puts `${operation} ${path}` on the system clipboard, where `operation` is `cp` or `mv`. Pasting in another FileCTRL window performs the equivalent of `${operation} ${path} ${current_directory}`, e.g. `cp filectrl.desktop ~/.local/share/applications/`. Clipboard text is pasted only when every path in it is absolute, so a shell line such as `cp build dist` copied from elsewhere is ignored. An entry the pasting window did not write itself, including one from another FileCTRL window, asks for confirmation first (<kbd>y</kbd> pastes, any other key cancels), since any program can put such text on the clipboard. The confirmation shows each path in full, except that where the terminal is too narrow a path loses its start to `…`, so the file name and the question stay visible. An entry from elsewhere with a `.` or `..` component in a path is refused.

A paste copies the way `cp -R` does without `-p`: the umask applies to each entry's mode, and the original's setuid, setgid and sticky bits are dropped (a directory created inside a setgid directory keeps the setgid bit it inherits when you belong to that directory's group; Linux clears the bit on a mode change by anyone else, as it does for `cp -R`). A cut that crosses filesystems copies and then removes the original, the way `mv` does: it keeps the modes, the modification times of files and directories and their access times (on macOS, a directory's access time becomes the time of the move; symlinks and special files keep neither), the extended attributes of files and directories (including the POSIX ACLs on Linux) where the destination accepts them (symlinks and special files keep none), and the group when you belong to it, except on a symlink (setuid and setgid only when the copy has the original's owner and group; a special file keeps only its permission bits). Like `mv`, it removes the whole original once every entry is copied, so something written into the original while the copy ran is removed with it, and from that point it can no longer be cancelled; an entry of it that cannot be removed is reported and the rest are still removed. An original that was replaced by another entry after the copy (another device or inode) is kept, and the move reports it. Hard links in the original are copied as separate files, as a plain `cp -R` copies them, so names that shared one file no longer do.

Without a system clipboard (e.g. over SSH or on a bare console), copy and paste still work within a single window. Pasting with nothing to paste shows a warning, which without a system clipboard says so, since an entry copied in another window would be unreachable. A confirmed delete clears the clipboard; declining it leaves the clipboard as it was.

When the destination already contains an entry with the same name, the paste stops and asks:

Key | Action
--- | ---
<kbd>s</kbd> | Skip this entry
<kbd>S</kbd> | Skip every collision, also in sources already running
<kbd>o</kbd> | Replace the existing entry
<kbd>O</kbd> | Replace every collision the paste meets
<kbd>Esc</kbd> | Abandon the rest of the paste

- An existing **directory** is never replaced, and a directory never replaces anything (like `cp -R` and `mv`), so pasting onto one of the same name asks with only the skip choices, as does pasting a directory onto any entry. Modifier chords are not choices: <kbd>Ctrl</kbd>+<kbd>o</kbd> abandons the paste.
- Two pasted entries of the same name never collide: like `mv a/x b/x dest/`, the first takes the name and the second is refused without asking, and stays on the clipboard. On a destination that treats two names as one (letter case, Unicode normalization: macOS, Windows and SMB shares, FAT and exFAT drives, Linux case-insensitive directories), an entry made since the paste began is never offered for replacement either: it is refused the same way, since it is most likely what an earlier source of the paste wrote under the other name. That needs a filesystem that records when an entry was created; where none does (NFS, most FUSE mounts, ext4 with small inodes), only identical names are refused.
- A paste replaces an entry only where it found it when it reached that source and you answered <kbd>o</kbd> or <kbd>O</kbd>, and only that entry as it was: if it has changed or another has taken its place by the time the source is copied or moved, it is left alone and reported. A replacement lands whole or not at all. A move within one filesystem is a single rename. A copy, or a cut across filesystems, is written into a hidden owner-only directory beside the entry, `.filectrl-<pid>-<n>`, and takes the entry's name only once complete: one that fails leaves the old entry as it was and the message says so, and one that is cancelled leaves it as it was without an error. That directory is removed when the replacement ends, or reported if it cannot be; one left behind by a process that was killed or quit part way can be deleted. A replacement needs room for both entries at once. Under a umask or default ACL that leaves new directories unreadable to their owner (`0o477`, `u::---`), the replacement, and a directory copy, fails and is reported: nothing is given a mode by name. A name that was free when the paste reached it and is taken by then (by another program or another paste) is never replaced either: <kbd>S</kbd>, given at any point before that source's copy or move writes the name, skips it, and otherwise it is reported when that source finishes. The same holds for every name inside a directory being copied. So of two pastes into one directory at once, the later never replaces what the earlier wrote without asking, unless you answered <kbd>O</kbd> in it. Another program replacing the entry in the moment between the last check and the rename that replaces it is not detected, and on a filesystem that cannot refuse a taken name in the rename itself, neither is a name taken in that moment.
- A cut that skipped an entry inside a directory, or failed to copy one, keeps its whole original: that entry is not at the destination, so removing the source would take the only copy of it. A cut whose entry itself was skipped leaves its original where it was, with nothing to report.
- A paste consumes the clipboard as its entries start. What never started stays on it (collisions you abandon, entries refused before starting), so pasting again retries exactly those; entries you skip deliberately do not. If nothing started at all, the clipboard is unchanged. An entry that fails or is cancelled after it started, including while it waits behind other operations, is reported and is not put back on the clipboard; its original is left where it was.

### Chmod

Chmod (<kbd>P</kbd>) never follows a symlink: a symlink is refused rather than having its target changed. Setting a mode without following links needs glibc 2.32 or newer, or `/proc` mounted; where neither holds (an old distribution, or a container without `/proc`), chmod fails with "Operation not supported".

### Entries that change after they are listed

Rename, chmod, delete, copy and cut act on whatever the path names when they run, like `mv`, `chmod` and `rm`, so an entry replaced since it was listed is the one acted on. Inside a tree being copied or deleted, a directory swapped for a symlink during the walk is not followed.

A delete continues past an entry it cannot remove, like `rm -rf`: it removes everything else, keeps the directories holding what failed, and reports the failures when it finishes. An empty directory it cannot open (mode 000) is removed, and an entry already gone counts as removed, so deleting marked entries that include both a directory and something inside it succeeds.

When the directory being viewed is renamed away, removed, or made unreadable, the next refresh reports it and stops watching it; <kbd>Ctrl</kbd>+<kbd>R</kbd> tries again. The bookmarks view watches the bookmarks directory instead, so one added or removed elsewhere shows up.

### Multi-select

Mark entries to apply chmod, copy, cut, or delete to several at once.

Key | Action
--- | ---
<kbd>v</kbd>/<kbd>Space</kbd> | Toggle a mark on the current row. In range mode, exit it instead, keeping the marks, as <kbd>v</kbd> leaves Visual mode in Vim
<kbd>V</kbd> | Enter range mode: the current row becomes the anchor. Press again to exit, keeping the marks
<kbd>Esc</kbd> | Clear all marks and exit range mode

In range mode, moving the cursor or clicking extends the marked range from the anchor to the cursor. Marks made before entering range mode are kept, so ranges and single marks combine. Outside range mode, clicking only moves the cursor. The mouse wheel scrolls the list three rows at a time without moving the cursor, so it never extends a range; the next key acts on the cursor and brings it back into view. Marking clears the clipboard. The notices bar shows the mark count as `[Selected] N items`, or `[Range] N items` while range mode is on.

Marks name entries but are stored as row positions, so what becomes of them depends on why the listing changed:

Change | Marks
--- | ---
Sorting, filtering, toggling hidden files | Cleared
Starting a search | Cleared
Reload (<kbd>Ctrl</kbd>+<kbd>r</kbd> or a watcher refresh) | Kept, re-found by path. An entry that is gone loses its mark
A search finishing or being cancelled | Kept
Showing bookmarks | Cleared
The bookmarks view reloading (a watcher refresh, or a bookmark operation finishing) | Kept, re-found by path, as is the cursor
Navigating to another directory | Cleared
Copying or cutting | Kept, so what is on the clipboard stays marked
chmod, delete, or pasting | Consumed by the operation

Range mode survives a directory reload in the same way: its anchor and the marks made before it are re-found by path, and the range extends from the anchor again once the cursor moves. It ends if the anchor entry is gone, and with every other change above.

### Filtering

The filter (<kbd>f</kbd>/<kbd>&#92;</kbd>) is a case-insensitive substring match against the Name column, so it matches what is on screen: the entry's own name in a normal listing, the path relative to the search root while searching, and the bookmark name in the bookmarks view. The table narrows as you type. <kbd>Enter</kbd> keeps the filter. <kbd>Esc</kbd>, or anything else that closes the prompt without submitting it (such as a double-click that opens a file), puts back the one that was applied when the prompt opened. Like any change to what is listed, the first edit clears the marks.

Directories carry a trailing `/` outside the bookmarks view, so `/` filters a listing down to directories, and `docs/` matches both the `docs` directory and, in search results, everything under it.

While a filter or hidden files leave entries out of a directory listing, the status bar's `# Items` reads `shown of total`, such as `3 of 120`. When the cursor is on a symlink, the status bar shows what it points to after `->`, as stored in the link.

### Searching

Search (<kbd>/</kbd>) walks the current directory recursively, matching a case-insensitive substring against each entry's name. Symlinked directories are not descended into. `search_max_depth` and `search_max_results` in `[file_system]` bound the walk; on reaching either, FileCTRL keeps the results it has and says so. Directories below the one searched that the walk cannot read are skipped and counted in one warning when it ends; a directory that cannot be searched at all is reported as an error.

Results appear as the walk finds them and settle into the sort order once it ends, whether it finished or was cancelled. The cursor then goes to the top row, unless you moved it or marked a row while the results streamed in, in which case it stays on that entry. Navigating to another directory stops the walk; a reload does not. Once the walk has ended, a reload (<kbd>Ctrl</kbd>+<kbd>R</kbd>, an operation finishing, or a watcher refresh) reads the results again by path: one deleted or renamed since drops out, and the rest show what they hold now. A finished search keeps its notice, with the query and how many results it found (`[Search: 42 results] query`), and the status bar's `# Items` counts the results while they are listed.

### Sorting

<kbd>n</kbd>/<kbd>m</kbd>/<kbd>s</kbd> sort by name, modified time, or size; clicking a column header does the same. A header shows its key in brackets (`[N]ame`) while that key is the column's initial. Sorting by the same column again reverses it. Each column starts in the direction it is usually reached for:

Column | Default direction
--- | ---
Name | A-Z
Modified | Newest first
Size | Largest first

The Name column orders by the text it displays (while searching, the path relative to the search root), ignoring case and a leading dot on each path segment, so a dot file sorts next to its neighbours the way `ls -a` does. Runs of digits compare as numbers, so `file2` sorts before `file10`; set `natural_sort = false` in the `[ui]` section to compare character by character instead. `sort_directories_first` in the same section groups directories first, for the Name column only. Entries with the same modified time or size are ordered by name, A-Z, whichever way the column points.

### Default keybindings

All keybindings can be [customized](#customizing-keybindings).

_**Normal mode**_

Actions | Keys
--- | ---
Select next, previous row | <kbd>↓</kbd>/<kbd>j</kbd>, <kbd>↑</kbd>/<kbd>k</kbd>
Select first, middle, last row | <kbd>Home</kbd>/<kbd>g</kbd>/<kbd>^</kbd>, <kbd>z</kbd>, <kbd>End</kbd>/<kbd>G</kbd> (Uppercase)/<kbd>$</kbd>
Select top, middle, bottom row | <kbd>H</kbd> (Uppercase), <kbd>M</kbd> (Uppercase), <kbd>L</kbd> (Uppercase)
Page down, up | <kbd>PgDn</kbd>/<kbd>Ctrl</kbd>+<kbd>d</kbd>/<kbd>Ctrl</kbd>+<kbd>f</kbd>, <kbd>PgUp</kbd>/<kbd>Ctrl</kbd>+<kbd>u</kbd>/<kbd>Ctrl</kbd>+<kbd>b</kbd>
Go to parent dir | <kbd>←</kbd>/<kbd>h</kbd>/<kbd>b</kbd>/<kbd>Backspace</kbd>
Go to previous dir | <kbd>-</kbd>
Go to home dir | <kbd>~</kbd>
Go to path | <kbd>:</kbd>/<kbd>Tab</kbd>
Open | <kbd>→</kbd>/<kbd>l</kbd>/<kbd>Enter</kbd>
Open current directory | <kbd>t</kbd>
Open new window | <kbd>w</kbd>
Open with... | <kbd>o</kbd>
Edit in `$VISUAL`/`$EDITOR`, page in `$PAGER` | <kbd>e</kbd>, <kbd>i</kbd>
Mark/unmark item | <kbd>v</kbd>/<kbd>Space</kbd>
Range mark | <kbd>V</kbd> (Uppercase)
Mark every shown row, ending range mode | <kbd>Ctrl</kbd>+<kbd>a</kbd>
Copy, Cut, Paste | <kbd>y</kbd>/<kbd>Ctrl</kbd>+<kbd>c</kbd>, <kbd>x</kbd>/<kbd>Ctrl</kbd>+<kbd>x</kbd>, <kbd>p</kbd>/<kbd>Ctrl</kbd>+<kbd>v</kbd>
Rename | <kbd>r</kbd>/<kbd>F2</kbd>
Chmod (octal) | <kbd>P</kbd> (Uppercase)
Create directory | <kbd>c</kbd>
Delete | <kbd>d</kbd>/<kbd>Delete</kbd>
Filter | <kbd>f</kbd>/<kbd>&#92;</kbd>
Search | <kbd>/</kbd>
Add bookmark | <kbd>B</kbd> (Uppercase)
Show bookmarks | <kbd>'</kbd>/<kbd>&#96;</kbd>
Refresh | <kbd>Ctrl</kbd>+<kbd>r</kbd>/<kbd>F5</kbd>
Sort by name, modified, size | <kbd>n</kbd>, <kbd>m</kbd>, <kbd>s</kbd>
Toggle show hidden files (search results always include them) | <kbd>.</kbd>
Cancel file or search operations | <kbd>K</kbd> (Uppercase)
Clear alerts, progress | <kbd>Ctrl</kbd>+<kbd>l</kbd>, <kbd>Ctrl</kbd>+<kbd>p</kbd>
Reset the view: clear the copied or cut entry, filter, marks and search, and leave the bookmarks view (with help shown, only closes help) | <kbd>Esc</kbd>
Toggle help | <kbd>?</kbd>
Quit (asks first while a copy, move or delete is running, which quitting would end part way through) | <kbd>q</kbd>

_**Prompt mode**_

Actions | Keys
--- | ---
Submit | <kbd>Enter</kbd>
Cancel | <kbd>Esc</kbd>
Reset to initial value | <kbd>Ctrl</kbd>+<kbd>u</kbd>/<kbd>Ctrl</kbd>+<kbd>z</kbd>
Select all | <kbd>Ctrl</kbd>+<kbd>a</kbd>
Copy, Cut, Paste text | <kbd>Ctrl</kbd>+<kbd>c</kbd>, <kbd>Ctrl</kbd>+<kbd>x</kbd>, <kbd>Ctrl</kbd>+<kbd>v</kbd>
Move cursor | <kbd>←</kbd>/<kbd>→</kbd>
Move cursor by word | <kbd>Ctrl</kbd>+<kbd>←</kbd>/<kbd>→</kbd>, <kbd>Alt</kbd>+<kbd>b</kbd>/<kbd>f</kbd>
Move cursor to start, end | <kbd>Home</kbd>, <kbd>Ctrl</kbd>+<kbd>e</kbd>/<kbd>End</kbd>
Select text | <kbd>Shift</kbd>+<kbd>←</kbd>/<kbd>→</kbd>
Select to line start, end | <kbd>Shift</kbd>+<kbd>Home</kbd>, <kbd>Shift</kbd>+<kbd>End</kbd>
Select by word | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>←</kbd>/<kbd>→</kbd>
Delete before, after cursor | <kbd>Backspace</kbd>, <kbd>Delete</kbd>
Delete word before, after cursor | <kbd>Ctrl</kbd>+<kbd>w</kbd>/<kbd>Alt</kbd>+<kbd>Backspace</kbd>, <kbd>Alt</kbd>+<kbd>d</kbd>/<kbd>Alt</kbd>+<kbd>Delete</kbd>
Delete to start, end | <kbd>Ctrl</kbd>+<kbd>j</kbd>, <kbd>Ctrl</kbd>+<kbd>k</kbd>
Accept path suggestion (cursor at end of input) | <kbd>Tab</kbd>
Cycle path suggestions (cursor at end of input) | <kbd>↓</kbd>/<kbd>↑</kbd>

In the Go to prompt, `~` alone or a leading `~/` stands for the home directory. Other input that is not an absolute path, `~backup` included, is relative to the current directory.

A suggestion is shown with its position as `(N of M)`, and cycling wraps in both directions. <kbd>Enter</kbd> with the cursor at the end of the input accepts the suggestion shown before going there, so `/tmp/fo` opens `/tmp/foo/` when that is the suggestion. Moving the cursor off the end of the input dismisses it. A directory of more than 10,000 entries offers no suggestions.

A key with no prompt binding is passed to the text input ([ratatui-textarea](https://github.com/ratatui/ratatui-textarea)), whose defaults are emacs-style: besides the keys above, <kbd>Ctrl</kbd>+<kbd>b</kbd>/<kbd>f</kbd> move by character, <kbd>Ctrl</kbd>+<kbd>h</kbd>/<kbd>d</kbd> delete before and after the cursor, and <kbd>Ctrl</kbd>+<kbd>y</kbd> pastes the text last cut in the prompt. The input is one line: <kbd>Tab</kbd> (outside the Go to prompt), <kbd>Enter</kbd> with a modifier, and <kbd>Ctrl</kbd>+<kbd>m</kbd> are ignored rather than inserted.

Text pasted through the terminal (bracketed paste) goes into a text prompt as one line, with its line breaks removed. A paste anywhere else, including a y/n prompt, is ignored, so pasted text never acts as keys.

> [!NOTE]
> <kbd>Ctrl</kbd>+<kbd>Shift</kbd> with a letter (a `"Ctrl+Shift+a"` binding, say) requires a terminal that supports the [kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/) (e.g. Alacritty): the legacy encoding sends one byte for both <kbd>Ctrl</kbd>+<kbd>a</kbd> and <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>a</kbd>, so the Shift cannot survive it. <kbd>Ctrl</kbd>+<kbd>Shift</kbd> with an arrow key does not need the protocol, because the legacy encoding does carry modifiers for arrows.
>
> Under tmux, add the following to `~/.tmux.conf` as well:
>
> ```conf
> set -g extended-keys on
> set -ga terminal-features ",*:extkeys"
> ```

## Configuration

The built-in [default configuration](./src/app/config/default_config.toml) is always the base. A config file merges on top of it, read from the first of:

1. The path given by `--config`
1. `config.toml` in the config directory, if it exists: `~/.config/filectrl/` on Linux, `~/Library/Application Support/filectrl/` on macOS. The examples below use the Linux path. A symlink there whose target is missing is an error, not an absent config

`--config` replaces the user config rather than adding to it, so a key the given file leaves out falls back to the built-in default, not to the config directory's `config.toml`.

`filectrl --write-default-config` writes the defaults to whichever of those two paths applies. It writes the configuration keys only; the theme keys are a separate file written by `--write-default-themes`.

Override only what you want to change:

```toml
# ~/.config/filectrl/config.toml
log_level = "warn"

[ui]
show_hidden_files = false
sort_directories_first = false
```

Logs are written to stderr only when it is redirected (e.g. `filectrl 2>filectrl.log`), since otherwise it is the terminal the interface is drawn on. `log_level` sets the level, and `$RUST_LOG` overrides it.

Validation is strict: an unrecognized key (a misspelled setting or theme property), an unknown modifier name, or an invalid value (such as a `refresh_debounce_milliseconds` below 100, an empty key list, or an opener without `%s` as its own unquoted word) makes FileCTRL exit with an error naming the file, rather than ignore it.

### Opening in other applications

- [andornaut@github /til/ubuntu#default-applications](https://github.com/andornaut/til/blob/main/docs/ubuntu.md#default-applications)
- [XDG MIME Applications](https://wiki.archlinux.org/title/XDG_MIME_Applications)

Key | Opens with
--- | ---
<kbd>l</kbd> | `openers.open_file`
<kbd>t</kbd> | `openers.open_directory`, for the current directory
<kbd>w</kbd> | `openers.open_filectrl_window`, a new `filectrl` window (on macOS by default, a Terminal window in the directory)
<kbd>o</kbd> | A picker of the applications that can open the selection
<kbd>e</kbd> | `$VISUAL`, else `$EDITOR`, else `vi`, in this terminal
<kbd>i</kbd> | `$PAGER`, else `less`, in this terminal

<kbd>e</kbd> and <kbd>i</kbd> suspend FileCTRL and run the program on the entry under the cursor, then return to the listing and refresh it. The variable is split into words the way a shell splits it (`code --wait` is a program and an option), and the path is passed as an argument of its own, never through a shell. A directory is refused. <kbd>Ctrl</kbd>+<kbd>c</kbd> and <kbd>Ctrl</kbd>+<kbd>&#92;</kbd> go to the program while it runs. A program that exits with an error, or cannot be started, is reported as an alert.

Each template runs with `sh -c`. The path is never written into the command: `%s` becomes a reference to it (`"$@"`), and the path is passed to the shell as an argument, so the shell expands it but never parses it. A file name therefore cannot run as a command wherever `%s` sits. Only a template that hands the text to another parser can still run it: `eval`, a nested `sh -c`, `ssh`, bash arithmetic such as `$(( %s ))`, or AppleScript's `do script`, which types a command line into Terminal's login shell. That is why the macOS default opens a Terminal window in the directory rather than starting `filectrl` in it.

Write `%s` unquoted, as its own word: `open %s`, not `open "%s"`. The reference carries its own quotes, so a `%s` inside double quotes is split into words and one inside single quotes stays the literal text `"$@"`. Neither is supported, and neither runs the name; a non-empty template without an unquoted `%s` word is refused when the config loads.

```toml
# Use [openers.linux] on Linux, or [openers.macos] on macOS.
# %s stands for the current directory, the selected entry, or a new window's
# directory. In run_in_terminal alone it stands for a command, each word its
# own argument (see "Open with..." below).
[openers.linux]
open_directory = "alacritty --working-directory %s"
open_file = "pcmanfm %s"
open_filectrl_window = "alacritty --command filectrl %s"
run_in_terminal = "alacritty --command %s"

[openers.macos]
open_directory = "open %s"
open_file = "open %s"
open_filectrl_window = "open -a Terminal %s"
run_in_terminal = "" # Linux only, ignored here
```

#### The "Open with" picker

<kbd>o</kbd> replaces the file table with the applications that can open the selection, leaving the breadcrumbs and status bar visible. The default application is listed first and marked `(default)`.

Key | Action
--- | ---
<kbd>↓</kbd>/<kbd>j</kbd>, <kbd>↑</kbd>/<kbd>k</kbd> | Move between applications
<kbd>Home</kbd>/<kbd>g</kbd>/<kbd>^</kbd>, <kbd>End</kbd>/<kbd>G</kbd>/<kbd>$</kbd> (and their normal-mode bindings) | Move to the first or last application
<kbd>PageDown</kbd>, <kbd>PageUp</kbd> (and their normal-mode bindings) | Move a page down or up
<kbd>→</kbd>/<kbd>l</kbd>/<kbd>Enter</kbd> | Open with the selected application
<kbd>1</kbd> to <kbd>9</kbd> | Open with that numbered application
<kbd>o</kbd> | Close the picker
<kbd>Esc</kbd> | Close the picker and reset the view: clear the copied or cut entry, filter, marks and search, and leave the bookmarks view

Only the first nine rows have a number; scroll to reach the rest. Applications that share a name are collapsed to the best ranked one.

The list is built per platform:

- **Linux:** the MIME type is resolved through the shared MIME database, including its parent types, so a `.rs` file also offers plain text editors. It is then matched against `mimeapps.list` and the `.desktop` files under `$XDG_DATA_DIRS/applications`, per the [mime-apps spec](https://specifications.freedesktop.org/mime-apps/latest-single/). The application directories are indexed once per run, so an application installed while FileCTRL is open is not offered until the next start. An entry whose `Exec` matches one of the shapes below, which could hand the file name to an interpreter as code, is not offered either, nor is one whose `Exec` is malformed, and the log names each at warn level. Relative directories in `$XDG_DATA_DIRS` and the other XDG variables are ignored, as the spec requires.
- **macOS:** Launch Services, which requires macOS 12 or newer. The chosen application is launched with `open -a`.

On Linux, a desktop entry's `Exec` is refused when it matches one of the shapes in the table, in which a value could reach an interpreter as code. Only these shapes are detected: a program whose first operand is its program text, such as `awk %f`, is still offered. An option cluster is an argument starting with a single `-` whose leading run of letters and digits holds `c`, `e`, `E`, `S`, `p`, `r`, `R` or `B` (`-c`, `-lc`, `-cx`, `-e`, `-E`, `-S`, `-p`, `-r`, `-R`, `-B`, `-verbose`, `-cprint(1)`, `-S%f`), or one of the long options `--eval`, `--exec`, `--execute`, `--execute-command`, `--print`, `--run` and `--split-string`, alone or with `=value`; it is taken to give code to run, whatever the program, and the code may be attached to it. Only `%f`, `%F`, `%u` and `%U` are substituted, and they are the field codes the table means; `%i`, `%c`, `%k` and the deprecated codes are removed from the command line, and a cluster is recognized after they are removed, so `perl -%ce %f` is the cluster `-e`.

`Exec` shape | Example | Result
--- | --- | ---
Field code with no cluster before it | `mpv %f`, `mpv --file=%f`, `foo %f -c bar` | Offered
Quoted argument that is only the code | `app "%f"` | Offered
Code after a long option or a `-` option that is not a cluster | `foo --file %f`, `foo --c=%f`, `foo -xvf %f`, `foo -C %f`, `flatpak run --command=foo org.x %U` | Offered
A removed code or `%%` after a cluster, with the file code before it | `foo %f -c %i` | Offered
Field code in or anywhere after a cluster, quoted or not | `sh -c %f`, `sh -c -x %f`, `perl -e %f`, `perl -E %f`, `env -S %f`, `env -S%f`, `node -p %f`, `php -r %f`, `node --eval=%f`, `python3 "-cimport sys; ..." %f` | Refused
No file code, so the appended path would follow a cluster | `sh -c`, `xterm -e htop` | Refused
Field code inside a quoted or escaped argument | `run --command "mpv %f"`, `app "--file=%f"` | Refused

The rule is deliberately broad and refuses some safe entries, such as `sh -c 'mpv "$1"' sh %f`, which passes the name to the script as a parameter. To open files through a shell command, use the `openers` templates above, which pass the path to the shell as an argument.

Two `openers` settings shape the list, and setting either to `""` drops its effect:

- Applications that need a terminal (`Terminal=true`) run inside `openers.run_in_terminal`, whose `%s` stands for the command, each word its own argument: `xterm -e %s` runs `xterm` with the arguments `-e`, `vim` and `/some file.txt`. The terminal must run the words after its option as a program and its arguments, as `xterm -e` and `alacritty --command` do. One that joins them into a string for a shell to parse again would run a file name as shell code.
- `openers.open_file` (or `openers.open_directory` for a directory) is offered last, showing its command template beside the setting name, so the picker still works with no application database. Without it, a path that matches nothing shows "No applications found".

### Theming

`[theme]` applies to truecolor terminals and `[theme256]` to 256-color terminals. FileCTRL detects truecolor support via `$COLORTERM`; `--no-truecolor` selects the 256-color theme regardless. There is no flag for the other direction: a terminal that supports truecolor but does not set `$COLORTERM` (common under tmux, and under some SSH and `sudo` sessions) gets the 256-color theme, so set the variable yourself with `COLORTERM=truecolor filectrl`.

#### Style properties

Each theme entry is a style. All three properties are optional; set `fg` or `bg` to `""` to inherit the parent widget's color.

Property | Format | Default
--- | --- | ---
`fg` | Color string | Inherited
`bg` | Color string | Inherited
`modifiers` | Array of strings | `[]`

- **Truecolor** (`[theme]`): hex strings like `"#FF0000"`, or named colors like `"Red"`
- **256 color** (`[theme256]`): decimal indexes `"0"` through `"255"`
- **Modifiers:** `"bold"`, `"dim"`, `"italic"`, `"underlined"`, `"blink"`, `"rapid_blink"`, `"reversed"`, `"crossed_out"`

```toml
[theme.table.selected]
bg = "#1A1A2E"

[theme.file_type.directory]
fg = "#E94560"
modifiers = ["bold"]
```

#### Theme sections

Section | Description
--- | ---
`[theme]` / `[theme256]` | Base foreground, background, and modifiers
`alert` | Alert bar (its own style, plus `error`, `info`, `warn`)
`breadcrumbs` | Path breadcrumbs (its own style, plus `ancestor`, `basename`, `bookmarks`, `search`, `separator`)
`clipboard` | Clipboard status indicators (`copy`, `cut`)
`file_modified_date` | Date column by age (`less_than_minute`, `less_than_hour`, `less_than_day`, `less_than_month`, `less_than_year`, `greater_than_year`)
`file_size` | Size column by magnitude (`bytes`, `kib`, `mib`, `gib`, `tib`, `pib`)
`file_type` | Row colors by file type (`directory`, `executable`, `symlink`, `regular_file`, etc.)
`help` | Help panel (its own style, plus `header`, `actions`, `shortcuts`)
`notice` | Notice bar (`filter`, `progress`, `search`, `search_loading`)
`open_with` | Open with... picker (its own style, plus `detail`, `selected`, `shortcut`)
`prompt` | Input prompt (`cursor`, `delete`, `goto_suggestion`, `input`, `label`, `selected`)
`scrollbar` | Scrollbar (`ends`, `thumb`, `track`, plus `show_ends` boolean)
`status` | Status bar (`detail`, `label`)
`table` | File table (`body`, `header`, `header_sorted`, `selected`, `marked`, `delete`, `bookmark`)

#### LS_COLORS integration

Off by default. With `ls_colors_take_precedence` in `[ui]`, colors from `$LS_COLORS` are applied on top of both themes' file type colors, whichever theme is included, including patterns such as `*.tar=01;31`. A pattern matches the end of the whole name as `ls` does: `*.gitignore` colors the dotfile `.gitignore`, case is ignored unless the same pattern is listed in two cases, and the last listed match wins. An explicit reset (a value of exactly `00`, `0`, or nothing, as in `di=00` or `*.txt=`) renders those entries plain, as `ls` does, except for the keys `ls` only consults while they are colored: `ow`, `st`, `tw`, `su`, `sg`, `ex` and `or` reset that way are skipped, so the entry takes the next rule's color (`ow=00` shows other-writable directories in the `di` color). Other values made only of reset codes, such as `0;00`, count as a color and render plain for every key. A reset clears the colors and attributes before it, so `31;00` renders plain too.

```toml
[ui]
ls_colors_take_precedence = true
```

#### External theme files

`include_files` merges other TOML files on top of the config:

```toml
include_files = ["theme.toml"]
```

- Relative paths resolve from the directory containing the file that lists them, as named: a symlinked config or include file resolves from the directory holding the link, not the one it points to. Absolute paths are used as-is
- Files merge in order, later ones taking precedence over the base config and over earlier files
- The value must be an array of strings, and every listed file must exist, be a regular file (or a symlink to one), and parse, or FileCTRL exits with an error. The same holds for the config file and for `--include`

Export the defaults, then copy and edit:

```bash
filectrl --write-default-themes  # writes ~/.config/filectrl/theme.toml
cp ~/.config/filectrl/theme.toml ~/.config/filectrl/solarized.toml
```

`--include`/`-i` applies a theme without editing the config. It is repeatable and merges in order, later ones taking precedence. Unlike `include_files`, relative paths resolve against the current directory:

```bash
filectrl -i ~/.config/filectrl/solarized.toml -i overrides.toml
```

#### Bundled themes

Theme | Inspired by | Screenshot
----- | ----------- | ----------
[IBM1970](./themes/ibm1970.toml) (default) | [vscode-ibm1970-theme](https://github.com/andornaut/vscode-ibm1970-theme) | [![IBM1970](./screenshots/IBM1970.png)](./screenshots/IBM1970.png)
[42KM](./themes/42km.toml) | [vscode-42km-theme](https://github.com/andornaut/vscode-42km-theme) | [![42KM](./screenshots/42KM.png)](./screenshots/42KM.png)

The release archives do not include the theme files, so from a source checkout:

```bash
filectrl --include themes/42km.toml
```

Otherwise, download the theme file first:

```bash
curl -fsSL --create-dirs -o ~/.config/filectrl/42km.toml \
  https://raw.githubusercontent.com/andornaut/filectrl/main/themes/42km.toml
filectrl --include ~/.config/filectrl/42km.toml
```

### Customizing keybindings

Keybindings live in the `[keybindings]` section of `config.toml`. A value is a single key string or an array of them.

```toml
[keybindings]
# Normal mode
quit = "q"
toggle_help = "?"
...
# Prompt mode
prompt_submit = "Enter"
prompt_reset = ["Ctrl+u", "Ctrl+z"]
...
```

Key strings support:

Form | Examples
--- | ---
Single characters | `"q"`, `"/"`, `"~"`, `"^"`, `"$"`
Uppercase (implies Shift) | `"G"`, `"V"`, `"N"`
Named keys (any case) | `"Enter"`, `"Esc"`, `"Backspace"`, `"Delete"`, `"Space"`, `"Tab"`, `"BackTab"`, `"Up"`, `"Down"`, `"Left"`, `"Right"`, `"Home"`, `"End"`, `"PgUp"`, `"PgDn"`
Aliases | `"Return"` (`"Enter"`), `"Escape"` (`"Esc"`), `"Del"` (`"Delete"`), `"PageUp"` (`"PgUp"`), `"PageDown"` (`"PgDn"`)
Function keys | `"F1"` to `"F24"`
Modifier prefixes (`Ctrl+`, `Shift+`, `Alt+`, any case) | `"Ctrl+c"`, `"Shift+Left"`, `"Alt+x"`, `"Ctrl+Shift+a"`

`"Shift+g"` is equivalent to `"G"`, and `"Shift+Tab"` to `"BackTab"`. For a character key, Shift on its own applies only to letters with a single uppercase form: a shifted digit or symbol arrives as the character it produces, so bind `"!"` rather than `"Shift+1"`, which is refused, as are `"Shift+Space"` (it arrives as a plain Space) and Shift on a letter such as `ß`. With <kbd>Ctrl</kbd> or <kbd>Alt</kbd>, write the letter lowercase and add `Shift+` for the uppercase one (`"Ctrl+Shift+g"` or `"Alt+Shift+g"`); `"Ctrl+G"` and `"Alt+G"` are refused. `"Alt+Shift+g"` works in any terminal, while `"Ctrl+Shift+g"` needs the kitty keyboard protocol (see the note under the prompt keys).

Binding one key to two actions in the same mode prevents startup, including a collision between a key you configured and a default you did not override. Assigning the same key to one action more than once is allowed.

Some keys are hardcoded and always work alongside any configured keys, scoped to their mode:

Mode | Hardcoded
--- | ---
Normal | Arrow keys, <kbd>Home</kbd>/<kbd>End</kbd>, <kbd>PageUp</kbd>/<kbd>PageDown</kbd>, <kbd>Esc</kbd>
Prompt | <kbd>Esc</kbd> (cancel), <kbd>Tab</kbd> (accept suggestion), <kbd>↓</kbd>/<kbd>↑</kbd> (cycle suggestions)

Because the scoping is per mode, <kbd>Tab</kbd> is still configurable in normal mode, where the default `goto` binding uses it. Binding a hardcoded key to a different action in the same mode prevents startup; binding it to its own action is allowed.

The help view (<kbd>?</kbd>) reflects the configured keybindings.

### Desktop entry

- ["Desktop Entry" specification](https://specifications.freedesktop.org/desktop-entry/desktop-entry-spec-latest.html)

To make `filectrl` the default application for opening directories:

```bash
cp filectrl.desktop ~/.local/share/applications/
xdg-mime default filectrl.desktop inode/directory
update-desktop-database ~/.local/share/applications/
```

## Developing

- [andornaut@github /til/rust](https://github.com/andornaut/til/blob/main/docs/rust.md)
- See [Cargo.toml](./Cargo.toml) for dependencies.
- [Download files and folders of various types to test colors](https://github.com/seebi/dircolors-solarized/raw/refs/heads/master/test-directory.tar.bz2)

```bash
# Run against a directory, logging to ./err
RUST_LOG=debug,notify=info cargo run -- fixtures/ 2>err

# Typecheck the macOS-only code without a Mac
rustup target add aarch64-apple-darwin
cargo check --target aarch64-apple-darwin
```

[`fixtures/`](./fixtures/) is a committed file tree for manual UI testing. Navigate into it with `cargo run` to exercise rendering edge cases:

Path | Covers
--- | ---
`file_types/` | Valid and broken symlinks, an executable, and a regular file
`no_delete/` | Delete and rename permission errors. Needs `chmod 555 fixtures/no_delete` first; git does not track the read-only bit
`scrolling/` | 48 entries with long filenames interspersed, for scrolling and multi-row truncation
Elsewhere | Executables, symlinks, hidden files, Unicode names, special characters, long filenames

Some cases need fixtures git cannot store; create them locally:

- Date-color and size-color buckets (mtimes, sparse files): `touch -t` and `truncate`
- A named pipe: `mkfifo`
- Other-writable and sticky directories: `chmod o+w`, `chmod +t`

### Git hooks

- [cargo-husky](https://github.com/rhysd/cargo-husky)

The pre-commit hook runs `cargo fmt --check`, `cargo test --locked` and `cargo clippy --locked --all-targets -- -D warnings`, then the same clippy for `aarch64-apple-darwin` when that target is installed (`rustup target add aarch64-apple-darwin`). It does not format for you: run `cargo fmt` and stage the result when the check fails.

[Changing cargo-husky configuration](https://github.com/rhysd/cargo-husky/issues/30):

1. Edit the hook script in [`.cargo-husky/hooks/`](./.cargo-husky/hooks/), or the `cargo-husky` entry under `[dev-dependencies]` in [Cargo.toml](./Cargo.toml)
1. `rm .git/hooks/pre-commit` (or other hook file)
1. `cargo clean`
1. `cargo test`
1. Verify that the changes have been applied to `.git/hooks/pre-commit`

### Releasing

Set `version` in [Cargo.toml](./Cargo.toml), then push a matching `v`-prefixed semantic version tag from an up-to-date `main`; the release fails if the tag does not match. The [release workflow](.github/workflows/release.yml) builds the binaries and creates the GitHub Release.

```bash
git tag -a v1.0.0 -m "Release v1.0.0"
git push origin v1.0.0
```

Pushes to `main` rebuild the rolling `dev` release. The workflow manages that tag; do not push it manually.
