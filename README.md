# FileCTRL

A Text User Interface (TUI) file manager for Linux and macOS.

## Usage

```
filectrl [directory to navigate to]
```

### Keyboard controls

***Normal mode***
Keys | Description
--- | ---
Esc, CTRL+c, or q | Quit
h,j,k,l | Left,Down,Up,Right
Enter, Right, f, or l | Navigate/Open selected
Backspace, Left, b or h | Navigate up/back one directory
Tab,SHIFT+Tab | Next focus,Previous focus
c | Clear error messages
Delete | Delete selected
r or F2 | Rename selected
Space | Unselect
CTRL+r or F5 | Refresh
? | Toggle show help
n | Sort by name (toggle direction)
m | Sort by modified (toggle direction)
s | Sort by size (toggle direction)

***Input mode***
Keys | Description
--- | ---
Esc or CTRL+c | Close the input prompt
Enter | Submit your input

## Developing

* [andornaut@github /til/rust](https://github.com/andornaut/til/blob/master/docs/rust.md)

```bash
cargo clippy
cargo fix --allow-dirty --allow-staged
cargo test
cargo run
cargo build --release
./target/debug/filectrl
```

### Git hooks

* [cargo-husky](https://github.com/rhysd/cargo-husky)

[Changing cargo-husky configuration](https://github.com/rhysd/cargo-husky/issues/30):

1. Edit the `[dev-dependencies.cargo-husky]` section of [Cargo.toml](./Cargo.toml)
1. `rm .git/hooks/pre-commit` (or other hook file)
1. `cargo clean`
1. `cargo test`
1. Verify that the changes have been applied to `.git/hooks/pre-commit`

### Notable dependencies

Name | Description
--- | ---
[color-to-tui](https://github.com/uttarayan21/color-to-tui) | Parse hex colors to `tui::style::Color`
[crossterm](https://github.com/crossterm-rs/crossterm)| Cross-platform terminal library ([Documentation](https://docs.rs/crossterm/latest/crossterm/))
[dirs-next](https://github.com/xdg-rs/dirs/tree/master/dirs) | Low-level library that provides conventional config/cache/data paths
[notify](https://github.com/notify-rs/notify)|Cross-platform filesystem notification library
[ratatui](https://github.com/tui-rs-revival/ratatui) | Library to build rich terminal user interfaces
[tui-rs](https://github.com/sayanarijit/tui-input/) | TUI input library
