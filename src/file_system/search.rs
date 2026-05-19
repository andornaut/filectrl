use std::{collections::VecDeque, fs, path::PathBuf, sync::mpsc::Sender, thread};

use log::warn;

use super::path_info::PathInfo;
use crate::command::{Command, progress::CancellationToken};

const MAX_SEARCH_DEPTH: u32 = 20;

/// Spawns a background thread that performs a breadth-first, case-insensitive
/// name search starting from `root`. Each matching entry is sent as a
/// `Command::SearchResult` through the channel. A `Command::ExitedSearch`
/// is sent when the traversal finishes (or is cancelled).
pub fn run_search(root: PathInfo, query: String, tx: Sender<Command>, cancel: CancellationToken) {
    thread::spawn(move || {
        let query_lower = query.to_lowercase();
        let root_path = root.path.clone();
        let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
        queue.push_back((root_path.clone(), 0));
        let mut depth_limit_hit = false;

        while let Some((dir, depth)) = queue.pop_front() {
            if cancel.is_cancelled() {
                let _ = tx.send(Command::ExitedSearch);
                return;
            }

            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) => {
                    warn!("Search: failed to read directory {dir:?}: {e}");
                    continue;
                }
            };

            for entry in entries {
                if cancel.is_cancelled() {
                    let _ = tx.send(Command::ExitedSearch);
                    return;
                }

                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        warn!("Search: failed to read entry in {dir:?}: {e}");
                        continue;
                    }
                };

                let entry_path = entry.path();
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();

                if name.to_lowercase().contains(&query_lower)
                    && let Ok(path_info) = PathInfo::try_from(entry_path.as_path())
                    && tx.send(Command::SearchResult(path_info)).is_err()
                {
                    return;
                }

                // Enqueue directories for BFS traversal (don't follow symlinks)
                if let Ok(metadata) = entry_path.symlink_metadata()
                    && metadata.is_dir()
                {
                    let next_depth = depth + 1;
                    if next_depth <= MAX_SEARCH_DEPTH {
                        queue.push_back((entry_path, next_depth));
                    } else if !depth_limit_hit {
                        depth_limit_hit = true;
                        let _ = tx.send(Command::AlertWarn(
                                format!("Search reached maximum depth of {MAX_SEARCH_DEPTH} levels; some results may be missing")
                            ));
                    }
                }
            }
        }

        let _ = tx.send(Command::ExitedSearch);
    });
}
