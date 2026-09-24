use std::{
    ffi::{CStr, CString},
    fs::File,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};

use nix::{NixPath, dir::Entry, unistd::unlinkat};

use super::sys::{
    AtFlags, CWD, Dir, Errno, FileType, Mode, OFlag, Stat, UnlinkatFlags, fstat, fstatat, openat,
};

use crate::command::progress::ActiveTask;

/// Walks the tree below `root` for a pre-scan, calling `visit` with each
/// entry's directory, its name, and whether it is a directory. The walk is the
/// delete walk's: relative to open directories, never through a symlink (a
/// directory swapped for one after it was listed is not descended into), and
/// holding only the directory being read open. A directory that cannot be
/// opened is skipped, and one whose parent cannot be reopened ends the walk
/// early, which leaves a smaller total rather than an error. Returns `None`
/// when the task was cancelled.
pub(super) fn scan_tree(
    active: &ActiveTask,
    root: &Path,
    mut visit: impl FnMut(&Dir, &CStr, bool),
) -> Option<()> {
    let level = |listed: std::io::Result<(Dir, Entries)>| {
        let (dir, entries) = listed.ok()?;
        DirId::of_dir(&dir).ok().map(|id| Level {
            dir: Some(dir),
            id,
            name: None,
            entries: entries.into_iter(),
            incomplete: false,
        })
    };
    let Some(root) = level(scan_and_list(None, root)) else {
        return Some(());
    };
    let mut stack = vec![root];
    while let Some(top) = stack.last_mut() {
        if active.is_cancelled() {
            return None;
        }
        let Some((name, is_directory)) = top.entries.next() else {
            let level = stack.pop().expect("stack is non-empty");
            let Some(parent) = stack.last_mut() else {
                break;
            };
            let child = level
                .dir
                .as_ref()
                .expect("the level being read holds its fd");
            match reopen_parent(child, parent.id) {
                Ok(dir) => parent.dir = Some(dir),
                Err(_) => break,
            }
            continue;
        };
        let dir = top.dir.as_ref().expect("the level being read holds its fd");
        visit(dir, &name, is_directory);
        if is_directory && let Some(child) = level(scan_and_list(Some(dir), name.as_c_str())) {
            // Closed until the walk returns here, through `reopen_parent`.
            top.dir = None;
            stack.push(child);
        }
    }
    Some(())
}

/// Unlinks `name` in `dir`: a file or symlink with `NoRemoveDir`, an empty
/// directory with `RemoveDir`. Neither follows a symlink. An entry that is
/// already gone counts as removed, like `rm -f`: whatever removed it in the
/// meantime left what the delete or the replace asked for.
pub(super) fn unlink_at(dir: impl AsFd, name: &CStr, flags: UnlinkatFlags) -> std::io::Result<()> {
    match unlinkat(dir, name, flags) {
        Err(Errno::ENOENT) => Ok(()),
        result => Ok(result?),
    }
}

/// Opens the directory `path` is in, following symlinks: the top-level
/// directories are the ones the user chose, however they are reached.
pub(super) fn open_parent(path: &Path) -> std::io::Result<File> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC;
    Ok(File::from(openat(CWD, parent, flags, Mode::empty())?))
}

/// `path`'s file name, for the `*at` calls.
pub(super) fn c_name(path: &Path) -> Option<CString> {
    CString::new(path.file_name()?.as_bytes()).ok()
}

/// Opens the directory `name` in `parent` as a `File`, without following a
/// symlink (see `open_directory`).
pub(super) fn open_directory_file<P: ?Sized + NixPath>(
    parent: impl AsFd,
    name: &P,
) -> std::io::Result<File> {
    Ok(File::from(open_directory_fd(parent, name)?))
}

/// The names in the open directory `dir`, read to the end. `copy_tree` checks
/// for a cancel between the entries.
///
/// Read through a new handle on "." rather than `dir`'s own, since reading
/// moves the handle's position. A directory removed since it was opened lists
/// as empty, including where "." can no longer be opened in it.
pub(super) fn list_names(dir: &File) -> std::io::Result<Vec<CString>> {
    let fd = match openat(dir, c".", READ_FLAGS, Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::ENOENT) => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut dir = Dir::from_fd(fd)?;
    let mut names = Vec::new();
    for entry in dir.iter().filter(is_named) {
        names.push(entry?.file_name().to_owned());
    }
    Ok(names)
}

/// Whether a directory entry read names an entry to copy, count or remove:
/// "." and ".." do not. An error is kept for the caller to report.
fn is_named(entry: &nix::Result<Entry>) -> bool {
    entry.as_ref().map_or(true, |entry| {
        entry.file_name() != c"." && entry.file_name() != c".."
    })
}

/// A directory's entries as `remove_path` lists them: each name, and whether it
/// is a directory to descend into.
pub(super) type Entries = Vec<(CString, bool)>;

