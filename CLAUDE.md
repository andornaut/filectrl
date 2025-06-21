# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

FileCtrl is a Terminal User Interface (TUI) file manager written in Rust using the Ratatui framework. It provides a keyboard-driven interface for file operations with extensive theming and configuration support.

## Common Development Commands

### Build and Run
- `cargo build` - Build the project
- `cargo build --release` - Build optimized release version
- `cargo run` - Run the application
- `cargo run -- <directory>` - Run with specific directory
- `cargo run -- --write-config` - Write default config to ~/.config/filectrl/config.toml

### Testing and Quality
- `cargo test` - Run tests
- `cargo clippy` - Run linter
- `cargo fix --allow-dirty --allow-staged` - Auto-fix linting issues

### Debugging
- `RUST_LOG=debug cargo run 2>err` - Run with debug logging to ./err file
- Set `log_level = "debug"` in config.toml for application-level logging

### Git Hooks
The project uses cargo-husky for git hooks. Pre-commit hooks run `cargo test` and `cargo check`. To modify hooks:
1. Edit `[dev-dependencies.cargo-husky]` in Cargo.toml
2. Remove `.git/hooks/pre-commit`
3. Run `cargo clean && cargo test`

## Architecture

### Core Structure
- `src/main.rs` - Entry point with CLI argument parsing using argh
- `src/lib.rs` - Main library with logging configuration and app initialization
- `src/app.rs` - Core application state and event loop
- `src/views/` - UI components organized by responsibility
- `src/file_system/` - File operations with async support and debouncing
- `src/command/` - Command handling system with modes and tasks

### Key Components

#### Configuration System
- `src/app/config/` - Configuration management
- `default_config.toml` - Comprehensive default configuration with theming
- Supports both truecolor and 256-color terminal themes
- Configuration precedence: CLI option → ~/.config/filectrl/config.toml → built-in defaults

#### File Operations
- `src/file_system/` - Handles all file system operations
- `async.rs` - Asynchronous file operations with buffering (64KB-64MB)
- `watcher.rs` - File system change monitoring with notify crate
- `debounce.rs` - Prevents excessive refreshes (100ms default)

#### UI Architecture
- Uses Ratatui for terminal UI rendering
- `src/views/root.rs` - Main view coordinator
- `src/views/table/` - File listing table with scrolling and selection
- `src/views/prompt/` - Input handling for rename/filter operations
- `src/views/status/` - Status bar with directory and selection info
- `src/views/notices/` - Progress indicators and clipboard status

#### Command System
- `src/command/` - Command processing with different modes
- Supports Normal, Filter, and Rename modes
- Task-based async operations for file operations

### Key Features
- Extensive keyboard navigation (vim-like bindings)
- Copy/cut/paste operations with clipboard integration
- Real-time file system monitoring
- Configurable external program integration
- Comprehensive theming with LS_COLORS support
- Double-click detection for file opening
- Word-based navigation in text inputs

### External Dependencies
- ratatui - Terminal UI framework
- rat-* crates - Additional UI widgets (from git)
- notify - File system watching
- cli-clipboard - System clipboard integration
- open - Opening files with default applications
- chrono - Date/time handling for file timestamps
- toml/serde - Configuration serialization

### Development Notes
- Uses cargo-husky for git pre-commit hooks
- Logging configured via RUST_LOG environment variable or config file
- Terminal truecolor detection with 256-color fallback
- Comprehensive error handling with anyhow
- Unicode-aware text processing for file names and input