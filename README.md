# FileCTRL

A Text User Interface (TUI) file manager for Linux and macOS.

![image](./screenshot.png)

## Usage

```
filectrl [directory to navigate to]
```

### Keyboard controls

***Normal mode***
Keys | Description
--- | ---
q | Quit
h / j / k / l | Left / Down / Up / Right
Enter, Right, f, l | Open selected
Backspace, Left, b, h | Navigate up one directory
Tab / SHIFT+Tab | Next focus / Previous focus
Delete | Delete selected
r, F2 | Rename selected
Space | Unselect
CTRL+r, F5 | Refresh
e | Clear error messages
? | Toggle help
n | Sort by name (toggle direction)
m | Sort by modified (toggle direction)
s | Sort by size (toggle direction)

***Filtered mode***
Keys | Description
--- | ---
Esc or CTRL+C | Exit filtered mode

***Input mode***
Keys | Description
--- | ---
Esc or CTRL+c | Exit input mode
Enter | Submit your input

## Developing

* [andornaut@github /til/rust](https://github.com/andornaut/til/blob/master/docs/rust.md)
* See [Cargo.toml](./Cargo.toml) for dependencies.

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
