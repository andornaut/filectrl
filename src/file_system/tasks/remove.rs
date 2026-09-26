use std::{fs, io::ErrorKind, path::Path, time::Instant};

use super::{
    PROGRESS_DEBOUNCE_PERCENTAGE, PROGRESS_MIN_INTERVAL, cancel_logging,
    walk::{list_entries, scan_tree},
};
use crate::{
    command::progress::ActiveTask,
    file_system::{debounce, path_info::compact},
};

/// Best-effort recursive entry count for the delete progress total, including `root`. Reads
/// directories only; an unlistable directory counts as one. `None` when cancelled.
pub(super) fn dir_total_entries(active: &ActiveTask, root: &Path) -> Option<u64> {
    let mut total: u64 = 1; // `root` itself, which appears in no listing.
    scan_tree(active, root, |_| total += 1)?;
    Some(total)
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Removal {
    /// A delete: cancellable between entries, advancing progress per entry.
    Delete,
    /// The source of a cross-device move, past the point a cancel could stop it.
    MovedSource,
}

/// Removes a file or directory tree like `rm -rf`: an entry that cannot be removed is recorded and
/// the walk continues, keeping the directories above it without reporting them. An unlistable
/// empty directory is removed, and an entry already gone counts as removed. Symlinks are removed,
/// never followed.
///
/// Returns the task and the errors, leaving finalization to the caller; `None` when cancelled
/// (already finalized).
pub(super) fn remove_path(
    path: &Path,
    is_directory: bool,
    active: ActiveTask,
    removal: Removal,
) -> Option<(ActiveTask, Vec<String>)> {
    let mut remover = Remover {
        // Debounced so a wide tree does not queue one progress command per entry ahead of
        // terminal input.
        debouncer: debounce::ProgressDebouncer::new(
            PROGRESS_DEBOUNCE_PERCENTAGE,
            PROGRESS_MIN_INTERVAL,
            active.total_size(),
        ),
        active,
        cancellable: removal == Removal::Delete,
        errors: Vec::new(),
    };
    let outcome = if is_directory {
        remover.remove_tree(path)
    } else {
        remover.remove_file(path)
    };
    let Remover { active, errors, .. } = remover;
    if outcome.is_err() {
        cancel_logging(&errors, active);
        return None;
    }
    Some((active, errors))
}

struct Cancelled;

struct Remover {
    active: ActiveTask,
    debouncer: debounce::ProgressDebouncer,
    /// A delete, which a cancel stops and whose progress counts entries. A move's progress measured
    /// bytes and is already complete.
    cancellable: bool,
    errors: Vec<String>,
}

impl Remover {
    fn check_cancelled(&self) -> Result<(), Cancelled> {
        if self.cancellable && self.active.is_cancelled() {
            return Err(Cancelled);
        }
        Ok(())
    }

    /// Counts one entry removed.
    fn advance(&mut self) {
        if self.cancellable {
            self.active.increment(1);
            if self.debouncer.should_trigger(Instant::now(), 1) {
                self.active.send_progress();
            }
        }
    }

    /// Removes what `result` says was removed, or records why not. Returns whether it is gone.
    fn removed(&mut self, path: &Path, result: std::io::Result<()>) -> bool {
        match result {
            Err(error) if error.kind() != ErrorKind::NotFound => {
                self.errors
                    .push(format!("Failed to delete {}: {error}", compact(path)));
                false
            }
            _ => {
                self.advance();
                true
            }
        }
    }

    fn remove_file(&mut self, path: &Path) -> Result<bool, Cancelled> {
        self.check_cancelled()?;
        Ok(self.removed(path, fs::remove_file(path)))
    }

    /// Removes the directory `dir` after everything in it, post-order. Returns whether it is gone:
    /// one holding an unremovable entry is kept without a second report.
    fn remove_tree(&mut self, dir: &Path) -> Result<bool, Cancelled> {
        self.check_cancelled()?;
        let entries = match list_entries(dir) {
            Ok(entries) => entries,
            // An empty directory that cannot be listed is removed anyway, like `rm -rf`.
            Err(error) => {
                if fs::remove_dir(dir).is_ok() {
                    self.advance();
                    return Ok(true);
                }
                if error.kind() == ErrorKind::NotFound {
                    return Ok(true);
                }
                self.errors.push(format!(
                    "Failed to read directory {}: {error}",
                    compact(dir)
                ));
                return Ok(false);
            }
        };
        let mut complete = true;
        for (name, is_directory) in entries {
            let path = dir.join(name);
            complete &= if is_directory {
                self.remove_tree(&path)?
            } else {
                self.remove_file(&path)?
            };
        }
        if !complete {
            return Ok(false);
        }
        Ok(self.removed(dir, fs::remove_dir(dir)))
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, fs, os::unix::fs::PermissionsExt, sync::mpsc};

    use test_case::test_case;

    use super::{
        super::{
            TaskCommand,
            test_support::{copy_task, run_to_end},
        },
        *,
    };
    use crate::{
        command::{Command, progress::TaskKind},
        file_system::path_info::PathInfo,
        test_support::TempDir,
    };

    #[test]
    fn remove_path_deletes_directory_tree() {
        let fx = TempDir::new("tasks");
        let root = fx.join("doomed");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("f.txt"), b"x").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let (active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );

        let (active, errors) = remove_path(&root, true, active, Removal::Delete).unwrap();
        active.done();
        assert!(errors.is_empty(), "{errors:?}");
        assert!(!root.exists());
    }

    #[test]
    fn dir_total_entries_counts_the_root_and_every_descendant() {
        let fx = TempDir::new("tasks");
        let root = fx.join("tree");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"x").unwrap();
        std::fs::write(root.join("sub").join("b.txt"), b"x").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let active = copy_task(tx);

        // root, a.txt, sub, sub/b.txt
        assert_eq!(Some(4), dir_total_entries(&active, &root));
        active.done();
    }

    #[test_case(false ; "a file")]
    #[test_case(true  ; "a symlink to a directory")]
    fn remove_path_unlinks_a_single_entry_without_following_it(is_symlink: bool) {
        let fx = TempDir::new("tasks_delete_single");
        let outside = fx.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep.txt"), b"keep").unwrap();
        let entry = fx.join("entry");
        if is_symlink {
            std::os::unix::fs::symlink(&outside, &entry).unwrap();
        } else {
            fs::write(&entry, b"x").unwrap();
        }
        let (tx, _rx) = mpsc::channel();
        let (active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );

        let (active, errors) = remove_path(&entry, false, active, Removal::Delete).unwrap();
        active.done();
        assert!(errors.is_empty(), "{errors:?}");

        assert!(entry.symlink_metadata().is_err());
        assert_eq!(
            b"keep".to_vec(),
            fs::read(outside.join("keep.txt")).unwrap()
        );
    }

    /// Root is not refused what a mode forbids, so refusal tests have nothing to show there.
    fn permissions_apply() -> bool {
        !nix::unistd::geteuid().is_root()
    }

    #[test]
    fn a_delete_continues_past_an_entry_it_cannot_remove() {
        if !permissions_apply() {
            eprintln!("skipped: root removes entries whatever the modes say");
            return;
        }
        let fx = TempDir::new("tasks_delete_continues");
        let root = fx.join("d");
        for name in ["a", "b", "c", "x", "y", "z"] {
            fs::create_dir_all(root.join(name)).unwrap();
            fs::write(root.join(name).join("f"), b"x").unwrap();
        }
        fs::set_permissions(root.join("a"), fs::Permissions::from_mode(0o500)).unwrap();

        let task = run_to_end(TaskCommand::Delete(
            PathInfo::try_from(root.as_path()).unwrap(),
        ));
        fs::set_permissions(root.join("a"), fs::Permissions::from_mode(0o700)).unwrap();

        let left: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(vec![OsStr::new("a")], left);
        assert!(root.join("a").join("f").exists());
        let message = task.expect("started").error_message().expect("reported");
        assert!(message.starts_with("Failed to delete"), "{message}");
        assert!(message.contains("/a/f\""), "{message}");
        assert!(!message.contains("more)"), "{message}");
    }

    #[test_case(true ; "the one selected")]
    #[test_case(false ; "one inside the selection")]
    fn a_delete_removes_an_empty_directory_it_cannot_open(selected: bool) {
        if !permissions_apply() {
            eprintln!("skipped: root opens a mode-000 directory anyway");
            return;
        }
        let fx = TempDir::new("tasks_delete_mode_000");
        let root = fx.join("d");
        let locked = root.join("e");
        fs::create_dir_all(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let doomed = if selected { &locked } else { &root };

        let task = run_to_end(TaskCommand::Delete(
            PathInfo::try_from(doomed.as_path()).unwrap(),
        ));
        let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o700));

        assert_eq!(None, task.expect("started").error_message());
        assert!(doomed.symlink_metadata().is_err());
    }

    #[test]
    fn remove_path_advances_progress_from_the_removals() {
        let fx = TempDir::new("tasks");
        let root = fx.join("doomed");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("f.txt"), b"x").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        // Larger than the three entries removed, so an overcount is not clamped away.
        let (active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            100,
        );

        let (active, errors) = remove_path(&root, true, active, Removal::Delete).unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        let completed_of = |command| match command {
            Command::Progress(task) => Some(task.progress().completed),
            _ => None,
        };
        let completed: Vec<u64> = rx.try_iter().filter_map(completed_of).collect();
        active.send_progress();
        let final_count = rx.try_iter().find_map(completed_of);
        active.done();

        assert_eq!(Some(&1), completed.first());
        // The number of updates depends on timing; each must exceed the last.
        assert!(completed.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(Some(3), final_count);
    }

    #[test]
    fn remove_path_stops_when_already_cancelled() {
        let fx = TempDir::new("tasks");
        let root = fx.join("kept");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("f.txt"), b"x").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let (active, _, token) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );
        token.cancel();

        assert!(remove_path(&root, true, active, Removal::Delete).is_none());
        assert!(root.join("sub").join("f.txt").exists());
    }

    /// Run by `remove_path_deletes_a_tree_deeper_than_the_open_file_limit` under a low limit; on
    /// its own it proves nothing.
    #[test]
    #[ignore = "run under a lowered open-file limit by the test below"]
    fn remove_path_under_a_low_open_file_limit() {
        if !crate::test_support::alone() {
            return;
        }
        let fx = TempDir::new("tasks_delete_deep");
        let root = fx.join("doomed");
        let mut deepest = root.clone();
        for _ in 0..200 {
            deepest.push("d");
        }
        fs::create_dir_all(&deepest).unwrap();
        fs::write(deepest.join("leaf.txt"), b"x").unwrap();
        let (tx, _rx) = mpsc::channel();
        let (active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );

        let (active, errors) = remove_path(&root, true, active, Removal::Delete).unwrap();
        active.done();

        assert!(errors.is_empty(), "{errors:?}");
        assert!(!root.exists());
    }

    /// One fd per level would need 200, over the limit of 64.
    #[test]
    fn remove_path_deletes_a_tree_deeper_than_the_open_file_limit() {
        crate::test_support::run_alone(
            "file_system::tasks::remove::tests::remove_path_under_a_low_open_file_limit",
            "ulimit -Sn 64 &&",
            &[],
        );
    }

    #[test]
    fn remove_path_unlinks_a_symlink_to_a_directory_without_following_it() {
        let fx = TempDir::new("tasks_delete_symlink");
        let outside = fx.join("outside");
        fs::create_dir_all(outside.join("sub")).unwrap();
        fs::write(outside.join("keep.txt"), b"keep").unwrap();
        let root = fx.join("doomed");
        fs::create_dir_all(root.join("sub")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("sub").join("link")).unwrap();
        let (tx, _rx) = mpsc::channel();
        let (active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );

        let (active, errors) = remove_path(&root, true, active, Removal::Delete).unwrap();
        active.done();
        assert!(errors.is_empty(), "{errors:?}");

        assert!(!root.exists());
        assert_eq!(
            b"keep".to_vec(),
            fs::read(outside.join("keep.txt")).unwrap()
        );
        assert!(outside.join("sub").is_dir());
    }
}
