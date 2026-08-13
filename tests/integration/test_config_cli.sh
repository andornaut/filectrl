#!/usr/bin/env bash
# Integration tests for configuration loading and the non-TUI CLI flags.
#
# Most of these need no terminal: --print-keybindings resolves the whole config
# chain and prints the result, which makes it the cheapest way to observe
# which layer won. The last two drive the TUI to prove a rebind reaches the
# key handler and not just the printed help.
#
# The command-line section covers the flags themselves: what each one accepts,
# and that a mistake in the invocation is reported rather than dropped.
#
# Precedence, lowest to highest:
#   built-in defaults
#   the user config at ~/.config/filectrl/config.toml, unless --config replaces it
#   include_files listed inside that config
#   --include paths, in order

source "$(dirname "${BASH_SOURCE[0]}")/harness.sh"

CONFIG_DIR() { echo "$SANDBOX/home/.config/filectrl"; }

# Writes a config that binds select_next to $2, at $1. The default is `j`, and
# `e`, `i` and `u` are unbound, so which key ends up on the "Select next" line
# names the layer that won.
write_select_next() {
    mkdir -p "$(dirname "$1")"
    printf '[keybindings]\nselect_next = "%s"\n' "$2" > "$1"
}

assert_select_next() {
    assert_run_output "Select next, previous row: +↓/$1,"
}

# ------------------------------------------------------------ writing defaults

test_write_default_config_writes_a_config_it_can_read_back() {
    run_filectrl --write-default-config
    assert_run_succeeded
    [ -f "$(CONFIG_DIR)/config.toml" ] || _fail "no config at $(CONFIG_DIR)/config.toml"
    # Round-trip: the file it writes has to be one it accepts
    run_filectrl --print-keybindings
    assert_run_succeeded
    assert_select_next 'j'
}

test_write_default_themes_writes_a_theme_it_can_read_back() {
    run_filectrl --write-default-themes
    assert_run_succeeded
    [ -f "$(CONFIG_DIR)/theme.toml" ] || _fail "no theme at $(CONFIG_DIR)/theme.toml"
    run_filectrl --print-keybindings -i "$(CONFIG_DIR)/theme.toml"
    assert_run_succeeded
}

# The config directory follows $XDG_CONFIG_HOME, so the path in the help text
# is not always the path written. Naming it is the only way the user learns
# which file to go and edit.
test_writing_a_default_names_the_file_it_wrote() {
    run_filectrl --write-default-config
    assert_run_succeeded
    assert_run_output "Wrote $(CONFIG_DIR)/config.toml"
    run_filectrl --write-default-themes
    assert_run_succeeded
    assert_run_output "Wrote $(CONFIG_DIR)/theme.toml"
}

# The two flags divide the defaults rather than restating them: everything the
# theme file carries is absent from the config file.
test_the_written_config_and_theme_do_not_overlap() {
    run_filectrl --write-default-config
    run_filectrl --write-default-themes
    grep -q '^\[theme' "$(CONFIG_DIR)/theme.toml" ||
        _fail "the theme file has no theme sections"
    ! grep -q '^\[theme' "$(CONFIG_DIR)/config.toml" ||
        _fail "the config file restates the theme"
}

test_writing_a_default_refuses_to_replace_an_existing_file() {
    mkdir -p "$(CONFIG_DIR)"
    printf '# hand written\n' > "$(CONFIG_DIR)/config.toml"
    run_filectrl --write-default-config
    assert_run_failed
    assert_run_output 'already exists; pass --force'
    grep -q 'hand written' "$(CONFIG_DIR)/config.toml" ||
        _fail "the existing config was replaced anyway"
}

test_force_replaces_an_existing_file() {
    mkdir -p "$(CONFIG_DIR)"
    printf '# hand written\n' > "$(CONFIG_DIR)/config.toml"
    run_filectrl --write-default-config --force
    assert_run_succeeded
    ! grep -q 'hand written' "$(CONFIG_DIR)/config.toml" ||
        _fail "--force left the existing config in place"
}

# A config path names where the config is written, not only where it is read
# from, and the theme lands beside it so that a relative include_files entry
# resolves.
test_writing_a_default_honors_the_config_path() {
    run_filectrl --write-default-config -c "$SANDBOX/elsewhere/mine.toml"
    assert_run_succeeded
    assert_run_output "Wrote $SANDBOX/elsewhere/mine.toml"
    run_filectrl --write-default-themes -c "$SANDBOX/elsewhere/mine.toml"
    assert_run_succeeded
    assert_run_output "Wrote $SANDBOX/elsewhere/theme.toml"
    ! [ -f "$(CONFIG_DIR)/config.toml" ] ||
        _fail "wrote to the default path as well"
    run_filectrl --print-keybindings -c "$SANDBOX/elsewhere/mine.toml"
    assert_run_succeeded
}

