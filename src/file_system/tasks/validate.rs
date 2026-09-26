use std::{
    fs,
    io::ErrorKind,
    os::fd::AsFd,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow};
use log::info;
use rustix::{
    fs::{AtFlags, CWD, RenameFlags, renameat, renameat_with, statat},
    io::Errno,
};

use crate::{
    command::{
        progress::{TaskKind, Transfer},
        result::CommandResult,
    },
    file_system::{
        conflicts::{same_file_refusal, verb},
        entry_id::{EntryId, Seen},
        path_info::{PathInfo, compact},
    },
};

/// Restats the source of a copy (or move when `is_move`) and validates it against `dir`.
pub(super) fn start_transfer(
    is_move: bool,
    dir: &PathInfo,
    overwrite: bool,
    path: &PathInfo,
) -> Result<(PathInfo, PathBuf, PathBuf, TaskKind), CommandResult> {
    let path = restat(path, verb(is_move))?;
    let (old_path, new_path) = validate_paths(&path, dir, is_move, overwrite)?;
    let transfer = Transfer {
        source: display_path(&old_path),
        destination: display_path(&new_path),
    };
    let kind = if is_move {
        TaskKind::Move(transfer)
    } else {
        TaskKind::Copy(transfer)
    };
    info!(
        "{}{} to {}",
        kind.prefix(),
        old_path.display(),
        new_path.display()
    );
    Ok((path, old_path, new_path, kind))
}

/// Reads the entry `listed` names again, without following a symlink in its own name: like `rm`,
/// `mv` and `chmod`, an operation acts on what the path names when it runs.
pub(in crate::file_system) fn restat(listed: &PathInfo, operation: &str) -> Result<PathInfo> {
    PathInfo::try_from(listed.as_path())
        .map_err(|error| anyhow!("Failed to {operation} {}: {error}", compact(&listed.path)))
}

/// Renames `old_path` to `new_path`, failing atomically with `AlreadyExists` if `new_path` is taken
/// (`fs::rename` replaces it).
pub(in crate::file_system) fn rename_no_replace(
    old_path: &Path,
    new_path: &Path,
) -> std::io::Result<()> {
    rename_no_replace_at(CWD, old_path, CWD, new_path)
}

/// `rename_no_replace` relative to `old_dir` and `new_dir`.
///
/// Uses rustix's `renameat_with` (`renameat2(RENAME_NOREPLACE)` on Linux, `renameatx_np` on macOS):
/// nix wraps glibc's `renameat2`, which glibc before 2.28 lacks. Where the flag is rejected it
/// falls back to `checked_rename_at`, with its narrow race. The errno is kept so callers can
/// dispatch on `error.kind()`.
pub(super) fn rename_no_replace_at<P: rustix::path::Arg + Copy>(
    old_dir: impl AsFd,
    old: P,
    new_dir: impl AsFd,
    new: P,
) -> std::io::Result<()> {
    match renameat_with(&old_dir, old, &new_dir, new, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(Errno::NOSYS | Errno::INVAL | Errno::NOTSUP) => {
            checked_rename_at(old_dir, old, new_dir, new)
        }
        Err(errno) => Err(errno.into()),
    }
}

/// A check then a plain `renameat`, for where the kernel cannot refuse a taken name itself.
fn checked_rename_at<P: rustix::path::Arg + Copy>(
    old_dir: impl AsFd,
    old: P,
    new_dir: impl AsFd,
    new: P,
) -> std::io::Result<()> {
    if statat(&new_dir, new, AtFlags::SYMLINK_NOFOLLOW).is_ok() {
        return Err(ErrorKind::AlreadyExists.into());
    }
    Ok(renameat(old_dir, old, new_dir, new)?)
}

/// An absolute rendering of `path` for the operations notice. Lexical, so it works for destinations
/// that do not exist yet; falls back to `path`.
pub(super) fn display_path(path: &Path) -> String {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    crate::visible_os(path.as_os_str()).into_owned()
}

