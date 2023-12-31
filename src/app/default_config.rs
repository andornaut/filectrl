pub(super) const DEFAULT_CONFIG_TOML: &'static str = r##"

# How long to wait to interpret multiple clicks to the same element as a double click
double_click_threshold_milliseconds = 300

# One of: off, error, warn, info, debug, or trace
# Logs are written to stderr
log_level = "off"

# Programs to use to open files or directories:
# %s will be replaced by the current directory path:
open_current_directory_template = "alacritty --working-directory %s"
open_new_window_template = "alacritty --command filectrl %s"
# %s will be replaced by the selected file or directory path:
open_selected_file_template = "pcmanfm %s"

[theme]
error_bg = "#373424"
error_fg = "#DC322F"

header_active_bg = "#CCC8B0"
header_active_fg = "#1D1F21"
header_bg = "#373424"
header_fg = "#CCC8B0"

help_bg = "#373424"
help_fg = "#CCC8B0"

prompt_input_bg = "#373424"
prompt_input_fg = "#CCC8B0"
prompt_label_bg = "#9C9977"
prompt_label_fg = "#1D1F21"

status_clipboard_bg = "#70C0B1"
status_clipboard_fg = "#1D1F21"
status_directory_bg = "#33A999"
status_directory_fg = "#1D1F21"
status_directory_label_bg = "#006B6B"
status_directory_label_fg = "#C5C8C6"
status_filter_bg = "#33A999"
status_filter_fg = "#1D1F21"
status_progress_bg = "#373424"
status_progress_fg = "#70C0B1"
status_progress_done_bg = "#373424"
status_progress_done_fg = "#73c991"
status_progress_error_bg = "#260908"
status_progress_error_fg = "#DC322F"
status_selected_bg = "#33A999"
status_selected_fg = "#1D1F21"
status_selected_label_bg = "#006B6B"
status_selected_label_fg = "#C5C8C6"

table_header_active_bg = "#9C9977"
table_header_active_fg = "#1D1F21"
table_header_bg = "#777755"
table_header_fg = "#1D1F21"

table_name_block_device_bg = "#81A2BE"
table_name_block_device_fg = "#F0C674"
table_name_character_device_bg = "#81A2BE"
table_name_character_device_fg = "#C5C8C6"
table_name_directory_bg = "#423F2E"
table_name_directory_fg = "#81A2BE"
table_name_fifo_bg = "#81A2BE"
table_name_fifo_fg = "#1D1F21"
table_name_file_bg = "#423F2E"
table_name_file_fg = "#C5C8C6"
table_name_setgid_bg = "#F0C674"
table_name_setgid_fg = "#1D1F21"
table_name_setuid_bg = "#CC6666"
table_name_setuid_fg = "#C5C8C6"
table_name_socket_bg = "#81A2BE"
table_name_socket_fg = "#B294BB"
table_name_sticky_bg = "#81A2BE"
table_name_sticky_fg = "#C5C8C6"
table_name_symlink_bg = "#423F2E"
table_name_symlink_fg = "#B294BB"
table_name_symlink_broken_bg = "#cc6666"
table_name_symlink_broken_fg = "#C5C8C6"

table_scrollbar_thumb_bg = "#373424"
table_scrollbar_thumb_fg = "#CCC8B0"
table_scrollbar_track_bg = "#423F2E"
table_scrollbar_track_fg = "#777755"

table_selected_bg = "#CCC8B0"
table_selected_fg = "#1D1F21"
"##;
