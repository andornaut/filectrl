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

/// Restats the source of a copy, or of a move when `is_move`, and validates
/// it against `dir`: the source as it is now, its path, the destination's,
/// and the task kind.
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

/// Reads the entry `listed` names again, without following a symlink in its
/// own name, for the type, mode and size it has now: the listing may be out of
/// date. Like `rm`, `mv` and `chmod`, an operation acts on whatever the path
/// names when it runs.
pub(in crate::file_system) fn restat(listed: &PathInfo, operation: &str) -> Result<PathInfo> {
    PathInfo::try_from(listed.as_path())
        .map_err(|error| anyhow!("Failed to {operation} {}: {error}", compact(&listed.path)))
}

/// Renames `old_path` to `new_path`, failing atomically with `AlreadyExists` if
/// `new_path` is taken, unlike `fs::rename`, which replaces it. A check made
/// before the rename leaves a window in which a file created at `new_path` is
/// silently replaced; folding the check into the rename closes it.
pub(in crate::file_system) fn rename_no_replace(
    old_path: &Path,
    new_path: &Path,
) -> std::io::Result<()> {
    rename_no_replace_at(CWD, old_path, CWD, new_path)
}

/// `rename_no_replace` for `old` in the directory `old_dir` and `new` in
/// `new_dir`; a path caller passes the current directory, which absolute paths
/// ignore and relative ones resolve against, matching `fs::rename`.
///
/// Linux uses `renameat2(RENAME_NOREPLACE)` and macOS `renameatx_np`, through
/// rustix's `renameat_with`, since `unsafe` is denied crate-wide. nix wraps
/// only glibc's `renameat2`, which glibc before 2.28 lacks, so a binary linked
/// against it fails to build for older sysroots such as cross's aarch64 image.
/// A kernel or filesystem that rejects the flag falls back to a check and a
/// plain rename (`checked_rename_at`), and keeps the narrow race. The errno is
/// kept otherwise (e.g. EXDEV as `CrossesDevices`, EEXIST as `AlreadyExists`)
/// so callers can dispatch on `error.kind()`.
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

/// `rename_no_replace_at` where the kernel cannot refuse a taken name itself:
/// a check, then a plain `renameat`, which never goes through `renameat2` and
/// so also works where that call is missing.
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

/// An absolute, display-friendly rendering of `path` for the operations
/// notice. Lexical only (no filesystem access), so it works for destination
/// paths that do not exist yet; falls back to the original path if it cannot
/// be absolutized.
pub(super) fn display_path(path: &Path) -> String {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    crate::visible_os(path.as_os_str()).into_owned()
}

/// The path with symlinks and `..` components resolved. Falls back to a lexical
/// absolutize-and-normalize when the path cannot be resolved, which is the
/// normal case for a destination that does not exist yet.
fn resolve(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        lexical_normalize(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
    })
}

/// The entry `path` names, with symlinks resolved in its parent directories
/// but not in its own name: a symlink is compared as itself, never as the file
/// it points at, which is a different entry.
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

/// Whether both paths name one file: the same device and inode, without
/// following a symlink in either.
pub(in crate::file_system) fn is_same_file(a: &Path, b: &Path) -> bool {
    EntryId::of_path(a).is_some_and(|a| EntryId::of_path(b) == Some(a))
}

/// Whether `link` is a symlink that resolves to the entry `entry` names.
fn is_link_to(link: &Path, entry: &Path) -> bool {
    link.symlink_metadata()
        .is_ok_and(|metadata| metadata.is_symlink())
        && link
            .canonicalize()
            .is_ok_and(|target| target == resolve_entry(entry))
}

/// How pasting an entry as `destination` would paste it onto itself. Such a
/// paste is refused, never offered as a collision: replacing the destination
/// with the source would replace the source itself, or a link to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::file_system) enum OntoItself {
    /// Both paths name one entry: the destination directory is the source's
    /// own, however either is spelled.
    SameEntry,
    /// Two names of one file (hard links, or one entry spelled two ways on a
    /// case-insensitive mount): replacing one with the other either deletes
    /// the file or does nothing, depending on how the names relate. `cp` and
    /// `mv` refuse both as the same file.
    SameFile,
    /// A symlink pasted over the entry it points at, which would replace the
    /// only copy of its data with a link to itself. `cp` and `mv` refuse it as
    /// the same file.
    LinksTo,
}

/// Whether pasting `source` as `destination` would paste it onto itself. The
/// paste queue and the task both ask this, so the prompt never offers to
/// replace what the task then refuses. Paths are compared resolved, so neither
/// a parent-dir segment (e.g. `/a/c/../b`) nor a symlinked directory can
/// disguise one as the other; a symlink in the last component is compared as
/// the link it is.
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

