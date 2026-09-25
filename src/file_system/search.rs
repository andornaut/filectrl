use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
};

use log::warn;

use super::{
    path_info::{PathInfo, compact},
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

/// "1 result" / "2 results". Both bounds are configurable, so either case is
/// reachable. `views::unicode::pluralize_items` is the same idea one layer up,
/// and stays there: this module is below the views.
fn plural(count: u32, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Sends an alert about the traversal itself, unless a newer search has
/// already superseded this one: the user would read a late alert as
/// describing their current search rather than the abandoned one.
///
/// The token is only ever set, never cleared, so a search that observes itself
/// cancelled here stays cancelled for the rest of its walk.
fn alert_unless_superseded(tx: &Sender<Command>, cancel: &CancellationToken, alert: Command) {
    if cancel.is_cancelled() {
        return;
    }
    let _ = tx.send(alert);
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
    let mut unreadable: u32 = 0;

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
            // A root that cannot be read leaves nothing searched, which the
            // count of skipped subdirectories would understate.
            Err(e) if depth == 0 => {
                let message = format!("Failed to search {}: {e}", compact(&dir));
                warn!("{message}");
                alert_unless_superseded(tx, cancel, Command::AlertError(message));
                continue;
            }
            Err(e) => {
                warn!("Failed to search {}: {e}", compact(&dir));
                if counts_as_unreadable(&e) {
                    unreadable += 1;
                }
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
                    warn!("Failed to search an entry of {}: {e}", compact(&dir));
                    continue;
                }
            };

            let entry_path = entry.path();
            // The name as its row shows it, which is also what the filter reads.
            let file_name = entry.file_name();
            let name = crate::visible_os(&file_name);

            if crate::contains_ignore_case(&name, &query_lower)
                && let Ok(path_info) = PathInfo::try_from(entry_path.as_path())
            {
                if result_count >= limits.max_results {
                    alert_unless_superseded(
                        tx,
                        cancel,
                        Command::AlertWarn(format!(
                            "Search stopped at {}",
                            plural(limits.max_results, "result")
                        )),
                    );
                    warn_unreadable(tx, cancel, unreadable);
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
                    alert_unless_superseded(
                        tx,
                        cancel,
                        Command::AlertWarn(format!(
                            "Search reached maximum depth of {}; some results may be missing",
                            plural(limits.max_depth, "level")
                        )),
                    );
                }
            }
        }
    }

    warn_unreadable(tx, cancel, unreadable);
    exit(&mut batcher);
}