/// `path` with symlinks and `..` resolved, or lexically normalized when it cannot be resolved (a
/// destination that does not exist yet).
fn resolve(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        lexical_normalize(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
    })
}

/// `path` with symlinks resolved in its parent directories but not in its own name, so a symlink is
/// compared as itself.
fn resolve_entry(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => {
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            resolve(parent).join(name)
        }
        _ => resolve(path),
    }
}

/// Whether both paths name one file (same device and inode), following neither symlink.
pub(in crate::file_system) fn is_same_file(a: &Path, b: &Path) -> bool {
    EntryId::of_path(a).is_some_and(|a| EntryId::of_path(b) == Some(a))
}

fn is_link_to(link: &Path, entry: &Path) -> bool {
    link.symlink_metadata()
        .is_ok_and(|metadata| metadata.is_symlink())
        && link
            .canonicalize()
            .is_ok_and(|target| target == resolve_entry(entry))
}

/// How pasting an entry as `destination` would paste it onto itself. Refused, never offered as a
/// collision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::file_system) enum OntoItself {
    /// Both paths name one entry: the destination directory is the source's own.
    SameEntry,
    /// Two names of one file (hard links, or a case-insensitive mount). `cp` and `mv` refuse it.
    SameFile,
    /// A symlink pasted over the entry it points at. `cp` and `mv` refuse it.
    LinksTo,
}

/// Whether pasting `source` as `destination` would paste it onto itself. Shared by the paste queue
/// and the task, so the prompt never offers what the task refuses. Paths are compared resolved; a
/// symlink in the last component is compared as itself.
pub(in crate::file_system) fn onto_itself(source: &Path, destination: &Path) -> Option<OntoItself> {
    if resolve_entry(source) == resolve_entry(destination) {
        Some(OntoItself::SameEntry)
    } else if is_same_file(source, destination) {
        Some(OntoItself::SameFile)
    } else if is_link_to(source, destination) {
        Some(OntoItself::LinksTo)
    } else {
        None
    }
}

/// Collapses `.` and `..` components lexically; `..` at the root is a no-op.
fn lexical_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub(super) fn validate_paths(
    source: &PathInfo,
    destination_directory: &PathInfo,
    is_move: bool,
    overwrite: bool,
) -> Result<(PathBuf, PathBuf), CommandResult> {
    let operation = verb(is_move);
    let old_path = source.path.clone();
    // The raw `OsStr` file name, not the lossy display name, so non-UTF-8 names survive.
    let Some(file_name) = source.path.file_name() else {
        return Err(anyhow!(
            "Cannot {operation} {}: path has no file name",
            compact(&old_path)
        )
        .into());
    };
    let new_path = destination_directory.path.join(file_name);

    let abs_old = resolve_entry(&old_path);
    let abs_new = resolve_entry(&new_path);

    if let Some(onto_itself) = onto_itself(&old_path, &new_path) {
        let error = match onto_itself {
            OntoItself::SameEntry => anyhow!(
                "Cannot {operation} {} into its own directory",
                compact(&old_path)
            ),
            OntoItself::SameFile => anyhow!(same_file_refusal(is_move, &old_path, &new_path)),
            OntoItself::LinksTo => anyhow!(
                "Cannot {operation} {}: it links to {}, the entry it would replace",
                compact(&old_path),
                compact(&new_path)
            ),
        };
        return Err(error.into());
    }

    // A directory copied into itself would recurse forever. Symlinks and files never descend, so
    // only directories are checked.
    if source.is_directory() && abs_new.starts_with(&abs_old) {
        return Err(anyhow!(
            "Cannot {operation} {} into its own subdirectory {}",
            compact(&old_path),
            compact(&new_path)
        )
        .into());
    }

    // An existing destination is replaced only when the paste asked; an existing directory never
    // is.
    match new_path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() => Err(anyhow!(
            "Cannot {operation} {} into {}: a directory of that name is already there",
            compact(&old_path),
            compact(&destination_directory.path)
        )
        .into()),
        Ok(_) if !overwrite => Err(anyhow!(
            "Cannot {operation} {} into {}: it already exists there",
            compact(&old_path),
            compact(&destination_directory.path)
        )
        .into()),
        // A directory never replaces anything, like `cp -R` and `mv`.
        _ if overwrite && source.is_directory() => Err(anyhow!(
            "Cannot {operation} {} into {}: a directory never replaces what holds its name",
            compact(&old_path),
            compact(&destination_directory.path)
        )
        .into()),
        _ => Ok((old_path, new_path)),
    }
}

