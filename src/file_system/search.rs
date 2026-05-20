use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
    sync::mpsc::Sender,
    thread,
    time::{Duration, Instant},
};

use log::warn;

use super::path_info::PathInfo;
use crate::command::{Command, progress::CancellationToken};

const MAX_SEARCH_DEPTH: u32 = 20;
const MAX_SEARCH_RESULTS: u32 = 10_000;

// Search hits are batched rather than sent one command per match: a flood of
// individual commands would sit ahead of terminal input in the single FIFO
// command channel, making the UI unresponsive mid-search. A batch is flushed
// once it reaches SEARCH_BATCH_SIZE or SEARCH_FLUSH_INTERVAL has elapsed
// (whichever comes first), so results still stream visibly when matches are
// sparse among many directories.
const SEARCH_BATCH_SIZE: usize = 128;
const SEARCH_FLUSH_INTERVAL: Duration = Duration::from_millis(80);

/// Sends the accumulated batch (if any) as a single `Command::SearchResults`.
/// Returns `false` if the channel is closed, signalling the caller to stop.
fn flush_batch(tx: &Sender<Command>, batch: &mut Vec<PathInfo>) -> bool {
    if batch.is_empty() {
        return true;
    }
    tx.send(Command::SearchResults(std::mem::take(batch)))
        .is_ok()
}

/// Spawns a background thread that performs a breadth-first, case-insensitive
/// name search starting from `root`. Matching entries are sent in batches as
/// `Command::SearchResults` through the channel. A `Command::ExitedSearch`
/// is sent when the traversal finishes (or is cancelled).
pub fn run_search(root: PathInfo, query: String, tx: Sender<Command>, cancel: CancellationToken) {
    thread::spawn(move || {
        let query_lower = query.to_lowercase();
        let root_path = root.path.clone();
        let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
        queue.push_back((root_path.clone(), 0));
        let mut depth_limit_hit = false;
        let mut result_count: u32 = 0;

        let mut batch: Vec<PathInfo> = Vec::new();
        let mut last_flush = Instant::now();

        while let Some((dir, depth)) = queue.pop_front() {
            if cancel.is_cancelled() {
                let _ = flush_batch(&tx, &mut batch);
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
                    let _ = flush_batch(&tx, &mut batch);
                    let _ = tx.send(Command::ExitedSearch);
                    return;
                }

                // Time-based flush so sparse matches still stream to the UI.
                if last_flush.elapsed() >= SEARCH_FLUSH_INTERVAL {
                    if !flush_batch(&tx, &mut batch) {
                        return;
                    }
                    last_flush = Instant::now();
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
                {
                    if result_count >= MAX_SEARCH_RESULTS {
                        let _ = flush_batch(&tx, &mut batch);
                        let _ = tx.send(Command::AlertWarn(format!(
                            "Search stopped at {MAX_SEARCH_RESULTS} results"
                        )));
                        let _ = tx.send(Command::ExitedSearch);
                        return;
                    }
                    result_count += 1;
                    batch.push(path_info);
                    if batch.len() >= SEARCH_BATCH_SIZE {
                        if !flush_batch(&tx, &mut batch) {
                            return;
                        }
                        last_flush = Instant::now();
                    }
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

        let _ = flush_batch(&tx, &mut batch);
        let _ = tx.send(Command::ExitedSearch);
    });
}
