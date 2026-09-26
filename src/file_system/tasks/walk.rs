//! Path-based directory listing shared by copy and delete. Nothing here follows a symlink.

use std::{
    ffi::OsString,
    fs::{self, Metadata},
    io::ErrorKind,
    path::Path,
};

use crate::command::progress::ActiveTask;

/// A directory's entries: each name, and whether it is a directory to descend into.
pub(super) type Entries = Vec<(OsString, bool)>;

/// Lists `dir` to the end, so nothing is removed or created in it while it is read (some
/// filesystems skip entries then) and only one directory is open at a time. `read_dir` never yields
/// "." or "..". The type comes from the directory entry or `lstat`, so a link to a directory is not
/// one; an entry gone before its type was read is left out.
pub(super) fn list_entries(dir: &Path) -> std::io::Result<Entries> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        match entry.file_type() {
            Ok(file_type) => entries.push((entry.file_name(), file_type.is_dir())),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(entries)
}

/// Walks the tree below `root` for a pre-scan, calling `visit` with each entry's metadata. Never
/// goes through a symlink. Unreadable directories and entries are skipped, leaving a smaller total.
/// `None` when cancelled.
pub(super) fn scan_tree(
    active: &ActiveTask,
    root: &Path,
    mut visit: impl FnMut(&Metadata),
) -> Option<()> {
    let mut directories = vec![root.to_path_buf()];
    while let Some(dir) = directories.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if active.is_cancelled() {
                return None;
            }
            // `DirEntry::metadata` does not follow a symlink.
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            visit(&metadata);
            if metadata.is_dir() {
                directories.push(entry.path());
            }
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::{
        super::{copy::dir_total_size, remove::dir_total_entries, test_support::copy_task_with},
        *,
    };
    use crate::test_support::TempDir;

    #[test]
    fn the_pre_scans_stop_when_the_task_is_cancelled() {
        let fx = TempDir::new("tasks");
        fs::write(fx.join("a.txt"), b"x").unwrap();
        let (tx, _rx) = mpsc::channel();
        let (active, token) = copy_task_with(tx, 1);
        token.cancel();

        assert_eq!(None, dir_total_size(&active, fx.path()));
        assert_eq!(None, dir_total_entries(&active, fx.path()));
        active.done();
    }

    #[test]
    fn a_listing_types_each_entry_without_following_links() {
        let fx = TempDir::new("tasks_list_entries");
        fs::write(fx.join("f"), b"x").unwrap();
        fs::create_dir(fx.join("d")).unwrap();
        std::os::unix::fs::symlink(fx.join("d"), fx.join("l")).unwrap();

        let mut entries = list_entries(fx.path()).unwrap();
        entries.sort();

        assert_eq!(
            vec![
                (OsString::from("d"), true),
                (OsString::from("f"), false),
                (OsString::from("l"), false),
            ],
            entries
        );
    }

    #[test]
    fn the_pre_scan_does_not_follow_a_symlink_to_a_directory() {
        let fx = TempDir::new("tasks_scan_link");
        let root = fx.join("root");
        fs::create_dir(&root).unwrap();
        let outside = fx.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let (tx, _rx) = mpsc::channel();
        let (active, _) = copy_task_with(tx, 1);
        let mut seen = 0;

        scan_tree(&active, &root, |metadata| {
            assert!(metadata.is_symlink());
            seen += 1;
        })
        .unwrap();
        active.done();

        assert_eq!(1, seen);
    }
}
