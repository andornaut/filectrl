# FileCTRL

FileCTRL is a light, opinionated, responsive, theme-able, and simple Text User Interface (TUI) file manager for Linux and macOS

![image](./screenshot.png)

## Installation

You can [download and install a pre-built binary](https://github.com/andornaut/filectrl/releases) for Linux or macOS:

```bash
curl -L https://github.com/andornaut/filectrl/releases/download/latest/filectrl-linux -o filectrl
chmod +x filectrl
sudo mv filectrl /usr/local/bin/
```

On macOS, allow the _unsigned_ `filectrl` binary to be executed:

```bash
xattr -d com.apple.quarantine filectrl
```

## Building

1. `git clone` and `cd` into this repository
1. Run ```cargo build --release && sudo cp target/release/filectrl /usr/local/bin/```

## Usage

Run `filectrl --help` to view the available command line arguments and options:

```text
Usage: filectrl [<directory>] [-c <config>] [--write-config]

FileCTRL is a light, opinionated, responsive, theme-able, and simple
Text User Interface (TUI) file manager for Linux and macOS

Positional Arguments:
  directory         path to a directory to navigate to

Options:
  -c, --config      path to a configuration file
  --write-config    write the default config to ~/.config/filectrl/config.toml,
                    then exit
  --help            display usage information
```

### Copy / paste

When you copy/cut a file or directory, FileCTRL puts `${operation} ${path}` into your clipboard buffer
(where `operation` is "cp" or "mv").
If you then paste into a second FileCTRL window, this second instance of FileCTRL will perform the equivalent of:
`${operation} ${path} ${current_directory}`, e.g. `cp filectrl.desktop ~/.local/share/applications/`.
Under the hood, FileCTRL doesn't actually invoke `cp` or `mv`, but implements similar operations using the Rust standard library.

### Keyboard controls

_**Normal mode**_

Keys | Description
--- | ---
q | Quit
↓/j, ↑/k, ←/h, →/l | Navigate
←/b/Backspace | Go to parent directory
→/f/l/Enter | Open the selected file or navigate to the selected directory
Home/g/^ | Go to first row
End/G/$ | Go to last row
CTRL+f/CTRL+d/PgDn | Scroll down one page
CTRL+b/CTRL+u/PgUp | Scroll up one page
Delete | Delete the selected file or directory
/ | Filter by name
CTRL+r/F5 | Refresh the current directory
r/F2 | Rename the selected file or directory
w | Open a new `filectrl` window
t | Open current directory in a terminal
a, c, p | Clear alerts, clipboard content, or progress bars
CTRL+c, CTRL+x, CTRL+v | Copy/Cut/Paste selected file or directory
n, m, s | Sort by name, modified date, or size
? | Toggle help

_**Filtering / Renaming mode**_

Keys | Description
--- | ---
Esc | Cancel and exit filtering/renaming mode
Enter | Submit your input and exit filtering/renaming mode
←/→ | Move cursor
CTRL+←/→ | Move cursor by word (delimited by whitespaces or punctuation)
Home/End | Move to beginning/end of line
Backspace/Delete | Delete character before/after cursor
SHIFT+←/→ | Select text
SHIFT+Home/End | Select to beginning/end of line
CTRL+SHIFT+←/→ | Select words (delimited by whitespaces or punctuation)
CTRL+a | Select all text
CTRL+c | Copy selected text
CTRL+x | Cut selected text
CTRL+v | Paste from clipboard

## Configuration

The configuration is drawn from the first of the following:

1. The path specified by the command line option: `--config-path`
1. The default path, if it exists: `~/.config/filectrl/config.toml`
1. The built-in [default configuration](./src/app/default_config.rs)

Run `filectrl --write-config` to write the [default configuration](./src/app/default_config.rs) to `~/.config/filectrl/config.toml`.

### Opening in other applications

- [andornaut@github /til/ubuntu#default-applications](https://github.com/andornaut/til/blob/master/docs/ubuntu.md#default-applications)
- [XDG MIME Applications](https://wiki.archlinux.org/title/XDG_MIME_Applications)

Keyboard key | Description
--- | ---
f | Open the selected file using the default application configured in your environment
o | Open the selected file using the program configured by: `open_selected_file_template`
t | Open the current directory in the program configured by: `open_current_directory_template`
w | Open a new `filectrl` window in the terminal configured by: `open_new_window_template`

```toml
# %s will be replaced by the current directory path:
open_current_directory_template = "alacritty --working-directory %s"
# %s will be replaced by the selected file or directory path:
open_selected_file_template = "pcmanfm %s"
```

### Theming

All colors can be changed by editing the configuration file:

```bash
filectrl --write-config
vim ~/.config/filectrl/config.toml
```

You can see all of the available theme variables in the [default configuration](./src/app/default_config.rs).

### Desktop entry

- ["Desktop Entry" specification](https://specifications.freedesktop.org/desktop-entry-spec/desktop-entry-spec-latest.html)

You can make `filectrl` the default application for opening directories. Start by copying the [`filectrl.desktop` file](./filectrl.desktop) to `~/.local/share/applications/`:

```bash
cp filectrl.desktop ~/.local/share/applications/
xdg-mime default filectrl.desktop inode/directory
update-desktop-database ~/.local/share/applications/
```

## Developing

- [andornaut@github /til/rust](https://github.com/andornaut/til/blob/master/docs/rust.md)
- See [Cargo.toml](./Cargo.toml) for dependencies.

```bash
cargo clippy
cargo fix --allow-dirty --allow-staged
cargo test
cargo run
cargo build --release
./target/debug/filectrl

# Log to ./err
RUST_LOG=debug cargo run 2>err
```

### Git hooks

- [cargo-husky](https://github.com/rhysd/cargo-husky)

[Changing cargo-husky configuration](https://github.com/rhysd/cargo-husky/issues/30):

1. Edit the `[dev-dependencies.cargo-husky]` section of [Cargo.toml](./Cargo.toml)
1. `rm .git/hooks/pre-commit` (or other hook file)
1. `cargo clean`
1. `cargo test`
1. Verify that the changes have been applied to `.git/hooks/pre-commit`