/// Collapses `.` and `..` components purely lexically (no filesystem access,
/// so it works for destinations that do not exist yet). `..` pops the previous
/// component; at the root it is a no-op.
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
    // Join the source's raw `OsStr` file name rather than its display name:
    // the display name is lossy UTF-8, which would silently mangle a non-UTF8
    // name at the destination.
    let Some(file_name) = source.path.file_name() else {
        return Err(anyhow!(
            "Cannot {operation} {}: path has no file name",
            compact(&old_path)
        )
        .into());
    };
    let new_path = destination_directory.path.join(file_name);

    // Compared resolved (see `onto_itself`), so a destination that reaches
    // into the source through a symlinked parent is still found below.
    let abs_old = resolve_entry(&old_path);
    let abs_new = resolve_entry(&new_path);

    if let Some(onto_itself) = onto_itself(&old_path, &new_path) {
        let error = match onto_itself {
            // Equal resolved paths mean the destination directory is the
            // source's own, so the message names the entry once.
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

    // Without this a copy creates the destination under the source and recurses
    // into it forever, filling the disk. Only a real directory can: `copy_path`
    // recreates a symlink as a link and copies a file as bytes, so neither
    // descends into what it is creating. Restricting the check to directories
    // also keeps it from refusing a symlink copied into the directory it points
    // at.
    if source.is_directory() && abs_new.starts_with(&abs_old) {
        return Err(anyhow!(
            "Cannot {operation} {} into its own subdirectory {}",
            compact(&old_path),
            compact(&new_path)
        )
        .into());
    }

    // Refuse to replace an existing destination unless the paste asked for it:
    // `File::create`/`fs::rename` would otherwise do so silently. An existing
    // directory is never replaced whatever was asked, because removing it would
    // take its contents with it and merging into it is not supported.
    match new_path.symlink_metadata() {
        // Both messages name the destination directory rather than the full
        // destination path: it differs from the source only in its directory,
        // so repeating the file name says nothing.
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
        // Nor does a directory replace anything, like `cp -R` and `mv`. The
        // paste never offers it, so this refuses only an overwrite granted
        // some other way.
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
    /// Nothing: the name is free, so nothing is replaced.
    Free,
    /// Another entry, or the one seen written since, which is not replaced
    /// (`conflicts::changed_refusal`).
    Changed,
}

/// What `found` at a name is, under the overwrite granted for `granted`.
pub(super) fn still_holds(granted: Seen, found: Option<Seen>) -> Holds {
    match found {
        None => Holds::Free,
        Some(found) if found == granted => Holds::Granted,
        Some(_) => Holds::Changed,
    }
}

/// What the name `path`, for which an overwrite was granted for `granted`,
/// holds now (`still_holds`), with the entry found there: looked at once, just
/// before the paste acts on it.
pub(super) fn look_again(granted: Seen, path: &Path) -> std::io::Result<(Holds, Option<Seen>)> {
    let found = Seen::of_path(path)?;
    Ok((still_holds(granted, found), found))
}

/// How `rename_for_move` ended.
#[derive(Debug)]
pub(super) enum Renamed {
    /// The source now holds the destination name.
    Moved,
    /// The name a granted overwrite would replace is another link to the
    /// source, which `rename(2)` onto it leaves in place while reporting
    /// success.
    SameFile,
    /// The name holds another entry than the one the overwrite was granted
    /// for, or that entry written since (`Holds::Changed`).
    Changed,
    /// A call failed. `kept` when it left the entry a granted overwrite was
    /// to replace at the name: the replacing rename failed, or looking at the
    /// name did. A rename that was not replacing anything leaves nothing
    /// granted behind.
    Failed { error: std::io::Error, kept: bool },
}

/// Renames `old_path` onto `new_path`, replacing the entry `overwrite` names
/// when it still holds the destination (`still_holds`), and refusing a taken
/// name otherwise: a name found free is taken without replacing anything, so
/// an entry that takes it before the rename is refused rather than replaced.
///
/// The kernel's atomic replace: no window with the destination missing, and a
/// rename that fails for an unrelated reason (a vanished source, a permission
/// error) leaves it untouched rather than destroyed for nothing. It refuses a
/// directory over a non-directory, as `mv` does. A destination that is another
/// link to the source is refused (`Renamed::SameFile`), since `rename(2)` onto
/// it does nothing and reports success. Another program replacing the entry
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

    /// A source built from `/`, so it reports as a directory: the subdirectory
    /// check below only applies to one, and every test using this helper is
    /// about a path rule rather than about the source's type.
    fn path_info(path: &str, basename: &str) -> PathInfo {
        let mut info = PathInfo::try_from(Path::new("/")).unwrap();
        info.path = PathBuf::from(path);
        info.display_name = basename.to_string();
        info
    }

    /// The message a refusal carried. `validate_paths` has five separate
    /// reasons to refuse, so `is_err` alone cannot tell whether a fixture
    /// reached the rule it was built for.
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
        // "/a/c/../b/d" resolves to "/a/b/d", inside the source, which a raw
        // component-wise prefix check on the path as written would not catch.
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
        // A lexical prefix check cannot see that `link` resolves inside `src`.
        // Copying through it would create the destination under the source and
        // then recurse into what it is creating, filling the disk.
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

        // The source resolves to the destination, but copying a symlink
        // recreates a link rather than descending into anything, so there is
        // no subtree to recurse into and nothing to reject.
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
        // A second path to the same directory, which the clipboard can easily
        // carry: it holds whatever absolute path the other window was showing.
        std::os::unix::fs::symlink(&real, fx.join("link")).unwrap();

        let src = PathInfo::try_from(real.join("f.txt").as_path()).unwrap();
        let dest = PathInfo::try_from(fx.join("link").as_path()).unwrap();

        // The two paths name one file, so replacing one with the other would
        // replace the source with itself. Without a granted overwrite the
        // existing-destination rule refuses the same paste, so the message is
        // what says the alias check is the one that fired.
        let message = rejection(validate_paths(&src, &dest, false, overwrite));
        assert!(message.ends_with("into its own directory"), "{message}");
        assert!(real.join("f.txt").exists());
    }

    /// `a/foo` is a symlink. Its own name is what is compared, so a paste into
    /// `b` meets `b/foo`, not the source's own directory, and whether that is a
    /// collision or a refusal depends on where the link points.
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

        // Even with the overwrite granted: replacing `b/foo` with the link
        // would leave a link to itself and lose the only copy of the data.
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
        // "/a/bb" must not be treated as inside "/a/b".
        let src = path_info("/a/b", "b");
        let dest = path_info("/a/bb", "bb");
        assert!(validate_paths(&src, &dest, false, false).is_ok());
    }

    #[test]
    fn validate_paths_preserves_non_utf8_source_names() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        // The display name is lossy UTF-8; the destination must be built from
        // the raw file name so the bytes survive.
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
        // A lossy rendering would show U+FFFD, the same as for any other
        // invalid byte, or for a name holding U+FFFD itself.
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

        // A directory is refused whatever the answer: removing it would take
        // its contents with it, and merging is not supported.
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

        // `symlink_metadata` rather than `exists`, which follows the link and
        // reports a dangling one as absent, silently overwriting it.
        let message = rejection(validate_paths(&src, &dest, false, false));
        assert!(message.ends_with("it already exists there"), "{message}");
    }

    #[test]
    fn a_failed_overwriting_move_leaves_the_destination_in_place() {
        let fx = TempDir::new("tasks_move_failure");
        let src = fx.join("gone.txt");
        let dst = fx.join("dest.txt");
        std::fs::write(&dst, b"dest").unwrap();

        // The source vanished while the task sat in the queue, which a single
        // worker running a long copy makes easy. A rename that fails leaves the
        // destination untouched.
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

        // The kernel replaces atomically here, so there is no moment in which
        // the destination is missing.
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

        // `rename` refuses a directory over a non-directory, as `mv` does, and
        // nothing clears the file to make way for it.
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

        // The paste never offers it, so only a granted overwrite reaches this.
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

        // Without a granted overwrite the destination is never touched, even
        // though the same function would replace it with one.
        let (error, kept) = failure(rename_for_move(None, &src, &dst));
        assert_eq!(ErrorKind::AlreadyExists, error.kind(), "{error}");
        assert!(!kept, "nothing was granted");
        assert_eq!(b"dest".to_vec(), std::fs::read(&dst).unwrap());
        assert!(src.exists());
    }

    /// An overwrite granted before the task started, when another process
    /// linked the destination name to the source since: `rename(2)` onto it
    /// would do nothing and report success, so the move would count as done
    /// with the source kept.
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

    /// A granted overwrite replaces only the entry it was granted for: the
    /// same entry is replaced, a free name is created, and any other entry is
    /// refused.
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

    /// A granted name found free is taken without replacing anything, so an
    /// entry that takes it before the rename is refused, and a failure there
    /// leaves nothing granted behind.
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

    /// A granted move that cannot look at the name (its directory has no
    /// search permission) cannot tell whether the entry granted still holds
    /// it, so the failure counts as leaving it.
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

    /// The error and `kept` of a rename that failed.
    fn failure(renamed: Renamed) -> (std::io::Error, bool) {
        match renamed {
            Renamed::Failed { error, kept } => (error, kept),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    /// The move itself: an entry that took the granted one's place is left
    /// alone, and so is the source.
    #[test]
    fn a_granted_overwrite_of_an_entry_since_replaced_is_refused() {
        let fx = TempDir::new("tasks_move_granted_changed");
        let src = fx.join("src.txt");
        let dst = fx.join("dest.txt");
        fs::write(&src, b"src").unwrap();
        fs::write(&dst, b"seen").unwrap();
        let granted = seen(&dst);
        // Kept under another name, so the one written next cannot reuse its
        // inode number.
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

    /// The fallback for a filesystem that rejects the no-replace flag, which
    /// `fs::rename` alone would turn into a silent replace.
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

    /// Relative to the directory handles given, not the current directory, as
    /// a replacement staged in a directory of its own lands: the rename, and
    /// the check and rename of its fallback alike.
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

    /// Refused by the kernel, not by the fallback's check: only the kernel's
    /// refusal carries an errno.
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

    /// The error kind is what sends a move to the copy fallback, so it has to
    /// survive the conversion. Needs a writable directory on another
    /// filesystem than the temporary one; skipped where there is none.
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
