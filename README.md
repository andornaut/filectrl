# FileCTRL

A Text User Interface (TUI) file manager for Linux and macOS.

## Developing

* [andornaut@github/til/rust](https://github.com/andornaut/til/blob/master/docs/rust.md)

```bash
cargo test
cargo clippy
cargo run
cargo build --release
./target/debug/filectrl
```

### Notable dependencies

Name | Description
--- | ---
[crossterm](https://github.com/crossterm-rs/crossterm)| Cross-platform terminal library ([Documentation](https://docs.rs/crossterm/latest/crossterm/))
[dirs-next](https://github.com/xdg-rs/dirs/tree/master/dirs) | Low-level library that provides conventional config/cache/data paths
[notify](https://github.com/notify-rs/notify)|Cross-platform filesystem notification library
[ratatui](https://github.com/tui-rs-revival/ratatui) | Library to build rich terminal user interfaces