/// Whether a directory below the root the walk failed to read counts towards
/// the unreadable warning. One removed, or replaced by a file, after it was
/// queued hid nothing that still exists there.
fn counts_as_unreadable(error: &std::io::Error) -> bool {
    !matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// Reports the directories the walk could not read, once when it ends, like
/// the depth warning: one per directory would bury the listing under repeats.
fn warn_unreadable(tx: &Sender<Command>, cancel: &CancellationToken, unreadable: u32) {
    if unreadable == 0 {
        return;
    }
    let directories = if unreadable == 1 {
        "1 directory".to_string()
    } else {
        format!("{unreadable} directories")
    };
    alert_unless_superseded(
        tx,
        cancel,
        Command::AlertWarn(format!(
            "{directories} could not be read; some results may be missing"
        )),
    );
}

#[cfg(test)]
mod tests {
    use std::{io::ErrorKind, sync::mpsc};

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

    fn errors(commands: &[Command]) -> Vec<String> {
        commands
            .iter()
            .filter_map(|command| match command {
                Command::AlertError(message) => Some(message.clone()),
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

        // Mixed case, so lowercasing only one side cannot match.
        let (commands, _) = run(&default_limits(), &root, "eAdM");

        assert_eq!(vec!["README.md".to_string()], matched_names(&commands));
    }

    // Linux only: APFS refuses a name that is not valid UTF-8.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_name_that_is_not_utf8_is_found_by_the_text_its_row_shows() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let root = TempDir::new("search_not_utf8");
        std::fs::write(root.join(OsStr::from_bytes(b"caf\xe9.txt")), b"").unwrap();
        std::fs::write(root.join("cafe.txt"), b"").unwrap();

        // The row shows the byte spelled out as `\xe9`, so that is what the
        // user types. A lossy conversion turns it into U+FFFD instead.
        let (commands, _) = run(&default_limits(), &root, "CAF\\XE9");

        assert_eq!(vec!["caf\\xe9.txt".to_string()], matched_names(&commands));
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
        // A second directory past the limit, beside level2.
        std::fs::create_dir(root.join("level0/level1/sibling")).unwrap();
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

    /// The shipped bounds are 20 and 10,000, so the singular reads only under
    /// a configured limit of one, which is where a hardcoded plural shows.
    #[test]
    fn a_limit_of_one_is_reported_in_the_singular() {
        let root = TempDir::new("search_singular_results");
        for i in 0..3 {
            std::fs::write(root.join(format!("hit{i}")), b"").unwrap();
        }
        let limits = Limits {
            max_results: 1,
            ..default_limits()
        };

        let (commands, _) = run(&limits, &root, "hit");

        assert_eq!(
            vec!["Search stopped at 1 result".to_string()],
            warnings(&commands)
        );
    }

    #[test]
    fn a_depth_of_one_is_reported_in_the_singular() {
        let root = nested_tree("search_singular_depth", 3);
        let limits = Limits {
            max_depth: 1,
            ..default_limits()
        };

        let (commands, _) = run(&limits, &root, "hit");

        assert_eq!(
            vec![
                "Search reached maximum depth of 1 level; some results may be missing".to_string()
            ],
            warnings(&commands)
        );
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

    /// Both warnings describe the walk rather than a result, so a search
    /// superseded mid-walk must stay silent: its warning would read as
    /// describing the search now on screen. Cancellation lands between the walk's
    /// own checks, which a synchronous test cannot schedule, so the rule is
    /// pinned on the helper both call sites route through.
    #[test]
    fn a_superseded_search_announces_no_warning() {
        let (tx, rx) = mpsc::channel();
        let cancel = CancellationToken::new();

        alert_unless_superseded(&tx, &cancel, Command::AlertWarn("live".to_string()));
        cancel.cancel();
        alert_unless_superseded(&tx, &cancel, Command::AlertWarn("superseded".to_string()));
        drop(tx);

        let commands: Vec<Command> = rx.into_iter().collect();
        assert_eq!(vec!["live".to_string()], warnings(&commands));
        assert_eq!(1, commands.len(), "nothing else may be sent: {commands:?}");
    }

    /// A directory below the root deleted or replaced after it was queued hides
    /// nothing; any other failure to read one does.
    #[test_case::test_case(ErrorKind::NotFound => false ; "a subdirectory removed")]
    #[test_case::test_case(ErrorKind::NotADirectory => false ; "a subdirectory replaced by a file")]
    #[test_case::test_case(ErrorKind::PermissionDenied => true ; "a subdirectory that is locked")]
    fn a_failed_read_counts_as_unreadable(kind: ErrorKind) -> bool {
        counts_as_unreadable(&std::io::Error::from(kind))
    }

    /// A search of a directory that is gone, or is not one, found nothing and
    /// says so, naming it, rather than counting it with the subdirectories.
    #[test]
    fn a_root_that_cannot_be_read_is_reported() {
        let root = TempDir::new("search_gone");
        let file = root.join("file");
        std::fs::write(&file, b"").unwrap();

        for gone in [root.join("missing"), file] {
            let (tx, rx) = mpsc::channel();
            search(
                &default_limits(),
                &tx,
                &CancellationToken::new(),
                &gone,
                "x",
                GENERATION,
            );
            drop(tx);
            let commands: Vec<Command> = rx.into_iter().collect();
            let cause = std::fs::read_dir(&gone).unwrap_err();
            assert_eq!(
                vec![format!("Failed to search {}: {cause}", compact(&gone))],
                errors(&commands),
                "{gone:?}"
            );
            assert!(warnings(&commands).is_empty(), "{commands:?}");
            assert_eq!(1, exits(&commands));
        }
    }

    /// A search cancelled before it starts sends only its exit, whatever its
    /// root: it reads nothing, so it has nothing to report. The root's own
    /// failure is checked against a cancel only in the branch after
    /// `read_dir`, which a cancel before the walk never reaches.
    #[test]
    fn a_search_cancelled_before_it_starts_sends_only_its_exit() {
        let root = TempDir::new("search_gone_superseded");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (tx, rx) = mpsc::channel();

        search(
            &default_limits(),
            &tx,
            &cancel,
            &root.join("missing"),
            "x",
            GENERATION,
        );
        drop(tx);

        let commands: Vec<Command> = rx.into_iter().collect();
        assert_eq!(
            vec![Command::ExitedSearch {
                generation: GENERATION
            }],
            commands
        );
    }

    /// Stopping at the result limit still reports what the walk skipped before
    /// it stopped. The hits sit two levels down, so the breadth-first walk
    /// reaches the locked directory before them whatever the readdir order.
    #[test]
    fn a_search_stopped_at_the_limit_still_reports_unreadable_directories() {
        use std::os::unix::fs::PermissionsExt;
        let root = TempDir::new("search_unreadable_limit");
        let hits = root.join("a").join("sub");
        std::fs::create_dir_all(&hits).unwrap();
        for i in 0..3 {
            std::fs::write(hits.join(format!("hit{i}")), b"").unwrap();
        }
        let locked = root.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads any directory, so the fixture proves nothing there.
        let readable = std::fs::read_dir(&locked).is_ok();
        let limits = Limits {
            max_results: 1,
            ..default_limits()
        };

        let commands = (!readable).then(|| run(&limits, &root, "hit").0);

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let Some(commands) = commands else {
            return;
        };
        assert_eq!(
            vec![
                "Search stopped at 1 result".to_string(),
                "1 directory could not be read; some results may be missing".to_string(),
            ],
            warnings(&commands)
        );
        assert_eq!(1, exits(&commands));
    }

    /// Like `find`, a directory the walk cannot read is reported rather than
    /// passed over in silence, once for the whole search.
    #[test_case::test_case(1 ; "one directory")]
    #[test_case::test_case(2 ; "several directories")]
    fn unreadable_directories_are_counted_in_one_warning(count: usize) {
        use std::os::unix::fs::PermissionsExt;
        let root = TempDir::new("search_unreadable");
        let locked: Vec<_> = (0..count)
            .map(|i| root.join(format!("locked{i}")))
            .collect();
        for dir in &locked {
            std::fs::create_dir(dir).unwrap();
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        // Root reads any directory, so the fixture proves nothing there.
        let readable = std::fs::read_dir(&locked[0]).is_ok();

        let commands = (!readable).then(|| run(&default_limits(), &root, "x").0);

        for dir in &locked {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let Some(commands) = commands else {
            return;
        };
        let directories = if count == 1 {
            "1 directory"
        } else {
            "2 directories"
        };
        assert_eq!(
            vec![format!(
                "{directories} could not be read; some results may be missing"
            )],
            warnings(&commands)
        );
        assert_eq!(1, exits(&commands));
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
