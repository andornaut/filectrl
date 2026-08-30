---
name: filectrl-tasks
description: Step-by-step checklists for common FileCtrl changes. Use when adding a keyboard binding, a bundled theme, a theme color, or a file operation, or when modifying the cargo-husky git hooks.
---

# FileCtrl task checklists

## Adding a keyboard binding

1. Add an `Action` variant in `src/app/config/keybindings.rs`, in its mode's group
2. Add the default binding under `[keybindings]` in `src/app/config/default_config.toml`
3. Handle the action in the owning view's `handle_key` (e.g. `src/views/table/handler.rs`)
4. Add the help label in `src/views/help/widget.rs`, which feeds both the help view and `--print-keybindings`
5. Update the keybindings table in `README.md`
6. Cover it in the owning view's unit tests (e.g. `src/views/table/navigation.rs`)

A key already bound to another action in the same mode fails the config load, so
pick one that is free. Keys listed in `HARDCODED_NORMAL` and `HARDCODED_PROMPT`
stay active regardless and cannot be rebound to a different action in their own
mode.

## Adding a bundled theme

1. Create a `.toml` in `themes/` (see `themes/42km.toml`)
2. Add it to the bundled themes table in `README.md`
3. Add a screenshot to `screenshots/`

Each theme's `file_type`, `file_modified_date`, and `file_size` sections should
use colors from that theme's own palette, like the rest of the theme.

## Adding a theme color

1. Add the field to the theme structs in `src/app/config/theme.rs`
2. Add the default in `src/app/config/default_theme.toml`, in both the truecolor section and `[theme256]`
3. Use it in the relevant view
4. Add the section to the theme sections table in `README.md` if it is a new group

An unknown key fails the config load, so a field added to the struct without a
default in both sections breaks every existing config that sets its group.

## Adding a file operation

1. Add a `TaskCommand` variant in `src/file_system/tasks.rs` (async copy/move/delete), or a helper in `src/file_system/operations.rs` (synchronous)
2. Wire it into the `CommandHandler for FileSystem` dispatch in `src/file_system/handler.rs`
3. Add the `Command` variant and route it from the relevant view
4. Update progress and notice handling in `src/command/progress.rs` and `src/views/notices.rs` if needed
5. Cover it in the unit tests of the module it lands in (`src/file_system/operations.rs` or `tasks.rs`)

Follow the message grammar: `Failed to <verb> <object>: <cause>` when the OS
refused, `Cannot <verb> <object>: <reason>` when filectrl refused first.

## Modifying the git hooks

1. Edit `[dev-dependencies.cargo-husky]` in `Cargo.toml`
2. Remove `.git/hooks/pre-commit`
3. Run `cargo clean && cargo test`
4. Verify the changes in `.git/hooks/pre-commit`
