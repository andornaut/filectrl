use std::{
    ffi::{CStr, CString},
    fs::File,
    os::{fd::AsFd, unix::ffi::OsStrExt},
    path::Path,
};

use nix::{NixPath, dir::Entry, unistd::unlinkat};

use super::sys::{AtFlags, CWD, Dir, Errno, FileType, Mode, OFlag, UnlinkatFlags, fstatat, openat};

use crate::{command::progress::ActiveTask, file_system::entry_id::EntryId};

/// The directories a walk is inside, from its root down to the one it works in. Only the deepest
/// holds its handles open, so depth is bounded by neither the open-file limit nor the stack:
/// `descend` closes the parent's handles and `reopen` opens them again through the child's "..",
/// refusing any that is not the directory listed (by device and inode).
///
/// `P` is the per-directory walk state and `H` its handles: one directory, or a copy's source and
/// destination in step.
pub(super) struct Walk<H: Handles, P> {
    levels: Vec<Level<H, P>>,
}

/// One directory of a `Walk`: its handles (`None` while the walk is below it), its identity, and
/// the walk's state for it.
pub(super) struct Level<H: Handles, P> {
    handles: Option<H>,
    id: H::Id,
    payload: P,
}

impl<H: Handles, P> Level<H, P> {
    pub(super) fn open(handles: H, id: H::Id, payload: P) -> Self {
        Self {
            handles: Some(handles),
            id,
            payload,
        }
    }
}

pub(super) trait Handles: Sized {
    /// Identifies the directories, to check reopened ones are the same.
    type Id: Copy;

    /// Reopens, through `self`'s "..", the directories `parent` identifies, refusing others with
    /// the error `moved`.
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

    pub(super) fn top(&mut self) -> Option<(&H, &mut P)> {
        let level = self.levels.last_mut()?;
        let handles = level
            .handles
            .as_ref()
            .expect("the level being worked in holds its handles");
        Some((handles, &mut level.payload))
    }

    /// `top`, with the identities of every directory on the walk.
    pub(super) fn top_and_lineage(&mut self) -> Option<(&H, &mut P, Lineage<'_, H, P>)> {
        let (level, above) = self.levels.split_last_mut()?;
        let handles = level
            .handles
            .as_ref()
            .expect("the level being worked in holds its handles");
        let lineage = Lineage {
            above,
            own: level.id,
        };
        Some((handles, &mut level.payload, lineage))
    }

    /// Enters `child`; the current directory's handles are closed until `reopen`.
    pub(super) fn descend(&mut self, child: Level<H, P>) {
        if let Some(parent) = self.levels.last_mut() {
            parent.handles = None;
        }
        self.levels.push(child);
    }

    /// Leaves the current directory, returning its handles and state. The parent stays closed until
    /// `reopen`.
    pub(super) fn pop(&mut self) -> Option<(H, P)> {
        let level = self.levels.pop()?;
        let handles = level
            .handles
            .expect("the level being worked in holds its handles");
        Some((handles, level.payload))
    }

    /// Reopens the parent's handles through `child`'s "..", refusing them with the error `moved`
    /// unless they are the directories listed.
    pub(super) fn reopen(&mut self, child: &H, moved: &str) -> std::io::Result<()> {
        if let Some(parent) = self.levels.last_mut() {
            parent.handles = Some(child.reopen_parent(parent.id, moved)?);
        }
        Ok(())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    /// Whether a directory already on the walk is one `matches` accepts. Descending into one (a
    /// bind mount of an ancestor) would never end, so walks refuse it, as `rm` and `cp` do.
    pub(super) fn holds(&self, matches: impl Fn(&H::Id) -> bool) -> bool {
        self.levels.iter().any(|level| matches(&level.id))
    }

    pub(super) fn payloads(&self) -> impl Iterator<Item = &P> {
        self.levels.iter().map(|level| &level.payload)
    }
}

/// The identities of every directory on a walk (`Walk::top_and_lineage`).
pub(super) struct Lineage<'a, H: Handles, P> {
    above: &'a [Level<H, P>],
    own: H::Id,
}

impl<H: Handles, P> Lineage<'_, H, P> {
    pub(super) fn holds(&self, matches: impl Fn(&H::Id) -> bool) -> bool {
        matches(&self.own) || self.above.iter().any(|level| matches(&level.id))
    }
}

/// Walks the tree below `root` for a pre-scan, calling `visit` with each entry's directory, name,
/// and whether it is a directory. Never goes through a symlink. Unopenable directories are skipped
/// and an unreopenable parent ends the walk early, leaving a smaller total. `None` when cancelled.
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
        if is_directory
            && let Some(child) = listed(open_unread(dir, name.as_c_str()))
            && !walk.holds(|id| *id == child.id)
        {
            walk.descend(child);
        }
    }
    Some(())
}

