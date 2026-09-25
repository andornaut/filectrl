use std::{
    ffi::{CStr, CString},
    fs::File,
    os::{fd::AsFd, unix::ffi::OsStrExt},
    path::Path,
};

use nix::{NixPath, dir::Entry, unistd::unlinkat};

use super::sys::{AtFlags, CWD, Dir, Errno, FileType, Mode, OFlag, UnlinkatFlags, fstatat, openat};

use crate::{command::progress::ActiveTask, file_system::entry_id::EntryId};

/// The directories a walk is inside, from its root down to the one it is
/// working in. Only that deepest directory holds its handles open, so depth is
/// not bounded by the open-file limit: `descend` closes the parent's, and
/// `reopen` opens them again through the child's "..", which is never a
/// symlink, refusing any that is not the directory that was listed (by device
/// and inode), since the child may have been moved in between. A walk outside
/// its tree would act on what it was never given.
///
/// Iterative, so depth cannot overflow the thread stack either. `P` is what
/// each walk keeps per directory (the entries left, say), and `H` its handles:
/// one directory, or a copy's source and destination walked in step.
pub(super) struct Walk<H: Handles, P> {
    levels: Vec<Level<H, P>>,
}

/// One directory of a `Walk`: its handles (`None` while the walk is below it),
/// its identity for reopening them, and the walk's own state for it.
pub(super) struct Level<H: Handles, P> {
    handles: Option<H>,
    id: H::Id,
    payload: P,
}

impl<H: Handles, P> Level<H, P> {
    /// A directory just opened as `handles`, which `id` identifies.
    pub(super) fn open(handles: H, id: H::Id, payload: P) -> Self {
        Self {
            handles: Some(handles),
            id,
            payload,
        }
    }
}

/// The open directories a walk works through at one level.
pub(super) trait Handles: Sized {
    /// What identifies them, to tell whether reopened ones are the same.
    type Id: Copy;

    /// Reopens, through `self`'s "..", the directories `parent` identifies,
    /// refusing them with the error `moved` otherwise.
    fn reopen_parent(&self, parent: Self::Id, moved: &str) -> std::io::Result<Self>;
}

impl Handles for File {
    type Id = EntryId;

    fn reopen_parent(&self, parent: EntryId, moved: &str) -> std::io::Result<Self> {
        let reopened = open_directory(self, c"..")?;
        if EntryId::of(&reopened)? != parent {
            return Err(std::io::Error::other(moved));
        }
        Ok(reopened)
    }
}

impl<H: Handles, P> Walk<H, P> {
    pub(super) fn new(root: Level<H, P>) -> Self {
        Self { levels: vec![root] }
    }

    /// The directory being worked in, `None` once the walk is done.
    pub(super) fn top(&mut self) -> Option<(&H, &mut P)> {
        let level = self.levels.last_mut()?;
        let handles = level
            .handles
            .as_ref()
            .expect("the level being worked in holds its handles");
        Some((handles, &mut level.payload))
    }

    /// Enters `child`, a directory in the one being worked in, whose handles
    /// are closed until `reopen`.
    pub(super) fn descend(&mut self, child: Level<H, P>) {
        if let Some(parent) = self.levels.last_mut() {
            parent.handles = None;
        }
        self.levels.push(child);
    }

    /// Leaves the directory being worked in, handing back its handles and
    /// state. Its parent, if any, stays closed until `reopen`.
    pub(super) fn pop(&mut self) -> Option<(H, P)> {
        let level = self.levels.pop()?;
        let handles = level
            .handles
            .expect("the level being worked in holds its handles");
        Some((handles, level.payload))
    }