/// One directory on `remove_path`'s stack: the open directory its entries are
/// unlinked through (`None` while the walk is below it), its identity, its name
/// in the parent (`None` for the root), the entries not yet removed, and
/// whether one of them could not be removed.
pub(super) struct Level {
    pub(super) dir: Option<Dir>,
    pub(super) id: DirId,
    pub(super) name: Option<CString>,
    pub(super) entries: <Entries as IntoIterator>::IntoIter,
    pub(super) incomplete: bool,
}

/// The device and inode of an entry, to tell whether one reopened by name is
/// the one that was listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct DirId {
    pub(super) dev: nix::libc::dev_t,
    pub(super) ino: nix::libc::ino_t,
}

impl DirId {
    pub(super) fn of(dir: impl AsFd) -> std::io::Result<Self> {
        Ok(Self::of_stat(&fstat(dir)?))
    }

    pub(super) fn of_dir(dir: &Dir) -> std::io::Result<Self> {
        Self::of(dir)
    }

    pub(super) fn of_stat(stat: &Stat) -> Self {
        Self {
            dev: stat.st_dev,
            ino: stat.st_ino,
        }
    }
}

/// Reopens the parent of the open directory `dir` through its "..", refusing
/// it with the error `moved` unless it is the directory `expected` identifies:
/// a directory moved elsewhere during a walk has a different "..", and
/// following it would act outside the tree.
pub(super) fn reopen_parent_fd(
    moved: &str,
    expected: DirId,
    dir: impl AsFd,
) -> std::io::Result<OwnedFd> {
    let parent = open_directory_fd(dir, c"..")?;
    if DirId::of(&parent)? != expected {
        return Err(std::io::Error::other(moved));
    }
    Ok(parent)
}

/// `reopen_parent_fd` for the delete walk.
pub(super) fn reopen_parent(dir: &Dir, expected: DirId) -> std::io::Result<Dir> {
    let moved = "it was moved while its contents were being deleted";
    Ok(Dir::from_fd(reopen_parent_fd(moved, expected, dir)?)?)
}

/// Opens the directory `name` in `parent` without following a symlink:
/// `O_NOFOLLOW` with `O_DIRECTORY` refuses a link in the last component (ENOTDIR
/// on Linux, ELOOP elsewhere), and `O_DIRECTORY` anything else that is not a
/// directory.
pub(super) fn open_directory<P: ?Sized + NixPath>(
    parent: impl AsFd,
    name: &P,
) -> std::io::Result<Dir> {
    Ok(Dir::from_fd(open_directory_fd(parent, name)?)?)
}

/// `open_directory`, as the fd itself.
fn open_directory_fd<P: ?Sized + NixPath>(parent: impl AsFd, name: &P) -> std::io::Result<OwnedFd> {
    Ok(openat(parent, name, DIRECTORY_FLAGS, Mode::empty())?)
}

/// How a directory is opened to be read.
const READ_FLAGS: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_DIRECTORY)
    .union(OFlag::O_CLOEXEC);

/// How `open_directory` opens a directory.
const DIRECTORY_FLAGS: OFlag = READ_FLAGS.union(OFlag::O_NOFOLLOW);

/// `open_directory` then `list_entries`. A `None` parent resolves `name`
/// against the current directory, like a path.
pub(super) fn open_and_list<P: ?Sized + NixPath>(
    parent: Option<&Dir>,
    name: &P,
) -> std::io::Result<(Dir, Entries)> {
    list_dir(open_directory(fd_or_cwd(parent), name)?)
}

/// The fd a name in `parent` is opened relative to: the current directory's
/// when there is none, so the name resolves like a path.
fn fd_or_cwd(parent: Option<&Dir>) -> BorrowedFd<'_> {
    parent.map_or(CWD, AsFd::as_fd)
}

/// `open_and_list` for a pre-scan, which on Linux leaves the directory's
/// access time alone (`O_NOATIME`, which only its owner may ask for; anyone
/// else lists it as usual). A move takes each directory's times just before
/// listing it to copy, so they would otherwise record the scan's read.
fn scan_and_list<P: ?Sized + NixPath>(
    parent: Option<&Dir>,
    name: &P,
) -> std::io::Result<(Dir, Entries)> {
    #[cfg(target_os = "linux")]
    {
        let dir = fd_or_cwd(parent);
        match openat(dir, name, DIRECTORY_FLAGS | OFlag::O_NOATIME, Mode::empty()) {
            Ok(fd) => return list_dir(Dir::from_fd(fd)?),
            Err(Errno::EPERM) => {}
            Err(error) => return Err(error.into()),
        }
    }
    open_and_list(parent, name)
}

/// `list_entries` on `dir`, which it hands back with them.
pub(super) fn list_dir(mut dir: Dir) -> std::io::Result<(Dir, Entries)> {
    let entries = list_entries(&mut dir)?;
    Ok((dir, entries))
}

