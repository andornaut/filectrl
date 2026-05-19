use std::{collections::VecDeque, fs, path::PathBuf, sync::mpsc::Sender, thread};

use log::warn;

use super::path_info::PathInfo;
use crate::command::{Command, progress::CancellationToken};

/// Spawns a background thread that performs a breadth-first, case-insensitive
/// name search starting from `root`. Each matching entry is sent as a
/// `Command::SearchResult` through the channel. A `Command::ExitedSearch`
/// is sent when the traversal finishes (or is cancelled).
pub fn run_search(root: PathInfo, query: String, tx: Sender<Command>, cancel: CancellationToken) {
    thread::spawn(move || {
        let query_lower = query.to_lowercase();
        let root_path = PathBuf::from(&root.path);
        let mut queue: VecDeque<PathBuf> = VecDeque::new();
        queue.push_back(root_path.clone());

        while let Some(dir) = queue.pop_front() {
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

                if name.to_lowercase().contains(&query_lower) {
                    if let Ok(path_info) = PathInfo::try_from(entry_path.as_path()) {
                        if tx.send(Command::SearchResult(path_info)).is_err() {
                            return;
                        }
                    }
                }

                // Enqueue directories for BFS traversal (don't follow symlinks)
                if let Ok(metadata) = entry_path.symlink_metadata() {
                    if metadata.is_dir() {
                        queue.push_back(entry_path);
                    }
                }
            }
        }

        let _ = tx.send(Command::ExitedSearch);
    });
}
