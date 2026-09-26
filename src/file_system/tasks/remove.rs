use std::{
    ffi::{CStr, CString, OsStr},
    fs::File,
    io::ErrorKind,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::Instant,
};

use super::{
    PROGRESS_DEBOUNCE_PERCENTAGE, PROGRESS_MIN_INTERVAL, cancel_logging,
    sys::{AtFlags, UnlinkatFlags, fstatat},
    walk::{
        Entries, Level, Walk, c_name, list_entries, open_directory, open_parent, scan_tree,
        unlink_at,
    },
};
use crate::{
    command::progress::ActiveTask,
    file_system::{debounce, entry_id::EntryId, path_info::compact},
};

/// Best-effort recursive entry count for the delete progress total, including `root`. Reads
/// directories only; an unlistable directory counts as one. `None` when cancelled.
pub(super) fn dir_total_entries(active: &ActiveTask, root: &Path) -> Option<u64> {
    let mut total: u64 = 1; // `root` itself, which appears in no listing.
    scan_tree(active, root, |_, _, _| total += 1)?;
    Some(total)
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Removal {
    /// A delete: cancellable between entries, advancing progress per entry.
    Delete,
    /// The source of a cross-device move, past the point a cancel could stop it. Only the entry the
    /// copy read is removed.
    MovedSource(EntryId),
}

/// The refusal to remove a moved source whose path names another entry than the one copied.
pub(super) fn replaced_after_copy(path: &Path) -> String {
    format!(
        "Cannot remove {}: it was replaced after it was copied",
        compact(path)
    )
}

/// Removes a file or directory tree like `rm -rf`: an entry that cannot be removed is recorded and
/// the walk continues, keeping the directories above it without reporting them. An unopenable empty
/// directory is removed, and an entry already gone counts as removed. Only a parent that cannot be
/// reopened as the one listed ends the walk.
///
/// A moved source is removed only while `path` still names the entry copied, by device and inode,
/// compared on what was opened relative to the parent's fd; every removal goes through that fd.
///
/// Returns the task and the errors, leaving finalization to the caller; `None` when cancelled
/// (already finalized).
pub(super) fn remove_path(
    path: &Path,
    is_directory: bool,
    mut active: ActiveTask,
    removal: Removal,
) -> Option<(ActiveTask, Vec<String>)> {
    let cancellable = removal == Removal::Delete;
    if cancellable && active.is_cancelled() {
        active.cancelled();
        return None;
    }
    let mut errors = Vec::new();
    let (root_parent, root_name) = match open_root_parent(path, removal) {
        Ok(Some(opened)) => opened,
        Ok(None) => {
            active.increment(1);
            return Some((active, errors));
        }
        Err(message) => {
            errors.push(message);
            return Some((active, errors));
        }
    };
    let root = RootAt {
        dir: &root_parent,
        name: &root_name,
    };
    if !is_directory {
        match remove_file_entry(path, &root, removal) {
            Ok(()) if cancellable => active.increment(1),
            Ok(()) => {}
            Err(message) => errors.push(message),
        }
        return Some((active, errors));
    }

    // Post-order walk. Each directory is read fully before anything in it is unlinked, since
    // unlinking while reading can skip entries on some filesystems (NFS). A level holds its name,
    // not its path, so memory does not grow with the square of the depth.
    let (dir, id, entries) = match open_root(path, &root, removal) {
        Ok(Some(opened)) => opened,
        Ok(None) => {
            if cancellable {
                active.increment(1);
            }
            return Some((active, errors));
        }
        Err(message) => {
            errors.push(message);
            return Some((active, errors));
        }
    };
    let mut walk = Walk::new(Level::open(dir, id, Removing::new(None, entries)));
    // Debounced so a wide tree does not queue one progress command per entry ahead of terminal
    // input.
    let mut debouncer = debounce::ProgressDebouncer::new(
        PROGRESS_DEBOUNCE_PERCENTAGE,
        PROGRESS_MIN_INTERVAL,
        active.total_size(),
    );
    while let Some((dir, level)) = walk.top() {
        if cancellable && active.is_cancelled() {
            cancel_logging(&errors, active);
            return None;
        }
        let Some((name, is_dir)) = level.entries.next() else {
            match remove_level(path, &root, &mut walk) {
                Ok(true) => advance(&mut active, &mut debouncer, cancellable),
                Ok(false) => {}
                Err(Unremoved::Failed(message)) => errors.push(message),
                Err(Unremoved::Lost(message)) => {
                    errors.push(message);
                    break;
                }
            }
            continue;
        };
        match remove_entry(dir, &name, is_dir) {
            Ok(None) => advance(&mut active, &mut debouncer, cancellable),
            Ok(Some((_, id, _))) if walk.holds(|level| *level == id) => {
                let looped = entry_path(path, &walk, &name);
                errors.push(format!(
                    "Cannot delete {}: it leads back to a directory above it",
                    compact(&looped)
                ));
                walk.top().expect("the walk is not done").1.incomplete = true;
            }
            Ok(Some((dir, id, entries))) => {
                walk.descend(Level::open(dir, id, Removing::new(Some(name), entries)));
            }
            Err((what, error)) => {
                let failed = entry_path(path, &walk, &name);
                errors.push(format!("{what} {}: {error}", compact(&failed)));
                walk.top().expect("the walk is not done").1.incomplete = true;
            }
        }
    }
    Some((active, errors))
}

/// Opens the directory `path` is in, with `path`'s name in it. `None` when a delete finds that
/// directory gone. The error is the message to record.
fn open_root_parent(path: &Path, removal: Removal) -> Result<Option<(File, CString)>, String> {
    let Some(name) = c_name(path) else {
        return Err(format!(
            "Cannot delete {}: path has no file name",
            compact(path)
        ));
    };
    match open_parent(path) {
        Ok(parent) => Ok(Some((parent, name))),
        Err(error) if removal == Removal::Delete && error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Failed to delete {}: {error}", compact(path))),
    }
}

/// Removes the entry `name` in `parent`, or opens and lists it when it is a directory to descend
/// into. An empty directory that cannot be opened is removed anyway, like `rm -rf`; otherwise the
/// open's error is returned.
fn remove_entry(
    parent: &File,
    name: &CStr,
    is_dir: bool,
) -> Result<Option<Opened>, (&'static str, std::io::Error)> {
    if !is_dir {
        return unlink_at(parent, name, UnlinkatFlags::NoRemoveDir)
            .map(|()| None)
            .map_err(|error| ("Failed to delete", error));
    }
    let opened = open_directory(parent, name)
        .and_then(|dir| Ok((list_entries(&dir)?, EntryId::of(&dir)?, dir)));
    match opened {
        Ok((entries, id, dir)) => Ok(Some((dir, id, entries))),
        Err(error) => unlink_at(parent, name, UnlinkatFlags::RemoveDir)
            .map(|()| None)
            .map_err(|_| ("Failed to read directory", error)),
    }
}

/// Opens and lists the directory `root` names, refusing a moved source unless it is the entry
/// copied. `None` when a delete removed it as an unopenable empty directory.
fn open_root(path: &Path, root: &RootAt<'_>, removal: Removal) -> Result<Option<Opened>, String> {
    let failed =
        |error: std::io::Error| format!("Failed to read directory {}: {error}", compact(path));
    let opened = open_directory(root.dir, root.name).and_then(|dir| Ok((EntryId::of(&dir)?, dir)));
    let (id, dir) = match opened {
        Ok(opened) => opened,
        // A moved source that cannot be opened cannot be compared, so it is kept.
        Err(error) if removal == Removal::Delete => {
            return unlink_at(root.dir, root.name, UnlinkatFlags::RemoveDir)
                .map(|()| None)
                .map_err(|_| failed(error));
        }
        Err(error) => return Err(failed(error)),
    };
    if let Removal::MovedSource(copied) = removal
        && id != copied
    {
        return Err(replaced_after_copy(path));
    }
    let entries = list_entries(&dir).map_err(failed)?;
    Ok(Some((dir, id, entries)))
}

/// Counts one entry removed by a delete. A move's progress measured bytes and is already complete.
fn advance(active: &mut ActiveTask, debouncer: &mut debounce::ProgressDebouncer, counts: bool) {
    if counts {
        active.increment(1);
        if debouncer.should_trigger(Instant::now(), 1) {
            active.send_progress();
        }
    }
}

/// `remove_path` for anything that is not a directory. Symlinks are removed, never followed. The
/// error is the message to record.
fn remove_file_entry(path: &Path, at: &RootAt<'_>, removal: Removal) -> Result<(), String> {
    let failed = |error: std::io::Error| format!("Failed to delete {}: {error}", compact(path));
    if let Removal::MovedSource(copied) = removal {
        let stat = fstatat(at.dir, at.name, AtFlags::AT_SYMLINK_NOFOLLOW)
            .map_err(|error| failed(error.into()))?;
        if EntryId::of_stat(&stat) != copied {
            return Err(replaced_after_copy(path));
        }
    }
    unlink_at(at.dir, at.name, UnlinkatFlags::NoRemoveDir).map_err(failed)
}

/// One directory `remove_path` is inside: its name in the parent (`None` for the root), the entries
/// left, and whether one could not be removed.
struct Removing {
    name: Option<CString>,
    entries: <Entries as IntoIterator>::IntoIter,
    incomplete: bool,
}

impl Removing {
    fn new(name: Option<CString>, entries: Entries) -> Self {
        Self {
            name,
            entries: entries.into_iter(),
            incomplete: false,
        }
    }
}

fn entry_path(root: &Path, walk: &Walk<File, Removing>, name: &CStr) -> PathBuf {
    let mut path = level_path(root, walk);
    path.push(OsStr::from_bytes(name.to_bytes()));
    path
}

fn level_path(root: &Path, walk: &Walk<File, Removing>) -> PathBuf {
    let mut path = root.to_path_buf();
    for name in walk.payloads().filter_map(|level| level.name.as_ref()) {
        path.push(OsStr::from_bytes(name.to_bytes()));
    }
    path
}

enum Unremoved {
    /// Removing it failed. The walk goes on.
    Failed(String),
    /// Its parent could not be reopened as the directory listed; the walk ends.
    Lost(String),
}

/// Leaves the current directory and removes it from its reopened parent (or `root_at` for the
/// root). Returns whether it was removed: one holding an unremovable entry is kept without a second
/// report, and so is its parent.
fn remove_level(
    root: &Path,
    root_at: &RootAt<'_>,
    walk: &mut Walk<File, Removing>,
) -> Result<bool, Unremoved> {
    let (dir, level) = walk.pop().expect("the walk is not done");
    let Removing {
        name, incomplete, ..
    } = level;
    if walk.is_empty() {
        drop(dir);
        if incomplete {
            return Ok(false);
        }
        // Through the parent the root was opened in; `rmdir` does not follow a symlink.
        return unlink_at(root_at.dir, root_at.name, UnlinkatFlags::RemoveDir)
            .map(|()| true)
            .map_err(|error| {
                Unremoved::Failed(format!("Failed to delete {}: {error}", compact(root)))
            });
    }
    let name = name.expect("only the root has no name");
    walk.reopen(&dir, "it was moved while its contents were being deleted")
        .map_err(|error| {
            let lost = level_path(root, walk);
            Unremoved::Lost(format!("Failed to delete {}: {error}", compact(&lost)))
        })?;
    drop(dir);
    let (parent_dir, parent) = walk.top().expect("the parent was reopened");
    if incomplete {
        parent.incomplete = true;
        return Ok(false);
    }
    if let Err(error) = unlink_at(parent_dir, &name, UnlinkatFlags::RemoveDir) {
        parent.incomplete = true;
        let failed = entry_path(root, walk, &name);
        return Err(Unremoved::Failed(format!(
            "Failed to delete {}: {error}",
            compact(&failed)
        )));
    }
    Ok(true)
}

type Opened = (File, EntryId, Entries);

/// The directory an operation's root entry was opened in, and its name there.
struct RootAt<'a> {
    dir: &'a File,
    name: &'a CStr,
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, sync::mpsc};

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

    #[test_case(true  ; "a directory")]
    #[test_case(false ; "a file")]
    fn a_moved_source_replaced_before_it_was_opened_is_kept(is_directory: bool) {
        let fx = TempDir::new("tasks_moved_replaced");
        let entry = fx.join("entry");
        let replacement = fx.join("replacement");
        if is_directory {
            fs::create_dir(&entry).unwrap();
            fs::create_dir(&replacement).unwrap();
            fs::write(replacement.join("keep.txt"), b"keep").unwrap();
        } else {
            fs::write(&entry, b"copied").unwrap();
            fs::write(&replacement, b"keep").unwrap();
        }
        let copied = EntryId::of_path(&entry).unwrap();
        // Renamed away rather than removed, so the replacement cannot reuse the inode.
        fs::rename(&entry, fx.join("renamed")).unwrap();
        fs::rename(&replacement, &entry).unwrap();
        let (tx, _rx) = mpsc::channel();

        let removal = Removal::MovedSource(copied);
        let (active, errors) = remove_path(&entry, is_directory, copy_task(tx), removal).unwrap();
        active.done();

        assert_eq!(vec![replaced_after_copy(&entry)], errors);
        let kept = if is_directory {
            entry.join("keep.txt")
        } else {
            entry.clone()
        };
        assert_eq!(b"keep".to_vec(), fs::read(kept).unwrap());
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
    fn a_moved_source_replaced_by_a_directory_that_cannot_be_opened_is_kept() {
        if !permissions_apply() {
            eprintln!("skipped: root opens a mode-000 directory anyway");
            return;
        }
        let fx = TempDir::new("tasks_moved_unopenable");
        let entry = fx.join("entry");
        fs::create_dir(&entry).unwrap();
        let copied = EntryId::of_path(&entry).unwrap();
        fs::rename(&entry, fx.join("renamed")).unwrap();
        fs::create_dir(&entry).unwrap();
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o000)).unwrap();
        let (tx, _rx) = mpsc::channel();

        let removal = Removal::MovedSource(copied);
        let (active, errors) = remove_path(&entry, true, copy_task(tx), removal).unwrap();
        active.done();
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o700)).unwrap();

        assert!(entry.is_dir());
        let [message] = errors.as_slice() else {
            panic!("expected one error, got {errors:?}");
        };
        assert!(message.starts_with("Failed to read directory"), "{message}");
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

    #[test]
    fn a_moved_source_already_gone_is_still_reported() {
        let fx = TempDir::new("tasks_moved_source_gone");
        let src = fx.join("f");
        fs::write(&src, b"x").unwrap();
        let (tx, _rx) = mpsc::channel();
        let copied = EntryId::of(File::open(&src).unwrap()).unwrap();
        fs::remove_file(&src).unwrap();

        let (active, errors) =
            remove_path(&src, false, copy_task(tx), Removal::MovedSource(copied)).unwrap();
        active.done();

        assert_eq!(1, errors.len(), "{errors:?}");
        assert!(errors[0].starts_with("Failed to delete"), "{errors:?}");
    }
}