# ------------------------------------------------------------------ command line

test_the_version_is_printed_and_documented() {
    run_filectrl --version
    assert_run_succeeded
    assert_run_output '^filectrl [0-9]+\.[0-9]+\.[0-9]+$'
    run_filectrl -V
    assert_run_succeeded
    assert_run_output '^filectrl [0-9]+\.[0-9]+\.[0-9]+$'
    # A flag absent from the help is one nobody finds
    run_filectrl --help
    assert_run_output '\-V, --version'
}

test_help_answers_to_both_spellings() {
    run_filectrl -h
    assert_run_succeeded
    assert_run_output '^Usage: filectrl'
    run_filectrl --help
    assert_run_succeeded
    assert_run_output '^Usage: filectrl'
}

# `--` ends option parsing, so what follows is the directory even when it is
# spelled like a flag.
test_a_double_dash_ends_the_options() {
    mkdir -p "$SANDBOX/--version"
    run_filectrl -- "$SANDBOX/--version"
    # Reaches the terminal check, which is as far as a captured run can get:
    # what matters is that it was read as a path and not as the version flag.
    assert_run_failed
    assert_run_output 'not a terminal'
}

test_two_actions_at_once_are_rejected() {
    run_filectrl --write-default-config --write-default-themes
    assert_run_failed
    assert_run_output '--write-default-config and --write-default-themes cannot be combined'
    assert_run_output 'Run filectrl --help'
}

# An argument that cannot change what the chosen action does is a mistake in
# the invocation, not something to drop on the floor.
test_an_argument_the_action_ignores_is_rejected() {
    run_filectrl --write-default-config -i "$SANDBOX/over.toml"
    assert_run_failed
    assert_run_output '--include has no effect with --write-default-config'

    run_filectrl --print-keybindings --no-truecolor
    assert_run_failed
    assert_run_output '--no-truecolor has no effect with --print-keybindings'

    run_filectrl --version "$SANDBOX/fixtures"
    assert_run_failed
    assert_run_output 'A directory argument has no effect with --version'

    run_filectrl --force
    assert_run_failed
    assert_run_output '--force has no effect without'
}

# The TUI drives the terminal directly, so a redirected stdout has to say so
# rather than surface as the errno from opening the controlling terminal.
test_running_without_a_terminal_says_so() {
    run_filectrl "$SANDBOX/fixtures"
    assert_run_failed
    assert_run_output 'Cannot start: standard output is not a terminal'
}

test_an_empty_directory_argument_is_rejected() {
    run_filectrl ""
    assert_run_failed
    assert_run_output 'Cannot open an empty path'
}

# ------------------------------------------------------------------ --print-keybindings

test_keybindings_prints_the_defaults_and_exits() {
    run_filectrl --print-keybindings
    assert_run_succeeded
    assert_run_output 'Normal Mode'
    assert_run_output 'Prompt Mode'
    assert_select_next 'j'
}

# -------------------------------------------------------------------- precedence

test_user_config_overrides_the_built_in_default() {
    write_select_next "$(CONFIG_DIR)/config.toml" u
    run_filectrl --print-keybindings
    assert_run_succeeded
    assert_select_next 'u'
}

test_cli_include_overrides_the_user_config() {
    write_select_next "$(CONFIG_DIR)/config.toml" u
    write_select_next "$SANDBOX/over.toml" e
    run_filectrl --print-keybindings -i "$SANDBOX/over.toml"
    assert_run_succeeded
    assert_select_next 'e'
}

test_the_last_cli_include_wins() {
    write_select_next "$SANDBOX/first.toml" e
    write_select_next "$SANDBOX/second.toml" i
    run_filectrl --print-keybindings -i "$SANDBOX/first.toml" -i "$SANDBOX/second.toml"
    assert_run_succeeded
    assert_select_next 'i'
}

test_config_include_files_override_the_config_that_lists_them() {
    write_select_next "$SANDBOX/listed.toml" e
    # include_files is a top-level key, so it has to precede the first table
    printf 'include_files = ["%s"]\n[keybindings]\nselect_next = "u"\n' \
        "$SANDBOX/listed.toml" > "$SANDBOX/main.toml"
    run_filectrl --print-keybindings -c "$SANDBOX/main.toml"
    assert_run_succeeded
    assert_select_next 'e'
}

