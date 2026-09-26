//! A paste's refusal and failure messages.

use std::path::Path;

use super::path_info::compact;

/// Refusal for a name an earlier source of the same paste took.
pub(super) fn same_name_refusal(is_move: bool, source: &Path, dest_dir: &Path) -> String {
    format!(
        "Cannot {} {} into {}: another source in this paste already takes that name there",
        verb(is_move),
        compact(source),
        compact(dest_dir)
    )
}

/// Refusal for a destination that is another name of the same file.
pub(super) fn same_file_refusal(is_move: bool, source: &Path, destination: &Path) -> String {
    format!(
        "Cannot {} {} to {}: they are the same file",
        verb(is_move),
        compact(source),
        compact(destination)
    )
}

/// A copy or move failure reported by the system.
pub(super) fn failed_transfer(
    is_move: bool,
    source: &Path,
    destination: &Path,
    error: &dyn std::fmt::Display,
) -> String {
    format!(
        "Failed to {} {} to {}: {error}",
        verb(is_move),
        compact(source),
        compact(destination)
    )
}

pub(super) fn verb(is_move: bool) -> &'static str {
    if is_move { "move" } else { "copy" }
}
