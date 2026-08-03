use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
};

use log::warn;

use super::{
    path_info::PathInfo,
    stream::{BATCH_FLUSH_INTERVAL, Batcher, batch_sender},
};
use crate::command::{Command, progress::CancellationToken};

const SEARCH_BATCH_SIZE: usize = 128;

/// Bounds on a single traversal, configured under `[file_system]`. Injected
/// rather than read from constants so that the limit behaviour can be exercised
/// without building a tree of `max_results` entries.
pub(super) struct Limits {
    pub(super) max_depth: u32,
    pub(super) max_results: u32,
}

/// Spawns a background thread that performs a breadth-first, case-insensitive
/// name search starting from `root`. Matching entries are sent in batches as
/// `Command::ListingBatch` through the channel. A `Command::ExitedSearch`
/// is sent when the traversal finishes (or is cancelled).
pub(super) fn run_search(
    limits: Limits,
    tx: Sender<Command>,
    cancel: CancellationToken,
    root: PathInfo,
    query: String,
    generation: u64,
) {
    thread::spawn(move || {
        search(&limits, &tx, &cancel, &root.path, &query, generation);
    });
}

/// Sends a warning about the traversal itself, unless a newer search has
/// already superseded this one: the user would read a late warning as
/// describing their current search rather than the abandoned one.
///
/// The token is only ever set, never cleared, so a search that observes itself
/// cancelled here stays cancelled for the rest of its walk.
fn warn_unless_superseded(tx: &Sender<Command>, cancel: &CancellationToken, message: String) {
    if cancel.is_cancelled() {
        return;
    }
    let _ = tx.send(Command::AlertWarn(message));
}