    /// Reopens the parent's handles through the ".." of `child`, which `pop`
    /// handed back, refusing them with the error `moved` unless they are the
    /// directories that were listed. Nothing to do once the root is popped.
    pub(super) fn reopen(&mut self, child: &H, moved: &str) -> std::io::Result<()> {
        if let Some(parent) = self.levels.last_mut() {
            parent.handles = Some(child.reopen_parent(parent.id, moved)?);
        }
        Ok(())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    /// Each directory's state, from the root down.
    pub(super) fn payloads(&self) -> impl Iterator<Item = &P> {
        self.levels.iter().map(|level| &level.payload)
    }
}

/// Walks the tree below `root` for a pre-scan, calling `visit` with each
/// entry's directory, its name, and whether it is a directory. Like the delete
/// walk, it is relative to open directories and never goes through a symlink
/// (a directory swapped for one after it was listed is not descended into). A directory that cannot be
/// opened is skipped, and one whose parent cannot be reopened ends the walk
/// early, which leaves a smaller total rather than an error. Returns `None`
/// when the task was cancelled.
pub(super) fn scan_tree(
    active: &ActiveTask,
    root: &Path,
    mut visit: impl FnMut(&File, &CStr, bool),
) -> Option<()> {
    let listed = |dir: std::io::Result<File>| {
        let dir = dir.ok()?;
        let entries = list_entries(&dir).ok()?;
        let id = EntryId::of(&dir).ok()?;
        Some(Level::open(dir, id, entries.into_iter()))
    };
    let Some(root) = listed(open_unread(CWD, root)) else {
        return Some(());
    };
    let mut walk = Walk::new(root);
    while let Some((dir, entries)) = walk.top() {
        if active.is_cancelled() {
            return None;
        }
        let Some((name, is_directory)) = entries.next() else {
            let (child, _) = walk.pop().expect("the walk is not done");
            if walk
                .reopen(&child, "it was moved while it was being scanned")
                .is_err()
            {
                break;
            }
            continue;
        };
        visit(dir, &name, is_directory);
        if is_directory && let Some(child) = listed(open_unread(dir, name.as_c_str())) {
            walk.descend(child);
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
    Ok(File::from(openat(CWD, parent, READ_FLAGS, Mode::empty())?))
}

/// `path`'s file name, for the `*at` calls.
pub(super) fn c_name(path: &Path) -> Option<CString> {
    CString::new(path.file_name()?.as_bytes()).ok()
}

/// Opens the directory `name` in `parent` without following a symlink:
/// `O_NOFOLLOW` with `O_DIRECTORY` refuses a link in the last component (ENOTDIR
/// on Linux, ELOOP elsewhere), and `O_DIRECTORY` anything else that is not a
/// directory.
pub(super) fn open_directory<P: ?Sized + NixPath>(
    parent: impl AsFd,
    name: &P,
) -> std::io::Result<File> {
    Ok(File::from(openat(
        parent,
        name,
        DIRECTORY_FLAGS,
        Mode::empty(),
    )?))
}

/// `open_directory` for a pre-scan, which on Linux leaves the directory's
/// access time alone (`O_NOATIME`, which only its owner may ask for; anyone
/// else lists it as usual). A move takes each directory's times just before
/// listing it to copy, so they would otherwise record the scan's read.
fn open_unread<P: ?Sized + NixPath>(parent: impl AsFd, name: &P) -> std::io::Result<File> {
    #[cfg(target_os = "linux")]
    match openat(
        &parent,
        name,
        DIRECTORY_FLAGS | OFlag::O_NOATIME,
        Mode::empty(),
    ) {
        Ok(fd) => return Ok(File::from(fd)),
        Err(Errno::EPERM) => {}
        Err(error) => return Err(error.into()),
    }
    open_directory(parent, name)
}

/// How a directory is opened to be read.
const READ_FLAGS: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_DIRECTORY)
    .union(OFlag::O_CLOEXEC);

/// How `open_directory` opens a directory.
const DIRECTORY_FLAGS: OFlag = READ_FLAGS.union(OFlag::O_NOFOLLOW);

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
pub(super) fn is_named(entry: &nix::Result<Entry>) -> bool {
    entry.as_ref().map_or(true, |entry| {
        entry.file_name() != c"." && entry.file_name() != c".."
    })
}

/// A directory's entries as `remove_path` lists them: each name, and whether it
/// is a directory to descend into.
pub(super) type Entries = Vec<(CString, bool)>;

/// Collects `(name, is_directory)` for each entry of `dir`, read to the end
/// before the caller deletes anything. The type comes from the directory entry,
/// or from an `lstat` where the filesystem does not report one, so a link to a
/// directory reports `false` and is unlinked rather than descended into.
///
/// Read through a duplicate of `dir`'s handle, which shares its position and
/// its flags (`O_NOATIME` from a pre-scan), since the stream takes ownership
/// of the one it reads.
///
/// Never checked for cancellation: the callers check between entries, and the
/// removal of a moved source, which a cancel must not stop part way, lists
/// through here too.
pub(super) fn list_entries(dir: &File) -> std::io::Result<Entries> {
    let mut stream = Dir::from_fd(dir.try_clone()?.into())?;
    let mut entries = Vec::new();
    for entry in stream.iter().filter(is_named) {
        let entry = entry?;
        let file_type = FileType::of_entry(entry.file_type());
        let name = entry.file_name();
        if let Some(is_directory) = is_listed_directory(dir, name, file_type) {
            entries.push((name.to_owned(), is_directory?));
        }
    }
    Ok(entries)
}

/// Whether the entry `name` of `dir`, listed as `file_type`, is a directory:
/// from an `lstat` when the filesystem did not report the type. `None` when
/// the entry is gone by then, which is how it would have been listed a moment
/// later, rather than a reason to fail the whole directory.
fn is_listed_directory(
    dir: &File,
    name: &CStr,
    file_type: FileType,
) -> Option<std::io::Result<bool>> {
    let file_type = match file_type {
        FileType::Unknown => match fstatat(dir, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) => FileType::of(&stat),
            Err(Errno::ENOENT) => return None,
            Err(errno) => return Some(Err(errno.into())),
        },
        file_type => file_type,
    };
    Some(Ok(file_type == FileType::Directory))
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
        let expected = EntryId::of(open_directory(CWD, &parent).unwrap()).unwrap();
        let child = open_directory(CWD, &parent.join("child")).unwrap();

        let reopened = child.reopen_parent(expected, "moved").unwrap();

        assert_eq!(expected, EntryId::of(&reopened).unwrap());
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
        let expected = EntryId::of(open_directory(CWD, &parent).unwrap()).unwrap();
        let child = open_directory(CWD, &parent.join("child")).unwrap();
        fs::rename(parent.join("child"), elsewhere.join("child")).unwrap();

        let error = child.reopen_parent(expected, "moved").unwrap_err();

        assert_eq!("moved", error.to_string());
    }