/// Unlinks `name` in `dir` without following a symlink. An entry already gone counts as removed,
/// like `rm -f`.
pub(super) fn unlink_at(dir: impl AsFd, name: &CStr, flags: UnlinkatFlags) -> std::io::Result<()> {
    match unlinkat(dir, name, flags) {
        Err(Errno::ENOENT) => Ok(()),
        result => Ok(result?),
    }
}

/// Opens the directory `path` is in, following symlinks: the top-level directories are the user's
/// choice.
pub(super) fn open_parent(path: &Path) -> std::io::Result<File> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    Ok(File::from(openat(CWD, parent, READ_FLAGS, Mode::empty())?))
}

pub(super) fn c_name(path: &Path) -> Option<CString> {
    CString::new(path.file_name()?.as_bytes()).ok()
}

/// Opens the directory `name` in `parent` without following a symlink.
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

/// `open_directory` for a pre-scan: on Linux `O_NOATIME` (owner only) keeps the scan from moving
/// the access time a move later copies.
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

const READ_FLAGS: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_DIRECTORY)
    .union(OFlag::O_CLOEXEC);

const DIRECTORY_FLAGS: OFlag = READ_FLAGS.union(OFlag::O_NOFOLLOW);

/// The names in `dir`, read to the end, through a new handle on "." so `dir`'s position is
/// untouched. A directory removed since it was opened lists as empty.
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

/// Whether a directory entry is one to copy, count or remove: not "." or "..". An error is kept for
/// the caller.
pub(super) fn is_named(entry: &nix::Result<Entry>) -> bool {
    entry.as_ref().map_or(true, |entry| {
        entry.file_name() != c"." && entry.file_name() != c".."
    })
}

/// A directory's entries: each name, and whether it is a directory to descend into.
pub(super) type Entries = Vec<(CString, bool)>;

/// Collects `(name, is_directory)` for each entry of `dir`, read to the end before anything is
/// deleted. The type comes from the directory entry or `lstat`, so a link to a directory is
/// `false`. Reads a duplicate handle (the stream takes ownership). Never checks for cancellation: a
/// moved source's removal must not stop part way.
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

/// Whether the entry `name`, listed as `file_type`, is a directory, using `lstat` when the type was
/// not reported. `None` when the entry is gone.
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

    #[test]
    fn a_walk_holds_every_directory_it_is_in() {
        let fx = TempDir::new("tasks_walk_holds");
        fs::create_dir_all(fx.join("root").join("child")).unwrap();
        fs::create_dir(fx.join("other")).unwrap();
        let id = |path: &Path| EntryId::of(open_directory(CWD, path).unwrap()).unwrap();
        let level = |path: &Path| Level::open(open_directory(CWD, path).unwrap(), id(path), ());
        let (root, child, other) = (
            id(&fx.join("root")),
            id(&fx.join("root").join("child")),
            id(&fx.join("other")),
        );
        let mut walk = Walk::new(level(&fx.join("root")));
        walk.descend(level(&fx.join("root").join("child")));

        let held = |walk: &mut Walk<File, ()>, wanted: EntryId| {
            let by_walk = walk.holds(|id| *id == wanted);
            let (_, (), lineage) = walk.top_and_lineage().unwrap();
            assert_eq!(by_walk, lineage.holds(|id| *id == wanted));
            by_walk
        };
        assert!(held(&mut walk, root));
        assert!(held(&mut walk, child));
        assert!(!held(&mut walk, other));
    }

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

        let error = open_directory(&parent, "link")
            .expect_err("a symlink must not be opened as a directory");

        let errno = error.raw_os_error();
        assert!(
            errno == Some(nix::libc::ENOTDIR) || errno == Some(nix::libc::ELOOP),
            "{error}"
        );
        assert!(open_directory(&parent, "real").is_ok());
    }

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

    fn listing_fixture(label: &str) -> TempDir {
        let fx = TempDir::new(label);
        fs::write(fx.join("f"), b"x").unwrap();
        fs::create_dir(fx.join("d")).unwrap();
        std::os::unix::fs::symlink(fx.join("d"), fx.join("l")).unwrap();
        fx
    }

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

        // No `fdopendir` follows to refuse a file, so the open itself has to.
        let error = open_directory(&parent, "f").expect_err("a file is not a directory");

        assert_eq!(Some(nix::libc::ENOTDIR), error.raw_os_error());
    }

    /// Simulates a filesystem that reports no type (XFS without `ftype`).
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
