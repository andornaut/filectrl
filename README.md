# FileCTRL

[![CI](https://github.com/andornaut/filectrl/actions/workflows/test.yml/badge.svg)](https://github.com/andornaut/filectrl/actions/workflows/test.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

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

On macOS, allow the _unsigned_ binary to run:

```bash
xattr -d com.apple.quarantine filectrl
```

## Building

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
`--force` | Replace an existing file when writing defaults
`--print-keybindings` | Print the keybindings, then exit
`--write-default-config` | Write the default config, then exit
`--write-default-themes` | Write the default theme as `theme.toml` beside the config, then exit
`-V`, `--version` | Print the version, then exit
`-h`, `--help` | Print usage, then exit

The four flags that act and exit are mutually exclusive, and each accepts only the arguments that change what it does:

Flag | Also accepts
--- | ---
`--print-keybindings` | `--config`, `--include`
`--write-default-config` | `--config`, `--force`
`--write-default-themes` | `--config`, `--force`
`--version` | nothing

Anything else is reported rather than ignored. Both write flags print the path they wrote and refuse to replace an existing file unless `--force` is given. That path follows `$XDG_CONFIG_HOME`, so it is not always under `~/.config`.

### Bookmarks

Bookmarks are symlinks to folders, stored in a `bookmarks/` directory beside the config file (e.g. `~/.config/filectrl/bookmarks/`).

Key | Action
--- | ---
<kbd>B</kbd> | Bookmark the current directory. The prompt defaults to the directory's name
<kbd>'</kbd> or <kbd>&#96;</kbd> | Show all bookmarks in the table
<kbd>Enter</kbd> | Navigate to the linked folder
<kbd>r</kbd>, <kbd>d</kbd> | Rename or delete the bookmark

Names must be unique, cannot be empty, and cannot contain a path separator.

### Copy / paste

Copying or cutting puts `${operation} ${path}` on the system clipboard, where `operation` is `cp` or `mv`. Pasting in another FileCTRL window performs the equivalent of `${operation} ${path} ${current_directory}`, e.g. `cp filectrl.desktop ~/.local/share/applications/`.

Without a system clipboard (e.g. over SSH or on a bare console), copy and paste still work within a single window. Pasting with nothing to paste and no system clipboard to read shows a warning, since an entry copied in another window would be unreachable.

When the destination already contains an entry with the same name, the paste stops and asks:

Key | Action
--- | ---
<kbd>s</kbd> | Skip this entry
<kbd>S</kbd> | Skip this entry and every later collision in the same paste
<kbd>o</kbd> | Replace the existing entry
<kbd>O</kbd> | Replace this and every later collision in the same paste
<kbd>Esc</kbd> | Abandon the rest of the paste

- An existing **directory** is never replaced, so only the skip choices are offered for one. Modifier chords are not choices: <kbd>Ctrl</kbd>+<kbd>o</kbd> abandons the paste.
- <kbd>S</kbd> and <kbd>O</kbd> also cover copies already running: if another program takes a name inside a directory being copied, the standing answer settles it without stopping the copy. Only <kbd>S</kbd> settles a directory. Anything left unsettled is reported when the copy finishes.
- A cut that skipped an entry keeps its original: the skipped entry is not at the destination, so removing the source would take the only copy of it.
- Whatever is not pasted (collisions you abandon, entries that failed) stays on the clipboard, so pasting again retries exactly those. Entries you skip deliberately do not. If nothing was pasted at all, the clipboard is unchanged.

### Multi-select

Mark entries to apply chmod, copy, cut, or delete to several at once.

Key | Action
--- | ---
<kbd>v</kbd>/<kbd>Space</kbd> | Toggle a mark on the current row
<kbd>V</kbd> | Enter range mode: the current row becomes the anchor. Press again to exit, keeping the marks
<kbd>Esc</kbd> | Clear all marks and exit range mode

In range mode, moving the cursor or clicking extends the marked range from the anchor to the cursor. Outside range mode, clicking only moves the cursor. Marking clears the clipboard.

Marks name entries but are stored as row positions, so what becomes of them depends on why the listing changed:

Change | Marks
--- | ---
Sorting, filtering, toggling hidden files | Cleared
Reload (<kbd>Ctrl</kbd>+<kbd>r</kbd> or a watcher refresh) | Kept, re-found by path. An entry that is gone loses its mark
A search finishing or being cancelled | Kept
Navigating to another directory | Cleared
chmod, copy, cut, delete | Consumed by the operation

### Filtering

The filter (<kbd>f</kbd>/<kbd>&#92;</kbd>) is a case-insensitive substring match against the Name column, so it matches what is on screen: the entry's own name in a normal listing, the path relative to the search root while searching, and the bookmark name in the bookmarks view.

Directories carry a trailing `/` outside the bookmarks view, so `/` filters a listing down to directories, and `docs/` matches both the `docs` directory and, in search results, everything under it.

### Searching

Search (<kbd>/</kbd>) walks the current directory recursively, matching a case-insensitive substring against each entry's name. Symlinked directories are not descended into. `search_max_depth` and `search_max_results` in `[file_system]` bound the walk; on reaching either, FileCTRL keeps the results it has and says so.

Results appear as the walk finds them and settle into the sort order once it ends, whether it finished or was cancelled.

### Sorting

<kbd>n</kbd>/<kbd>m</kbd>/<kbd>s</kbd> sort by name, modified time, or size; clicking a column header does the same. Sorting by the same column again reverses it. Each column starts in the direction it is usually reached for:

Column | Default direction
--- | ---
Name | A-Z
Modified | Newest first
Size | Largest first

The Name column orders by the text it displays (while searching, the path relative to the search root), ignoring case and a leading dot on each path segment, so a dot file sorts next to its neighbours the way `ls -a` does. `sort_directories_first` in the `[ui]` section groups directories first, for the Name column only.

### Default keybindings

All keybindings can be [customized](#customizing-keybindings).

_**Normal mode**_

Actions | Keys
--- | ---
Select next, previous row | <kbd>↓</kbd>/<kbd>j</kbd>, <kbd>↑</kbd>/<kbd>k</kbd>
Select first, middle, last row | <kbd>Home</kbd>/<kbd>g</kbd>/<kbd>^</kbd>, <kbd>z</kbd>, <kbd>End</kbd>/<kbd>G</kbd> (Uppercase)/<kbd>$</kbd>
Select top, middle, bottom visible row | <kbd>H</kbd> (Uppercase), <kbd>M</kbd> (Uppercase), <kbd>L</kbd> (Uppercase)
Page down, up | <kbd>Ctrl</kbd>+<kbd>d</kbd>/<kbd>Ctrl</kbd>+<kbd>f</kbd>/<kbd>PgDn</kbd>, <kbd>Ctrl</kbd>+<kbd>u</kbd>/<kbd>Ctrl</kbd>+<kbd>b</kbd>/<kbd>PgUp</kbd>
Go to parent dir | <kbd>←</kbd>/<kbd>h</kbd>/<kbd>b</kbd>/<kbd>Backspace</kbd>
Go to previous dir | <kbd>-</kbd>
Go to home dir | <kbd>~</kbd>
Go to path | <kbd>:</kbd>/<kbd>Tab</kbd>
Open | <kbd>→</kbd>/<kbd>l</kbd>/<kbd>Enter</kbd>
Open current directory | <kbd>t</kbd>
Open new window | <kbd>w</kbd>
Open with... | <kbd>o</kbd>
Mark/unmark item | <kbd>v</kbd>/<kbd>Space</kbd>
Range mark | <kbd>V</kbd> (Uppercase)
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
Toggle show hidden files | <kbd>.</kbd>
Cancel file or search operations | <kbd>K</kbd> (Uppercase)
Clear alerts, progress | <kbd>Ctrl</kbd>+<kbd>a</kbd>, <kbd>Ctrl</kbd>+<kbd>p</kbd>
Clear clipboard/filter/marks/search, exit bookmarks view | <kbd>Esc</kbd>
Toggle help | <kbd>?</kbd>
Quit | <kbd>q</kbd>

_**Prompt mode**_

Actions | Keys
--- | ---
Submit | <kbd>Enter</kbd>
Cancel | <kbd>Esc</kbd>
Reset to initial value | <kbd>Ctrl</kbd>+<kbd>u</kbd>/<kbd>Ctrl</kbd>+<kbd>z</kbd>
Select all | <kbd>Ctrl</kbd>+<kbd>a</kbd>
Copy, Cut, Paste text | <kbd>Ctrl</kbd>+<kbd>c</kbd>, <kbd>Ctrl</kbd>+<kbd>x</kbd>, <kbd>Ctrl</kbd>+<kbd>v</kbd>
Move cursor | <kbd>←</kbd>/<kbd>→</kbd>
Move cursor by word | <kbd>Ctrl</kbd>+<kbd>←</kbd>/<kbd>→</kbd>
Move cursor to start, end | <kbd>Home</kbd>, <kbd>Ctrl</kbd>+<kbd>e</kbd>/<kbd>End</kbd>
Select text | <kbd>Shift</kbd>+<kbd>←</kbd>/<kbd>→</kbd>
Select to line start, end | <kbd>Shift</kbd>+<kbd>Home</kbd>, <kbd>Shift</kbd>+<kbd>End</kbd>
Select by word | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>←</kbd>/<kbd>→</kbd>
Delete before, after cursor | <kbd>Backspace</kbd>, <kbd>Delete</kbd>
Accept path suggestion (cursor at end of input) | <kbd>Tab</kbd>
Cycle path suggestions (cursor at end of input) | <kbd>↓</kbd>/<kbd>↑</kbd>

> [!NOTE]
> <kbd>Ctrl</kbd>+<kbd>Shift</kbd> keybindings require a terminal that supports the [kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/) (e.g. Alacritty). tmux users must also add the following to `~/.tmux.conf`:
>
> ```conf
> set -g extended-keys on
> set -ga terminal-features ",*:extkeys"
> ```

## Configuration

The configuration is drawn from the first of:

1. The path given by `--config`
1. `~/.config/filectrl/config.toml`, if it exists
1. The built-in [default configuration](./src/app/config/default_config.toml)

`filectrl --write-default-config` writes the defaults to that path. It writes the configuration keys only; the theme keys are a separate file written by `--write-default-themes`.

Override only what you want to change:

```toml
# ~/.config/filectrl/config.toml
[openers.linux]
open_directory = "alacritty --working-directory %s"
open_filectrl_window = "alacritty --command filectrl %s"
```

Validation is strict: an unrecognized key (a misspelled setting or theme property), an unknown modifier name, or an invalid value (such as `buffer_min_bytes` exceeding `buffer_max_bytes`) makes FileCTRL exit with an error rather than ignore it.

### Opening in other applications

- [andornaut@github /til/ubuntu#default-applications](https://github.com/andornaut/til/blob/master/docs/ubuntu.md#default-applications)
- [XDG MIME Applications](https://wiki.archlinux.org/title/XDG_MIME_Applications)

Key | Opens with
--- | ---
<kbd>l</kbd> | `openers.open_file`
<kbd>t</kbd> | `openers.open_directory`, for the current directory
<kbd>w</kbd> | `openers.open_filectrl_window`, a new `filectrl` window
<kbd>o</kbd> | A picker of the applications that can open the selection

```toml
# Use [openers.linux] on Linux, or [openers.macos] on macOS.
# %s is replaced by the relevant path at runtime.
[openers.linux]
# %s will be replaced by the path to the current working directory:
open_directory = "alacritty --working-directory %s"
# %s will be replaced by the path to the selected file or directory:
open_file = "pcmanfm %s"
open_filectrl_window = "alacritty --command filectrl %s"
# %s will be replaced by a command line, not a path (see "Open with..." below):
run_in_terminal = "alacritty --command %s"

[openers.macos]
open_directory = "open %s"
open_file = "open %s"
open_filectrl_window = "open -a Terminal %s"
# run_in_terminal is Linux only and is ignored here
run_in_terminal = ""
```

#### Open with...

<kbd>o</kbd> replaces the file table with the applications that can open the selection, leaving the breadcrumbs and status bar visible. The default application is listed first and marked `(default)`.

Key | Action
--- | ---
<kbd>↓</kbd>/<kbd>j</kbd>, <kbd>↑</kbd>/<kbd>k</kbd> | Move between applications
<kbd>→</kbd>/<kbd>l</kbd>/<kbd>Enter</kbd> | Open with the selected application
<kbd>1</kbd> to <kbd>9</kbd> | Open with that numbered application
<kbd>o</kbd> | Close the picker
<kbd>Esc</kbd> | Close the picker and reset the view (clears the clipboard, filter and marks, and leaves search or bookmarks)

Only the first nine rows have a number; scroll to reach the rest. Applications that share a name are collapsed to the best ranked one.

The list is built per platform:

- **Linux:** the MIME type is resolved through the shared MIME database, including its parent types (a `.rs` file also offers plain text editors), then matched against `mimeapps.list` and the `.desktop` files under `$XDG_DATA_DIRS/applications`, per the [mime-apps spec](https://specifications.freedesktop.org/mime-apps/latest-single/). The application directories are indexed once per run, so an application installed while FileCTRL is open is not offered until the next start.
- **macOS:** Launch Services, which requires macOS 12 or newer. The chosen application is launched with `open -a`.

- Applications that need a terminal (`Terminal=true`) run inside `openers.run_in_terminal`, whose `%s` is a command line rather than a path: `xterm -e %s` becomes `xterm -e vim '/some file.txt'`. Set it to `""` to leave them out.
- `openers.open_file` (or `openers.open_directory` for a directory) is offered last when set, showing its command template alongside the setting name, so the picker works without an application database. Set it to `""` to leave it out, in which case a path with no matching application shows "No applications found".

### Theming

`[theme]` applies to truecolor terminals and `[theme256]` to 256-color terminals. FileCTRL detects truecolor support via `$COLORTERM`; `--no-truecolor` selects the 256-color theme regardless.

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
`alert` | Alert bar (`base`, `error`, `info`, `warn`)
`breadcrumbs` | Path breadcrumbs (`base`, `ancestor`, `basename`, `separator`)
`clipboard` | Clipboard status indicators (`copy`, `cut`, `delete`)
`file_modified_date` | Date column by age (`less_than_minute`, `less_than_hour`, `less_than_day`, `less_than_month`, `less_than_year`, `greater_than_year`)
`file_size` | Size column by magnitude (`bytes`, `kib`, `mib`, `gib`, `tib`, `pib`)
`file_type` | Row colors by file type (`directory`, `executable`, `symlink`, `regular_file`, etc.)
`help` | Help panel (`base`, `header`, `actions`, `shortcuts`)
`notice` | Notice bar (`filter`, `progress`)
`open_with` | Open with... picker (`base`, `detail`, `selected`, `shortcut`)
`prompt` | Input prompt (`cursor`, `input`, `label`, `selected`)
`scrollbar` | Scrollbar (`ends`, `thumb`, `track`, plus `show_ends` boolean)
`status` | Status bar (`detail`, `label`)
`table` | File table (`body`, `header`, `header_sorted`, `selected`, `marked`, `delete`, `bookmark`)

#### LS_COLORS integration

With `ls_colors_take_precedence`, colors from `$LS_COLORS` are applied on top of the configured file type colors, including extension patterns such as `*.tar=01;31`.

```toml
[theme.file_type]
ls_colors_take_precedence = true
```

#### External theme files

`include_files` merges other TOML files on top of the config:

```toml
include_files = ["theme.toml"]
```

- Relative paths resolve from the directory containing the config file; absolute paths are used as-is
- Files merge in order, later ones taking precedence over the base config and over earlier files
- The value must be an array of strings, and every listed file must exist and parse, or FileCTRL exits with an error

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

```bash
filectrl --include themes/42km.toml
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
Named keys | `"Enter"`, `"Esc"`, `"Backspace"`, `"Delete"`, `"Space"`, `"Tab"`, `"BackTab"`, `"Up"`, `"Down"`, `"Left"`, `"Right"`, `"Home"`, `"End"`, `"PgUp"`, `"PgDn"`
Function keys | `"F2"`, `"F5"`
Modifier prefixes | `"Ctrl+c"`, `"Shift+Left"`, `"Ctrl+Shift+a"`

`"Shift+g"` is equivalent to `"G"`, and `"Shift+Tab"` to `"BackTab"`.

Binding one key to two actions in the same mode prevents startup, including a collision between a key you configured and a default you did not override. Assigning the same key to one action more than once is allowed.

Some keys are hardcoded and always work alongside any configured keys, scoped to their mode:

Mode | Hardcoded
--- | ---
Normal | Arrow keys, <kbd>Home</kbd>/<kbd>End</kbd>, <kbd>PageUp</kbd>/<kbd>PageDown</kbd>, <kbd>Esc</kbd>
Prompt | <kbd>Esc</kbd> (cancel), <kbd>Tab</kbd> (accept suggestion), <kbd>↓</kbd>/<kbd>↑</kbd> (cycle suggestions)

Because the scoping is per mode, <kbd>Tab</kbd> is still configurable in normal mode, where the default `goto` binding uses it. Binding a hardcoded key to a different action in the same mode prevents startup; binding it to its own action is allowed.

The help view (<kbd>?</kbd>) reflects the configured keybindings.

### Desktop entry

- ["Desktop Entry" specification](https://specifications.freedesktop.org/desktop-entry-spec/desktop-entry-spec-latest.html)

To make `filectrl` the default application for opening directories:

```bash
cp filectrl.desktop ~/.local/share/applications/
xdg-mime default filectrl.desktop inode/directory
update-desktop-database ~/.local/share/applications/
```

## Developing

- [andornaut@github /til/rust](https://github.com/andornaut/til/blob/master/docs/rust.md)
- See [Cargo.toml](./Cargo.toml) for dependencies.
- [Download files and folders of various types to test colors](https://github.com/seebi/dircolors-solarized/raw/refs/heads/master/test-directory.tar.bz2)

```bash
cargo clippy
cargo fix --allow-dirty --allow-staged
cargo test
cargo run
cargo build --release

# Log to ./err
RUST_LOG=debug,notify=info cargo run -- fixtures/ 2>err
```

[`fixtures/`](./fixtures/) is a committed file tree for manual UI testing. Navigate into it with `cargo run` to exercise rendering edge cases:

Path | Covers
--- | ---
`file_types/` | Named pipe, symlinks, executable, and directory permission variants (other-writable, sticky)
`no_delete/` | Delete and rename permission errors. Needs `chmod 555 fixtures/no_delete` first; git does not track the read-only bit
`scrolling/` | 48 entries with long filenames interspersed, for scrolling and multi-row truncation
Elsewhere | Executables, symlinks, hidden files, Unicode names, special characters, long filenames

Date-color and size-color buckets need fixtures git cannot store (mtimes, sparse files); create them locally with `touch -t` and `truncate`.

### Git hooks

- [cargo-husky](https://github.com/rhysd/cargo-husky)

[Changing cargo-husky configuration](https://github.com/rhysd/cargo-husky/issues/30):

1. Edit the `[dev-dependencies.cargo-husky]` section of [Cargo.toml](./Cargo.toml)
1. `rm .git/hooks/pre-commit` (or other hook file)
1. `cargo clean`
1. `cargo test`
1. Verify that the changes have been applied to `.git/hooks/pre-commit`

### Releasing

Push a semantic version tag from an up-to-date `main`. The [release workflow](.github/workflows/release.yml) builds the binaries and creates the GitHub Release.

```bash
git tag -a v1.0.0 -m "Release v1.0.0"
git push origin v1.0.0
```

Pushes to `main` rebuild the rolling `dev` release. The workflow manages that tag; do not push it manually.