/// `unlink_at` in an open `Dir`.
pub(super) fn unlink(dir: &Dir, name: &CStr, flags: UnlinkatFlags) -> std::io::Result<()> {
    unlink_at(dir, name, flags)
}

/// Collects `(name, is_directory)` for each entry of `dir`, read to the end
/// before the caller deletes anything. The type comes from the directory entry,
/// or from an `lstat` where the filesystem does not report one, so a link to a
/// directory reports `false` and is unlinked rather than descended into.
///
/// Never checked for cancellation: the callers check between entries, and the
/// removal of a moved source, which a cancel must not stop part way, lists
/// through here too.
fn list_entries(dir: &mut Dir) -> std::io::Result<Entries> {
    let mut read = Vec::new();
    for entry in dir.iter().filter(is_named) {
        let entry = entry?;
        read.push((
            entry.file_name().to_owned(),
            FileType::of_entry(entry.file_type()),
        ));
    }
    // Typed once the stream is read, since `fstatat` needs the handle the
    // stream borrows while it is being read.
    read.into_iter()
        .map(|(name, file_type)| {
            let file_type = match file_type {
                FileType::Unknown => FileType::of(&fstatat(
                    &*dir,
                    name.as_c_str(),
                    AtFlags::AT_SYMLINK_NOFOLLOW,
                )?),
                file_type => file_type,
            };
            Ok((name, file_type == FileType::Directory))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::mpsc};

    use super::{
        super::{copy::dir_total_size, remove::dir_total_entries, test_support::copy_task},
        *,
    };
    use crate::{command::progress::TaskKind, test_support::TempDir};

    #[test]
    fn the_pre_scans_stop_when_the_task_is_cancelled() {
        let fx = TempDir::new("tasks");
        std::fs::write(fx.join("a.txt"), b"x").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let (active, _, token) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );
        token.cancel();

        // Both scans run before any file is touched and take as long as the
        // tree is large, so a cancel must not have to wait one out.
        assert_eq!(None, dir_total_size(&active, fx.path()));
        assert_eq!(None, dir_total_entries(&active, fx.path()));
        active.done();
    }

    #[test]
    fn reopening_a_parent_finds_the_directory_that_was_listed() {
        let fx = TempDir::new("tasks_reopen_parent");
        let parent = fx.join("parent");
        fs::create_dir_all(parent.join("child")).unwrap();
        let expected = DirId::of_dir(&open_directory(CWD, &parent).unwrap()).unwrap();
        let child = open_directory(CWD, &parent.join("child")).unwrap();

        let reopened = reopen_parent(&child, expected).unwrap();

        assert_eq!(expected, DirId::of_dir(&reopened).unwrap());
    }

    /// A child moved elsewhere during the walk has a different "..", which the
    /// walk must not continue into.
    #[test]
    fn reopening_the_parent_of_a_moved_directory_is_refused() {
        let fx = TempDir::new("tasks_reopen_moved");
        let parent = fx.join("parent");
        let elsewhere = fx.join("elsewhere");
        fs::create_dir_all(parent.join("child")).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        let expected = DirId::of_dir(&open_directory(CWD, &parent).unwrap()).unwrap();
        let child = open_directory(CWD, &parent.join("child")).unwrap();
        fs::rename(parent.join("child"), elsewhere.join("child")).unwrap();

        let error = reopen_parent(&child, expected).unwrap_err();

        assert_eq!(
            "it was moved while its contents were being deleted",
            error.to_string()
        );
    }

    #[test]
    fn open_directory_refuses_a_symlink_to_a_directory() {
        let fx = TempDir::new("tasks_open_directory_symlink");
        fs::create_dir(fx.join("real")).unwrap();
        std::os::unix::fs::symlink(fx.join("real"), fx.join("link")).unwrap();
        let parent = open_directory(CWD, fx.path()).unwrap();

        // A directory swapped for a link after it was listed has to fail to
        // open, or the walk would descend into the link's target.
        let error = open_directory(&parent, "link")
            .expect_err("a symlink must not be opened as a directory");

        let errno = error.raw_os_error();
        assert!(
            errno == Some(nix::libc::ENOTDIR) || errno == Some(nix::libc::ELOOP),
            "{error}"
        );
        assert!(open_directory(&parent, "real").is_ok());
    }

    /// The copy's counterpart of `reopening_the_parent_of_a_moved_directory_is_refused`.
    #[test]
    fn a_copy_refuses_to_return_through_a_directory_that_was_moved() {
        let fx = TempDir::new("tasks_copy_reopen_moved");
        fs::create_dir_all(fx.join("parent").join("child")).unwrap();
        fs::create_dir(fx.join("elsewhere")).unwrap();
        let parent = open_directory_file(CWD, &fx.join("parent")).unwrap();
        let expected = DirId::of(&parent).unwrap();
        let child = open_directory_file(&parent, "child").unwrap();
        assert_eq!(
            expected,
            DirId::of(reopen_parent_fd("moved", expected, &child).unwrap()).unwrap()
        );

        fs::rename(
            fx.join("parent").join("child"),
            fx.join("elsewhere").join("child"),
        )
        .unwrap();

        assert!(reopen_parent_fd("moved", expected, &child).is_err());
    }

    /// A directory the pre-scan listed is swapped for a link to one outside
    /// the tree before the scan reads it. The link is not followed.
    #[test]
    fn the_pre_scan_does_not_follow_a_directory_swapped_for_a_symlink() {
        let fx = TempDir::new("tasks_scan_swapped");
        let root = fx.join("root");
        fs::create_dir_all(root.join("sub")).unwrap();
        let outside = fx.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"x").unwrap();
        let (tx, _rx) = mpsc::channel();
        let active = copy_task(tx);
        let mut seen = Vec::new();

        scan_tree(&active, &root, |_, name, _| {
            seen.push(name.to_owned());
            if name == c"sub" {
                fs::rename(root.join("sub"), fx.join("sub.orig")).unwrap();
                std::os::unix::fs::symlink(&outside, root.join("sub")).unwrap();
            }
        })
        .unwrap();

        assert_eq!(vec![c"sub".to_owned()], seen);
    }

    /// A directory holding a file, a directory and a link to that directory.
    fn listing_fixture(label: &str) -> TempDir {
        let fx = TempDir::new(label);
        fs::write(fx.join("f"), b"x").unwrap();
        fs::create_dir(fx.join("d")).unwrap();
        std::os::unix::fs::symlink(fx.join("d"), fx.join("l")).unwrap();
        fx
    }

    /// "." and ".." name no entry, and a link to a directory is not one to
    /// descend into.
    #[test]
    fn a_listing_skips_the_dot_entries_and_types_each_entry() {
        let fx = listing_fixture("tasks_list_entries");

        let (_, mut entries) = open_and_list(None, fx.path()).unwrap();
        entries.sort();

        assert_eq!(
            vec![
                (c"d".to_owned(), true),
                (c"f".to_owned(), false),
                (c"l".to_owned(), false),
            ],
            entries
        );
    }

    #[test]
    fn the_names_listed_to_copy_skip_the_dot_entries() {
        let fx = listing_fixture("tasks_list_names");
        let dir = open_directory_file(CWD, fx.path()).unwrap();

        let mut names = list_names(&dir).unwrap();
        names.sort();

        assert_eq!(
            vec![c"d".to_owned(), c"f".to_owned(), c"l".to_owned()],
            names
        );
    }

    /// A copy's source removed after it was opened has nothing left to copy,
    /// which is not a failure to read it.
    #[test]
    fn a_directory_removed_after_it_was_opened_lists_as_empty() {
        let fx = TempDir::new("tasks_list_removed");
        fs::create_dir(fx.join("gone")).unwrap();
        let dir = open_directory_file(CWD, &fx.join("gone")).unwrap();
        fs::remove_dir(fx.join("gone")).unwrap();

        assert_eq!(Vec::<CString>::new(), list_names(&dir).unwrap());
    }

    #[test]
    fn open_directory_refuses_a_regular_file_as_not_a_directory() {
        let fx = listing_fixture("tasks_open_directory_file");
        let parent = open_directory(CWD, fx.path()).unwrap();

        // Through the copy's handle too, which no `fdopendir` stands behind to
        // refuse a file after the open.
        let error = open_directory(&parent, "f").expect_err("a file is not a directory");
        let file_error = open_directory_file(&parent, "f").expect_err("nor as a handle");

        assert_eq!(Some(nix::libc::ENOTDIR), error.raw_os_error());
        assert_eq!(Some(nix::libc::ENOTDIR), file_error.raw_os_error());
    }

    /// Like `rm -f`: whatever removed the entry left what was asked for. Any
    /// other failure keeps its errno.
    #[test]
    fn unlinking_counts_an_entry_already_gone_as_removed() {
        let fx = listing_fixture("tasks_unlink_gone");
        let parent = open_directory_file(CWD, fx.path()).unwrap();

        unlink_at(&parent, c"missing", UnlinkatFlags::NoRemoveDir).unwrap();
        fs::write(fx.join("d").join("inner"), b"x").unwrap();
        let error = unlink_at(&parent, c"d", UnlinkatFlags::RemoveDir).unwrap_err();

        assert_eq!(std::io::ErrorKind::DirectoryNotEmpty, error.kind());
        unlink_at(&parent, c"f", UnlinkatFlags::NoRemoveDir).unwrap();
        assert!(!fx.join("f").exists());
    }
}
