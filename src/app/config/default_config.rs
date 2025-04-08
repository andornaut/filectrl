pub(super) const DEFAULT_CONFIG_TOML: &str = r##"
# Whether to apply $LS_COLORS on top of any styles configured in [theme.file_types]
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
alert_bg = "#373424"
alert_fg = "#9f8800"

alert_error_bg = "#373424"
alert_error_fg = "#DC322F"

alert_info_bg = "#373424"
alert_info_fg = "#73c991"

alert_warning_bg = "#373424"
alert_warning_fg = "#E6DB74"

header_active_bg = "#CCC8B0"
header_active_fg = "#1D1F21"
header_bg = "#373424"
header_fg = "#CCC8B0"

help_bg = "#373424"
help_fg = "#9C9977"

notice_clipboard_bg = "#70C0B1"
notice_clipboard_fg = "#1D1F21"
notice_filter_bg = "#33A999"
notice_filter_fg = "#1D1F21"
notice_progress_bg = "#006B6B"
notice_progress_done_bg = "#373424"
notice_progress_done_fg = "#73c991"
notice_progress_error_bg = "#260908"
notice_progress_error_fg = "#DC322F"
notice_progress_fg = "#70C0B1"

prompt_input_bg = "#373424"
prompt_input_fg = "#CCC8B0"
prompt_label_bg = "#9C9977"
prompt_label_fg = "#1D1F21"

status_directory_bg = "#33A999"
status_directory_fg = "#1D1F21"
status_directory_label_bg = "#006B6B"
status_directory_label_fg = "#C5C8C6"
status_selected_bg = "#33A999"
status_selected_fg = "#1D1F21"
status_selected_label_bg = "#006B6B"
status_selected_label_fg = "#C5C8C6"

table_body_bg = "#373424"
table_body_fg = "#FFFFFF"
table_copied_bg = "#33A999"
table_copied_fg = "#006400"  # Dark Green
table_cut_bg = "#33A999"
table_cut_fg = "#800080"  # Purple
table_header_active_bg = "#9C9977"
table_header_active_fg = "#1D1F21"
table_header_bg = "#777755"
table_header_fg = "#1D1F21"
table_scrollbar_begin_bg = "#777755"
# Whether to show the up/down arrows at the beginning and end of the scrollbar
table_scrollbar_begin_end_enabled = false
table_scrollbar_begin_fg = "#373424"
table_scrollbar_end_bg = "#777755"
table_scrollbar_end_fg = "#373424"
table_scrollbar_thumb_bg = "#373424"
table_scrollbar_thumb_fg = "#CCC8B0"
table_scrollbar_track_bg = "#423F2E"
table_scrollbar_track_fg = "#777755"
table_selected_bg = "#CCC8B0"
table_selected_fg = "#1D1F21"

[theme.file_modified_date]
less_than_minute_bg = ""
less_than_minute_fg = "#00FFFF"  # Sky Blue
less_than_minute_modifiers = []
less_than_day_bg = ""
less_than_day_fg = "#00FF00"    # Bright Green
less_than_day_modifiers = []
less_than_month_bg = ""
less_than_month_fg = "#FFFF00"  # Yellow
less_than_month_modifiers = []
less_than_year_bg = ""
less_than_year_fg = "#FF00FF"   # Magenta
less_than_year_modifiers = []
greater_than_year_bg = ""
greater_than_year_fg = "#FF0000" # Red
greater_than_year_modifiers = []

[theme.file_sizes]
bytes_bg = ""
bytes_fg = "#87CEEB"  # Sky Blue
bytes_modifiers = []
kib_bg = ""
kib_fg = "#00FFFF"    # Cyan
kib_modifiers = []
mib_bg = ""
mib_fg = "#00FF00"    # Bright Green
mib_modifiers = []
gib_bg = ""
gib_fg = "#FFFF00"    # Yellow
gib_modifiers = []
tib_bg = ""
tib_fg = "#FF00FF"    # Magenta
tib_modifiers = []
pib_bg = ""
pib_fg = "#FF0000"    # Red
pib_modifiers = []

[theme.file_types]
# n.b. When the top-level option `apply_ls_colors` is set to true, these options
# are superceded by the $LS_COLORS environment variable
# Using Solarized 256-dark theme colors

# Normal file default (rs=0)
normal_file_bg = ""
# Solarized 256-dark: normal_file_fg = "#808080"  # 244
normal_file_fg = "#E4E4E4"  # 254
normal_file_modifiers = []

# Regular file (fi)
regular_file_bg = ""
# Solarized 256-dark: regular_file_fg = "#808080"  # 244
regular_file_fg = "#E4E4E4"  # 254
regular_file_modifiers = []

# Directory (di=00;38;5;33)
directory_bg = ""
directory_fg = "#0087FF"  # 33
directory_modifiers = []

# Other-writable directory (ow=48;5;235;38;5;33)
directory_other_writable_bg = "#262626"  # 235
directory_other_writable_fg = "#0087FF"  # 33
directory_other_writable_modifiers = []

# Symlink (ln=01;38;5;37)
symlink_bg = ""
symlink_fg = "#00AFAF"  # 37
symlink_modifiers = ["bold"]

# Pipe/FIFO (pi=48;5;230;38;5;136;01)
pipe_bg = "#FFFFD7"  # 230
pipe_fg = "#AF8700"  # 136
pipe_modifiers = ["bold"]

# Socket (so=48;5;230;38;5;136;01)
socket_bg = "#FFFFD7"  # 230
socket_fg = "#AF8700"  # 136
socket_modifiers = ["bold"]

# Door (do=48;5;230;38;5;136;01)
door_bg = "#FFFFD7"  # 230
door_fg = "#AF8700"  # 136
door_modifiers = ["bold"]

# Block device (bd=48;5;230;38;5;244;01)
block_device_bg = "#FFFFD7"  # 230
block_device_fg = "#808080"  # 244
block_device_modifiers = ["bold"]

# Character device (cd=48;5;230;38;5;244;01)
character_device_bg = "#FFFFD7"  # 230
character_device_fg = "#808080"  # 244
character_device_modifiers = ["bold"]

# Broken symlink (or=48;5;235;38;5;160)
symlink_broken_bg = "#262626"  # 235
symlink_broken_fg = "#D70000"  # 160
symlink_broken_modifiers = []

# Missing file (mi=00)
missing_bg = ""
missing_fg = ""
missing_modifiers = []

# Executable (ex=01;38;5;64)
executable_bg = ""
executable_fg = "#5F8700"  # 64
executable_modifiers = ["bold"]

# Sticky directory (st=48;5;33;38;5;230)
directory_sticky_bg = "#0087FF"  # 33
directory_sticky_fg = "#FFFFD7"  # 230
directory_sticky_modifiers = []

# Sticky and other-writable directory (tw=48;5;64;38;5;230)
directory_sticky_other_writable_bg = "#5F8700"  # 64
directory_sticky_other_writable_fg = "#FFFFD7"  # 230
directory_sticky_other_writable_modifiers = []

# Setgid (sg=48;5;136;38;5;230)
setgid_bg = "#AF8700"  # 136
setgid_fg = "#FFFFD7"  # 230
setgid_modifiers = []

# Setuid (su=48;5;160;38;5;230)
setuid_bg = "#D70000"  # 160
setuid_fg = "#FFFFD7"  # 230
setuid_modifiers = []
"##;