test_cli_include_overrides_the_configs_own_include_files() {
    write_select_next "$SANDBOX/listed.toml" e
    write_select_next "$SANDBOX/cli.toml" i
    printf 'include_files = ["%s"]\n' "$SANDBOX/listed.toml" > "$SANDBOX/main.toml"
    run_filectrl --print-keybindings -c "$SANDBOX/main.toml" -i "$SANDBOX/cli.toml"
    assert_run_succeeded
    assert_select_next 'i'
}

# --config names the config outright; it does not merge with the user's.
test_explicit_config_replaces_the_user_config() {
    write_select_next "$(CONFIG_DIR)/config.toml" u
    printf '[keybindings]\nselect_previous = "e"\n' > "$SANDBOX/other.toml"
    run_filectrl --print-keybindings -c "$SANDBOX/other.toml"
    assert_run_succeeded
    assert_select_next 'j' # the built-in default, not the user config's `u`
    assert_run_output '↑/e'
}

test_a_partial_config_keeps_the_defaults_it_does_not_mention() {
    write_select_next "$(CONFIG_DIR)/config.toml" u
    run_filectrl --print-keybindings
    assert_run_succeeded
    assert_run_output 'Quit: +q'    # untouched
    assert_run_output '↑/k'         # untouched
}

# ---------------------------------------------------------------- rejected input

test_malformed_toml_is_rejected() {
    printf 'this is not toml =\n' > "$SANDBOX/bad.toml"
    run_filectrl --print-keybindings -c "$SANDBOX/bad.toml"
    assert_run_failed
    assert_run_output 'Failed to parse TOML'
}

test_an_unknown_config_key_is_rejected() {
    printf '[ui]\nnot_a_real_option = 1\n' > "$SANDBOX/unknown.toml"
    run_filectrl --print-keybindings -c "$SANDBOX/unknown.toml"
    assert_run_failed
    assert_run_output "Unknown configuration key: 'ui.not_a_real_option'"
}

test_a_missing_config_path_is_rejected() {
    run_filectrl --print-keybindings -c "$SANDBOX/absent.toml"
    assert_run_failed
    assert_run_output "Failed to read config file $SANDBOX/absent.toml"
}

test_a_missing_include_path_is_rejected() {
    run_filectrl --print-keybindings -i "$SANDBOX/absent.toml"
    assert_run_failed
    assert_run_output "Failed to read include file $SANDBOX/absent.toml"
}

test_include_files_must_be_an_array() {
    printf 'include_files = "not-an-array.toml"\n' > "$SANDBOX/main.toml"
    run_filectrl --print-keybindings -c "$SANDBOX/main.toml"
    assert_run_failed
    assert_run_output "'include_files' must be an array of file paths"
}

# Two actions on one key would make the second unreachable, so the load fails
# rather than picking a winner.
test_a_key_bound_to_two_actions_is_rejected() {
    printf '[keybindings]\nselect_next = "o"\n' > "$SANDBOX/clash.toml"
    run_filectrl --print-keybindings -c "$SANDBOX/clash.toml"
    assert_run_failed
    assert_run_output "Key 'o' is bound to both"
}

test_a_nonexistent_directory_argument_is_rejected() {
    run_filectrl "$SANDBOX/no_such_directory"
    assert_run_failed
    assert_run_output "Failed to open $SANDBOX/no_such_directory: No such file"
}

test_a_file_as_the_directory_argument_is_rejected() {
    run_filectrl "$SANDBOX/fixtures/a.txt"
    assert_run_failed
    assert_run_output "Cannot open $SANDBOX/fixtures/a.txt: not a directory"
}

# ------------------------------------------------- rebinding, end to end in the TUI

test_a_rebound_key_moves_the_selection() {
    write_select_next "$SANDBOX/rebind.toml" e
    EXTRA_INCLUDES=("$SANDBOX/rebind.toml")
    app_start
    assert_selected "documents/"
    send e
    assert_selected "executables/"
    send e
    assert_selected "file_types/"
}

# The default is not kept as an alias: rebinding moves the action off `j`.
test_rebinding_releases_the_default_key() {
    write_select_next "$SANDBOX/rebind.toml" e
    EXTRA_INCLUDES=("$SANDBOX/rebind.toml")
    app_start
    send j # now unbound, so this must do nothing
    send e # and this must land on row 2, not row 3
    assert_selected "executables/"
    assert_running
}

run_tests