/// What holds a name an overwrite was granted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Holds {
    /// The entry seen, unchanged (`Seen`), which may be replaced.
    Granted,
    /// Nothing: the name is free.
    Free,
    /// Another entry, or the one seen written since, which is not replaced.
    Changed,
}

pub(super) fn still_holds(granted: Seen, found: Option<Seen>) -> Holds {
    match found {
        None => Holds::Free,
        Some(found) if found == granted => Holds::Granted,
        Some(_) => Holds::Changed,
    }
}

/// What the name `path` holds now relative to `granted` (`still_holds`), with the entry found.
pub(super) fn look_again(granted: Seen, path: &Path) -> std::io::Result<(Holds, Option<Seen>)> {
    let found = Seen::of_path(path)?;
    Ok((still_holds(granted, found), found))
}

#[derive(Debug)]
pub(super) enum Renamed {
    Moved,
    /// The granted name is another link to the source, which `rename(2)` leaves in place while
    /// reporting success.
    SameFile,
    /// The name holds another entry than the one granted, or that entry written since.
    Changed,
    /// A call failed. `kept` when the entry a granted overwrite was to replace is still at the
    /// name.
    Failed {
        error: std::io::Error,
        kept: bool,
    },
}

/// Renames `old_path` onto `new_path`, replacing the entry `overwrite` names if it still holds the
/// destination (`still_holds`), otherwise only taking a free name.
///
/// The kernel's replace is atomic, so a failed rename leaves the destination untouched. A
/// destination that is another link to the source is refused (`Renamed::SameFile`). A replacement
/// between the check and the rename is not detected.
pub(super) fn rename_for_move(
    overwrite: Option<Seen>,
    old_path: &Path,
    new_path: &Path,
) -> Renamed {
    let replace = match overwrite.map(|granted| look_again(granted, new_path)) {
        Some(Err(error)) => return Renamed::Failed { error, kept: true },
        Some(Ok((_, Some(found)))) if EntryId::of_path(old_path) == Some(found.id()) => {
            return Renamed::SameFile;
        }
        Some(Ok((Holds::Granted, _))) => true,
        None | Some(Ok((Holds::Free, _))) => false,
        Some(Ok((Holds::Changed, _))) => return Renamed::Changed,
    };
    let renamed = if replace {
        fs::rename(old_path, new_path)
    } else {
        rename_no_replace(old_path, new_path)
    };
    match renamed {
        Ok(()) => Renamed::Moved,
        Err(error) => Renamed::Failed {
            error,
            kept: replace,
        },
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use std::{fs::File, os::unix::fs::PermissionsExt};

    use nix::libc;

    use super::*;
    use crate::{command::Command, file_system::entry_id::seen, test_support::TempDir};

    /// A source built from `/`, so it reports as a directory, which the subdirectory check
    /// requires.
    fn path_info(path: &str, basename: &str) -> PathInfo {
        let mut info = PathInfo::try_from(Path::new("/")).unwrap();
        info.path = PathBuf::from(path);
        info.display_name = basename.to_string();
        info
    }

    /// The refusal message, to tell which of `validate_paths`'s rules fired.
    fn rejection(result: Result<(PathBuf, PathBuf), CommandResult>) -> String {
        let refusal = result.expect_err("the paths should have been refused");
        match Command::try_from(refusal) {
            Ok(Command::AlertError(message)) => message,
            other => panic!("expected an AlertError, got {other:?}"),
        }
    }

    #[test]
    fn validate_paths_rejects_identical_source_and_destination() {
        let src = path_info("/a/b", "b");
        let dest = path_info("/a", "a");
        let message = rejection(validate_paths(&src, &dest, false, false));
        assert!(message.ends_with("into its own directory"), "{message}");
    }

    #[test]
    fn validate_paths_rejects_destination_inside_source() {
        let src = path_info("/a/b", "b");
        let dest = path_info("/a/b/c", "c");
        let message = rejection(validate_paths(&src, &dest, false, false));
        assert!(message.contains("into its own subdirectory"), "{message}");
    }

    #[test]
    fn validate_paths_rejects_destination_inside_source_via_parent_dir() {
        // Resolves to "/a/b/d", inside the source.
        let src = path_info("/a/b", "b");
        let dest = path_info("/a/c/../b/d", "d");
        let message = rejection(validate_paths(&src, &dest, false, false));
        assert!(message.contains("into its own subdirectory"), "{message}");
    }

    #[test]
    fn validate_paths_rejects_a_destination_that_symlinks_into_the_source() {
        let fx = TempDir::new("tasks");
        let src = fx.join("src");
        std::fs::create_dir_all(src.join("inner")).unwrap();
        let link = fx.join("link");
        std::os::unix::fs::symlink(src.join("inner"), &link).unwrap();

        let source = path_info(src.to_str().unwrap(), "src");
        let dest = path_info(link.to_str().unwrap(), "link");
        let message = rejection(validate_paths(&source, &dest, false, false));
        assert!(message.contains("into its own subdirectory"), "{message}");
    }

    #[test]
    fn validate_paths_allows_a_symlink_copied_into_the_directory_it_points_at() {
        let fx = TempDir::new("tasks");
        let target = fx.join("target");
        std::fs::create_dir_all(&target).unwrap();
        let link = fx.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        // Copying a symlink recreates a link, so there is nothing to recurse into.
        let source = PathInfo::try_from(link.as_path()).unwrap();
        let dest = PathInfo::try_from(target.as_path()).unwrap();
        assert!(validate_paths(&source, &dest, false, false).is_ok());
    }

    #[test_case(true  ; "overwrite granted")]
    #[test_case(false ; "overwrite not granted")]
    fn validate_paths_rejects_a_destination_that_aliases_the_source(overwrite: bool) {
        let fx = TempDir::new("tasks");
        let real = fx.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("f.txt"), b"precious").unwrap();
        std::os::unix::fs::symlink(&real, fx.join("link")).unwrap();

        let src = PathInfo::try_from(real.join("f.txt").as_path()).unwrap();
        let dest = PathInfo::try_from(fx.join("link").as_path()).unwrap();

        // Without a granted overwrite the existing-destination rule refuses too; the message says
        // which fired.
        let message = rejection(validate_paths(&src, &dest, false, overwrite));
        assert!(message.ends_with("into its own directory"), "{message}");
        assert!(real.join("f.txt").exists());
    }

    /// `a/foo` is a symlink pasted into `b`; the outcome depends on where it points.
    fn link_pasted_into(label: &str, target: &str) -> (TempDir, PathInfo, PathInfo) {
        let fx = TempDir::new(label);
        let (a, b, c) = (fx.join("a"), fx.join("b"), fx.join("c"));
        for dir in [&a, &b, &c] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(b.join("foo"), b"data").unwrap();
        std::fs::write(c.join("foo"), b"other").unwrap();
        std::os::unix::fs::symlink(fx.join(target).join("foo"), a.join("foo")).unwrap();
        let src = PathInfo::try_from(a.join("foo").as_path()).unwrap();
        let dest = PathInfo::try_from(b.as_path()).unwrap();
        (fx, src, dest)
    }

    #[test]
    fn validate_paths_refuses_a_symlink_pasted_over_the_entry_it_points_at() {
        let (fx, src, dest) = link_pasted_into("tasks_link_over_target", "b");

        let message = rejection(validate_paths(&src, &dest, false, true));

        assert!(message.ends_with("the entry it would replace"), "{message}");
        assert_eq!(
            b"data".to_vec(),
            std::fs::read(fx.join("b").join("foo")).unwrap()
        );
    }

    #[test_case(false ; "a copy")]
    #[test_case(true ; "a move")]
    fn validate_paths_refuses_a_hard_link_of_the_source(is_move: bool) {
        let fx = TempDir::new("tasks_hard_link");
        let (a, b) = (fx.join("a"), fx.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("f"), b"data").unwrap();
        std::fs::hard_link(a.join("f"), b.join("f")).unwrap();
        let src = PathInfo::try_from(a.join("f").as_path()).unwrap();
        let dest = PathInfo::try_from(b.as_path()).unwrap();

        let message = rejection(validate_paths(&src, &dest, is_move, true));

        assert!(message.ends_with("they are the same file"), "{message}");
    }

    #[test]
    fn validate_paths_treats_a_symlink_to_another_file_as_an_ordinary_collision() {
        let (_fx, src, dest) = link_pasted_into("tasks_link_elsewhere", "c");

        let message = rejection(validate_paths(&src, &dest, false, false));

        assert!(message.ends_with("already exists there"), "{message}");
    }

    #[test]
    fn validate_paths_allows_sibling_destination() {
        let src = path_info("/a/b", "b");
        let dest = path_info("/x", "x");
        let (old_path, new_path) =
            validate_paths(&src, &dest, false, false).expect("should be allowed");
        assert_eq!(PathBuf::from("/a/b"), old_path);
        assert_eq!(PathBuf::from("/x/b"), new_path);
    }

    #[test]
    fn validate_paths_allows_destination_with_shared_prefix_but_different_component() {
        let src = path_info("/a/b", "b");
        let dest = path_info("/a/bb", "bb");
        assert!(validate_paths(&src, &dest, false, false).is_ok());
    }

    #[test]
    fn validate_paths_preserves_non_utf8_source_names() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let name = OsStr::from_bytes(b"caf\xe9.txt");
        let mut src = path_info("/a/placeholder", "placeholder");
        src.path = PathBuf::from("/a").join(name);
        let dest = path_info("/x", "x");
        let (_, new_path) = validate_paths(&src, &dest, false, false).expect("should be allowed");
        assert_eq!(PathBuf::from("/x").join(name), new_path);
    }

    #[test]
    fn display_path_spells_out_a_byte_that_is_not_utf8() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let path = Path::new(OsStr::from_bytes(b"/a/caf\xe9.txt"));

        assert_eq!("/a/caf\\xe9.txt", display_path(path));
    }

    #[test]
    fn validate_paths_rejects_source_without_file_name() {
        let src = path_info("/", "");
        let dest = path_info("/x", "x");
        let message = rejection(validate_paths(&src, &dest, false, false));
        assert!(message.ends_with("path has no file name"), "{message}");
    }

    #[test]
    fn validate_paths_rejects_existing_destination() {
        let fx = TempDir::new("tasks");
        std::fs::write(fx.join("existing.txt"), b"x").unwrap();
        let src = path_info("/elsewhere/existing.txt", "existing.txt");
        let dest = path_info(fx.path().to_str().unwrap(), "dir");
        let message = rejection(validate_paths(&src, &dest, false, false));
        assert!(message.ends_with("it already exists there"), "{message}");
    }

    #[test]
    fn validate_paths_rejects_a_destination_directory_of_the_same_name() {
        let fx = TempDir::new("tasks");
        std::fs::create_dir_all(fx.join("existing.txt")).unwrap();
        let src = path_info("/elsewhere/existing.txt", "existing.txt");
        let dest = path_info(fx.path().to_str().unwrap(), "dir");

        for overwrite in [false, true] {
            let message = rejection(validate_paths(&src, &dest, false, overwrite));
            assert!(
                message.ends_with("a directory of that name is already there"),
                "{message}"
            );
        }
    }

    #[test]
    fn validate_paths_rejects_existing_broken_symlink_destination() {
        let fx = TempDir::new("tasks");
        let link = fx.join("existing.txt");
        std::os::unix::fs::symlink(fx.join("missing"), &link).unwrap();
        let src = path_info("/elsewhere/existing.txt", "existing.txt");
        let dest = path_info(fx.path().to_str().unwrap(), "dir");

        // `exists` follows the link and reports a dangling one as absent.
        let message = rejection(validate_paths(&src, &dest, false, false));
        assert!(message.ends_with("it already exists there"), "{message}");
    }

    #[test]
    fn a_failed_overwriting_move_leaves_the_destination_in_place() {
        let fx = TempDir::new("tasks_move_failure");
        let src = fx.join("gone.txt");
        let dst = fx.join("dest.txt");
        std::fs::write(&dst, b"dest").unwrap();

        let (error, kept) = failure(rename_for_move(seen(&dst), &src, &dst));
        assert_eq!(ErrorKind::NotFound, error.kind(), "{error}");
        assert!(kept, "the replacing rename failed with the entry in place");
        assert_eq!(b"dest".to_vec(), std::fs::read(&dst).unwrap());
    }

    #[test]
    fn an_overwriting_move_replaces_a_file_without_clearing_it_first() {
        let fx = TempDir::new("tasks_move_atomic");
        let src = fx.join("src.txt");
        let dst = fx.join("dest.txt");
        std::fs::write(&src, b"src").unwrap();
        std::fs::write(&dst, b"dest").unwrap();

        assert!(matches!(
            rename_for_move(seen(&dst), &src, &dst),
            Renamed::Moved
        ));
        assert_eq!(b"src".to_vec(), std::fs::read(&dst).unwrap());
        assert!(!src.exists());
    }

    #[test]
    fn an_overwriting_move_never_replaces_a_file_with_a_directory() {
        let fx = TempDir::new("tasks_move_dir_over_file");
        let src = fx.join("srcdir");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("inner.txt"), b"src").unwrap();
        let dst = fx.join("dest");
        std::fs::write(&dst, b"dest").unwrap();

        let (error, kept) = failure(rename_for_move(seen(&dst), &src, &dst));
        assert_eq!(Some(libc::ENOTDIR), error.raw_os_error(), "{error}");
        assert!(kept);
        assert_eq!(b"dest".to_vec(), std::fs::read(&dst).unwrap());
        assert!(src.join("inner.txt").exists());
    }

    #[test_case(false ; "a copy")]
    #[test_case(true ; "a move")]
    fn validate_paths_refuses_a_directory_granted_an_overwrite(is_move: bool) {
        let fx = TempDir::new("tasks_validate_dir_over_file");
        let src = fx.join("src").join("name");
        std::fs::create_dir_all(&src).unwrap();
        let dest = fx.join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("name"), b"dest").unwrap();
        let src = PathInfo::try_from(src.as_path()).unwrap();
        let dest = PathInfo::try_from(dest.as_path()).unwrap();

        let message = rejection(validate_paths(&src, &dest, is_move, true));

        assert!(
            message.ends_with("never replaces what holds its name"),
            "{message}"
        );
    }

    #[test]
    fn a_move_without_overwrite_refuses_an_existing_destination() {
        let fx = TempDir::new("tasks_move_no_overwrite");
        let src = fx.join("src.txt");
        let dst = fx.join("dest.txt");
        std::fs::write(&src, b"src").unwrap();
        std::fs::write(&dst, b"dest").unwrap();

        let (error, kept) = failure(rename_for_move(None, &src, &dst));
        assert_eq!(ErrorKind::AlreadyExists, error.kind(), "{error}");
        assert!(!kept, "nothing was granted");
        assert_eq!(b"dest".to_vec(), std::fs::read(&dst).unwrap());
        assert!(src.exists());
    }

    /// Another process linked the destination name to the source after the overwrite was granted.
    #[test]
    fn a_granted_overwrite_onto_a_hard_link_to_the_source_is_refused() {
        let fx = TempDir::new("tasks_move_granted_link");
        let src = fx.join("src.txt");
        let dst = fx.join("dest.txt");
        std::fs::write(&src, b"src").unwrap();
        std::fs::hard_link(&src, &dst).unwrap();

        let renamed = rename_for_move(seen(&dst), &src, &dst);

        assert!(matches!(renamed, Renamed::SameFile), "{renamed:?}");
        assert!(src.exists());
        assert!(dst.exists());
    }

    #[test]
    fn a_granted_overwrite_holds_only_for_the_entry_it_was_granted_for() {
        let fx = TempDir::new("tasks_still_holds");
        let (granted, other) = (fx.join("granted"), fx.join("other"));
        fs::write(&granted, b"g").unwrap();
        fs::write(&other, b"o").unwrap();
        let (granted, other) = (seen(&granted).unwrap(), seen(&other).unwrap());

        assert_eq!(Holds::Granted, still_holds(granted, Some(granted)));
        assert_eq!(Holds::Free, still_holds(granted, None));
        assert_eq!(Holds::Changed, still_holds(granted, Some(other)));
    }

    #[test]
    fn a_granted_overwrite_of_a_name_freed_since_does_not_replace() {
        let fx = TempDir::new("tasks_move_granted_freed");
        let (src, dst) = (fx.join("src.txt"), fx.join("dest.txt"));
        fs::write(&src, b"src").unwrap();
        fs::write(&dst, b"seen").unwrap();
        let granted = seen(&dst);
        fs::remove_file(&dst).unwrap();
        fs::remove_file(&src).unwrap();

        let (error, kept) = failure(rename_for_move(granted, &src, &dst));

        assert_eq!(ErrorKind::NotFound, error.kind(), "{error}");
        assert!(!kept, "nothing granted was there to keep");
    }

    /// Its directory has no search permission, so the name cannot be looked at.
    #[test]
    fn a_granted_move_that_cannot_look_at_the_name_leaves_it() {
        let fx = TempDir::new("tasks_move_unsearchable");
        let (src, dest) = (fx.join("src.txt"), fx.join("dest"));
        fs::write(&src, b"src").unwrap();
        fs::create_dir(&dest).unwrap();
        let target = dest.join("one");
        fs::write(&target, b"granted").unwrap();
        let granted = seen(&target);
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o444)).unwrap();
        if fs::symlink_metadata(&target).is_ok() {
            eprintln!("skipped: a directory without search permission can be searched here");
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }

        let renamed = rename_for_move(granted, &src, &target);
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();

        let (error, kept) = failure(renamed);
        assert_eq!(Some(libc::EACCES), error.raw_os_error(), "{error}");
        assert!(kept);
        assert_eq!(b"granted".to_vec(), fs::read(&target).unwrap());
    }

    fn failure(renamed: Renamed) -> (std::io::Error, bool) {
        match renamed {
            Renamed::Failed { error, kept } => (error, kept),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_granted_overwrite_of_an_entry_since_replaced_is_refused() {
        let fx = TempDir::new("tasks_move_granted_changed");
        let src = fx.join("src.txt");
        let dst = fx.join("dest.txt");
        fs::write(&src, b"src").unwrap();
        fs::write(&dst, b"seen").unwrap();
        let granted = seen(&dst);
        // Kept under another name, so the next entry cannot reuse its inode number.
        fs::rename(&dst, fx.join("kept")).unwrap();
        fs::write(&dst, b"since").unwrap();

        let renamed = rename_for_move(granted, &src, &dst);

        assert!(matches!(renamed, Renamed::Changed), "{renamed:?}");
        assert_eq!(b"since".to_vec(), fs::read(&dst).unwrap());
        assert!(src.exists());
    }

    #[test]
    fn rename_no_replace_moves_to_new_destination() {
        let fx = TempDir::new("tasks");
        let src = fx.join("a.txt");
        std::fs::write(&src, b"x").unwrap();
        let dst = fx.join("b.txt");

        rename_no_replace(&src, &dst).unwrap();
        assert!(!src.exists());
        assert_eq!(b"x".to_vec(), std::fs::read(&dst).unwrap());
    }

    #[test]
    fn the_checked_rename_refuses_a_taken_name() {
        let fx = TempDir::new("tasks_checked_rename");
        let src = fx.join("a.txt");
        fs::write(&src, b"a").unwrap();
        let dst = fx.join("b.txt");
        fs::write(&dst, b"b").unwrap();

        let error = checked_rename_at(CWD, &src, CWD, &dst).unwrap_err();

        assert_eq!(ErrorKind::AlreadyExists, error.kind());
        assert_eq!(b"b".to_vec(), fs::read(&dst).unwrap());
        assert!(src.exists());
    }

    #[test]
    fn the_checked_rename_moves_to_a_free_name() {
        let fx = TempDir::new("tasks_checked_rename_free");
        let src = fx.join("a.txt");
        fs::write(&src, b"a").unwrap();
        let dst = fx.join("b.txt");

        checked_rename_at(CWD, &src, CWD, &dst).unwrap();

        assert_eq!(b"a".to_vec(), fs::read(&dst).unwrap());
        assert!(!src.exists());
    }

    #[test_case(|a, b, c, d| rename_no_replace_at(a, b, c, d) ; "the rename")]
    #[test_case(|a, b, c, d| checked_rename_at(a, b, c, d) ; "its fallback")]
    fn a_rename_between_open_directories_refuses_a_taken_name(
        rename: fn(&File, &str, &File, &str) -> std::io::Result<()>,
    ) {
        let fx = TempDir::new("tasks_rename_at");
        let (from, to) = (fx.join("from"), fx.join("to"));
        fs::create_dir(&from).unwrap();
        fs::create_dir(&to).unwrap();
        fs::write(from.join("a"), b"a").unwrap();
        fs::write(to.join("a"), b"taken").unwrap();
        fs::write(from.join("b"), b"b").unwrap();
        let (from_dir, to_dir) = (File::open(&from).unwrap(), File::open(&to).unwrap());

        let error = rename(&from_dir, "a", &to_dir, "a").unwrap_err();
        rename(&from_dir, "b", &to_dir, "b").unwrap();

        assert_eq!(ErrorKind::AlreadyExists, error.kind(), "{error}");
        assert_eq!(b"taken".to_vec(), fs::read(to.join("a")).unwrap());
        assert_eq!(b"a".to_vec(), fs::read(from.join("a")).unwrap());
        assert_eq!(b"b".to_vec(), fs::read(to.join("b")).unwrap());
        assert!(!from.join("b").exists());
    }

    /// Only the kernel's refusal carries an errno.
    #[test]
    fn a_rename_onto_a_taken_name_is_refused_as_already_exists() {
        let fx = TempDir::new("tasks_rename_taken");
        let (src, dst) = (fx.join("a"), fx.join("b"));
        fs::write(&src, b"a").unwrap();
        fs::write(&dst, b"b").unwrap();

        let error = rename_no_replace(&src, &dst).unwrap_err();

        assert_eq!(ErrorKind::AlreadyExists, error.kind());
        assert_eq!(Some(libc::EEXIST), error.raw_os_error());
        assert_eq!(b"b".to_vec(), fs::read(&dst).unwrap());
    }

    #[test]
    fn a_rename_of_a_missing_source_is_not_found() {
        let fx = TempDir::new("tasks_rename_missing");

        let error = rename_no_replace(&fx.join("missing"), &fx.join("b")).unwrap_err();

        assert_eq!(ErrorKind::NotFound, error.kind());
    }

    /// Needs a writable directory on another filesystem; skipped where there is none.
    #[test]
    fn a_rename_to_another_filesystem_crosses_devices() {
        let fx = TempDir::new("tasks_rename_xdev");
        let Some((_removed, other)) = super::super::test_support::other_device(&fx) else {
            eprintln!("skipped: no writable directory on another filesystem");
            return;
        };
        let src = fx.join("a");
        fs::write(&src, b"a").unwrap();

        let (error, _) = failure(rename_for_move(None, &src, &other.join("a")));

        assert_eq!(ErrorKind::CrossesDevices, error.kind());
        assert!(src.exists());
    }
}