/// The traversal itself, run on the caller's thread.
fn search(
    limits: &Limits,
    tx: &Sender<Command>,
    cancel: &CancellationToken,
    root: &Path,
    query: &str,
    generation: u64,
) {
    let query_lower = query.to_lowercase();
    let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), 0));
    let mut depth_limit_hit = false;
    let mut result_count: u32 = 0;

    let send = batch_sender(tx, generation);
    let mut batcher = Batcher::new(SEARCH_BATCH_SIZE, BATCH_FLUSH_INTERVAL);
    // Every exit path flushes pending hits and self-cancels before
    // announcing the exit: the cancel stack treats a cancelled token as
    // "nothing left to cancel", so a cancel keypress racing the
    // in-flight exit cannot relabel a completed search as cancelled.
    let exit = |batcher: &mut Batcher| {
        let _ = batcher.flush(&send);
        cancel.cancel();
        let _ = tx.send(Command::ExitedSearch { generation });
    };

    while let Some((dir, depth)) = queue.pop_front() {
        if cancel.is_cancelled() {
            exit(&mut batcher);
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
                exit(&mut batcher);
                return;
            }

            // Time-based flush so sparse matches still stream to the UI.
            if !batcher.flush_if_due(&send) {
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
            {
                if result_count >= limits.max_results {
                    warn_unless_superseded(
                        tx,
                        cancel,
                        format!("Search stopped at {} results", limits.max_results),
                    );
                    exit(&mut batcher);
                    return;
                }
                result_count += 1;
                if !batcher.push(path_info, &send) {
                    return;
                }
            }

            // Enqueue directories for BFS traversal. `file_type` does not
            // follow symlinks (so a link to a directory is not descended
            // into) and is usually free from the readdir data, unlike a
            // per-entry stat.
            if let Ok(file_type) = entry.file_type()
                && file_type.is_dir()
            {
                let next_depth = depth + 1;
                if next_depth <= limits.max_depth {
                    queue.push_back((entry_path, next_depth));
                } else if !depth_limit_hit {
                    // Warn once however many directories are turned away, so a
                    // wide tree cannot bury the listing under repeats.
                    depth_limit_hit = true;
                    warn_unless_superseded(
                        tx,
                        cancel,
                        format!(
                            "Search reached maximum depth of {} levels; some results may be missing",
                            limits.max_depth
                        ),
                    );
                }
            }
        }
    }

    exit(&mut batcher);
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::test_support::TempDir;

    const GENERATION: u64 = 7;

    /// The shipped limits, from `default_config.toml`.
    fn default_limits() -> Limits {
        Limits {
            max_depth: 20,
            max_results: 10_000,
        }
    }

    /// Runs a search to completion on this thread and returns everything it
    /// sent, so each test can assert on the whole conversation.
    fn run(limits: &Limits, root: &TempDir, query: &str) -> (Vec<Command>, CancellationToken) {
        let (tx, rx) = mpsc::channel();
        let cancel = CancellationToken::new();
        search(limits, &tx, &cancel, root.path(), query, GENERATION);
        drop(tx);
        (rx.into_iter().collect(), cancel)
    }

    /// The display names of every entry that reached the table, in batch order.
    fn matched_names(commands: &[Command]) -> Vec<String> {
        commands
            .iter()
            .flat_map(|command| match command {
                Command::ListingBatch { items, generation } => {
                    assert_eq!(GENERATION, *generation, "batches must carry the generation");
                    items.clone()
                }
                _ => Vec::new(),
            })
            .map(|info| info.display_name)
            .collect()
    }

    fn warnings(commands: &[Command]) -> Vec<String> {
        commands
            .iter()
            .filter_map(|command| match command {
                Command::AlertWarn(message) => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    fn exits(commands: &[Command]) -> usize {
        commands
            .iter()
            .filter(|command| matches!(command, Command::ExitedSearch { .. }))
            .count()
    }

    /// A chain of nested directories, each holding one file named `hit`.
    fn nested_tree(label: &str, depth: u32) -> TempDir {
        let root = TempDir::new(label);
        let mut path = root.path().to_path_buf();
        for level in 0..depth {
            path = path.join(format!("level{level}"));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("hit"), b"").unwrap();
        }
        root
    }

    #[test]
    fn matching_is_case_insensitive_and_matches_a_substring() {
        let root = TempDir::new("search_match");
        std::fs::write(root.join("README.md"), b"").unwrap();
        std::fs::write(root.join("notes.txt"), b"").unwrap();

        let (commands, _) = run(&default_limits(), &root, "eadm");

        assert_eq!(vec!["README.md".to_string()], matched_names(&commands));
    }

    #[test]
    fn a_finished_search_announces_exactly_one_exit() {
        let root = TempDir::new("search_exit");
        std::fs::write(root.join("a"), b"").unwrap();

        let (commands, cancel) = run(&default_limits(), &root, "a");

        assert_eq!(1, exits(&commands));
        // The exit self-cancels so that a cancel keypress racing it finds
        // nothing left to cancel and cannot relabel a completed search.
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn results_below_the_depth_limit_are_found_without_a_warning() {
        // max_depth 3 admits level0/level1/level2, which is where the
        // deepest `hit` lives.
        let root = nested_tree("search_depth_ok", 3);
        let limits = Limits {
            max_depth: 3,
            ..default_limits()
        };

        let (commands, _) = run(&limits, &root, "hit");

        assert_eq!(3, matched_names(&commands).len());
        assert!(warnings(&commands).is_empty(), "{:?}", warnings(&commands));
    }

    #[test]
    fn the_depth_limit_stops_the_descent_and_warns_once() {
        let root = nested_tree("search_depth_hit", 4);
        let limits = Limits {
            max_depth: 2,
            ..default_limits()
        };

        let (commands, _) = run(&limits, &root, "hit");

        // Only the two admitted levels are searched; the rest are unreachable.
        assert_eq!(2, matched_names(&commands).len());
        // One warning however many directories were turned away, so a wide
        // tree cannot bury the listing under repeats of the same alert.
        let warnings = warnings(&commands);
        assert_eq!(1, warnings.len(), "{warnings:?}");
        assert!(warnings[0].contains("maximum depth of 2"), "{warnings:?}");
    }

    #[test]
    fn the_result_limit_truncates_the_search_and_warns() {
        let root = TempDir::new("search_result_limit");
        for i in 0..5 {
            std::fs::write(root.join(format!("hit{i}")), b"").unwrap();
        }
        let limits = Limits {
            max_results: 2,
            ..default_limits()
        };

        let (commands, _) = run(&limits, &root, "hit");

        assert_eq!(2, matched_names(&commands).len());
        let warnings = warnings(&commands);
        assert_eq!(1, warnings.len(), "{warnings:?}");
        assert!(warnings[0].contains("stopped at 2 results"), "{warnings:?}");
        // Truncating is still a normal finish, so the consumers' search state
        // is unwound exactly once.
        assert_eq!(1, exits(&commands));
    }

    #[test]
    fn a_search_cancelled_before_it_starts_yields_no_results() {
        let root = TempDir::new("search_cancelled");
        std::fs::write(root.join("hit"), b"").unwrap();
        let (tx, rx) = mpsc::channel();
        let cancel = CancellationToken::new();
        cancel.cancel();

        search(
            &default_limits(),
            &tx,
            &cancel,
            root.path(),
            "hit",
            GENERATION,
        );
        drop(tx);
        let commands: Vec<Command> = rx.into_iter().collect();

        assert!(matched_names(&commands).is_empty());
        // The exit still fires: it is what clears the consumers' search state.
        assert_eq!(1, exits(&commands));
    }

    /// The depth and truncation warnings both describe the walk rather than a
    /// result, so a search that has been superseded mid-walk must stay silent:
    /// its warning would be read as describing the search now on screen.
    ///
    /// Cancellation lands between the walk's own cancel checks, which a
    /// synchronous test cannot schedule, so the rule is pinned here on the
    /// helper both call sites route through.
    #[test]
    fn a_superseded_search_announces_no_warning() {
        let (tx, rx) = mpsc::channel();
        let cancel = CancellationToken::new();

        warn_unless_superseded(&tx, &cancel, "live".to_string());
        cancel.cancel();
        warn_unless_superseded(&tx, &cancel, "superseded".to_string());
        drop(tx);

        let commands: Vec<Command> = rx.into_iter().collect();
        assert_eq!(vec!["live".to_string()], warnings(&commands));
        assert_eq!(1, commands.len(), "nothing else may be sent: {commands:?}");
    }

    #[test]
    fn a_symlinked_directory_is_not_descended_into() {
        let root = TempDir::new("search_symlink");
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("hit"), b"").unwrap();
        // Descending through links would revisit the tree and, for a link to
        // an ancestor, never terminate.
        std::os::unix::fs::symlink(&real, root.join("link")).unwrap();

        let (commands, _) = run(&default_limits(), &root, "hit");

        assert_eq!(vec!["hit".to_string()], matched_names(&commands));
    }
}