    /// A walk reopens the parent it returns to, and stops at a parent that
    /// is no longer the one it listed.
    #[test]
    fn a_walk_returns_only_to_the_parent_it_left() {
        let fx = TempDir::new("tasks_walk_reopen");
        fs::create_dir_all(fx.join("parent").join("child")).unwrap();
        fs::create_dir(fx.join("elsewhere")).unwrap();
        let level = |path: &Path, name: &str| {
            let dir = open_directory(CWD, path).unwrap();
            let id = EntryId::of(&dir).unwrap();
            Level::open(dir, id, name.to_string())
        };
        let mut walk = Walk::new(level(&fx.join("parent"), "parent"));
        walk.descend(level(&fx.join("parent").join("child"), "child"));
        assert_eq!(
            vec!["parent", "child"],
            walk.payloads().map(String::as_str).collect::<Vec<_>>()
        );

        let (child, _) = walk.pop().unwrap();
        walk.reopen(&child, "moved").unwrap();
        let (parent, payload) = walk.top().unwrap();
        assert_eq!("parent", payload);
        assert!(parent.try_clone().is_ok());

        walk.descend(level(&fx.join("parent").join("child"), "child"));
        fs::rename(
            fx.join("parent").join("child"),
            fx.join("elsewhere").join("child"),
        )
        .unwrap();
        let (child, _) = walk.pop().unwrap();
        assert_eq!(
            "moved",
            walk.reopen(&child, "moved").unwrap_err().to_string()
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

        let mut entries = list_entries(&open_directory(CWD, fx.path()).unwrap()).unwrap();
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
        let dir = open_directory(CWD, fx.path()).unwrap();

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
        let dir = open_directory(CWD, &fx.join("gone")).unwrap();
        fs::remove_dir(fx.join("gone")).unwrap();

        assert_eq!(Vec::<CString>::new(), list_names(&dir).unwrap());
    }

    #[test]
    fn open_directory_refuses_a_regular_file_as_not_a_directory() {
        let fx = listing_fixture("tasks_open_directory_file");
        let parent = open_directory(CWD, fx.path()).unwrap();

        // No `fdopendir` stands behind the handle to refuse a file after the
        // open, so the open itself has to.
        let error = open_directory(&parent, "f").expect_err("a file is not a directory");

        assert_eq!(Some(nix::libc::ENOTDIR), error.raw_os_error());
    }

    /// A filesystem that reports no type (XFS without `ftype`) is typed by an
    /// `lstat`, so a link to a directory is still not one, and an entry gone
    /// by then is left out rather than failing the directory.
    #[test]
    fn an_entry_listed_without_a_type_is_typed_by_lstat_or_left_out_when_gone() {
        let fx = listing_fixture("tasks_list_unknown_type");
        let dir = open_directory(CWD, fx.path()).unwrap();
        let typed =
            |name: &CStr| is_listed_directory(&dir, name, FileType::Unknown).map(Result::unwrap);

        assert_eq!(Some(true), typed(c"d"));
        assert_eq!(Some(false), typed(c"l"));
        assert_eq!(Some(false), typed(c"f"));
        assert_eq!(None, typed(c"missing"));
    }

    /// Like `rm -f`: whatever removed the entry left what was asked for. Any
    /// other failure keeps its errno.
    #[test]
    fn unlinking_counts_an_entry_already_gone_as_removed() {
        let fx = listing_fixture("tasks_unlink_gone");
        let parent = open_directory(CWD, fx.path()).unwrap();

        unlink_at(&parent, c"missing", UnlinkatFlags::NoRemoveDir).unwrap();
        fs::write(fx.join("d").join("inner"), b"x").unwrap();
        let error = unlink_at(&parent, c"d", UnlinkatFlags::RemoveDir).unwrap_err();

        assert_eq!(std::io::ErrorKind::DirectoryNotEmpty, error.kind());
        unlink_at(&parent, c"f", UnlinkatFlags::NoRemoveDir).unwrap();
        assert!(!fx.join("f").exists());
    }
}
