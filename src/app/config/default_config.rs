pub(super) const DEFAULT_CONFIG_TOML: &'static str = r##"
# Whether to apply $LS_COLORS on top of any styles configured in [theme.files]
apply_ls_colors = true

# How long to wait to interpret multiple clicks to the same element as a double click
double_click_threshold_milliseconds = 300

# One of: off, error, warn, info, debug, or trace
# Logs are written to stderr
log_level = "off"

# Programs to use to open files or directories:
# %s will be replaced by the path to the current working directory:
open_current_directory_template = "alacritty --working-directory %s"
open_new_window_template = "alacritty --command filectrl %s"
# %s will be replaced by the path to the selected file or directory:
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

table_body_bg = "#373424"
table_body_fg = "#FFFFFF"
table_header_active_bg = "#9C9977"
table_header_active_fg = "#1D1F21"
table_header_bg = "#777755"
table_header_fg = "#1D1F21"
# Whether to show the up/down arrows at the beginning and end of the scrollbar
table_scrollbar_begin_end_enabled = false
table_scrollbar_begin_bg = "#777755"
table_scrollbar_begin_fg = "#373424"
table_scrollbar_end_bg = "#777755"
table_scrollbar_end_fg = "#373424"
table_scrollbar_thumb_bg = "#373424"
table_scrollbar_thumb_fg = "#CCC8B0"
table_scrollbar_track_bg = "#423F2E"
table_scrollbar_track_fg = "#777755"
table_selected_bg = "#CCC8B0"
table_selected_fg = "#1D1F21"

# Size unit colors
size_bytes_bg = "#373424"
size_bytes_fg = "#87CEEB"  # Sky Blue
size_bytes_modifiers = []
size_kib_bg = "#373424"
size_kib_fg = "#00FFFF"    # Cyan
size_kib_modifiers = []
size_mib_bg = "#373424"
size_mib_fg = "#00FF00"    # Bright Green
size_mib_modifiers = []
size_gib_bg = "#373424"
size_gib_fg = "#FFFF00"    # Yellow
size_gib_modifiers = []
size_tib_bg = "#373424"
size_tib_fg = "#FF00FF"    # Magenta
size_tib_modifiers = []
size_pib_bg = "#373424"
size_pib_fg = "#FF0000"    # Red
size_pib_modifiers = []

[theme.files]
# From https://raw.githubusercontent.com/seebi/dircolors-solarized/refs/heads/master/dircolors.ansi-dark

# Normal file default (rs=0)
normal_file_bg = ""
normal_file_fg = ""
normal_file_modifiers = []

# Regular file (fi)
regular_file_bg = ""
regular_file_fg = ""
regular_file_modifiers = []

# Directory (di=01;34)
directory_bg = ""
directory_fg = "#0000FF"
directory_modifiers = ["bold"]

# Other-writable directory (ow=34;42)
directory_other_writable_bg = "#00FF00"
directory_other_writable_fg = "#0000FF"
directory_other_writable_modifiers = []

# Symlink (ln=01;36)
symlink_bg = ""
symlink_fg = "#00FFFF"
symlink_modifiers = ["bold"]

# Pipe/FIFO (pi=40;33)
pipe_bg = "#FFFF00"
pipe_fg = "#000000"
pipe_modifiers = []

# Socket (so=01;35)
socket_bg = ""
socket_fg = "#FF00FF"
socket_modifiers = ["bold"]

# Door (do=01;35)
door_bg = ""
door_fg = "#FF00FF"
door_modifiers = ["bold"]

# Block device (bd=40;33;01)
block_device_bg = "#FFFF00"
block_device_fg = "#000000"
block_device_modifiers = ["bold"]

# Character device (cd=40;33;01)
character_device_bg = "#FFFF00"
character_device_fg = "#000000"
character_device_modifiers = ["bold"]

# Broken symlink (or=40;31;01)
symlink_broken_bg = "#FF0000"
symlink_broken_fg = "#000000"
symlink_broken_modifiers = ["bold"]

# Missing file (mi=00)
missing_bg = ""
missing_fg = ""
missing_modifiers = []

# Executable (ex=01;32)
executable_bg = ""
executable_fg = "#00FF00"
executable_modifiers = ["bold"]

# Sticky directory (st=37;44)
directory_sticky_bg = "#0000FF"
directory_sticky_fg = "#FFFFFF"
directory_sticky_modifiers = []

# Sticky and other-writable directory (tw=30;42)
directory_sticky_other_writable_bg = "#00FF00"
directory_sticky_other_writable_fg = "#000000"
directory_sticky_other_writable_modifiers = []

# Setgid (sg=30;43)
setgid_bg = "#FFFF00"
setgid_fg = "#000000"
setgid_modifiers = []

# Setuid (su=37;41)
setuid_bg = "#FF0000"
setuid_fg = "#FFFFFF"
setuid_modifiers = []

# Pattern-based styles
"##;
