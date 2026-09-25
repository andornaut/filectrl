use super::super::sys::{
    AtFlags, Errno, FileType, Stat, fstat, fstatat, mkdirat, mode_bits, stat_mode,
};
use super::super::walk::{Handles, Level, Walk, list_names, open_directory};
use super::attributes::{apply_final_mode, apply_times};
use super::entry::{copy_entry, resolve_raced};
use super::{At, CopyContext, Paths, Source, refused_or_failed};
use crate::file_system::conflicts::verb;
use crate::file_system::entry_id::EntryId;
use crate::file_system::path_info::compact;
use std::ffi::{CStr, CString};
use std::fs::File;

/// Copies the directory tree below `root`, then finishes each directory: its
/// mode, and its times for a move. The source and destination are walked in
/// step, holding only the directories being worked in open (see `Walk`).
///
/// A cancel stops the walk between entries, and every directory entered is
/// still finished on the way out, so none is left owner-only.
pub(super) fn copy_tree(context: &mut CopyContext<'_>, root: CopyLevel, paths: &mut Paths) -> bool {
    let mut walk = Walk::new(root);
    let mut cancelled = false;
    while let Some((Pair { src, dst }, level, lineage)) = walk.top_and_lineage() {
        cancelled |= context.active.is_cancelled();
        let next = if cancelled { None } else { level.names.next() };
        let Some(name) = next else {
            let (handles, level) = walk.pop().expect("the walk is not done");
            #[cfg(test)]
            if let Some(on_leave) = context.on_leave.as_mut() {
                on_leave(&paths.old);
            }
            // The parent is reopened through this directory before it gets
            // its final mode, which may take the owner's search permission
            // (a umask of 0o177, a default ACL of `u::rw-`) that going
            // through it needs.
            let reopened = walk.reopen(&handles, "it was relocated while it was being read");
            finish_directory(
                context,
                paths,
                &handles.dst,
                &level.source,
                level.umask_left,
            );
            if walk.is_empty() {
                break;
            }
            paths.pop();
            if let Err(error) = reopened {
                // Neither this directory nor any above it can be reached
                // again, so the walk ends here. They keep the owner-only mode
                // they were created with. An error with no errno is the walk's
                // own refusal of a parent that is no longer the one listed.
                context.errors.push(refused_or_failed(
                    context.is_move,
                    &compact(&paths.old),
                    &error,
                ));
                return !cancelled;
            }
            continue;
        };
        paths.push(&name);
        let stat = match fstatat(src, name.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(error) => {
                context.errors.push(format!(
                    "Failed to read metadata for {}: {error}",
                    compact(&paths.old)
                ));
                paths.pop();
                continue;
            }
        };
        let at = At {
            src,
            dst,
            src_name: &name,
            dst_name: &name,
        };
        if FileType::of(&stat) == FileType::Directory {
            let id = EntryId::of_stat(&stat);
            if lineage.holds(|level| level.src == id) {
                context.errors.push(format!(
                    "Cannot {} {}: it leads back to a directory above it",
                    verb(context.is_move),
                    compact(&paths.old)
                ));
            } else if let Some(child) = enter_directory(context, &at, paths, &stat) {
                walk.descend(child);
                // The child's path stays pushed until it is finished.
                continue;
            }
        } else if !copy_entry(context, &at, paths, &stat) {
            cancelled = true;
        }
        paths.pop();
    }
    !cancelled
}

/// Creates and opens the destination directory `at` names, then opens and
/// lists the source. `None` when there is nothing to descend into, having
/// recorded why.
///
/// The destination is created owner-writable and searchable so its children
/// can be created (`make_directory`), and `finish_directory` gives it its
/// mode once they are.
/// For a copy it starts with the other bits `stat` gives the source, which the
/// umask trims: `finish_directory` reads back what the umask left. The source
/// opened must be the directory `stat` describes, and its own metadata is what
/// the copy finishes with. A directory this copy created is refused before
/// anything is created for it.
pub(super) fn enter_directory(
    context: &mut CopyContext<'_>,
    at: &At<'_>,
    paths: &Paths,
    stat: &Stat,
) -> Option<CopyLevel> {
    let id = EntryId::of_stat(stat);
    if context.created.contains(&id) {
        context.errors.push(format!(
            "Cannot {} {}: it is inside the destination being written",
            verb(context.is_move),
            compact(&paths.old)
        ));
        return None;
    }
    let (dst, dst_id, umask_left) = make_directory(context, at, paths, stat)?;
    context.created.insert(dst_id);
    context.mark_written(at, paths);
    // Abandons the subtree, leaving the destination directory empty and with
    // its final mode.
    let give_up = |context: &mut CopyContext<'_>, message: String| {
        context.errors.push(message);
        finish_directory(context, paths, &dst, &Source::of(stat), umask_left);
    };
    let read_failure = |error: &dyn std::fmt::Display| {
        format!("Failed to read directory {}: {error}", compact(&paths.old))
    };
    let opened = open_directory(at.src, at.src_name).and_then(|src| Ok((fstat(&src)?, src)));
    let (opened, src) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            give_up(context, read_failure(&error));
            return None;
        }
    };
    if EntryId::of_stat(&opened) != id {
        let message = format!(
            "Cannot {} {}: it was replaced while it was being read",
            verb(context.is_move),
            compact(&paths.old)
        );
        give_up(context, message);
        return None;
    }
    let names = match list_names(&src) {
        Ok(names) => names,
        Err(error) => {
            give_up(context, read_failure(&error));
            return None;
        }
    };
    let source = Source::read(context.is_move, &paths.old, &src, &opened);
    Some(Level::open(
        Pair { src, dst },
        Pair {
            src: id,
            dst: dst_id,
        },
        Copying {
            source,
            umask_left,
            names: names.into_iter(),
        },
    ))
}

/// Makes the destination directory `at` names for the source `stat`
/// describes and opens it (`open_created`). Returns it, which entry it is,
/// and the mode the umask left when owner access had to be added; `None` when
/// it cannot be used, having recorded why.
///
/// Like `cp -R`, the directory opened is whatever holds the name once it is
/// made, without following a symlink there: another user who can write the
/// parent and swaps a directory in at the name gets it written into.
pub(super) fn make_directory(
    context: &mut CopyContext<'_>,
    at: &At<'_>,
    paths: &Paths,
    stat: &Stat,
) -> Option<(File, EntryId, Option<u32>)> {
    let creation = if context.is_move {
        0o700
    } else {
        (stat_mode(stat) & 0o777) | 0o700
    };
    let made = match mkdirat(at.dst, at.dst_name, mode_bits(creation)) {
        // Taken since the name was free, which is never replaced
        // (`resolve_raced`); skipping drops the subtree.
        Err(Errno::EEXIST) => {
            resolve_raced(context, paths);
            return None;
        }
        result => result
            .map_err(std::io::Error::from)
            .and_then(|()| open_created(at.dst, at.dst_name)),
    };
    match made {
        Ok(made) => Some(made),
        Err(error) => {
            // The subtree cannot be copied at all; skip it and continue with
            // the siblings.
            context.errors.push(format!(
                "Failed to create directory {}: {error}",
                compact(&paths.new)
            ));
            None
        }
    }
}

/// Opens the directory `name` a copy just created in `parent`, with owner
/// access added when the umask or a default ACL took it away: a umask such as
/// `0o277` leaves it unwritable, and no child could be created in it. `cp`
/// does the same. Returns it, which entry it is, and the mode the umask left
/// when access was added, which `finish_directory` puts back before computing
/// the final mode from it. One left without owner read (a umask of `0o477`, a
/// default ACL of `u::---`) cannot be opened, and is reported: nothing is ever
/// given a mode by name.
pub(super) fn open_created(
    parent: &File,
    name: &CStr,
) -> std::io::Result<(File, EntryId, Option<u32>)> {
    let dir = open_directory(parent, name)?;
    let id = EntryId::of(&dir)?;
    let left = grant_owner_access(&dir);
    Ok((dir, id, left))
}

/// Adds owner access to the open directory `dst` when it lacks it, returning
/// the mode it had.
pub(super) fn grant_owner_access(dst: &File) -> Option<u32> {
    let mode = stat_mode(&fstat(dst).ok()?) & 0o7777;
    if mode & 0o700 == 0o700 {
        return None;
    }
    nix::sys::stat::fchmod(dst, mode_bits(mode | 0o700)).ok()?;
    Some(mode)
}

/// Gives a copied directory its final mode, and for a move the source's
/// times, now that its children are written: writing them is what moved its
/// own modification time, and a mode without owner-write would have stopped
/// them being created. Through the handle, so a path swapped since cannot
/// redirect either. `umask_left` is the mode `grant_owner_access` replaced.
pub(super) fn finish_directory(
    context: &CopyContext<'_>,
    paths: &Paths,
    dst: &File,
    source: &Source,
    umask_left: Option<u32>,
) {
    if let Some(mode) = umask_left {
        let _ = nix::sys::stat::fchmod(dst, mode_bits(mode));
    }
    if context.is_move {
        apply_times(&paths.new, source, dst);
    }
    apply_final_mode(context, paths, dst, source);
}

/// A source directory and the destination directory it is copied into, or
/// their identities: `copy_tree` walks the two in step.
#[derive(Clone, Copy)]
pub(super) struct Pair<T> {
    pub(super) src: T,
    pub(super) dst: T,
}

impl Handles for Pair<File> {
    type Id = Pair<EntryId>;

    fn reopen_parent(&self, parent: Pair<EntryId>, moved: &str) -> std::io::Result<Self> {
        Ok(Self {
            src: self.src.reopen_parent(parent.src, moved)?,
            dst: self.dst.reopen_parent(parent.dst, moved)?,
        })
    }
}

/// One directory `copy_tree` is inside: the source's metadata for finishing
/// the copy of it, and the names not yet copied.
pub(super) struct Copying {
    pub(super) source: Source,
    /// The mode the umask left on the destination, when owner access was
    /// added over it.
    pub(super) umask_left: Option<u32>,
    pub(super) names: std::vec::IntoIter<CString>,
}

/// A directory `copy_tree` has entered.
pub(super) type CopyLevel = Level<Pair<File>, Copying>;
