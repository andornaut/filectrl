use std::{
    ffi::{CStr, CString, OsStr},
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    os::{
        fd::AsFd,
        unix::{ffi::OsStrExt, fs::PermissionsExt},
    },
    path::{Path, PathBuf},
    time::Instant,
};

use log::warn;
use nix::{
    fcntl::{FcntlArg, fcntl},
    sys::time::TimeSpec,
    unistd::{Gid, fchown},
};
use rustix::fs::XattrFlags;

use super::{
    PROGRESS_DEBOUNCE_PERCENTAGE, PROGRESS_MIN_INTERVAL, cancel_logging,
    sys::{
        AtFlags, Errno, FileType, Mode, OFlag, Stat, UnlinkatFlags, fstat, fstatat, mkdirat,
        mode_bits, openat, readlinkat, stat_mode, symlinkat,
    },
    validate::{Holds, rename_no_replace_at, still_holds},
    walk::{
        Handles, Level, Walk, c_name, list_names, open_directory, open_parent, scan_tree, unlink_at,
    },
};
use crate::{
    command::progress::ActiveTask,
    file_system::{
        conflicts::{
            Conflicts, changed_refusal, failed_transfer, not_replaced, raced_in_copy_refusal,
            raced_refusal, verb,
        },
        debounce,
        entry_id::{EntryId, Seen},
        path_info::{PathInfo, compact},
    },
};

/// The settings and shared state one copy carries from the task down to every
/// entry of the tree, so that adding another does not lengthen every signature
/// in between.
struct CopyContext<'a> {
    /// One read buffer for the whole tree; see `copy_with_progress`.
    buffer: &'a mut [u8],
    /// The paste's standing `*All` answer, whose "skip all" skips a name taken
    /// since it was free instead of recording it (`resolve_raced`).
    conflicts: &'a Conflicts,
    /// The copy is the one a move across devices makes. It keeps each entry's
    /// full mode, times and extended attributes, as `mv` does, so the move
    /// leaves what a same-device rename would have, and its messages call it
    /// a move. A copy takes the umask and drops the special bits instead, as
    /// `cp` does.
    is_move: bool,
    /// The top-level source file, already opened by `prepare_destination`.
    /// `copy_file` takes it in place of opening the path again. `None` for any
    /// other source.
    source: Option<File>,
    /// The staging directory the top-level entry is being written into while
    /// it replaces another (`replace_entry`), for what is made by path.
    staging: Option<PathBuf>,
    /// The entry the copy created in that staging directory, the only one it
    /// lands or removes there (`Staging`).
    staged: Option<EntryId>,
    /// Entries a standing "skip all" left alone. Counted separately from the
    /// errors: skipping is a choice rather than a failure, but a move still
    /// must not remove a source whose entries never reached the destination.
    skipped: usize,
    /// The top-level entry itself was skipped, so nothing was copied.
    top_skipped: bool,
    /// Something now holds the top-level destination name that the copy put
    /// there, whole or not: what a move that fails leaves behind.
    wrote: bool,
    /// The directories this copy created, which it never descends into as a
    /// source: a destination swapped for a link into the source tree, or a
    /// bind mount of it, would otherwise copy the copy into itself without end.
    created: std::collections::HashSet<EntryId>,
    /// The top-level source that was copied, which is the entry a move removes
    /// afterwards and no other.
    root: Option<EntryId>,
    /// One debouncer for the whole tree, against its total: one per file would
    /// send an update for every file, since a debouncer's first call triggers.
    progress: debounce::ProgressDebouncer,
    /// Called with each directory the walk leaves, just before it goes back
    /// to the parent through it: where a test moves or locks the tree.
    #[cfg(test)]
    on_leave: Option<OnLeave<'a>>,
    /// Called with each directory the copy has just made, before it opens
    /// it: where a test swaps another in at its name.
    #[cfg(test)]
    on_made: Option<OnLeave<'a>>,
}

/// What `CopyContext::on_leave` calls.
#[cfg(test)]
type OnLeave<'a> = Box<dyn FnMut(&Path) + 'a>;

impl<'a> CopyContext<'a> {
    fn new(
        settings: CopySettings<'a>,
        buffer: &'a mut [u8],
        source: Option<File>,
        total_size: u64,
    ) -> Self {
        Self {
            buffer,
            conflicts: settings.conflicts,
            is_move: settings.is_move,
            source,
            staging: None,
            staged: None,
            skipped: 0,
            top_skipped: false,
            wrote: false,
            created: std::collections::HashSet::new(),
            root: None,
            progress: debounce::ProgressDebouncer::new(
                PROGRESS_DEBOUNCE_PERCENTAGE,
                PROGRESS_MIN_INTERVAL,
                total_size,
            ),
            #[cfg(test)]
            on_leave: None,
            #[cfg(test)]
            on_made: None,
        }
    }

    /// Records that the copy created the entry `at` names (`paths`), which at
    /// the top level means the destination now holds something of it. While
    /// the entry is staged (`replace_entry`) it is beside the name, not at it,
    /// and which entry it is is recorded instead, so that only that entry is
    /// landed or removed.
    fn mark_written(&mut self, at: &At<'_>, paths: &Paths) {
        if paths.depth != 0 {
            return;
        }
        if self.staging.is_some() {
            self.staged = fstatat(at.dst, at.dst_name, AtFlags::AT_SYMLINK_NOFOLLOW)
                .ok()
                .map(|stat| EntryId::of_stat(&stat));
        } else {
            self.wrote = true;
        }
    }

    /// How a failure to write the entry `paths` names reads: `own`, which
    /// names the entry as what failed, or while the entry is staged to replace
    /// another, the transfer that failed, since the entry at the name is not
    /// what failed and is left as it was (`replace_entry`).
    fn written_failure(
        &self,
        paths: &Paths,
        own: impl FnOnce() -> String,
        error: &dyn std::fmt::Display,
    ) -> String {
        if self.staging.is_some() {
            failed_transfer(self.is_move, &paths.old, &paths.new, error)
        } else {
            own()
        }
    }

    /// Where the entry `paths` names is being created, for what is made by
    /// path (`make_node` on macOS): in the staging directory for a top-level
    /// entry that is replacing another, and at its destination otherwise.
    fn node_path(&self, paths: &Paths) -> PathBuf {
        match (&self.staging, paths.new.file_name()) {
            (Some(staging), Some(name)) if paths.depth == 0 => staging.join(name),
            _ => paths.new.clone(),
        }
    }

    /// What the copy left behind, with the `errors` it recorded.
    fn into_outcome(self, errors: Vec<String>) -> CopyOutcome {
        CopyOutcome {
            errors,
            skipped: self.skipped,
            top_skipped: self.top_skipped,
            wrote: self.wrote,
            root: self.root,
            #[cfg(test)]
            progress_threshold: self.progress.threshold(),
        }
    }
}

/// What `prepare_destination` hands the copy: the top-level source file it
/// opened, and the entry a granted overwrite lets the copy replace, if it
/// still held the name.
#[cfg_attr(test, derive(Default))]
pub(super) struct Prepared {
    pub(super) source: Option<File>,
    pub(super) replace: Option<Seen>,
}

/// What a tree copy left behind: the entries that could not be written, how
/// many a standing "skip all" left alone and whether the top-level entry was
/// one, and which entry was copied.
#[derive(Default)]
pub(super) struct CopyOutcome {
    pub(super) errors: Vec<String>,
    pub(super) skipped: usize,
    pub(super) top_skipped: bool,
    /// Something the copy made holds the top-level destination name.
    pub(super) wrote: bool,
    pub(super) root: Option<EntryId>,
    /// The tree debouncer's threshold, which a copy too quick to outlast the
    /// time floor gives no other way to observe.
    #[cfg(test)]
    pub(super) progress_threshold: u64,
}

/// The copy read buffer, one per task. Benchmarked fastest on ext4, btrfs and
/// tmpfs, and it keeps cancel latency near 10 ms under writeback throttling;
/// the same size coreutils `cp` reads in.
const COPY_BUFFER_BYTES: usize = 128 * 1024;

/// What a copy was asked for, the same for every entry of its tree: whether it
/// is the copy a move across devices makes (`CopyContext::is_move`), and the
/// paste's standing answer (`CopyContext::conflicts`).
#[derive(Clone, Copy)]
pub(super) struct CopySettings<'a> {
    pub(super) is_move: bool,
    pub(super) conflicts: &'a Conflicts,
}

/// The byte-copy stage shared by copy and cross-device move: for a directory
/// source, scans the real transfer total (a directory entry's own size is not
/// the transfer size) and applies it via `set_total`, and copies the tree.
/// `prepared` is what `prepare_destination` found, and `listed` the source as
/// the task was started for it: its type, mode and size.
///
/// Returns `None` when the task was cancelled, in which case it has already
/// been finalized via `active.cancelled()`. Otherwise returns the task and
/// what the walk left behind, for the caller to finalize.
pub(super) fn copy_with_progress(
    settings: CopySettings<'_>,
    prepared: Prepared,
    listed: &PathInfo,
    mut active: ActiveTask,
    old_path: &Path,
    new_path: &Path,
) -> Option<(ActiveTask, CopyOutcome)> {
    let is_directory = listed.is_directory();
    let total_size = if is_directory {
        let Some(size) = dir_total_size(&active, old_path) else {
            active.cancelled();
            return None;
        };
        active.set_total(size);
        size
    } else {
        listed.size
    };
    // One buffer for the whole tree: allocating it per file would zero a fresh
    // one for every small file in a large directory.
    let mut buffer = vec![0; COPY_BUFFER_BYTES];
    let mut context = CopyContext::new(settings, &mut buffer, prepared.source, total_size);
    let mut errors = Vec::new();
    if !copy_path(
        &mut context,
        &mut active,
        &mut errors,
        prepared.replace,
        listed,
        old_path,
        new_path,
    ) {
        cancel_logging(&errors, active);
        return None;
    }
    Some((active, context.into_outcome(errors)))
}

/// Best-effort recursive size for the progress total. Entries that cannot be
/// read are skipped here; the copy itself reports them as errors.
///
/// Returns `None` when the task was cancelled. The walk runs before any bytes
/// are copied and takes as long as the tree is large, so it observes the token
/// itself rather than leaving a cancel acknowledged but still running.
pub(super) fn dir_total_size(active: &ActiveTask, root: &Path) -> Option<u64> {
    let mut total: u64 = 0;
    scan_tree(active, root, |dir, name, is_directory| {
        if is_directory {
            return;
        }
        let stat = fstatat(dir, name, AtFlags::AT_SYMLINK_NOFOLLOW);
        if let Ok(stat) = stat
            && FileType::of(&stat) != FileType::Symlink
        {
            total = total.saturating_add(u64::try_from(stat.st_size).unwrap_or(0));
        }
    })?;
    Some(total)
}

/// The copy functions below follow coreutils `cp -R`/`mv` semantics: an entry
/// that cannot be copied is recorded in `errors` and the copy continues with
/// the remaining entries. Each returns `false` only when the task was
/// cancelled, in which case the caller must finalize with
/// `active.cancelled()`; otherwise the caller finalizes via `finalize`.
/// A cancelled copy leaves the partially copied destination in place, like an
/// interrupted `cp`; the destination is not removed. A replacement is the
/// exception: it is written beside the entry it replaces and removed when it
/// does not complete (`replace_entry`).
///
/// Every entry is read, created and changed relative to an open directory on
/// its side, never through a path, and nothing is opened through a symlink. An
/// entry swapped for a link while the copy runs therefore fails rather than
/// leading the copy out of the tree: reading a file outside the source, or
/// creating one or changing a mode outside the destination. Only the top-level
/// parents, which the user chose, are opened by path.
///
/// An entry's mode and owner are read from the file actually copied, never
/// from an earlier stat of its name: another file renamed over the name in
/// between would otherwise be given the first one's mode. `listed` is the
/// source as the task was started for it, and one that is no longer of its
/// type is refused. A non-directory replaces the entry `replace` names, if
/// given, whole or not at all (`replace_entry`), and a failure before that is
/// attempted says the entry was left.
fn copy_path(
    context: &mut CopyContext<'_>,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    replace: Option<Seen>,
    listed: &PathInfo,
    old_path: &Path,
    new_path: &Path,
) -> bool {
    let is_move = context.is_move;
    let verb = verb(is_move);
    let kept = if replace.is_some() {
        not_replaced(new_path)
    } else {
        String::new()
    };
    let failed =
        |error: &dyn std::fmt::Display| failed_transfer(is_move, old_path, new_path, error) + &kept;
    let (Some(src_name), Some(dst_name)) = (c_name(old_path), c_name(new_path)) else {
        errors.push(format!(
            "Cannot {verb} {}: path has no file name{kept}",
            compact(old_path)
        ));
        return true;
    };
    let opened = open_parent(old_path).and_then(|src| {
        // The file `prepare_destination` opened is the one copied, so its
        // metadata is the one that counts.
        let stat = match &context.source {
            Some(file) => fstat(file)?,
            None => fstatat(&src, src_name.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW)?,
        };
        Ok((src, open_parent(new_path)?, stat))
    });
    let (src_parent, dst_parent, stat) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            errors.push(failed(&error));
            return true;
        }
    };
    context.root = Some(EntryId::of_stat(&stat));
    if type_bits(stat_mode(&stat)) != type_bits(listed.mode()) {
        errors.push(format!(
            "Cannot {verb} {}: its type changed since it was selected{kept}",
            compact(old_path)
        ));
        return true;
    }
    let mut paths = Paths {
        old: old_path.to_path_buf(),
        new: new_path.to_path_buf(),
        depth: 0,
    };
    let at = At {
        src: &src_parent,
        dst: &dst_parent,
        src_name: &src_name,
        dst_name: &dst_name,
    };
    // Counted before the entry itself, so a skip there tells the skipped
    // top-level entry apart from one inside the tree.
    let skipped_before = context.skipped;
    if !listed.is_directory() {
        let finished = match replace {
            Some(granted) => replace_entry(context, active, errors, granted, &at, &paths, &stat),
            None => copy_entry(&at, &paths, active, errors, context, &stat),
        };
        context.top_skipped = context.skipped > skipped_before;
        return finished;
    }
    let Some(level) = enter_directory(&at, &paths, errors, context, &stat) else {
        context.top_skipped = context.skipped > skipped_before;
        return true;
    };
    // Only the directories being worked in are held open.
    drop((src_parent, dst_parent));
    copy_tree(level, &mut paths, active, errors, context)
}

/// Copies the directory tree below `root`, then finishes each directory: its
/// mode, and its times for a move. The source and destination are walked in
/// step, holding only the directories being worked in open (see `Walk`).
///
/// A cancel stops the walk between entries, and every directory entered is
/// still finished on the way out, so none is left owner-only.
fn copy_tree(
    root: CopyLevel,
    paths: &mut Paths,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
) -> bool {
    let mut walk = Walk::new(root);
    let mut cancelled = false;
    while let Some((Pair { src, dst }, level)) = walk.top() {
        cancelled |= active.is_cancelled();
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
                let verb = verb(context.is_move);
                let shape = if error.raw_os_error().is_some() {
                    "Failed to"
                } else {
                    "Cannot"
                };
                errors.push(format!("{shape} {verb} {}: {error}", compact(&paths.old)));
                return !cancelled;
            }
            continue;
        };
        paths.push(&name);
        let stat = match fstatat(src, name.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(error) => {
                errors.push(format!(
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
            if let Some(child) = enter_directory(&at, paths, errors, context, &stat) {
                walk.descend(child);
                // The child's path stays pushed until it is finished.
                continue;
            }
        } else if !copy_entry(&at, paths, active, errors, context, &stat) {
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
fn enter_directory(
    at: &At<'_>,
    paths: &Paths,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
    stat: &Stat,
) -> Option<CopyLevel> {
    let id = EntryId::of_stat(stat);
    if context.created.contains(&id) {
        errors.push(format!(
            "Cannot {} {}: it is inside the destination being written",
            verb(context.is_move),
            compact(&paths.old)
        ));
        return None;
    }
    let (dst, dst_id, umask_left) = make_directory(at, paths, errors, context, stat)?;
    context.created.insert(dst_id);
    context.mark_written(at, paths);
    // Abandons the subtree, leaving the destination directory empty and with
    // its final mode.
    let give_up = |errors: &mut Vec<String>, context: &CopyContext<'_>, message: String| {
        errors.push(message);
        finish_directory(context, paths, &dst, &Source::of(stat), umask_left);
    };
    let read_failure = |error: &dyn std::fmt::Display| {
        format!("Failed to read directory {}: {error}", compact(&paths.old))
    };
    let opened = open_directory(at.src, at.src_name).and_then(|src| Ok((fstat(&src)?, src)));
    let (opened, src) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            give_up(errors, context, read_failure(&error));
            return None;
        }
    };
    if EntryId::of_stat(&opened) != id {
        let message = format!(
            "Cannot {} {}: it was replaced while it was being read",
            verb(context.is_move),
            compact(&paths.old)
        );
        give_up(errors, context, message);
        return None;
    }
    let names = match list_names(&src) {
        Ok(names) => names,
        Err(error) => {
            give_up(errors, context, read_failure(&error));
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
/// describes and opens it, once it proves to be the one just made
/// (`open_created`). Returns it, which entry it is, and the mode the umask
/// left when owner access had to be added; `None` when it cannot be used,
/// having recorded why.
fn make_directory(
    at: &At<'_>,
    paths: &Paths,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
    stat: &Stat,
) -> Option<(File, EntryId, Option<u32>)> {
    let creation = if context.is_move {
        0o700
    } else {
        (stat_mode(stat) & 0o777) | 0o700
    };
    // What is created in the parent from here on is born no earlier than its
    // last change now (`floor_across`).
    let before = fstat(at.dst).map(|stat| changed(&stat));
    let created: std::io::Result<(i64, i64)> = match before {
        Err(errno) => Err(errno.into()),
        Ok(before) => match mkdirat(at.dst, at.dst_name, mode_bits(creation)) {
            // Taken since the name was free, which is never replaced
            // (`resolve_raced`); skipping drops the subtree.
            Err(Errno::EEXIST) => {
                resolve_raced(context, errors, paths);
                return None;
            }
            result => result.map(|()| before).map_err(Into::into),
        },
    };
    #[cfg(test)]
    if created.is_ok()
        && let Some(on_made) = context.on_made.as_mut()
    {
        on_made(&paths.new);
    }
    let opened = created.and_then(|before| {
        let floor = floor_across(before, changed(&fstat(at.dst)?));
        open_created(at.dst, at.dst_name, floor)
    });
    match opened {
        Ok(opened) => Some(opened),
        // Another directory swapped in at the name: nothing is written into
        // it and its mode is left as it was. An empty one is removed, as
        // anyone who can write the parent could remove it; one with entries
        // is not this copy's to touch.
        Err(error) if NotNew::is(&error) => {
            let _ = unlink_at(at.dst, at.dst_name, UnlinkatFlags::RemoveDir);
            errors.push(format!(
                "Cannot {} {} to {}: {error}",
                verb(context.is_move),
                compact(&paths.old),
                compact(&paths.new)
            ));
            None
        }
        Err(error) => {
            // The subtree cannot be copied at all; skip it and continue with
            // the siblings.
            errors.push(format!(
                "Failed to create directory {}: {error}",
                compact(&paths.new)
            ));
            None
        }
    }
}

/// Opens the directory `name` a copy just created in `parent`, once it proves
/// to be that directory (`open_new_directory`, against `floor`), with owner
/// access added when the umask or a default ACL took it away: a umask such as
/// `0o277` leaves it unwritable, and no child could be created in it. `cp`
/// does the same. Returns it, which entry it is, and the mode the umask left
/// when access was added, which `finish_directory` puts back before computing
/// the final mode from it.
fn open_created(
    parent: &File,
    name: &CStr,
    floor: Option<(i64, i64)>,
) -> std::io::Result<(File, EntryId, Option<u32>)> {
    let (dir, id, had) = open_new_directory(parent, name, floor, |had| had | 0o700)?;
    let left = had.or_else(|| grant_owner_access(&dir));
    Ok((dir, id, left))
}

/// Opens the directory `name` just made in `parent` and checks that it is
/// that directory (`is_new_empty_directory`, against `floor`): another user
/// who can write `parent` could otherwise swap one of this user's own
/// directories in at the name, which the copy would then write into and
/// change the mode of. Returns it, which entry it is, and the mode it had
/// when that was changed to open it.
///
/// A directory left without owner read or search (a umask of `0o477`, a
/// default ACL of `u::---`) cannot be opened to check it, so it is held
/// through a handle that needs no permission, checked there, and given the
/// mode `grant` makes of the one it has (`open_unreadable`); that mode is put
/// back if it then proves not to be empty.
fn open_new_directory(
    parent: &File,
    name: &CStr,
    floor: Option<(i64, i64)>,
    grant: impl FnOnce(u32) -> u32,
) -> std::io::Result<(File, EntryId, Option<u32>)> {
    let (dir, had) = match open_directory(parent, name) {
        Err(error) if error.raw_os_error() == Some(nix::libc::EACCES) => {
            let admit = |held: &std::os::fd::OwnedFd| {
                if is_new_empty_directory(Seen::of_handle(held)?.created(), floor, true) {
                    Ok(())
                } else {
                    Err(NotNew::error())
                }
            };
            let (dir, had) = open_unreadable(parent, name, admit, grant)
                .map_err(|refused| if NotNew::is(&refused) { refused } else { error })?;
            (dir, Some(had))
        }
        opened => (opened?, None),
    };
    let seen = Seen::of_handle(&dir)?;
    if !is_new_empty_directory(seen.created(), floor, is_empty_directory(&dir)?) {
        if let Some(had) = had {
            let _ = nix::sys::stat::fchmod(&dir, mode_bits(had));
        }
        return Err(NotNew::error());
    }
    Ok((dir, seen.id(), had))
}

/// The refusal of a directory found where one was just made that proves not
/// to be it (`is_new_empty_directory`): another swapped in at its name.
#[derive(Debug)]
struct NotNew;

impl std::fmt::Display for NotNew {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the directory it was creating was replaced")
    }
}

impl std::error::Error for NotNew {}

impl NotNew {
    fn error() -> std::io::Error {
        std::io::Error::other(Self)
    }

    fn is(error: &std::io::Error) -> bool {
        matches!(error.get_ref(), Some(inner) if inner.is::<Self>())
    }
}

/// Adds owner access to the open directory `dst` when it lacks it, returning
/// the mode it had.
fn grant_owner_access(dst: &File) -> Option<u32> {
    let mode = stat_mode(&fstat(dst).ok()?) & 0o7777;
    if mode & 0o700 == 0o700 {
        return None;
    }
    nix::sys::stat::fchmod(dst, mode_bits(mode | 0o700)).ok()?;
    Some(mode)
}

/// Opens the directory `name` in `parent` that its owner cannot read, by
/// giving it the mode `mode` makes of the one it has, and returns it with
/// the mode it had. Nothing is changed by name: the directory is held first
/// through an `O_PATH` handle, which needs no permission on it and does not
/// follow a symlink at the name, `admit` sees it through that handle, the
/// mode is set on the entry that handle holds (through `/proc/self/fd`,
/// which names the entry rather than a path another process could swap),
/// and the open is of that same entry (`.` below the handle). Linux only:
/// elsewhere there is no such handle, and the directory stays unreadable
/// (`EACCES`), as it does where `/proc` is not mounted.
#[cfg(target_os = "linux")]
fn open_unreadable(
    parent: &File,
    name: &CStr,
    admit: impl FnOnce(&std::os::fd::OwnedFd) -> std::io::Result<()>,
    mode: impl FnOnce(u32) -> u32,
) -> std::io::Result<(File, u32)> {
    use std::os::fd::AsRawFd;
    let held = openat(
        parent,
        name,
        OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    admit(&held)?;
    let had = stat_mode(&fstat(&held)?) & 0o7777;
    let by_handle = format!("/proc/self/fd/{}", held.as_raw_fd());
    fs::set_permissions(&by_handle, fs::Permissions::from_mode(mode(had)))?;
    let dir = open_directory(&held, c".")?;
    if EntryId::of(&dir)? != EntryId::of(&held)? {
        return Err(std::io::Error::from(Errno::EACCES));
    }
    Ok((dir, had))
}

#[cfg(not(target_os = "linux"))]
fn open_unreadable(
    _parent: &File,
    _name: &CStr,
    _admit: impl FnOnce(&std::os::fd::OwnedFd) -> std::io::Result<()>,
    _mode: impl FnOnce(u32) -> u32,
) -> std::io::Result<(File, u32)> {
    Err(std::io::Error::from(Errno::EACCES))
}

/// Sets the mode of `name` in `parent` without following a symlink at the
/// name. `EOPNOTSUPP` is returned as it is, never retried with a change that
/// follows: it is what a symlink at the name answers, as well as a system
/// that cannot change a mode without following (glibc before 2.32), as
/// `operations::set_mode_without_following` does.
fn set_mode_by_name(parent: &File, name: &CStr, mode: u32) -> std::io::Result<()> {
    use nix::sys::stat::{FchmodatFlags, fchmodat};
    Ok(fchmodat(
        parent,
        name,
        mode_bits(mode),
        FchmodatFlags::NoFollowSymlink,
    )?)
}

/// Gives a copied directory its final mode, and for a move the source's
/// times, now that its children are written: writing them is what moved its
/// own modification time, and a mode without owner-write would have stopped
/// them being created. Through the handle, so a path swapped since cannot
/// redirect either. `umask_left` is the mode `grant_owner_access` replaced.
fn finish_directory(
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

/// Copies the non-directory entry `at` names, of the type `stat` gives it: a
/// symlink, a regular file, or a special file. Returns `false` only when
/// cancelled.
fn copy_entry(
    at: &At<'_>,
    paths: &Paths,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
    stat: &Stat,
) -> bool {
    // The type comes from an `lstat`, so a symlink (even one pointing at a
    // directory) is recreated as a link rather than followed.
    match FileType::of(stat) {
        FileType::Symlink => {
            copy_symlink(at, paths, errors, context);
            true
        }
        FileType::RegularFile => copy_file(at, paths, active, errors, context),
        _ => {
            copy_special(at, paths, errors, context, stat);
            true
        }
    }
}

/// Recreates the symlink `at` names, pointing at the same (possibly relative,
/// possibly dangling) target. The target is never followed, so no bytes are
/// transferred and no permissions are applied.
fn copy_symlink(
    at: &At<'_>,
    paths: &Paths,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
) {
    let target = match readlinkat(at.src, at.src_name) {
        Ok(read) => read,
        Err(error) => {
            errors.push(format!(
                "Failed to read symlink {}: {error}",
                compact(&paths.old)
            ));
            return;
        }
    };
    match symlinkat(target.as_os_str(), at.dst, at.dst_name) {
        Ok(()) => context.mark_written(at, paths),
        Err(Errno::EEXIST) => resolve_raced(context, errors, paths),
        Err(error) => {
            let own = || format!("Failed to create symlink {}: {error}", compact(&paths.new));
            errors.push(context.written_failure(paths, own, &error));
        }
    }
}

/// Copies a file chunk-by-chunk, sending debounced progress updates via
/// `active`. The destination is never more readable than the source while it
/// is written (see `create_file_at`), and gets its final mode through the
/// handle once the copy stops, however it stops. Failures are recorded in
/// `errors`; returns `false` only when cancelled.
///
/// The mode and owner applied are those of the file opened.
fn copy_file(
    at: &At<'_>,
    paths: &Paths,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
) -> bool {
    let failed = |error: &dyn std::fmt::Display| {
        failed_transfer(context.is_move, &paths.old, &paths.new, error)
    };
    // Opened before the destination is created, so a source that cannot be
    // read leaves nothing behind.
    let opened = match context.source.take() {
        Some(file) => Ok(file),
        None => open_source_file(at.src, at.src_name),
    };
    let opened = opened.and_then(|file| Ok((fstat(&file)?, file)));
    let (stat, mut old_file) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            errors.push(failed(&error));
            return true;
        }
    };
    let source = &Source::read(context.is_move, &paths.old, &old_file, &stat);
    let mut new_file = match create_file_at(at.dst, at.dst_name, source.mode, context.is_move) {
        Ok(file) => {
            context.mark_written(at, paths);
            file
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            resolve_raced(context, errors, paths);
            return true;
        }
        Err(error) => {
            errors.push(failed(&error));
            return true;
        }
    };

    let not_cancelled = loop {
        if active.is_cancelled() {
            // Like interrupted `cp`: leave the partially written destination
            // file in place rather than removing it.
            break false;
        }

        match old_file.read(context.buffer) {
            Ok(0) => {
                // Before `new_file` is dropped: writing is what moves the
                // modification time, so it has to be restored once the last
                // byte is written.
                if context.is_move {
                    apply_times(&paths.new, source, &new_file);
                }
                break true;
            }
            Ok(bytes) => match new_file.write_all(&context.buffer[..bytes]) {
                Ok(()) => {
                    active.increment(bytes as u64);
                    if context
                        .progress
                        .should_trigger(Instant::now(), bytes as u64)
                    {
                        active.send_progress();
                    }
                }
                Err(error) => {
                    let own = || format!("Failed to write {}: {error}", compact(&paths.new));
                    errors.push(context.written_failure(paths, own, &error));
                    break true;
                }
            },
            Err(error) => {
                errors.push(format!("Failed to read {}: {error}", compact(&paths.old)));
                break true;
            }
        }
    };
    apply_final_mode(context, paths, &new_file, source);
    not_cancelled
}

/// Recreates a special file (FIFO, socket, or device node) as a fresh node
/// with the source's permission bits, like `cp -R` does. No bytes are
/// transferred: reading a FIFO would block until a writer appears. FIFOs and
/// sockets need no privileges; device nodes require root, so as a normal user
/// they record a "not permitted" error here, exactly as `cp` reports.
fn copy_special(
    at: &At<'_>,
    paths: &Paths,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
    stat: &Stat,
) {
    let file_type = FileType::of(stat);
    if !matches!(
        file_type,
        FileType::Fifo | FileType::Socket | FileType::BlockDevice | FileType::CharacterDevice
    ) {
        errors.push(format!(
            "Cannot {} {}: unsupported file type",
            verb(context.is_move),
            compact(&paths.old)
        ));
        return;
    }
    // Everything comes from the one `lstat` the type came from. Device nodes
    // need the source's device numbers; the rest take zero.
    let path = context.node_path(paths);
    match make_node(at, &path, file_type, stat_mode(stat), stat.st_rdev) {
        Ok(()) => context.mark_written(at, paths),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            resolve_raced(context, errors, paths);
            return;
        }
        Err(error) => {
            let own = || {
                format!(
                    "Failed to create special file {}: {error}",
                    compact(&paths.new)
                )
            };
            errors.push(context.written_failure(paths, own, &error));
            return;
        }
    }
    if context.is_move {
        // The source's group first, as a moved file or directory gets it, so the
        // group bits restored below are granted to the group they were granted
        // to before. Best effort, the same way.
        let _ = nix::unistd::fchownat(
            at.dst,
            at.dst_name,
            None,
            Some(Gid::from_raw(stat.st_gid)),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        );
        restore_node_mode(context, at, paths, errors, stat_mode(stat));
    }
}

/// Gives a node a move created the source's permission bits, which the umask
/// trimmed at creation and `mv` keeps. By name, since a FIFO cannot be opened
/// without blocking, and without following a link swapped in at the name since,
/// as `operations::set_mode_without_following` does. A filesystem that cannot
/// set a mode that way leaves the node as created, with a warning.
fn restore_node_mode(
    context: &CopyContext<'_>,
    at: &At<'_>,
    paths: &Paths,
    errors: &mut Vec<String>,
    source_mode: u32,
) {
    let Err(error) = set_mode_by_name(at.dst, at.dst_name, source_mode & 0o777) else {
        return;
    };
    if error.raw_os_error() == Some(nix::libc::EOPNOTSUPP) {
        warn!("Failed to set the mode of {}: {error}", compact(&paths.new));
    } else if context.staging.is_some() {
        // The entry at the name is not what failed; `replace_entry` says it
        // was left.
        errors.push(format!(
            "Failed to set the mode of the replacement for {}: {error}",
            compact(&paths.new)
        ));
    } else {
        errors.push(format!(
            "Failed to chmod {} to {:o}: {error}",
            compact(&paths.new),
            source_mode & 0o777
        ));
    }
}

/// Creates the node `at` names in the destination directory. `_path` is where
/// that is, which only macOS needs.
#[cfg(not(target_os = "macos"))]
fn make_node(
    at: &At<'_>,
    _path: &Path,
    file_type: FileType,
    source_mode: u32,
    device: nix::libc::dev_t,
) -> std::io::Result<()> {
    let (kind, device) = node_kind(file_type, device);
    Ok(nix::sys::stat::mknodat(
        at.dst,
        at.dst_name,
        kind,
        mode_bits(source_mode & 0o777),
        device,
    )?)
}

/// Creates the node `at` names, at `path`: macOS has no `mknodat`. A parent
/// swapped for a symlink since it was opened could place the node outside the
/// tree; an empty FIFO or socket there carries nothing, and a device node
/// needs root.
#[cfg(target_os = "macos")]
fn make_node(
    _at: &At<'_>,
    path: &Path,
    file_type: FileType,
    source_mode: u32,
    device: nix::libc::dev_t,
) -> std::io::Result<()> {
    let (kind, device) = node_kind(file_type, device);
    Ok(nix::sys::stat::mknod(
        path,
        kind,
        mode_bits(source_mode & 0o777),
        device,
    )?)
}

/// The node type `mknod` takes for `file_type`, and the device numbers it
/// takes with it: a device node the source's, anything else zero.
fn node_kind(
    file_type: FileType,
    device: nix::libc::dev_t,
) -> (nix::sys::stat::SFlag, nix::libc::dev_t) {
    use nix::sys::stat::SFlag;

    match file_type {
        FileType::BlockDevice => (SFlag::S_IFBLK, device),
        FileType::CharacterDevice => (SFlag::S_IFCHR, device),
        FileType::Socket => (SFlag::S_IFSOCK, 0),
        _ => (SFlag::S_IFIFO, 0),
    }
}

/// The file type bits of `mode`.
fn type_bits(mode: u32) -> u32 {
    mode & 0o170_000
}

/// Opens a regular file to copy from. `O_NOFOLLOW` refuses a symlink swapped
/// in since the entry was listed, and `O_NONBLOCK` keeps a FIFO swapped in
/// from blocking the open (and with it the worker every operation shares);
/// anything that is not a regular file is then refused before a byte is read,
/// so a device such as `/dev/zero` cannot be read without end either.
fn open_source_file(dir: impl AsFd, name: &CStr) -> std::io::Result<File> {
    let flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC;
    let fd = openat(dir, name, flags, Mode::empty())?;
    if FileType::of(&fstat(&fd)?) != FileType::RegularFile {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "it is no longer a regular file",
        ));
    }
    let status = OFlag::from_bits_retain(fcntl(&fd, FcntlArg::F_GETFL)?);
    fcntl(&fd, FcntlArg::F_SETFL(status - OFlag::O_NONBLOCK))?;
    Ok(File::from(fd))
}

/// Creates the destination of a file copy. `O_EXCL` fails atomically if the
/// name is taken, closing the same window as `rename_no_replace`: without it a
/// file that appeared since `validate_paths` ran would be truncated, and
/// `O_NOFOLLOW` keeps a symlink planted at the name from being written through.
///
/// A copy is created with the source's permission bits, which the umask trims
/// as it does for `cp`: the file is never readable by more users than the
/// source, even part way through. A move is created with the source's owner
/// bits only and given its full mode at the end, since `mv` keeps the mode
/// whatever the umask.
fn create_file_at(
    dir: impl AsFd,
    name: &CStr,
    source_mode: u32,
    is_move: bool,
) -> std::io::Result<File> {
    let creation = if is_move {
        (source_mode & 0o700) | 0o600
    } else {
        source_mode & 0o777
    };
    let flags =
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    Ok(File::from(openat(dir, name, flags, mode_bits(creation))?))
}

/// Applies the mode a copied entry ends with, through its handle.
///
/// A copy keeps the permission bits the umask left at creation, and never the
/// source's setuid, setgid or sticky bits, like `cp` without `-p`: a file
/// copied into a directory someone else can reach must not become a setuid
/// program of the user who copied it. A directory keeps the setgid bit it
/// inherited from its parent. The creation mode had owner access added so the entry
/// could be written, so the source's owner bits are put back.
///
/// A move keeps the full mode, like `mv`, except setuid and setgid when the
/// copy is not owned by the source's user and group: `mv` clears them when it
/// cannot carry the ownership over, or a program would run as whoever moved it.
/// It first gives the copy the source's group, as `mv` does, so the group bits
/// it keeps are granted to the group they were granted to before rather than
/// to whichever group the destination assigned. Best effort: a user can only
/// give a file a group they belong to, and the comparison below then clears
/// setgid for a group that did not carry over.
///
/// A move also gets the source's extended attributes, like `mv`. They include
/// the POSIX ACLs on Linux, without which the named users and groups an ACL
/// grants would be dropped and the mask, which is what the source's group bits
/// hold, granted to the owning group instead. Before the mode: an ACL sets the
/// mode's permission bits from its own entries, and the mode then sets the
/// mask from the group bits, which are the source's mask, so the order changes
/// nothing but closes the window in which the owning group has the mask's
/// access. Best effort, since a destination that cannot hold an attribute (no
/// ACLs on FAT, a namespace only root may write) is no reason to fail the move.
fn apply_final_mode(context: &CopyContext<'_>, paths: &Paths, file: &File, source: &Source) {
    if context.is_move {
        let _ = fchown(file, None, Some(Gid::from_raw(source.gid)));
        for (name, value) in &source.attributes {
            if let Err(error) = rustix::fs::fsetxattr(file, name, value, XattrFlags::empty()) {
                warn!(
                    "Failed to copy the extended attribute {} to {}: {error}",
                    crate::visible_os(OsStr::from_bytes(name.to_bytes())),
                    compact(&paths.new)
                );
            }
        }
    }
    let Ok(created) = fstat(file) else {
        return;
    };
    let created_mode = stat_mode(&created) & 0o777;
    let mode = if context.is_move {
        let special = if created.st_uid == source.uid && created.st_gid == source.gid {
            0o7000
        } else {
            0o1000
        };
        source.mode & (0o777 | special)
    } else {
        // A directory created in a setgid directory inherits the setgid bit,
        // which `cp -R` keeps so what is later created in it takes the shared
        // group.
        let inherited = if FileType::of(&created) == FileType::Directory {
            stat_mode(&created) & 0o2000
        } else {
            0
        };
        (created_mode & 0o077) | (source.mode & created_mode & 0o700) | inherited
    };
    if let Err(error) = file.set_permissions(fs::Permissions::from_mode(mode)) {
        warn!("Failed to set the mode of {}: {error}", compact(&paths.new));
    }
}

/// Gives the open `target`, the copy at `path`, `source`'s access and
/// modification times. Best effort: a filesystem that cannot record them is
/// not a reason to fail the operation.
fn apply_times(path: &Path, source: &Source, target: &File) {
    let Some(times) = &source.times else {
        return;
    };
    if let Err(error) = nix::sys::stat::futimens(target, &times.access, &times.modification) {
        warn!("Failed to set the times of {}: {error}", compact(path));
    }
}

/// An entry to copy, named relative to an open directory on each side.
struct At<'a> {
    src: &'a File,
    dst: &'a File,
    src_name: &'a CStr,
    dst_name: &'a CStr,
}

/// The paths of the entry being copied, for messages only: every system call
/// goes through an `At`. One pair for the whole walk, extended on the way down
/// and cut back on the way up, so memory does not grow with depth squared.
struct Paths {
    old: PathBuf,
    new: PathBuf,
    /// How far below the top-level entry these are: 0 for the entry itself.
    depth: usize,
}

impl Paths {
    fn push(&mut self, name: &CStr) {
        let name = OsStr::from_bytes(name.to_bytes());
        self.old.push(name);
        self.new.push(name);
        self.depth += 1;
    }

    fn pop(&mut self) {
        self.old.pop();
        self.new.pop();
        self.depth -= 1;
    }
}

/// What the copy needs of a source entry: its mode, its owner for deciding
/// whether a move keeps the setuid and setgid bits, and for a move its times
/// and extended attributes. Taken from the entry before the copy reads it,
/// since reading moves its access time.
struct Source {
    mode: u32,
    uid: u32,
    gid: u32,
    times: Option<Times>,
    /// Each extended attribute the source has, by name.
    attributes: Vec<(CString, Vec<u8>)>,
}

impl Source {
    fn of(stat: &Stat) -> Self {
        Self {
            mode: stat_mode(stat),
            uid: stat.st_uid,
            gid: stat.st_gid,
            times: times_of(stat),
            attributes: Vec::new(),
        }
    }

    /// `of` the open `file`, whose `stat` it is, with its extended attributes
    /// for a move. `path` names it in a warning.
    fn read(is_move: bool, path: &Path, file: &File, stat: &Stat) -> Self {
        let mut source = Self::of(stat);
        if is_move {
            let is_directory = FileType::of(stat) == FileType::Directory;
            source.attributes = read_attributes(path, is_directory, file);
        }
        source
    }
}

/// How many times a size-then-read is retried when the value grew in between.
const SIZE_ATTEMPTS: usize = 3;

/// A variable-length value read by asking `read` for its size (an empty
/// buffer) and then reading it. `ERANGE` means it grew in between, so it is
/// measured again, a bounded number of times. A value measured empty is
/// empty: an empty buffer would only measure it again.
fn read_sized(
    read: impl Fn(&mut [u8]) -> rustix::io::Result<usize>,
) -> rustix::io::Result<Vec<u8>> {
    for _ in 0..SIZE_ATTEMPTS {
        let size = read(&mut [])?;
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut buffer = vec![0; size];
        match read(&mut buffer) {
            Ok(len) => {
                buffer.truncate(len);
                return Ok(buffer);
            }
            Err(rustix::io::Errno::RANGE) => {}
            Err(error) => return Err(error),
        }
    }
    Err(rustix::io::Errno::RANGE)
}

/// The POSIX ACLs, which carry permissions, so they are still asked for by
/// name when the full list cannot be read; `true` marks the one only a
/// directory has.
#[cfg(target_os = "linux")]
const ACL_ATTRIBUTES: [(&CStr, bool); 2] = [
    (c"system.posix_acl_access", false),
    (c"system.posix_acl_default", true),
];
#[cfg(not(target_os = "linux"))]
const ACL_ATTRIBUTES: [(&CStr, bool); 0] = [];

/// The ACL attribute names an entry of this kind can have.
fn acl_names(is_directory: bool) -> Vec<CString> {
    ACL_ATTRIBUTES
        .into_iter()
        .filter(|&(_, directory_only)| is_directory || !directory_only)
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// Every extended attribute of `file` with its value. Best effort: one that
/// cannot be read is not copied. When the list itself cannot be read, the ACLs
/// are still read by name where the platform has them. Listing reported as
/// unsupported is not warned about: it is what a filesystem without extended
/// attributes answers (FAT), where the reads by name then fail too, and also
/// what one that serves reading an attribute but not listing them does (some
/// FUSE filesystems). `path` names `file` in a warning.
fn read_attributes(path: &Path, is_directory: bool, file: &File) -> Vec<(CString, Vec<u8>)> {
    let listed = read_sized(|buffer| rustix::fs::flistxattr(file, buffer));
    attribute_names(path, is_directory, listed)
        .into_iter()
        .filter_map(|name| {
            let value = read_attribute(file, &name)?;
            Some((name, value))
        })
        .collect()
}

/// The attribute names to read, from what listing those of `path` returned.
fn attribute_names(
    path: &Path,
    is_directory: bool,
    listed: rustix::io::Result<Vec<u8>>,
) -> Vec<CString> {
    match listed {
        Ok(list) => list
            .split(|&byte| byte == 0)
            .filter_map(|name| CString::new(name).ok().filter(|name| !name.is_empty()))
            .collect(),
        Err(error) => {
            let names = acl_names(is_directory);
            if error != rustix::io::Errno::NOTSUP {
                let copied = if names.is_empty() {
                    "none are copied"
                } else {
                    "only its ACLs are copied"
                };
                warn!(
                    "Failed to list the extended attributes of {}: {error}; {copied}",
                    compact(path)
                );
            }
            names
        }
    }
}

/// The value of the extended attribute `name` on `file`, or `None` when it has
/// none or it cannot be read.
fn read_attribute(file: &File, name: &CStr) -> Option<Vec<u8>> {
    read_sized(|buffer| rustix::fs::fgetxattr(file, name, buffer)).ok()
}

/// An entry's access and modification times, as `futimens` takes them.
struct Times {
    access: TimeSpec,
    modification: TimeSpec,
}

/// `stat`'s access and modification times, in the form `futimens` takes.
/// `None` only where one does not fit it, which no real file reaches.
// The field types vary by target, and on some they already are the ones
// `TimeSpec` has.
#[allow(clippy::useless_conversion, clippy::unnecessary_fallible_conversions)]
fn times_of(stat: &Stat) -> Option<Times> {
    Some(Times {
        access: TimeSpec::new(
            stat.st_atime.try_into().ok()?,
            stat.st_atime_nsec.try_into().ok()?,
        ),
        modification: TimeSpec::new(
            stat.st_mtime.try_into().ok()?,
            stat.st_mtime_nsec.try_into().ok()?,
        ),
    })
}

/// A source directory and the destination directory it is copied into, or
/// their identities: `copy_tree` walks the two in step.
#[derive(Clone, Copy)]
struct Pair<T> {
    src: T,
    dst: T,
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
struct Copying {
    source: Source,
    /// The mode the umask left on the destination, when owner access was
    /// added over it.
    umask_left: Option<u32>,
    names: std::vec::IntoIter<CString>,
}

/// A directory `copy_tree` has entered.
type CopyLevel = Level<Pair<File>, Copying>;

/// Settles an entry's destination name (`paths.new`), taken since the queue
/// saw it free: when the task started for the top-level entry, and at any time
/// inside a directory this copy created. Nothing decided to replace what holds
/// it now (another program, another paste, or a name the filesystem folds
/// onto one this copy or paste wrote), so it is never replaced. A standing
/// "skip all" skips it, which also makes a move keep its source; otherwise it
/// is recorded like any other entry that could not be written.
fn resolve_raced(context: &mut CopyContext<'_>, errors: &mut Vec<String>, paths: &Paths) {
    // The staging directory was created empty for the one entry, so a name
    // taken in it is no race another paste or program could win fairly: it
    // is refused, never skipped, and what holds it is never landed.
    if paths.depth == 0 && context.staging.is_some() {
        errors.push(staging_changed(context.is_move, paths));
    } else if context.conflicts.skips_raced() {
        context.skipped += 1;
    } else if paths.depth == 0 {
        errors.push(raced_refusal(context.is_move, &paths.old, &paths.new));
    } else {
        errors.push(raced_in_copy_refusal(
            context.is_move,
            &paths.old,
            &paths.new,
        ));
    }
}

/// Copies the top-level entry `at` names over the entry `granted` describes,
/// which a granted overwrite lets it replace, whole or not at all.
///
/// The copy is made in a directory of its own beside the destination
/// (`Staging`) and renamed over the name only once it is complete, and only
/// if `granted` still holds the name (`land`). The entry granted is never
/// removed before its replacement exists, so a copy that fails, is cancelled,
/// or finds the name changed leaves it as it was, which a failure says, and
/// the staging directory is removed with what the copy made in it
/// (`clear_staging`). Returns `false` only when cancelled.
fn replace_entry(
    context: &mut CopyContext<'_>,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    granted: Seen,
    at: &At<'_>,
    paths: &Paths,
    stat: &Stat,
) -> bool {
    let parent = paths.new.parent().unwrap_or(Path::new("."));
    match Staging::create(at.dst, parent, staging_names(), at.dst_name) {
        Ok(staging) => {
            let replacement = Replacement { granted, staging };
            replace_through(context, active, errors, replacement, at, paths, stat)
        }
        Err(error) => {
            errors.push(
                refused_or_failed(context.is_move, paths, &error) + &not_replaced(&paths.new),
            );
            true
        }
    }
}

/// What a granted overwrite replaces an entry with: the entry `granted`
/// describes, and the directory its replacement is written into.
struct Replacement<'a> {
    granted: Seen,
    staging: Staging<'a>,
}

/// `replace_entry` once its staging directory is made: copies the entry into
/// it, lands the copy when it is whole, and removes the staging directory.
fn replace_through(
    context: &mut CopyContext<'_>,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    replacement: Replacement<'_>,
    at: &At<'_>,
    paths: &Paths,
    stat: &Stat,
) -> bool {
    let Replacement {
        granted,
        mut staging,
    } = replacement;
    let staged = At {
        src: at.src,
        dst: &staging.dir,
        src_name: at.src_name,
        dst_name: at.dst_name,
    };
    let errors_before = errors.len();
    context.staging = Some(staging.path.clone());
    let finished = copy_entry(&staged, paths, active, errors, context, stat);
    context.staging = None;
    staging.staged = context.staged.take();
    let kept = not_replaced(&paths.new);
    for error in &mut errors[errors_before..] {
        error.push_str(&kept);
    }
    // A staged copy is never skipped: a name taken in its staging directory
    // is refused (`resolve_raced`), so only an error or a cancel stops it.
    if finished && errors.len() == errors_before {
        match land(context, &staging, granted, at, paths) {
            Ok(true) => context.wrote = true,
            Ok(false) => {}
            Err(message) => errors.push(message),
        }
    }
    clear_staging(active, staging);
    finished
}

/// How an error making the staging directory reads: filectrl's own refusal
/// (no errno) in the `Cannot` form, anything else as the transfer that failed.
fn refused_or_failed(is_move: bool, paths: &Paths, error: &std::io::Error) -> String {
    if error.raw_os_error().is_none() {
        format!(
            "Cannot {} {} to {}: {error}",
            verb(is_move),
            compact(&paths.old),
            compact(&paths.new)
        )
    } else {
        failed_transfer(is_move, &paths.old, &paths.new, error)
    }
}

/// The refusal of an entry whose staging directory holds something the copy
/// did not put there, which is neither landed nor removed.
fn staging_changed(is_move: bool, paths: &Paths) -> String {
    format!(
        "Cannot {} {} to {}: its staging directory was changed",
        verb(is_move),
        compact(&paths.old),
        compact(&paths.new)
    )
}

/// Removes `staging`, and when it cannot be, says where it was left: in the
/// log and as a warning, since a hidden directory holding a partial copy is
/// otherwise found only by chance.
fn clear_staging(active: &ActiveTask, staging: Staging<'_>) {
    let path = staging.path.clone();
    if let Err(error) = staging.remove() {
        let message = format!(
            "Failed to remove the staging directory {}: {error}",
            compact(&path)
        );
        warn!("{message}");
        active.warn(message);
    }
}

/// Renames the replacement staged under `at`'s name in `staging` onto that
/// name in the destination: over the entry `granted`, if it still holds the
/// name (`still_holds`), and without replacing anything if the name is free
/// now, so an entry that took it since is refused rather than replaced.
/// Only the entry the copy made is landed: another at its name in the staging
/// directory is refused. Another program replacing the entry between the
/// check and the rename is not detected. `Ok(false)` when a standing "skip
/// all" skipped a name taken since (`landed`); the error is the whole message
/// for the task's errors.
fn land(
    context: &mut CopyContext<'_>,
    staging: &Staging<'_>,
    granted: Seen,
    at: &At<'_>,
    paths: &Paths,
) -> Result<bool, String> {
    let kept = not_replaced(&paths.new);
    if staging.staged.is_none() || staging.holds_staged() != staging.staged {
        return Err(staging_changed(context.is_move, paths) + &kept);
    }
    let found = Seen::at(at.dst, at.dst_name).map_err(|error| {
        failed_transfer(context.is_move, &paths.old, &paths.new, &error) + &kept
    })?;
    let replaces = match still_holds(granted, found) {
        Holds::Granted => true,
        Holds::Free => false,
        Holds::Changed => return Err(changed_refusal(context.is_move, &paths.old, &paths.new)),
    };
    let renamed = rename_staged(replaces, &staging.dir, at.dst, at.dst_name);
    landed(context, paths, replaces, renamed)
}

/// What the rename that lands a replacement (`renamed`) means. A name taken
/// since the check, which only a rename that replaces nothing can find, is
/// skipped under a standing "skip all" (`Ok(false)`), as a raced name
/// anywhere else is, and refused as raced otherwise. Any other failure is
/// one, and only a rename that `replaces` the entry granted leaves that entry
/// at the name, which the message then says.
fn landed(
    context: &mut CopyContext<'_>,
    paths: &Paths,
    replaces: bool,
    renamed: std::io::Result<()>,
) -> Result<bool, String> {
    match renamed {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            if context.conflicts.skips_raced() {
                context.skipped += 1;
                Ok(false)
            } else {
                Err(raced_refusal(context.is_move, &paths.old, &paths.new))
            }
        }
        Err(error) => {
            let failed = failed_transfer(context.is_move, &paths.old, &paths.new, &error);
            if replaces {
                Err(failed + &not_replaced(&paths.new))
            } else {
                Err(failed)
            }
        }
    }
}

/// Renames the entry `name` in `staging` to `name` in `dst`: over what holds
/// it when `replaces`, with a plain `renameat`, which works where `renameat2`
/// is missing, and otherwise without replacing anything (`AlreadyExists` when
/// the name is taken).
fn rename_staged(replaces: bool, staging: &File, dst: &File, name: &CStr) -> std::io::Result<()> {
    if replaces {
        Ok(rustix::fs::renameat(staging, name, dst, name)?)
    } else {
        rename_no_replace_at(staging, name, dst, name)
    }
}

/// How many names `staging_names` offers before giving up.
const STAGING_ATTEMPTS: u64 = 16;

/// Names for a staging directory, hidden and unique to this process and call.
fn staging_names() -> impl Iterator<Item = CString> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let pid = std::process::id();
    (0..STAGING_ATTEMPTS).map(move |_| {
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        CString::new(format!(".filectrl-{pid}-{serial}")).expect("the name holds no NUL")
    })
}

/// A directory of its own beside a destination, owner-only, which a
/// replacement named `entry` is written into before it takes the
/// destination's name. It is created with the first of the names offered
/// that is free, so an entry that already has one is never touched, and it is
/// removed (`remove`, or when dropped) with the entry the copy made in it
/// (`staged`) if that is still there: through its own handle, and then by
/// name, which only removes an empty directory and only while the name still
/// holds this one (`unlink_all`).
struct Staging<'a> {
    parent: &'a File,
    name: CString,
    /// Where it is, for what is made by path and for a warning.
    path: PathBuf,
    dir: File,
    /// Which directory `dir` is.
    id: EntryId,
    entry: CString,
    /// The entry the copy created at `entry`, the only one landed or removed.
    staged: Option<EntryId>,
    removed: bool,
}

impl<'a> Staging<'a> {
    /// Creates a staging directory in `parent`, which is at `parent_path`.
    /// Having no free name is filectrl's own refusal, an error with no errno.
    fn create(
        parent: &'a File,
        parent_path: &Path,
        names: impl IntoIterator<Item = CString>,
        entry: &CStr,
    ) -> std::io::Result<Self> {
        for name in names {
            // What is created in `parent` from here on is born no earlier than
            // its last change now (`floor_across`).
            let before = changed(&fstat(parent)?);
            match mkdirat(parent, name.as_c_str(), mode_bits(0o700)) {
                Ok(()) => {}
                Err(Errno::EEXIST) => continue,
                Err(errno) => return Err(errno.into()),
            }
            let floor = floor_across(before, changed(&fstat(parent)?));
            let path = parent_path.join(OsStr::from_bytes(name.to_bytes()));
            return Self::adopt(parent, name, path, floor, entry);
        }
        Err(std::io::Error::new(
            ErrorKind::AlreadyExists,
            "no free name for a staging directory",
        ))
    }

    /// Takes the directory just made at `name` in `parent` (at `path`) as the
    /// staging directory, once it proves to be that directory (`open_owned`,
    /// against `floor`). Whatever holds the name is removed if it is an empty
    /// directory, which anyone who can write `parent` could remove as well,
    /// and otherwise left where it is: a directory swapped in at the name,
    /// with entries, is not this copy's to touch.
    fn adopt(
        parent: &'a File,
        name: CString,
        path: PathBuf,
        floor: Option<(i64, i64)>,
        entry: &CStr,
    ) -> std::io::Result<Self> {
        match open_owned(parent, &name, &path, floor) {
            Ok((dir, id)) => Ok(Self {
                parent,
                name,
                path,
                dir,
                id,
                entry: entry.to_owned(),
                staged: None,
                removed: false,
            }),
            Err(error) => {
                let _ = unlink_at(parent, &name, UnlinkatFlags::RemoveDir);
                Err(error)
            }
        }
    }

    /// Which entry holds `entry` in the staging directory now, if any.
    fn holds_staged(&self) -> Option<EntryId> {
        fstatat(
            &self.dir,
            self.entry.as_c_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .ok()
        .map(|stat| EntryId::of_stat(&stat))
    }

    /// Removes the staging directory, with its entry if that is still there.
    fn remove(mut self) -> std::io::Result<()> {
        self.removed = true;
        self.unlink_all()
    }

    /// Unlinks the entry the copy made, if it still holds its name, through
    /// the directory's own handle, then the directory by name, which only
    /// succeeds when it is empty, and only while the name still holds this
    /// directory: another swapped in at it is left alone.
    fn unlink_all(&self) -> std::io::Result<()> {
        if self.staged.is_some() && self.holds_staged() == self.staged {
            unlink_at(&self.dir, &self.entry, UnlinkatFlags::NoRemoveDir)?;
        }
        let at_name = fstatat(
            self.parent,
            self.name.as_c_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )?;
        if EntryId::of_stat(&at_name) != self.id {
            return Err(std::io::Error::other(
                "another entry holds its name, and was left alone",
            ));
        }
        unlink_at(self.parent, &self.name, UnlinkatFlags::RemoveDir)
    }
}

/// The refusal of a staging directory that proves not to be the one just
/// made: another directory swapped in at its name.
#[derive(Debug)]
struct StagingReplaced;

impl std::fmt::Display for StagingReplaced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("its staging directory was replaced")
    }
}

impl std::error::Error for StagingReplaced {}

impl StagingReplaced {
    fn error() -> std::io::Error {
        std::io::Error::other(Self)
    }

    #[cfg(test)]
    fn is(error: &std::io::Error) -> bool {
        matches!(error.get_ref(), Some(inner) if inner.is::<Self>())
    }
}

/// Whether a directory found at the name a directory was just made at can be
/// that directory: `empty`, and born no earlier than `floor` where there is
/// one. Another user who can write the parent could otherwise swap one of
/// this user's own directories in at the name, which the copy would then
/// change the mode of, write into, and remove from. Its owner is not
/// compared: a mount without per-user ownership (vfat or exfat with `uid=`,
/// CIFS, sshfs, NFS with `root_squash`) shows a new directory as another
/// user's, and a directory another user owns and swaps in gives them nothing
/// they lack, since they can write the parent and the copy acts only on
/// entries it made.
///
/// The birth time is what tells an old directory from the new one; where the
/// filesystem records none, the change time stands in, which a rename moves
/// forward, so there only emptiness is checked in effect, as it is where
/// there is no floor.
fn is_new_empty_directory(born: (i64, i64), floor: Option<(i64, i64)>, empty: bool) -> bool {
    empty && floor.is_none_or(|floor| born >= floor)
}

/// The floor a directory made in a parent between two looks at the parent's
/// change time (`before` and `after`) is born no earlier than: `before`, by
/// the filesystem's own clock, which a server's clock differing from this
/// machine's cannot upset, and which nobody can set where it is a real change
/// time. Where it went backwards across the creation it is not one: sshfs,
/// vfat and exfat report the modification time, which a user or an archive
/// can put in the future, as the change time. There nothing bounds the birth
/// time (`None`).
fn floor_across(before: (i64, i64), after: (i64, i64)) -> Option<(i64, i64)> {
    (before <= after).then_some(before)
}

/// When `stat`'s entry last changed, with nanoseconds.
// The field types vary by target.
#[allow(clippy::unnecessary_cast)]
fn changed(stat: &Stat) -> (i64, i64) {
    (stat.st_ctime as i64, stat.st_ctime_nsec as i64)
}

/// Whether the open directory `dir` has no entries, read through a duplicate
/// of its handle, which needs only the read permission it was opened with.
fn is_empty_directory(dir: &File) -> std::io::Result<bool> {
    let mut stream = nix::dir::Dir::from_fd(dir.try_clone()?.into())?;
    for entry in stream.iter() {
        let entry = entry?;
        if entry.file_name() != c"." && entry.file_name() != c".." {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Opens the directory `name` just created in `parent`, at `path`, once it
/// proves to be that directory (`open_new_directory`, against `floor`), and
/// makes it owner-only, which a umask (`0o277`) or a default ACL can keep it
/// from being: without owner access nothing could be written into it.
/// Returns it and which directory it is. A mount that refuses a mode change
/// to the user who created the directory (vfat or exfat without `uid=`, CIFS
/// without unix extensions) has no per-user permissions to keep, so that
/// refusal is only logged: a directory that really cannot be written still
/// fails the write into it.
fn open_owned(
    parent: &File,
    name: &CStr,
    path: &Path,
    floor: Option<(i64, i64)>,
) -> std::io::Result<(File, EntryId)> {
    let (dir, id, _) = open_new_directory(parent, name, floor, |_| 0o700).map_err(|error| {
        if NotNew::is(&error) {
            StagingReplaced::error()
        } else {
            error
        }
    })?;
    match nix::sys::stat::fchmod(&dir, mode_bits(0o700)) {
        Err(Errno::EPERM) => warn!(
            "Failed to make the staging directory {} owner-only: {}",
            compact(path),
            std::io::Error::from(Errno::EPERM)
        ),
        result => result?,
    }
    Ok((dir, id))
}

impl Drop for Staging<'_> {
    /// Best effort, for a staging directory `remove` was not called on (an
    /// unwinding test): a removal that fails leaves a hidden, owner-only
    /// directory behind, and says where it is.
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        if let Err(error) = self.unlink_all() {
            warn!(
                "Failed to remove the staging directory {}: {error}",
                compact(&self.path)
            );
        }
    }
}

/// Checks that the entry a granted overwrite names still holds the
/// destination, then opens a regular-file source. Returns the task and what it
/// found, or `None` when the task was finalized here and must not continue.
///
/// The destination is checked first, so a copy that could never land opens
/// nothing, and again just before the copy lands (`land`). A name found free
/// is left for the copy to create, which refuses it if it is taken again by
/// then. A failure here that leaves the entry granted at the name says so: a
/// source that cannot be opened while the entry still holds the name, and a
/// look at the name that fails. Only a non-directory source gets here with an
/// overwrite: `validate_paths` refuses one for a directory.
///
/// Opening the source here means one that cannot be read (mode 000, or gone)
/// fails the task with the destination untouched, and the copy then reads the
/// handle that was checked rather than reopening the path. Other types are not
/// opened: a FIFO would block, and a directory or symlink is not read as bytes.
pub(super) fn prepare_destination(
    active: ActiveTask,
    settings: CopySettings<'_>,
    overwrite: Option<Seen>,
    old_path: &Path,
    new_path: &Path,
    source_mode: u32,
) -> Option<(ActiveTask, Prepared)> {
    let failed = |error: &dyn std::fmt::Display| {
        failed_transfer(settings.is_move, old_path, new_path, error)
    };
    let replace = match overwrite {
        None => None,
        Some(granted) => match Seen::of_path(new_path).map(|found| still_holds(granted, found)) {
            Ok(Holds::Granted) => Some(granted),
            Ok(Holds::Free) => None,
            Ok(Holds::Changed) => {
                active.error(changed_refusal(settings.is_move, old_path, new_path));
                return None;
            }
            Err(error) => {
                active.error(failed(&error) + &not_replaced(new_path));
                return None;
            }
        },
    };
    let source = if unix_mode::is_file(source_mode) {
        let name = c_name(old_path).ok_or_else(|| std::io::Error::from(ErrorKind::InvalidInput));
        match open_parent(old_path).and_then(|parent| open_source_file(&parent, &name?)) {
            Ok(file) => Some(file),
            Err(error) if replace.is_some() => {
                active.error(failed(&error) + &not_replaced(new_path));
                return None;
            }
            Err(error) => {
                active.error(failed(&error));
                return None;
            }
        }
    } else {
        None
    };
    Some((active, Prepared { source, replace }))
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use test_case::test_case;

    use super::{
        super::sys::CWD,
        super::test_support::{
            KINDS, Kind, STANDING, answered, assert_made, copy_task, destination, every_case,
            finished_task, make, mode_of, no_paste, paste_after, paste_after_unless, paste_over,
            seen, staging_left, within_deadline,
        },
        *,
    };
    use crate::{
        command::{
            Command, ConflictChoice,
            progress::{Progress, TaskKind, Transfer},
        },
        test_support::TempDir,
    };

    /// The settings of a copy, or of the copy a move across devices makes
    /// when `is_move`, in a paste nobody answered anything for.
    fn settings(is_move: bool) -> CopySettings<'static> {
        CopySettings {
            is_move,
            conflicts: no_paste(),
        }
    }

    /// A copy context for a test, with no standing answer, so a raced name
    /// is recorded as an error. `is_move` makes it the copy a move makes.
    fn context(is_move: bool, buffer: &mut [u8]) -> CopyContext<'_> {
        CopyContext::new(settings(is_move), buffer, None, 0)
    }

    /// The source at `path` as a task is started for it.
    fn listed(path: impl AsRef<Path>) -> PathInfo {
        PathInfo::try_from(path.as_ref()).unwrap()
    }

    /// Copies `src` to `dst` as a copy, or as a move's copy stage when
    /// `is_move`, returning the errors recorded.
    fn copy_one(is_move: bool, src: &Path, dst: &Path) -> Vec<String> {
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();
        assert!(copy_path(
            &mut context(is_move, &mut [0u8; 64]),
            &mut active,
            &mut errors,
            None,
            &listed(src),
            src,
            dst,
        ));
        active.done();
        errors
    }

    // Linux only: recreating a socket goes through mknod, which macOS refuses
    // to an unprivileged process with EPERM.
    #[cfg(target_os = "linux")]
    #[test]
    fn copy_path_recreates_socket() {
        let fx = TempDir::new("tasks");
        let src = fx.join("sock");
        let _listener = std::os::unix::net::UnixListener::bind(&src).unwrap();
        let dst = fx.join("sock_copy");

        let errors = copy_one(false, &src, &dst);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert!(unix_mode::is_socket(mode_of(&dst)));
        assert!(src.exists());
    }

    #[test]
    fn copy_path_recreates_fifo() {
        let fx = TempDir::new("tasks");
        let src = fx.join("fifo");
        nix::unistd::mkfifo(&src, nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap();
        let dst = fx.join("fifo_copy");

        let errors = copy_one(false, &src, &dst);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let dst_mode = mode_of(&dst);
        assert!(unix_mode::is_fifo(dst_mode));
        assert_eq!(0o644, dst_mode & 0o7777);
    }

    #[test]
    fn copy_path_continues_past_unreadable_entries() {
        let fx = TempDir::new("tasks");
        let src = fx.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), b"a").unwrap();
        std::fs::write(src.join("bad"), b"x").unwrap();
        std::fs::write(src.join("c.txt"), b"c").unwrap();
        let mut permissions = std::fs::metadata(src.join("bad")).unwrap().permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(src.join("bad"), permissions).unwrap();
        // A chmod-000 file is still readable by root (CAP_DAC_OVERRIDE) and on
        // mounts that ignore permissions. Probe what this filesystem actually
        // does rather than inspecting the euid, so the assertions below match
        // the environment instead of being skipped in it.
        let is_unreadable = std::fs::File::open(src.join("bad")).is_err();

        let dst = fx.join("dst");

        let errors = copy_one(false, &src, &dst);

        if is_unreadable {
            // Like cp -R: the unreadable entry is recorded, not fatal.
            assert_eq!(1, errors.len(), "expected one error: {errors:?}");
            assert!(errors[0].contains("bad"), "unexpected error: {}", errors[0]);
            assert!(!dst.join("bad").exists());
        } else {
            // Nothing was unreadable here, so this is a plain full copy.
            assert!(errors.is_empty(), "unexpected errors: {errors:?}");
            assert!(dst.join("bad").exists());
        }
        // The walk must reach the entries on both sides of "bad" either way: a
        // failed entry must not abort the siblings.
        assert!(dst.join("a.txt").exists());
        assert!(dst.join("c.txt").exists());
    }

    #[test]
    fn copy_path_reuses_one_buffer_without_leaking_bytes_between_files() {
        let fx = TempDir::new("tasks");
        let src = fx.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a_long.txt"), b"aaaaaaaaaaaaaaaa").unwrap();
        std::fs::write(src.join("b_short.txt"), b"b").unwrap();
        let dst = fx.join("dst");

        // One buffer serves the whole tree, so a short file copied after a
        // longer one must not pick up the previous file's trailing bytes.
        let errors = copy_one(false, &src, &dst);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");

        assert_eq!(
            b"aaaaaaaaaaaaaaaa".to_vec(),
            std::fs::read(dst.join("a_long.txt")).unwrap()
        );
        assert_eq!(
            b"b".to_vec(),
            std::fs::read(dst.join("b_short.txt")).unwrap()
        );
    }

    /// The modification times of `path` and everything under it, by relative
    /// name, so a tree can be compared against its copy.
    fn modified_times(root: &Path) -> Vec<(PathBuf, std::time::SystemTime)> {
        let mut times = vec![(
            PathBuf::from("."),
            fs::metadata(root).unwrap().modified().unwrap(),
        )];
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                let metadata = fs::metadata(&path).unwrap();
                if metadata.is_dir() {
                    stack.push(path.clone());
                }
                times.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    metadata.modified().unwrap(),
                ));
            }
        }
        times.sort();
        times
    }

    #[test]
    fn a_preserving_copy_keeps_the_modification_times_of_the_whole_tree() {
        let fx = TempDir::new("tasks_times");
        let src = fx.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), b"a").unwrap();
        fs::write(src.join("sub").join("b.txt"), b"b").unwrap();
        // Backdate everything, deepest first so writing a child does not move
        // the parent's time again.
        let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        for relative in ["sub/b.txt", "sub", "a.txt", "."] {
            let file = File::options().read(true).open(src.join(relative)).unwrap();
            file.set_times(fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
        let before = modified_times(&src);
        let dst = fx.join("dst");

        // A same-device move is a rename, which keeps the timestamps. The
        // cross-device fallback copies, so it has to put them back or the
        // result depends on which mount the destination is on.
        let errors = copy_one(true, &src, &dst);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");

        assert_eq!(before, modified_times(&dst));
    }

    /// The outcome a name taken since the copy started must have: skipped
    /// under a standing "skip all", otherwise recorded, and never replaced.
    /// `nested` when it is inside a directory the copy created.
    fn assert_raced(
        case: &str,
        standing: Option<ConflictChoice>,
        (is_move, nested): (bool, bool),
        (old, new): (&Path, &Path),
        (errors, skipped): (&[String], usize),
    ) {
        if standing == Some(ConflictChoice::SkipAll) {
            assert!(errors.is_empty(), "{case}: {errors:?}");
            assert_eq!(1, skipped, "{case}");
        } else {
            let verb = if is_move { "move" } else { "copy" };
            let reason = if nested {
                "the name is already taken there, by another entry or one the destination \
                 treats as the same"
            } else {
                "another entry took that name after the paste checked it"
            };
            let refusal = format!(
                "Cannot {verb} {} to {}: {reason}",
                compact(old),
                compact(new)
            );
            assert_eq!(vec![refusal], errors, "{case}");
            assert_eq!(0, skipped, "{case}");
        }
    }

    /// Every kind of source against every kind of occupant, under every
    /// standing answer, for a copy and for a move's copy stage.
    fn raced_cases() -> Vec<(Kind, Kind, Option<ConflictChoice>, bool)> {
        KINDS
            .into_iter()
            .flat_map(|kind| KINDS.into_iter().map(move |occupant| (kind, occupant)))
            .flat_map(|(kind, occupant)| {
                STANDING
                    .into_iter()
                    .map(move |standing| (kind, occupant, standing))
            })
            .flat_map(|(kind, occupant, standing)| {
                [false, true]
                    .into_iter()
                    .map(move |is_move| (kind, occupant, standing, is_move))
            })
            .collect()
    }

    /// A name another process took at the top-level destination after the task
    /// started is never replaced, whatever the standing answer: nothing decided
    /// to replace what holds it now. "Skip all" skips it, and says so for the
    /// top-level entry; otherwise it is recorded.
    #[test]
    fn a_name_taken_at_the_top_level_since_the_copy_started_is_never_replaced() {
        every_case(raced_cases(), |&raced| {
            within_deadline(move || top_raced(raced));
        });
    }

    fn top_raced((kind, occupant, standing, is_move): (Kind, Kind, Option<ConflictChoice>, bool)) {
        let case = format!("{kind:?} onto {occupant:?}, {standing:?}, move {is_move}");
        let fx = TempDir::new("tasks_raced_top");
        let (src, dst) = (fx.join("src"), fx.join("dst"));
        fs::create_dir(&src).unwrap();
        fs::create_dir(&dst).unwrap();
        let (old, new) = (src.join("entry"), dst.join("entry"));
        make(kind, "src", &old);
        make(occupant, "raced", &new);
        let id = EntryId::of_path(&new);
        let conflicts = answered(standing);
        let mut buffer = [0u8; 64];
        let mut context = CopyContext::new(
            CopySettings {
                is_move,
                conflicts: &conflicts,
            },
            &mut buffer,
            None,
            0,
        );
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();

        assert!(copy_path(
            &mut context,
            &mut active,
            &mut errors,
            None,
            &listed(&old),
            &old,
            &new,
        ));
        active.done();

        assert_raced(
            &case,
            standing,
            (is_move, false),
            (&old, &new),
            (&errors, context.skipped),
        );
        assert_eq!(
            standing == Some(ConflictChoice::SkipAll),
            context.top_skipped,
            "{case}"
        );
        assert_made(&case, occupant, "raced", id, &new);
    }

    /// Copies the tree `src/tree`, with `depth` directories of `sub` below it
    /// holding one `entry` of `kind`, into `dst/tree` under `standing`, with
    /// `occupant` put at the entry's destination once the copy has created the
    /// directory it goes in and before it copies anything into it. Returns
    /// what the copy left behind and the entry's two paths.
    fn copy_into_a_raced_tree(
        (kind, occupant, standing, is_move): (Kind, Kind, Option<ConflictChoice>, bool),
        depth: usize,
    ) -> (TempDir, CopyOutcome, PathBuf, PathBuf) {
        let fx = TempDir::new("tasks_raced_tree");
        let mut src = fx.join("src").join("tree");
        let mut dst = fx.join("dst").join("tree");
        let mut dirs = vec![(src.clone(), dst.clone())];
        for _ in 1..depth {
            src.push("sub");
            dst.push("sub");
            dirs.push((src.clone(), dst.clone()));
        }
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(fx.join("dst")).unwrap();
        make(kind, "src", &src.join("entry"));
        let conflicts = answered(standing);
        let mut buffer = [0u8; 64];
        let mut context = CopyContext::new(
            CopySettings {
                is_move,
                conflicts: &conflicts,
            },
            &mut buffer,
            None,
            0,
        );
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();
        // Each directory entered in turn, as the walk enters them, down to the
        // one the entry goes in.
        let mut level = None;
        for (depth, (src_dir, dst_dir)) in dirs.iter().enumerate() {
            let (src_parent, dst_parent) =
                (open_parent(src_dir).unwrap(), open_parent(dst_dir).unwrap());
            let name = c_name(src_dir).unwrap();
            let stat = fstatat(&src_parent, name.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW).unwrap();
            let at = At {
                src: &src_parent,
                dst: &dst_parent,
                src_name: &name,
                dst_name: &name,
            };
            let paths = Paths {
                old: src_dir.clone(),
                new: dst_dir.clone(),
                depth,
            };
            level = Some(
                enter_directory(&at, &paths, &mut errors, &mut context, &stat)
                    .expect("the directory should be entered"),
            );
        }
        let (old, new) = (src.join("entry"), dst.join("entry"));
        make(occupant, "raced", &new);
        let mut paths = Paths {
            old: src,
            new: dst,
            depth: dirs.len() - 1,
        };

        assert!(copy_tree(
            level.expect("at least one directory"),
            &mut paths,
            &mut active,
            &mut errors,
            &mut context
        ));
        active.done();
        (fx, context.into_outcome(errors), old, new)
    }

    /// Inside a directory the copy created, at any depth, a name taken is
    /// never replaced, whatever the standing answer: it may be the copy's own
    /// entry under a name the destination folds together with another. "Skip
    /// all" still skips it, and it is not the top-level entry; otherwise it is
    /// recorded, so a move keeps its source.
    #[test]
    fn a_name_taken_inside_a_created_directory_is_never_replaced() {
        let cases: Vec<_> = raced_cases()
            .into_iter()
            .flat_map(|case| [1, 2].into_iter().map(move |depth| (case, depth)))
            .collect();
        every_case(cases, |&case| within_deadline(move || nested_raced(case)));
    }

    fn nested_raced((raced, depth): ((Kind, Kind, Option<ConflictChoice>, bool), usize)) {
        let (kind, occupant, standing, is_move) = raced;
        let case =
            format!("{kind:?} onto {occupant:?} at depth {depth}, {standing:?}, move {is_move}");
        let (_fx, outcome, old, new) = copy_into_a_raced_tree(raced, depth);

        assert_raced(
            &case,
            standing,
            (is_move, true),
            (&old, &new),
            (&outcome.errors, outcome.skipped),
        );
        assert!(!outcome.top_skipped, "{case}");
        assert_made(&case, occupant, "raced", None, &new);
    }

    /// A move across devices whose copy skipped an entry inside the tree under
    /// "skip all" keeps its whole source, from what the copy itself reports.
    #[test]
    fn a_move_whose_copy_skipped_an_inner_entry_keeps_its_source() {
        let raced = (Kind::File, Kind::File, Some(ConflictChoice::SkipAll), true);
        let (fx, outcome, old, _new) = copy_into_a_raced_tree(raced, 1);
        let tree = fx.join("src").join("tree");
        let (tx, rx) = mpsc::channel();

        super::super::finish_cross_device_move(copy_task(tx), outcome, &tree, true);

        assert_eq!(
            Some(format!(
                "Skipped 1 entry, so the original {} was kept",
                compact(&tree)
            )),
            finished_task(&rx).error_message()
        );
        assert!(old.exists());
    }

    // ── prepare_destination: the ordering every queued operation depends on ──

    /// A granted overwrite whose entry still holds the name hands it to the
    /// copy to replace once its replacement is whole, and removes nothing up
    /// front.
    #[test]
    fn a_granted_overwrite_is_handed_to_the_copy_and_nothing_is_removed() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_overwrite");
        let granted = Seen::of_path(&dst).unwrap();

        let (active, prepared) =
            prepare_destination(active, settings(false), granted, &src, &dst, mode_of(&src))
                .expect("the task should continue");

        assert_eq!(granted, prepared.replace);
        assert!(prepared.source.is_some());
        assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
        active.done();
    }

    /// An entry changed since the overwrite was granted is refused before
    /// anything is copied, so no copy is made that could never land.
    #[test_case(false ; "a copy")]
    #[test_case(true ; "a move")]
    fn a_granted_entry_changed_since_is_refused_before_anything_is_copied(is_move: bool) {
        let (fx, src, dst, _active, _token) = destination("tasks_prepare_changed");
        let granted = Seen::of_path(&dst).unwrap();
        // Kept under another name, so the one written next cannot reuse its
        // inode number.
        fs::rename(&dst, fx.join("kept")).unwrap();
        fs::write(&dst, b"since").unwrap();
        let (tx, rx) = mpsc::channel();

        let prepared = prepare_destination(
            copy_task(tx),
            settings(is_move),
            granted,
            &src,
            &dst,
            mode_of(&src),
        );

        assert!(prepared.is_none());
        let verb = if is_move { "move" } else { "copy" };
        assert_eq!(
            Some(format!(
                "Cannot {verb} {} to {}: the entry there changed after the paste checked it",
                compact(&src),
                compact(&dst)
            )),
            finished_task(&rx).error_message()
        );
        assert_eq!(b"since".to_vec(), fs::read(&dst).unwrap());
        assert_eq!(Vec::<String>::new(), staging_left(fx.path()));
    }

    #[test]
    fn a_destination_survives_when_overwrite_was_not_granted() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_no_overwrite");

        let (active, prepared) =
            prepare_destination(active, settings(false), None, &src, &dst, mode_of(&src))
                .expect("the task should continue");

        assert_eq!(None, prepared.replace);
        assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
        active.done();
    }

    /// Something else removed the entry between the prompt and the worker,
    /// which leaves the name free for the copy to create.
    #[test]
    fn a_destination_that_already_vanished_is_not_an_error() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_vanished");
        let granted = Seen::of_path(&dst).unwrap();
        fs::remove_file(&dst).unwrap();

        let (active, prepared) =
            prepare_destination(active, settings(false), granted, &src, &dst, mode_of(&src))
                .expect("the task should continue");

        assert_eq!(None, prepared.replace);
        active.done();
    }

    #[test]
    fn an_unreadable_source_leaves_the_destination_it_would_have_replaced() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_source_unreadable");
        fs::set_permissions(&src, fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads a mode-000 file anyway (CAP_DAC_OVERRIDE), as do mounts
        // that ignore permissions; probe rather than inspect the euid.
        let is_unreadable = File::open(&src).is_err();

        let granted = Seen::of_path(&dst).unwrap();
        let prepared =
            prepare_destination(active, settings(false), granted, &src, &dst, mode_of(&src));

        if is_unreadable {
            assert!(prepared.is_none());
        } else {
            let (active, prepared) = prepared.expect("a readable source should continue");
            assert!(prepared.source.is_some());
            active.done();
        }
        assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
    }

    #[test_case(false, "copy" ; "a copy")]
    #[test_case(true, "move" ; "a move")]
    fn an_unreadable_source_is_reported_under_the_operation_that_failed(is_move: bool, verb: &str) {
        let fx = TempDir::new("tasks_prepare_verb");
        let src = fx.join("missing.txt");
        let dst = fx.join("dest.txt");
        let (tx, rx) = mpsc::channel();
        // A regular-file mode with no file behind it, so opening fails for
        // root as well.
        let prepared = prepare_destination(
            copy_task(tx),
            settings(is_move),
            None,
            &src,
            &dst,
            0o100_644,
        );

        assert!(prepared.is_none());
        let message = finished_task(&rx)
            .error_message()
            .expect("an error")
            .clone();
        assert!(
            message.starts_with(&format!("Failed to {verb} ")),
            "{message}"
        );
    }

    /// A move's file is created owner-only and gets the source's other bits
    /// once the copy stops; a copy's gets no more than the source has, which
    /// the umask trims. Either way a file still being written is readable by
    /// no one the source is not.
    #[test_case(true => 0o600 ; "a move is created owner only")]
    #[test_case(false => 0 ; "a copy is created with no bit the source lacks")]
    fn a_copied_file_is_never_more_readable_than_its_source_while_written(is_move: bool) -> u32 {
        let fx = TempDir::new("tasks_create_file_mode");
        let dir = open_parent(&fx.join("dst.txt")).unwrap();

        let _file = create_file_at(&dir, c"dst.txt", 0o100_640, is_move).unwrap();

        let mode = mode_of(&fx.join("dst.txt")) & 0o7777;
        if is_move { mode } else { mode & !0o640 }
    }

    /// A copy task already cancelled, so the walk stops at its first check.
    fn cancelled_copy_task() -> ActiveTask {
        let (tx, rx) = mpsc::channel();
        std::mem::forget(rx);
        let (active, _, token) = ActiveTask::new(
            tx,
            TaskKind::Copy(Transfer {
                source: String::new(),
                destination: String::new(),
            }),
            1,
        );
        token.cancel();
        active
    }

    #[test]
    fn a_cancelled_file_copy_is_left_with_the_source_mode() {
        let fx = TempDir::new("tasks_cancelled_file_mode");
        let src = fx.join("src.txt");
        fs::write(&src, b"src").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o640)).unwrap();
        let dst = fx.join("dst.txt");
        let mut active = cancelled_copy_task();
        let mut errors = Vec::new();

        // Cancelled after the destination is created and before a byte is
        // written. The partial file stays, like an interrupted `cp`, and gets
        // the source's mode: neither the owner-only mode it was created with
        // nor anything broader than the source.
        assert!(!copy_path(
            &mut context(false, &mut [0u8; 64]),
            &mut active,
            &mut errors,
            None,
            &listed(&src),
            &src,
            &dst,
        ));
        active.cancelled();

        assert!(dst.exists());
        assert_eq!(0o640, mode_of(&dst) & 0o7777);
    }

    #[test]
    fn a_cancelled_directory_copy_is_left_with_the_source_mode() {
        let fx = TempDir::new("tasks_cancelled_directory_mode");
        let src = fx.join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a.txt"), b"a").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o750)).unwrap();
        let dst = fx.join("dst");
        let mut active = cancelled_copy_task();
        let mut errors = Vec::new();

        // Cancelled at the first entry, after the directory is created.
        assert!(!copy_path(
            &mut context(false, &mut [0u8; 64]),
            &mut active,
            &mut errors,
            None,
            &listed(&src),
            &src,
            &dst,
        ));
        active.cancelled();

        assert!(!dst.join("a.txt").exists());
        assert_eq!(0o750, mode_of(&dst) & 0o7777);
    }

    // ── a replacement lands whole or not at all ──────────────────────────────

    /// A replacement that cannot be created leaves the entry it was to
    /// replace, and nothing of itself: a device node needs privileges a user
    /// does not have, so creating one fails after the copy began.
    #[test]
    fn a_replacement_that_cannot_be_created_leaves_the_entry_it_would_have_replaced() {
        if nix::unistd::geteuid().is_root() {
            eprintln!("skipped: root may create a device node");
            return;
        }
        let fx = TempDir::new("tasks_replace_cannot_create");
        let new = fx.join("null");
        fs::write(&new, b"dest").unwrap();

        let (_, task) = paste_over(false, fx.path(), Path::new("/dev/null"));

        assert_eq!(
            Some(format!(
                "Failed to copy \"/dev/null\" to {}: Operation not permitted (os error 1); {} was \
                 not replaced",
                compact(&new),
                compact(&new)
            )),
            task.error_message()
        );
        assert_eq!(b"dest".to_vec(), fs::read(&new).unwrap());
        assert_eq!(Vec::<String>::new(), staging_left(fx.path()));
    }

    /// A replacement cancelled part way leaves the entry it was to replace,
    /// and nothing of itself.
    #[test]
    fn a_cancelled_replacement_leaves_the_entry_it_would_have_replaced() {
        let (fx, src, dst, active, _token) = destination("tasks_replace_cancelled");
        active.done();
        let mut buffer = [0u8; 64];
        let mut context = CopyContext::new(settings(false), &mut buffer, None, 0);
        let mut active = cancelled_copy_task();
        let mut errors = Vec::new();

        let finished = copy_path(
            &mut context,
            &mut active,
            &mut errors,
            seen(&dst),
            &listed(&src),
            &src,
            &dst,
        );
        active.cancelled();

        assert!(!finished);
        assert_eq!(Vec::<String>::new(), errors);
        assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
        assert_eq!(Vec::<String>::new(), staging_left(fx.path()));
    }

    /// The entry granted is checked again just before the replacement takes
    /// its name: one changed while the replacement was written is refused
    /// and left as it is, the replacement is removed, and nothing counts as
    /// written. An entry unchanged is replaced, and that counts.
    ///
    /// An entry removed meanwhile leaves the name free, which the replacement
    /// takes without replacing anything.
    #[test_case(false, Landed::Unchanged ; "a copy over the entry granted")]
    #[test_case(true, Landed::Unchanged ; "a move over the entry granted")]
    #[test_case(false, Landed::Changed ; "a copy over an entry changed since")]
    #[test_case(true, Landed::Changed ; "a move over an entry changed since")]
    #[test_case(false, Landed::Removed ; "a copy onto a name freed since")]
    #[test_case(true, Landed::Removed ; "a move onto a name freed since")]
    fn a_replacement_lands_only_on_the_entry_granted(is_move: bool, since: Landed) {
        let (fx, src, dst, active, _token) = destination("tasks_replace_lands_granted");
        active.done();
        let granted = seen(&dst);
        let changed = since == Landed::Changed;
        match since {
            Landed::Unchanged => {}
            // Kept under another name, so the one written next cannot reuse
            // its inode number.
            Landed::Changed => {
                fs::rename(&dst, fx.join("kept")).unwrap();
                fs::write(&dst, b"since").unwrap();
            }
            Landed::Removed => fs::remove_file(&dst).unwrap(),
        }
        let mut buffer = [0u8; 64];
        let mut context = CopyContext::new(settings(is_move), &mut buffer, None, 0);
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();

        assert!(copy_path(
            &mut context,
            &mut active,
            &mut errors,
            granted,
            &listed(&src),
            &src,
            &dst,
        ));
        active.done();
        let outcome = context.into_outcome(errors);

        if changed {
            let verb = if is_move { "move" } else { "copy" };
            assert_eq!(
                vec![format!(
                    "Cannot {verb} {} to {}: the entry there changed after the paste checked it",
                    compact(&src),
                    compact(&dst)
                )],
                outcome.errors
            );
            assert_eq!(b"since".to_vec(), fs::read(&dst).unwrap());
        } else {
            assert_eq!(Vec::<String>::new(), outcome.errors);
            assert_eq!(b"src".to_vec(), fs::read(&dst).unwrap());
        }
        assert_eq!(!changed, outcome.wrote);
        assert_eq!(Vec::<String>::new(), staging_left(fx.path()));
    }

    /// A name taken between the last check and the rename that lands a
    /// replacement is refused as raced, never counted as landed, and skipped
    /// under a standing "skip all"; any other failure of that rename is one,
    /// which says the entry granted was left only when the rename was the one
    /// replacing it.
    #[test_case(false ; "a copy")]
    #[test_case(true ; "a move")]
    fn what_the_rename_that_lands_a_replacement_means(is_move: bool) {
        let mut buffer = [0u8; 8];
        let mut context = CopyContext::new(settings(is_move), &mut buffer, None, 0);
        let paths = Paths {
            old: PathBuf::from("/src/one"),
            new: PathBuf::from("/dest/one"),
            depth: 0,
        };
        let verb = if is_move { "move" } else { "copy" };
        let taken = || Err(ErrorKind::AlreadyExists.into());
        let denied = || Err(ErrorKind::PermissionDenied.into());

        assert_eq!(Ok(true), landed(&mut context, &paths, true, Ok(())));
        assert_eq!(
            Err(format!(
                "Cannot {verb} \"/src/one\" to \"/dest/one\": another entry took that name after \
                 the paste checked it"
            )),
            landed(&mut context, &paths, false, taken())
        );
        assert_eq!(
            Err(format!(
                "Failed to {verb} \"/src/one\" to \"/dest/one\": permission denied; \
                 \"/dest/one\" was not replaced"
            )),
            landed(&mut context, &paths, true, denied())
        );
        assert_eq!(
            Err(format!(
                "Failed to {verb} \"/src/one\" to \"/dest/one\": permission denied"
            )),
            landed(&mut context, &paths, false, denied()),
            "a rename onto a free name leaves nothing granted behind"
        );
        assert_eq!(0, context.skipped);
    }

    /// Under a standing "skip all" a name taken just before the landing is
    /// skipped, as a raced name anywhere else is: counted, not an error.
    #[test]
    fn a_landing_that_finds_the_name_taken_is_skipped_under_skip_all() {
        let conflicts = answered(Some(ConflictChoice::SkipAll));
        let mut buffer = [0u8; 8];
        let mut context = CopyContext::new(
            CopySettings {
                is_move: false,
                conflicts: &conflicts,
            },
            &mut buffer,
            None,
            0,
        );
        let paths = Paths {
            old: PathBuf::from("/src/one"),
            new: PathBuf::from("/dest/one"),
            depth: 0,
        };

        let landed = landed(
            &mut context,
            &paths,
            false,
            Err(ErrorKind::AlreadyExists.into()),
        );

        assert_eq!(Ok(false), landed);
        assert_eq!(1, context.skipped);
    }

    /// What holds a granted name by the time the replacement lands.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Landed {
        Unchanged,
        Changed,
        Removed,
    }

    /// Landing a staged replacement: over what holds the name only when told
    /// to, onto a free name, and never over a name taken since, which is
    /// refused with the staged entry and the occupant both left as they are.
    #[test_case(true, true => (Ok(()), "staged".to_string(), false) ; "replacing what holds the name")]
    #[test_case(false, false => (Ok(()), "staged".to_string(), false) ; "onto a free name")]
    #[test_case(false, true => (Err(ErrorKind::AlreadyExists), "taken".to_string(), true) ; "onto a taken name without replacing")]
    fn a_staged_replacement_lands(
        replaces: bool,
        taken: bool,
    ) -> (Result<(), ErrorKind>, String, bool) {
        let fx = TempDir::new("tasks_rename_staged");
        let (staging, dst) = (fx.join("staging"), fx.join("dst"));
        fs::create_dir(&staging).unwrap();
        fs::create_dir(&dst).unwrap();
        fs::write(staging.join("entry"), b"staged").unwrap();
        if taken {
            fs::write(dst.join("entry"), b"taken").unwrap();
        }
        let open = |dir: &Path| File::open(dir).unwrap();

        let landed = rename_staged(replaces, &open(&staging), &open(&dst), c"entry")
            .map_err(|error| error.kind());

        (
            landed,
            String::from_utf8(fs::read(dst.join("entry")).unwrap()).unwrap(),
            staging.join("entry").exists(),
        )
    }

    /// A replacement that lands leaves nothing of its staging behind, and the
    /// destination holds the whole source, for a copy and for a move.
    #[test_case(false ; "a copy")]
    #[test_case(true ; "a move")]
    fn a_replacement_that_lands_leaves_no_staging_behind(is_move: bool) {
        let fx = TempDir::new("tasks_replace_lands");
        let (src, dest) = (fx.join("src"), fx.join("dest"));
        fs::create_dir(&src).unwrap();
        fs::create_dir(&dest).unwrap();
        fs::write(src.join("one"), b"source").unwrap();
        fs::write(dest.join("one"), b"dest").unwrap();

        let (new, task) = paste_over(is_move, &dest, &src.join("one"));

        assert_eq!(None, task.error_message());
        assert_eq!(b"source".to_vec(), fs::read(&new).unwrap());
        assert_eq!(!is_move, src.join("one").exists());
        assert_eq!(Vec::<String>::new(), staging_left(&dest));
    }

    /// A destination whose modification time is in the future (an archive
    /// extracted with such times, a clock stepped back) still takes a
    /// replacement: the staging directory is checked against the change
    /// time, which nobody can set.
    #[test]
    fn a_destination_modified_in_the_future_takes_a_replacement() {
        let fx = TempDir::new("tasks_replace_future");
        let (src, dest) = (fx.join("src"), fx.join("dest"));
        fs::create_dir(&dest).unwrap();
        fs::write(&src, b"source").unwrap();
        fs::write(dest.join("src"), b"dest").unwrap();
        let ahead = std::time::SystemTime::now() + Duration::from_hours(1);
        File::open(&dest).unwrap().set_modified(ahead).unwrap();

        let (new, task) = paste_over(false, &dest, &src);

        assert_eq!(None, task.error_message());
        assert_eq!(b"source".to_vec(), fs::read(&new).unwrap());
        assert_eq!(Vec::<String>::new(), staging_left(&dest));
    }

    /// A staging directory takes the first name offered that is free, and
    /// never touches an entry that already holds one.
    #[test]
    fn a_staging_directory_never_takes_a_name_already_there() {
        let fx = TempDir::new("tasks_staging_names");
        fs::write(fx.join("taken"), b"keep").unwrap();
        let parent = open_parent(&fx.join("x")).unwrap();

        let staging = Staging::create(
            &parent,
            fx.path(),
            [c"taken".to_owned(), c"free".to_owned()],
            c"entry",
        )
        .unwrap();

        assert_eq!(c"free", staging.name.as_c_str());
        assert_eq!(fx.join("free"), staging.path);
        assert!(fx.join("free").is_dir());
        drop(staging);
        assert!(!fx.join("free").exists());
        assert_eq!(b"keep".to_vec(), fs::read(fx.join("taken")).unwrap());
    }

    /// A directory found where one was just made is taken for it only when
    /// it is empty and, where there is a floor, was born no earlier than it.
    /// Its owner is not asked about: a mount without per-user ownership shows
    /// the one just made as another user's.
    #[test_case((5, 0), Some((5, 0)), true => true ; "the one just made")]
    #[test_case((6, 1), Some((5, 0)), true => true ; "born after the floor")]
    #[test_case((4, 999_999_999), Some((5, 0)), true => false ; "born before the floor")]
    #[test_case((5, 0), Some((5, 0)), false => false ; "not empty")]
    #[test_case((4, 0), None, true => true ; "no floor, empty")]
    #[test_case((6, 0), None, false => false ; "no floor, not empty")]
    fn what_can_be_a_new_directory(
        born: (i64, i64),
        floor: Option<(i64, i64)>,
        empty: bool,
    ) -> bool {
        is_new_empty_directory(born, floor, empty)
    }

    /// The change time read before a directory is made bounds its birth time
    /// only when the change time read after is no earlier: one that went
    /// backwards is a modification time a user can set (sshfs, vfat, exfat).
    #[test_case((5, 0), (5, 0) => Some((5, 0)) ; "unchanged")]
    #[test_case((5, 0), (6, 3) => Some((5, 0)) ; "moved forward")]
    #[test_case((9, 0), (5, 0) => None ; "went backwards")]
    #[test_case((5, 7), (5, 6) => None ; "went back by a nanosecond")]
    fn what_bounds_a_new_directory(before: (i64, i64), after: (i64, i64)) -> Option<(i64, i64)> {
        floor_across(before, after)
    }

    /// Filectrl's own refusal to use a staging directory reads in the
    /// `Cannot` form.
    #[test]
    fn a_replaced_staging_directory_is_refused_in_filectrls_words() {
        let paths = Paths {
            old: PathBuf::from("/src/one"),
            new: PathBuf::from("/dest/one"),
            depth: 0,
        };

        assert_eq!(
            format!(
                "Cannot move {} to {}: its staging directory was replaced",
                compact(Path::new("/src/one")),
                compact(Path::new("/dest/one"))
            ),
            refused_or_failed(true, &paths, &StagingReplaced::error())
        );
    }

    /// A directory of this user's swapped in at the staging name between its
    /// creation and its opening: `theirs`, made before the floor, then renamed
    /// over the empty directory just made. It is refused; one with an entry
    /// is left as it was (its mode, its entries, and its name), and an empty
    /// one is removed, as anyone who can write the parent could remove it.
    fn swap_in_at_the_staging_name(
        label: &str,
        mode: u32,
        entry: bool,
    ) -> (TempDir, std::io::Error) {
        let fx = TempDir::new(label);
        let parent = File::open(fx.path()).unwrap();
        let theirs = fx.join("theirs");
        fs::create_dir(&theirs).unwrap();
        if entry {
            fs::write(theirs.join("one"), b"keep").unwrap();
        }
        // Past any timestamp granularity, then a change to the parent, so
        // the floor is later than `theirs` was born.
        thread::sleep(Duration::from_millis(20));
        fs::write(fx.join("later"), b"").unwrap();
        let floor = changed(&fstat(&parent).unwrap());
        fs::create_dir(fx.join("s")).unwrap();
        // Renamed before its mode is set: macOS refuses to rename a directory
        // its owner cannot write.
        fs::rename(&theirs, fx.join("s")).unwrap();
        fs::set_permissions(fx.join("s"), fs::Permissions::from_mode(mode)).unwrap();

        let error = Staging::adopt(
            &parent,
            c"s".to_owned(),
            fx.join("s"),
            Some(floor),
            c"entry",
        )
        .err()
        .expect("the swapped directory is refused");

        // Only Linux can hold a directory its owner cannot read to look
        // inside it; elsewhere it is refused as unreadable.
        if cfg!(target_os = "linux") || mode & 0o500 == 0o500 {
            assert!(StagingReplaced::is(&error), "{error}");
        } else {
            assert_eq!(
                Some(nix::errno::Errno::EACCES as i32),
                error.raw_os_error(),
                "{error}"
            );
        }
        if !entry {
            assert!(fs::symlink_metadata(fx.join("s")).is_err());
            return (fx, error);
        }
        assert_eq!(mode, mode_of(&fx.join("s")) & 0o7777);
        fs::set_permissions(fx.join("s"), fs::Permissions::from_mode(0o755)).unwrap();
        {
            assert_eq!(
                b"keep".to_vec(),
                fs::read(fx.join("s").join("one")).unwrap()
            );
        }
        (fx, error)
    }

    /// A staging directory made just now and this user's, which holds an
    /// entry by the time it is opened, is refused by its entry alone.
    #[test]
    fn a_new_staging_directory_holding_an_entry_is_refused() {
        let fx = TempDir::new("tasks_staging_not_empty");
        let parent = File::open(fx.path()).unwrap();
        let floor = changed(&fstat(&parent).unwrap());
        fs::create_dir(fx.join("s")).unwrap();
        fs::write(fx.join("s").join("one"), b"keep").unwrap();

        let error = Staging::adopt(
            &parent,
            c"s".to_owned(),
            fx.join("s"),
            Some(floor),
            c"entry",
        )
        .err()
        .expect("a staging directory holding an entry is refused");

        assert!(StagingReplaced::is(&error), "{error}");
        assert_eq!(
            b"keep".to_vec(),
            fs::read(fx.join("s").join("one")).unwrap()
        );
    }

    /// One holding an entry is refused by its entries, whatever its times.
    #[test]
    fn a_directory_with_entries_swapped_in_at_the_staging_name_is_refused() {
        let (_fx, error) = swap_in_at_the_staging_name("tasks_staging_swap_full", 0o555, true);

        assert_eq!(None, error.raw_os_error());
        assert_eq!("its staging directory was replaced", error.to_string());
    }

    /// One the umask would have left unreadable is refused too, and its mode
    /// is left as it was, whether it is refused before or after it is given
    /// access to be read.
    #[test]
    fn an_unreadable_directory_swapped_in_at_the_staging_name_keeps_its_mode() {
        swap_in_at_the_staging_name("tasks_staging_swap_locked", 0o000, true);
    }

    /// An empty one is refused by its birth time, where the filesystem
    /// records one: it was born before the staging directory was made.
    #[test]
    fn an_empty_directory_swapped_in_at_the_staging_name_is_refused_by_its_birth() {
        let probe = TempDir::new("tasks_staging_swap_probe");
        if !crate::file_system::entry_id::records_birth_time(probe.path()) {
            eprintln!("skipped: this filesystem records no birth time");
            return;
        }
        swap_in_at_the_staging_name("tasks_staging_swap_empty", 0o755, false);
    }

    /// An entry in the staging directory the copy did not put there is never
    /// taken for the copy, landed, or removed, whatever the standing answer:
    /// the copy is refused, and the entry granted is left as it was.
    #[test_case(None ; "with no standing answer")]
    #[test_case(Some(ConflictChoice::SkipAll) ; "under skip all")]
    fn an_entry_planted_in_the_staging_directory_is_never_landed_or_removed(
        standing: Option<ConflictChoice>,
    ) {
        let fx = TempDir::new("tasks_staging_planted");
        let (src, dst) = (fx.join("src"), fx.join("dst"));
        fs::write(&src, b"src").unwrap();
        fs::write(&dst, b"dest").unwrap();
        let granted = seen(&dst).unwrap();
        let parent = File::open(fx.path()).unwrap();
        let staging = Staging::create(&parent, fx.path(), [c"s".to_owned()], c"dst").unwrap();
        fs::write(fx.join("s").join("dst"), b"planted").unwrap();
        let conflicts = answered(standing);
        let mut buffer = [0u8; 64];
        let mut context = CopyContext::new(
            CopySettings {
                is_move: false,
                conflicts: &conflicts,
            },
            &mut buffer,
            None,
            0,
        );
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();
        let stat = fstatat(&parent, c"src", AtFlags::AT_SYMLINK_NOFOLLOW).unwrap();
        let at = At {
            src: &parent,
            dst: &parent,
            src_name: c"src",
            dst_name: c"dst",
        };
        let paths = Paths {
            old: src.clone(),
            new: dst.clone(),
            depth: 0,
        };

        let finished = replace_through(
            &mut context,
            &mut active,
            &mut errors,
            Replacement { granted, staging },
            &at,
            &paths,
            &stat,
        );
        active.done();

        assert!(finished);
        assert_eq!(
            vec![format!(
                "Cannot copy {} to {}: its staging directory was changed; {} was not replaced",
                compact(&src),
                compact(&dst),
                compact(&dst)
            )],
            errors
        );
        assert_eq!(0, context.skipped);
        assert_eq!(
            b"planted".to_vec(),
            fs::read(fx.join("s").join("dst")).unwrap()
        );
        assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
    }

    /// The entry the copy staged, replaced in the staging directory before it
    /// lands, is refused: what took its place is neither landed nor removed.
    #[test]
    fn a_staged_entry_replaced_before_it_lands_is_refused() {
        let fx = TempDir::new("tasks_staging_replaced_entry");
        let dest = fx.join("d");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("one"), b"granted").unwrap();
        let granted = seen(&dest.join("one")).unwrap();
        let handle = File::open(&dest).unwrap();
        let staging = staged_beside(&handle, &dest, b"staged");
        // Written first under another name, so it cannot reuse the staged
        // entry's inode number.
        fs::write(dest.join("s").join("other"), b"planted").unwrap();
        fs::rename(dest.join("s").join("other"), dest.join("s").join("one")).unwrap();
        let mut buffer = [0u8; 8];
        let mut context = CopyContext::new(settings(false), &mut buffer, None, 0);
        let at = At {
            src: &handle,
            dst: &handle,
            src_name: c"one",
            dst_name: c"one",
        };
        let paths = Paths {
            old: fx.join("src"),
            new: dest.join("one"),
            depth: 0,
        };

        let landed = land(&mut context, &staging, granted, &at, &paths);
        let removed = staging.remove();

        assert_eq!(
            Err(format!(
                "Cannot copy {} to {}: its staging directory was changed; {} was not replaced",
                compact(&fx.join("src")),
                compact(&dest.join("one")),
                compact(&dest.join("one"))
            )),
            landed
        );
        assert_eq!(
            Some(nix::libc::ENOTEMPTY),
            removed.unwrap_err().raw_os_error()
        );
        assert_eq!(
            b"planted".to_vec(),
            fs::read(dest.join("s").join("one")).unwrap()
        );
        assert_eq!(b"granted".to_vec(), fs::read(dest.join("one")).unwrap());
    }

    /// An empty directory swapped in at the staging name after the staging
    /// directory was made is not removed in its place.
    #[test]
    fn an_empty_directory_at_the_staging_name_since_is_not_removed() {
        let fx = TempDir::new("tasks_staging_empty_swapped");
        let parent = open_parent(&fx.join("x")).unwrap();
        let staging = Staging::create(&parent, fx.path(), [c"s".to_owned()], c"entry").unwrap();
        fs::rename(fx.join("s"), fx.join("moved")).unwrap();
        fs::create_dir(fx.join("s")).unwrap();

        let removed = staging.remove();

        assert_eq!(
            "another entry holds its name, and was left alone",
            removed.unwrap_err().to_string()
        );
        assert!(fx.join("s").is_dir());
    }

    /// Owner-only, so what is staged is never reachable by others before it
    /// lands.
    #[test]
    fn a_staging_directory_is_owner_only() {
        let fx = TempDir::new("tasks_staging_mode");
        let parent = open_parent(&fx.join("x")).unwrap();

        let staging = Staging::create(&parent, fx.path(), [c"s".to_owned()], c"entry").unwrap();

        assert_eq!(0o700, mode_of(&fx.join("s")) & 0o7777);
        drop(staging);
    }

    /// A staging directory that is dropped removes the entry staged in it and
    /// then itself.
    #[test]
    fn a_dropped_staging_directory_takes_its_entry_with_it() {
        let fx = TempDir::new("tasks_staging_drop");
        let parent = open_parent(&fx.join("x")).unwrap();
        let mut staging = Staging::create(&parent, fx.path(), [c"s".to_owned()], c"entry").unwrap();
        stage(&mut staging, b"staged");

        drop(staging);

        assert!(!fx.join("s").exists());
    }

    /// Removal goes through the staging directory's own handle, then by name
    /// only for an empty directory: one planted at its name since, holding an
    /// entry of the same name, is left alone.
    #[test]
    fn a_staging_directory_swapped_since_is_not_emptied() {
        let fx = TempDir::new("tasks_staging_swapped");
        let parent = open_parent(&fx.join("x")).unwrap();
        let mut staging = Staging::create(&parent, fx.path(), [c"s".to_owned()], c"entry").unwrap();
        stage(&mut staging, b"staged");
        fs::rename(fx.join("s"), fx.join("moved")).unwrap();
        fs::create_dir(fx.join("s")).unwrap();
        fs::write(fx.join("s").join("entry"), b"planted").unwrap();

        drop(staging);

        assert_eq!(
            b"planted".to_vec(),
            fs::read(fx.join("s").join("entry")).unwrap()
        );
        assert!(!fx.join("moved").join("entry").exists());
    }

    /// Every name offered is another, within a call and across calls, so no
    /// two replacements can meet in one staging directory.
    #[test]
    fn staging_names_are_never_offered_twice() {
        let first: Vec<CString> = staging_names().collect();
        let second: Vec<CString> = staging_names().collect();
        let all: std::collections::HashSet<&CString> = first.iter().chain(&second).collect();

        assert_eq!(first.len() + second.len(), all.len());
        assert_eq!(STAGING_ATTEMPTS, u64::try_from(first.len()).unwrap());
        let pid = format!(".filectrl-{}-", std::process::id());
        assert!(
            all.iter()
                .all(|name| name.to_str().unwrap().starts_with(&pid)),
            "{all:?}"
        );
    }

    /// Every name offered taken: nothing is created and nothing is touched.
    #[test]
    fn a_staging_directory_with_no_free_name_is_refused() {
        let fx = TempDir::new("tasks_staging_none");
        fs::write(fx.join("taken"), b"keep").unwrap();
        let parent = open_parent(&fx.join("x")).unwrap();

        let error = Staging::create(&parent, fx.path(), [c"taken".to_owned()], c"entry")
            .err()
            .expect("no name was free");

        assert_eq!(ErrorKind::AlreadyExists, error.kind());
        // No errno: filectrl's own refusal, which `replace_entry` words so.
        assert_eq!(None, error.raw_os_error());
        assert_eq!("no free name for a staging directory", error.to_string());
        assert_eq!(b"keep".to_vec(), fs::read(fx.join("taken")).unwrap());
    }

    /// A staging directory is removed from the directory it was made in,
    /// through that directory's handle, even once that directory has been
    /// renamed and another made at its old name holds one of the same name.
    #[test_case(false ; "removed")]
    #[test_case(true ; "dropped")]
    fn a_staging_directory_is_removed_from_the_directory_it_was_made_in(dropped: bool) {
        let fx = TempDir::new("tasks_staging_parent_moved");
        let made_in = fx.join("p");
        fs::create_dir(&made_in).unwrap();
        let parent = File::open(&made_in).unwrap();
        let staging = Staging::create(&parent, &made_in, [c"s".to_owned()], c"entry").unwrap();
        fs::rename(&made_in, fx.join("q")).unwrap();
        fs::create_dir_all(made_in.join("s")).unwrap();

        if dropped {
            drop(staging);
        } else {
            staging.remove().unwrap();
        }

        assert!(!fx.join("q").join("s").exists());
        assert!(made_in.join("s").is_dir());
    }

    /// Writes `contents` as the entry of `staging`, recorded as the one the
    /// copy made, as `copy_entry` records it.
    fn stage(staging: &mut Staging<'_>, contents: &[u8]) {
        let entry = staging
            .path
            .join(OsStr::from_bytes(staging.entry.to_bytes()));
        fs::write(&entry, contents).unwrap();
        staging.staged = EntryId::of_path(&entry);
    }

    /// A staging directory with `staged` written as its entry, `one`, beside
    /// `dir`'s own `one`, for `land`.
    fn staged_beside<'a>(dir: &'a File, dir_path: &Path, staged: &[u8]) -> Staging<'a> {
        let mut staging = Staging::create(dir, dir_path, [c"s".to_owned()], c"one").unwrap();
        stage(&mut staging, staged);
        staging
    }

    /// `land` looks at the name and renames onto it through the directory
    /// handle it is given: the directory renamed away and another made at its
    /// path is not what it lands in.
    #[test]
    fn a_landing_goes_through_the_directory_it_was_given() {
        let fx = TempDir::new("tasks_land_handle");
        let dest = fx.join("d");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("one"), b"granted").unwrap();
        let granted = seen(&dest.join("one")).unwrap();
        let handle = File::open(&dest).unwrap();
        let staging = staged_beside(&handle, &dest, b"staged");
        fs::rename(&dest, fx.join("moved")).unwrap();
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("one"), b"elsewhere").unwrap();
        let mut buffer = [0u8; 8];
        let mut context = CopyContext::new(settings(false), &mut buffer, None, 0);
        let at = At {
            src: &handle,
            dst: &handle,
            src_name: c"one",
            dst_name: c"one",
        };
        let paths = Paths {
            old: fx.join("src"),
            new: dest.join("one"),
            depth: 0,
        };

        assert_eq!(Ok(true), land(&mut context, &staging, granted, &at, &paths));
        staging.remove().unwrap();

        assert_eq!(
            b"staged".to_vec(),
            fs::read(fx.join("moved").join("one")).unwrap()
        );
        assert_eq!(b"elsewhere".to_vec(), fs::read(dest.join("one")).unwrap());
    }

    /// A landing whose rename fails for another reason than a taken name is
    /// a failure that leaves the entry granted, and says so. The staging
    /// directory can then not be removed either, which `remove` reports.
    #[test]
    fn a_landing_that_fails_leaves_the_entry_and_says_so() {
        let fx = TempDir::new("tasks_land_fails");
        let dest = fx.join("d");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("one"), b"granted").unwrap();
        let granted = seen(&dest.join("one")).unwrap();
        let handle = File::open(&dest).unwrap();
        let staging = staged_beside(&handle, &dest, b"staged");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(dest.join("probe"), b"").is_ok() {
            eprintln!("skipped: a directory without write permission can be written here");
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let mut buffer = [0u8; 8];
        let mut context = CopyContext::new(settings(false), &mut buffer, None, 0);
        let at = At {
            src: &handle,
            dst: &handle,
            src_name: c"one",
            dst_name: c"one",
        };
        let paths = Paths {
            old: fx.join("src"),
            new: dest.join("one"),
            depth: 0,
        };

        let landed = land(&mut context, &staging, granted, &at, &paths);
        let removed = staging.remove();
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            Err(format!(
                "Failed to copy {} to {}: Permission denied (os error 13); {} was not replaced",
                compact(&fx.join("src")),
                compact(&dest.join("one")),
                compact(&dest.join("one"))
            )),
            landed
        );
        assert_eq!(Some(nix::libc::EACCES), removed.unwrap_err().raw_os_error());
        assert_eq!(b"granted".to_vec(), fs::read(dest.join("one")).unwrap());
    }

    /// A symlink at the name of a directory being given access is never
    /// followed: the handle is refused, the link's target keeps its mode,
    /// whether it is a directory or a file, and so does a mode change by name
    /// (which only ever acts on a node the copy just made).
    #[test_case(true ; "a link to a directory")]
    #[test_case(false ; "a link to a file")]
    fn giving_a_directory_access_never_follows_a_symlink(to_directory: bool) {
        let fx = TempDir::new("tasks_access_link");
        let (target, mode) = if to_directory {
            let target = fx.join("dir");
            fs::create_dir(&target).unwrap();
            (target, 0o755)
        } else {
            let target = fx.join("file");
            fs::write(&target, b"f").unwrap();
            (target, 0o600)
        };
        fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
        std::os::unix::fs::symlink(&target, fx.join("link")).unwrap();
        let parent = File::open(fx.path()).unwrap();

        let opened = open_unreadable(&parent, c"link", |_| Ok(()), |_| 0o777);
        let set = set_mode_by_name(&parent, c"link", 0o777);

        assert!(opened.is_err());
        // Linux refuses a mode change on a symlink; macOS changes the link's
        // own mode. Neither reaches the target.
        #[cfg(target_os = "linux")]
        assert_eq!(Some(nix::libc::EOPNOTSUPP), set.unwrap_err().raw_os_error());
        #[cfg(not(target_os = "linux"))]
        let _ = set;
        assert_eq!(mode, mode_of(&target) & 0o7777);
    }

    /// Another entry exchanged in at the name of an unreadable directory once
    /// it is held (the `admit` step, between holding it and changing its
    /// mode) never has its mode changed: the change goes to the directory the
    /// handle holds, wherever it now is, and that is the one opened.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_entry_exchanged_in_at_an_unreadable_directory_keeps_its_mode() {
        let fx = TempDir::new("tasks_access_exchange");
        let (dir, victim) = (fx.join("dir"), fx.join("victim"));
        fs::create_dir(&dir).unwrap();
        fs::write(&victim, b"private").unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
        let expected = EntryId::of_path(&dir).unwrap();
        let parent = File::open(fx.path()).unwrap();
        let exchange = |_: &std::os::fd::OwnedFd| {
            rustix::fs::renameat_with(
                &parent,
                c"dir",
                &parent,
                c"victim",
                rustix::fs::RenameFlags::EXCHANGE,
            )?;
            Ok(())
        };

        let opened = open_unreadable(&parent, c"dir", exchange, |had| had | 0o700);

        let (opened, had) = opened.unwrap();
        assert_eq!(0o000, had);
        assert_eq!(expected, EntryId::of(&opened).unwrap());
        // The names were exchanged: `dir` now holds the file, `victim` the
        // directory.
        assert_eq!(0o600, mode_of(&dir) & 0o7777);
        assert_eq!(0o700, mode_of(&victim) & 0o7777);
    }

    /// Without a handle that needs no permission (anywhere but Linux), a new
    /// directory the owner cannot read stays unreadable: it is reported, and
    /// nothing is changed by name.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn an_unreadable_new_directory_is_reported_where_it_cannot_be_held() {
        let fx = TempDir::new("tasks_access_unheld");
        fs::create_dir(fx.join("dir")).unwrap();
        fs::set_permissions(fx.join("dir"), fs::Permissions::from_mode(0o000)).unwrap();
        let parent = File::open(fx.path()).unwrap();

        let opened = open_created(&parent, c"dir", None);

        let mode = mode_of(&fx.join("dir")) & 0o7777;
        fs::set_permissions(fx.join("dir"), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            Some(nix::libc::EACCES),
            opened.err().and_then(|e| e.raw_os_error())
        );
        assert_eq!(0o000, mode);
    }

    /// A landing that cannot look at the name (a directory without search
    /// permission) cannot tell what holds it, so the entry granted may still
    /// be there, and the message says it was not replaced.
    #[test]
    fn a_landing_that_cannot_look_at_the_name_says_the_entry_was_left() {
        let fx = TempDir::new("tasks_land_unsearchable");
        let dest = fx.join("d");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("one"), b"granted").unwrap();
        let granted = seen(&dest.join("one")).unwrap();
        let handle = File::open(&dest).unwrap();
        let staging = staged_beside(&handle, &dest, b"staged");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o444)).unwrap();
        if fs::symlink_metadata(dest.join("one")).is_ok() {
            eprintln!("skipped: a directory without search permission can be searched here");
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let mut buffer = [0u8; 8];
        let mut context = CopyContext::new(settings(true), &mut buffer, None, 0);
        let at = At {
            src: &handle,
            dst: &handle,
            src_name: c"one",
            dst_name: c"one",
        };
        let paths = Paths {
            old: fx.join("src"),
            new: dest.join("one"),
            depth: 0,
        };

        let landed = land(&mut context, &staging, granted, &at, &paths);
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
        staging.remove().unwrap();

        assert_eq!(
            Err(format!(
                "Failed to move {} to {}: Permission denied (os error 13); {} was not replaced",
                compact(&fx.join("src")),
                compact(&dest.join("one")),
                compact(&dest.join("one"))
            )),
            landed
        );
        assert_eq!(b"granted".to_vec(), fs::read(dest.join("one")).unwrap());
    }

    /// A staging directory that cannot be removed is reported to the user as
    /// a warning naming where it was left, not only logged.
    #[test]
    fn a_staging_directory_that_cannot_be_removed_is_reported() {
        let fx = TempDir::new("tasks_staging_left");
        let dest = fx.join("d");
        fs::create_dir(&dest).unwrap();
        let handle = File::open(&dest).unwrap();
        let staging = staged_beside(&handle, &dest, b"staged");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(dest.join("probe"), b"").is_ok() {
            eprintln!("skipped: a directory without write permission can be written here");
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let (tx, rx) = mpsc::channel();
        let active = copy_task(tx);
        while rx.try_recv().is_ok() {}

        clear_staging(&active, staging);
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
        active.done();

        let warnings: Vec<String> = rx
            .try_iter()
            .filter_map(|command| match command {
                Command::AlertWarn(message) => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(
            vec![format!(
                "Failed to remove the staging directory {}: Permission denied (os error 13)",
                compact(&dest.join("s"))
            )],
            warnings
        );
        assert!(dest.join("s").exists(), "it is where the warning says");
    }

    #[test]
    fn a_vanished_source_leaves_the_destination_it_would_have_replaced() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_source_gone");
        fs::remove_file(&src).unwrap();

        // A task can wait behind a long operation on the shared worker, so the
        // source it was going to copy may be gone by the time it runs.
        let granted = Seen::of_path(&dst).unwrap();
        let prepared = prepare_destination(active, settings(false), granted, &src, &dst, 0o100_644);

        assert!(prepared.is_none());
        assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
    }

    /// A granted destination that cannot be looked at (its directory has no
    /// search permission) may still hold the entry granted, so the failure
    /// says it was not replaced, and nothing is opened or copied.
    #[test]
    fn a_granted_destination_that_cannot_be_looked_at_says_it_was_left() {
        let fx = TempDir::new("tasks_prepare_unsearchable");
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
        let (tx, rx) = mpsc::channel();

        let prepared = prepare_destination(
            copy_task(tx),
            settings(false),
            granted,
            &src,
            &target,
            mode_of(&src),
        );
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(prepared.is_none());
        assert_eq!(
            Some(format!(
                "Failed to copy {} to {}: Permission denied (os error 13); {} was not replaced",
                compact(&src),
                compact(&target),
                compact(&target)
            )),
            finished_task(&rx).error_message()
        );
        assert_eq!(b"granted".to_vec(), fs::read(&target).unwrap());
    }

    #[test]
    fn dir_total_size_sums_the_files_of_the_whole_tree_but_not_its_symlinks() {
        let fx = TempDir::new("tasks");
        let root = fx.join("tree");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"abc").unwrap();
        fs::write(root.join("sub").join("b.txt"), b"defgh").unwrap();
        // Recreated as a link, so no bytes are transferred for it.
        std::os::unix::fs::symlink(root.join("a.txt"), root.join("link")).unwrap();
        let (tx, _rx) = mpsc::channel();
        let active = copy_task(tx);

        assert_eq!(Some(8), dir_total_size(&active, &root));
        active.done();
    }

    #[test]
    fn copy_path_advances_progress_from_the_bytes_written() {
        let fx = TempDir::new("tasks_copy_progress");
        let src = fx.join("src.bin");
        fs::write(&src, [7u8; 200]).unwrap();
        let (tx, rx) = mpsc::channel();
        let (mut active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Copy(Transfer {
                source: String::new(),
                destination: String::new(),
            }),
            200,
        );
        let mut errors = Vec::new();

        assert!(copy_path(
            &mut context(false, &mut [0u8; 64]),
            &mut active,
            &mut errors,
            None,
            &listed(&src),
            &src,
            &fx.join("dst.bin"),
        ));
        active.done();
        let completed: Vec<u64> = rx
            .try_iter()
            .filter_map(|command| match command {
                Command::Progress(task) if !task.is_terminal() => {
                    Some(task.combine_progress(&Progress::default()).completed)
                }
                _ => None,
            })
            .collect();

        // One 64-byte chunk is the first update, before `done` fills the bar,
        // and each later update reports more than the last.
        assert_eq!(Some(&64), completed.first());
        assert!(completed.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn a_tree_of_small_files_sends_progress_per_share_of_the_total_not_per_file() {
        const FILES: usize = 1000;
        let fx = TempDir::new("tasks_copy_tree_progress");
        let src = fx.join("src");
        fs::create_dir(&src).unwrap();
        for i in 0..FILES {
            fs::write(src.join(i.to_string()), [7u8; 10]).unwrap();
        }
        let (tx, rx) = mpsc::channel();
        let active = copy_task(tx);

        let start = Instant::now();
        let (active, outcome) = copy_with_progress(
            settings(false),
            Prepared::default(),
            &PathInfo::try_from(src.as_path()).unwrap(),
            active,
            &src,
            &fx.join("dst"),
        )
        .expect("not cancelled");
        let elapsed = start.elapsed();
        active.done();
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        let updates: Vec<Progress> = rx
            .try_iter()
            .filter_map(|command| match command {
                Command::Progress(task) if !task.is_terminal() => {
                    Some(task.combine_progress(&Progress::default()))
                }
                _ => None,
            })
            .collect();

        // `set_total` sends one, the first chunk another, and the floor admits
        // one more per interval elapsed. One per file would be a thousand.
        let bound = 2 + elapsed.as_millis() / PROGRESS_MIN_INTERVAL.as_millis();
        assert!(
            (updates.len() as u128) <= bound,
            "{} updates in {elapsed:?} for {FILES} files",
            updates.len()
        );
        // The share is of the tree's bytes, so every update reports that total,
        // and the first chunk counts toward it rather than filling the bar.
        let total = FILES as u64 * 10;
        assert!(
            updates.iter().all(|progress| progress.total == total),
            "{updates:?}"
        );
        let [first, chunk, ..] = updates.as_slice() else {
            panic!("expected the total and the first chunk, got {updates:?}");
        };
        assert_eq!(0, first.completed);
        assert_eq!(10, chunk.completed);
        // The copy finishes inside the floor, so the updates above cannot tell
        // a share of the total from none at all. The threshold can.
        assert_eq!(
            total * PROGRESS_DEBOUNCE_PERCENTAGE / 100,
            outcome.progress_threshold
        );
        assert_ne!(0, outcome.progress_threshold);
    }

    #[test]
    fn copy_path_recreates_a_symlink_without_following_it() {
        let fx = TempDir::new("tasks");
        let target = fx.join("target.txt");
        std::fs::write(&target, b"hello").unwrap();
        std::fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();

        let link = fx.join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let dst = fx.join("copied_link.txt");

        assert!(unix_mode::is_symlink(mode_of(&link)));

        let errors = copy_one(false, &link, &dst);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");

        // The destination must itself be a symlink pointing at the same target,
        // not a regular file containing the target's bytes.
        let dst_meta = fs::symlink_metadata(&dst).unwrap();
        assert!(dst_meta.is_symlink(), "destination must be a symlink");
        assert_eq!(fs::read_link(&dst).unwrap(), target);

        // The link's target must be untouched: copy must not chmod through the
        // link or rewrite its contents.
        let target_mode = fs::symlink_metadata(&target).unwrap().permissions().mode() & 0o7777;
        assert_eq!(target_mode, 0o600, "copy must not chmod the symlink target");
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
    }

    /// The permission bits the umask leaves of `0o777`. Probed rather than
    /// read, since reading the umask means setting it for every thread.
    fn umask_leaves(dir: &Path) -> u32 {
        use std::os::unix::fs::OpenOptionsExt;
        let probe = dir.join("umask_probe");
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o777)
            .open(&probe)
            .unwrap();
        let leaves = mode_of(&probe) & 0o777;
        fs::remove_file(&probe).unwrap();
        leaves
    }

    // A copy is `cp` without `-p`: the umask applies and the special bits go,
    // so a file copied where others can reach it is not a setuid program of
    // the user who copied it. A move is `mv`, which keeps the mode.
    #[test_case(0o4755, false ; "a copy drops setuid")]
    #[test_case(0o2755, false ; "a copy drops setgid")]
    #[test_case(0o777, false ; "a copy takes the umask")]
    #[test_case(0o4755, true ; "a move keeps setuid on the user's own file")]
    #[test_case(0o777, true ; "a move ignores the umask")]
    fn a_copied_file_keeps_the_special_bits_and_ignores_the_umask_only_when_moved(
        mode: u32,
        is_move: bool,
    ) {
        let fx = TempDir::new("tasks_file_mode");
        let src = fx.join("src");
        fs::write(&src, b"x").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();

        let errors = copy_one(is_move, &src, &fx.join("dst"));

        assert!(errors.is_empty(), "{errors:?}");
        let expected = if is_move {
            mode
        } else {
            mode & 0o777 & umask_leaves(fx.path())
        };
        assert_eq!(expected, mode_of(&fx.join("dst")) & 0o7777);
    }

    /// Run by `a_copy_takes_the_umask_on_the_owner_bits_too` under a umask
    /// that clears owner bits; on its own it proves nothing. The fixture is
    /// made before the umask is set, so the directory can still be written.
    /// The directory's child can only be created if the copy gives the
    /// directory owner access while filling it.
    #[test]
    #[ignore = "run under a umask of 0o277 by the test below"]
    fn a_copy_under_a_umask_clearing_owner_bits() {
        if !crate::test_support::alone() {
            return;
        }
        let fx = TempDir::new("tasks_file_mode_umask");
        let src = fx.join("src");
        fs::write(&src, b"x").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o777)).unwrap();
        let src_dir = fx.join("src_dir");
        fs::create_dir(&src_dir).unwrap();
        fs::write(src_dir.join("child"), b"x").unwrap();
        fs::set_permissions(&src_dir, fs::Permissions::from_mode(0o777)).unwrap();
        let replaced = fx.join("replaced");
        fs::create_dir(&replaced).unwrap();
        fs::write(replaced.join("src"), b"old").unwrap();
        nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o277));

        let errors = copy_one(false, &src, &fx.join("dst"));
        let dir_errors = copy_one(false, &src_dir, &fx.join("dst_dir"));
        // A replacement is staged in a directory the umask leaves without
        // owner write, which it has to give back to write into it.
        let (new, task) = paste_over(false, &replaced, &src);

        let leaves = 0o777 & umask_leaves(fx.path());
        let dir_mode = mode_of(&fx.join("dst_dir")) & 0o7777;
        // Given back before anything is asserted, so a failure still lets the
        // fixture be removed.
        fs::set_permissions(fx.join("dst_dir"), fs::Permissions::from_mode(0o700)).unwrap();

        assert!(errors.is_empty(), "{errors:?}");
        assert!(dir_errors.is_empty(), "{dir_errors:?}");
        assert_eq!(None, task.error_message());
        assert_eq!(b"x".to_vec(), fs::read(&new).unwrap());
        assert_eq!(Vec::<String>::new(), staging_left(&replaced));
        assert_eq!(leaves, mode_of(&fx.join("dst")) & 0o7777);
        assert_eq!(leaves, dir_mode);
        assert_eq!(
            b"x",
            &fs::read(fx.join("dst_dir").join("child")).unwrap()[..]
        );
    }

    /// A copy's owner bits are the source's as the umask leaves them, like
    /// `cp`: a umask that clears the owner's write bit clears it on the copy.
    /// The umask is process-wide, so the copy runs in a process of its own.
    #[test]
    fn a_copy_takes_the_umask_on_the_owner_bits_too() {
        crate::test_support::run_alone(
            "file_system::tasks::copy::tests::a_copy_under_a_umask_clearing_owner_bits",
            "",
            &[],
        );
    }

    /// The umask `a_tree_copy_under_a_umask` runs under, named by this
    /// variable in the process it runs in.
    const TREE_UMASK: &str = "FILECTRL_TEST_TREE_UMASK";

    /// A directory holding another, copied under a umask whose mode leaves
    /// the owner no search permission, which going back up through the
    /// directory needs.
    #[test]
    #[ignore = "run under a umask by the test below"]
    fn a_tree_copy_under_a_umask() {
        if !crate::test_support::alone() {
            return;
        }
        let umask = std::env::var(TREE_UMASK).unwrap();
        let umask = u32::from_str_radix(&umask, 8).unwrap();
        let fx = TempDir::new("tasks_tree_umask");
        let src = fx.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub").join("child"), b"x").unwrap();
        fs::write(src.join("file"), b"y").unwrap();
        for dir in [src.join("sub"), src.clone()] {
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
        }
        nix::sys::stat::umask(mode_bits(umask));

        let errors = copy_one(false, &src, &fx.join("dst"));
        let dst = fx.join("dst");
        let modes = (mode_of(&dst) & 0o7777, {
            // The top directory may leave its owner no search permission.
            fs::set_permissions(&dst, fs::Permissions::from_mode(0o700)).unwrap();
            mode_of(&dst.join("sub")) & 0o7777
        });
        fs::set_permissions(dst.join("sub"), fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(Vec::<String>::new(), errors);
        let leaves = 0o777 & !umask;
        assert_eq!((leaves, leaves), modes);
        assert_eq!(
            b"x".to_vec(),
            fs::read(dst.join("sub").join("child")).unwrap()
        );
        assert_eq!(b"y".to_vec(), fs::read(dst.join("file")).unwrap());
    }

    /// The umask is process-wide, so each copy runs in a process of its own.
    #[test_case("177" ; "read and write but no search")]
    #[test_case("377" ; "read only")]
    fn a_tree_copies_whole_under_a_umask_that_clears_owner_search(umask: &str) {
        crate::test_support::run_alone(
            "file_system::tasks::copy::tests::a_tree_copy_under_a_umask",
            "",
            &[(TREE_UMASK, OsStr::new(umask))],
        );
    }

    /// A default ACL that takes the owner's search permission, or all of its
    /// access, from every new directory: the copy still reaches the whole
    /// tree. Needs `setfacl` and ACLs on the temporary filesystem; skipped
    /// otherwise.
    #[test_case("u::rw-,g::r-x,o::---", 0o600 ; "no search")]
    #[test_case("u::---,g::r-x,o::---", 0o000 ; "no access")]
    fn a_tree_copies_whole_under_a_default_acl_taking_owner_access(acl: &str, owner: u32) {
        let fx = TempDir::new("tasks_tree_default_acl");
        let src = fx.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub").join("child"), b"x").unwrap();
        let dest = fx.join("dest");
        fs::create_dir(&dest).unwrap();
        if !setfacl(&["-d", "-m", acl], &dest) {
            eprintln!("skipped: no default ACLs here");
            return;
        }

        let errors = copy_one(false, &src, &dest.join("src"));
        let copied = dest.join("src");
        // The ACL takes the same access from the copied directories as from
        // anything created there: the owner access added to write them is
        // taken back when they are finished. Read before the access is given
        // back so the tree can be read and removed.
        // Each from the top down: a directory without search permission
        // hides what is in it.
        let child = copied.join("sub").join("child");
        let owners = [copied.clone(), copied.join("sub"), child].map(|entry| {
            let owner = mode_of(&entry) & 0o700;
            let _ = fs::set_permissions(&entry, fs::Permissions::from_mode(0o700));
            owner
        });

        assert_eq!(Vec::<String>::new(), errors);
        assert_eq!([owner, owner, owner], owners);
        assert_eq!(
            b"x".to_vec(),
            fs::read(copied.join("sub").join("child")).unwrap()
        );
    }

    /// A directory copied into a setgid directory whose default ACL takes
    /// all owner access from what is created there keeps the setgid bit it
    /// inherits, which giving it owner access to write it must not drop.
    #[test]
    fn a_directory_copied_under_a_default_acl_taking_owner_access_keeps_its_setgid() {
        let fx = TempDir::new("tasks_tree_default_acl_setgid");
        let src = fx.join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("child"), b"x").unwrap();
        let dest = fx.join("dest");
        fs::create_dir(&dest).unwrap();
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o2775)).unwrap();
        if mode_of(&dest) & 0o2000 == 0 || !setfacl(&["-d", "-m", "u::---,g::r-x,o::---"], &dest) {
            eprintln!("skipped: no setgid directory with a default ACL here");
            return;
        }

        let errors = copy_one(false, &src, &dest.join("src"));
        let mode = mode_of(&dest.join("src"));
        fs::set_permissions(dest.join("src"), fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(Vec::<String>::new(), errors);
        assert_eq!(0o2000, mode & 0o2000, "{mode:o}");
    }

    /// Enters `fx/src/tree` (holding `f`) for `fx/dst/tree`, as a copy or as
    /// a move's copy stage, with a directory of this user's, `fx/dst/private`
    /// (owner-only, holding `secret` unless `empty`), swapped in at
    /// `dst/tree` the moment the copy made it. Returns whether the walk
    /// entered it and the errors recorded.
    fn enter_with_a_swap(fx: &TempDir, is_move: bool, empty: bool) -> (bool, Vec<String>) {
        let src = fx.join("src").join("tree");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("f"), b"x").unwrap();
        let dst_dir = fx.join("dst");
        let private = dst_dir.join("private");
        fs::create_dir_all(&private).unwrap();
        if !empty {
            fs::write(private.join("secret"), b"keep").unwrap();
        }
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        // Past any timestamp granularity, then a change to the parent, so the
        // floor the copy takes is later than `private` was born.
        thread::sleep(Duration::from_millis(20));
        fs::write(dst_dir.join("later"), b"").unwrap();
        let dst = dst_dir.join("tree");
        let mut buffer = [0u8; 64];
        let mut context = CopyContext::new(settings(is_move), &mut buffer, None, 0);
        let expected = dst.clone();
        context.on_made = Some(Box::new(move |made| {
            assert_eq!(expected, made);
            fs::rename(&private, made).unwrap();
        }));
        let mut errors = Vec::new();
        let (src_parent, dst_parent) = (open_parent(&src).unwrap(), open_parent(&dst).unwrap());
        let (src_name, dst_name) = (c_name(&src).unwrap(), c_name(&dst).unwrap());
        let stat = fstatat(
            &src_parent,
            src_name.as_c_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .unwrap();
        let at = At {
            src: &src_parent,
            dst: &dst_parent,
            src_name: &src_name,
            dst_name: &dst_name,
        };
        let paths = Paths {
            old: src,
            new: dst,
            depth: 0,
        };
        let entered = enter_directory(&at, &paths, &mut errors, &mut context, &stat).is_some();
        (entered, errors)
    }

    /// The refusal of a directory swapped in where the copy made one.
    fn swapped(is_move: bool, fx: &TempDir) -> Vec<String> {
        vec![format!(
            "Cannot {} {} to {}: the directory it was creating was replaced",
            verb(is_move),
            compact(&fx.join("src").join("tree")),
            compact(&fx.join("dst").join("tree"))
        )]
    }

    /// A directory of this user's swapped in at the name a copy or a move
    /// just made a directory at is refused: nothing is written into it and
    /// its mode and entries are left as they were, so a move cannot give it
    /// the source's mode.
    #[test_case(false ; "a copy")]
    #[test_case(true ; "a move")]
    fn a_directory_swapped_in_where_one_was_made_is_left_alone(is_move: bool) {
        let fx = TempDir::new("tasks_made_swapped");

        let (entered, errors) = enter_with_a_swap(&fx, is_move, false);

        let tree = fx.join("dst").join("tree");
        assert!(!entered);
        assert_eq!(swapped(is_move, &fx), errors);
        assert_eq!(0o700, mode_of(&tree) & 0o7777);
        let names: Vec<_> = fs::read_dir(&tree)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(vec![std::ffi::OsString::from("secret")], names);
        assert_eq!(b"keep".to_vec(), fs::read(tree.join("secret")).unwrap());
    }

    /// An empty one is told from the new directory by its birth time, where
    /// the filesystem records one, and is removed: anyone who can write the
    /// parent could remove it as well.
    #[test_case(false ; "a copy")]
    #[test_case(true ; "a move")]
    fn an_empty_directory_swapped_in_where_one_was_made_is_refused(is_move: bool) {
        let fx = TempDir::new("tasks_made_swapped_empty");
        if !crate::file_system::entry_id::records_birth_time(fx.path()) {
            eprintln!("skipped: the filesystem records no birth time");
            return;
        }

        let (entered, errors) = enter_with_a_swap(&fx, is_move, true);

        assert!(!entered);
        assert_eq!(swapped(is_move, &fx), errors);
        assert!(fs::symlink_metadata(fx.join("dst").join("tree")).is_err());
    }

    /// Copies the directory `src` to `dst` through `copy_tree`, with
    /// `on_leave` called as each directory is left, returning whether the walk
    /// finished and the errors it recorded.
    fn copy_tree_leaving(
        src: &Path,
        dst: &Path,
        active: &mut ActiveTask,
        on_leave: Box<dyn FnMut(&Path)>,
    ) -> (bool, Vec<String>) {
        let mut buffer = [0u8; 64];
        let mut context = CopyContext::new(settings(false), &mut buffer, None, 0);
        context.on_leave = Some(on_leave);
        let mut errors = Vec::new();
        let (src_parent, dst_parent) = (open_parent(src).unwrap(), open_parent(dst).unwrap());
        let (src_name, dst_name) = (c_name(src).unwrap(), c_name(dst).unwrap());
        let stat = fstatat(
            &src_parent,
            src_name.as_c_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .unwrap();
        let at = At {
            src: &src_parent,
            dst: &dst_parent,
            src_name: &src_name,
            dst_name: &dst_name,
        };
        let mut paths = Paths {
            old: src.to_path_buf(),
            new: dst.to_path_buf(),
            depth: 0,
        };
        let level = enter_directory(&at, &paths, &mut errors, &mut context, &stat)
            .expect("the directory should be entered");
        let finished = copy_tree(level, &mut paths, active, &mut errors, &mut context);
        (finished, errors)
    }

    /// `src/tree/sub/deeper/file`, and where it is copied to.
    fn deep_tree(label: &str) -> (TempDir, PathBuf, PathBuf) {
        let fx = TempDir::new(label);
        let tree = fx.join("src").join("tree");
        fs::create_dir_all(tree.join("sub").join("deeper")).unwrap();
        fs::write(tree.join("sub").join("deeper").join("file"), b"x").unwrap();
        fs::create_dir(fx.join("dst")).unwrap();
        let dst = fx.join("dst").join("tree");
        (fx, tree, dst)
    }

    /// A directory moved elsewhere while the copy is inside it cannot be gone
    /// back up through: the walk ends there with filectrl's own refusal,
    /// naming the directory it could not return to.
    #[test]
    fn a_directory_moved_while_its_copy_is_inside_it_ends_the_walk() {
        let (fx, tree, dst) = deep_tree("tasks_walk_relocated");
        let (deeper, sub, elsewhere) = (
            tree.join("sub").join("deeper"),
            tree.join("sub"),
            fx.join("elsewhere"),
        );
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);

        let (finished, errors) = copy_tree_leaving(
            &tree,
            &dst,
            &mut active,
            Box::new(move |left| {
                if left == deeper {
                    fs::rename(&sub, &elsewhere).unwrap();
                }
            }),
        );
        active.done();

        assert!(finished);
        assert_eq!(
            vec![format!(
                "Cannot copy {}: it was relocated while it was being read",
                compact(&tree)
            )],
            errors
        );
    }

    /// A directory that cannot be gone back up into for a reason of the
    /// system's is reported in the `Failed to` form, with the reason.
    #[test]
    fn a_directory_that_cannot_be_returned_to_is_reported_with_its_errno() {
        let (fx, tree, dst) = deep_tree("tasks_walk_locked");
        let probe = fx.join("probe");
        fs::create_dir(&probe).unwrap();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = fs::read_dir(&probe).is_err();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o700)).unwrap();
        if !unreadable {
            eprintln!("skipped: a directory without permissions can be read here");
            return;
        }
        let (deeper, locked) = (tree.join("sub").join("deeper"), tree.clone());
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);

        let (finished, errors) = copy_tree_leaving(
            &tree,
            &dst,
            &mut active,
            Box::new(move |left| {
                if left == deeper {
                    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
                }
            }),
        );
        active.done();
        fs::set_permissions(&tree, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(finished);
        assert_eq!(
            vec![format!(
                "Failed to copy {}: Permission denied (os error 13)",
                compact(&tree)
            )],
            errors
        );
    }

    /// A walk cancelled before it meets a directory it cannot return to
    /// still ends as cancelled.
    #[test]
    fn a_cancelled_walk_that_cannot_return_ends_as_cancelled() {
        let (fx, tree, dst) = deep_tree("tasks_walk_relocated_cancelled");
        let (deeper, sub, elsewhere) = (
            tree.join("sub").join("deeper"),
            tree.join("sub"),
            fx.join("elsewhere"),
        );
        let (tx, _rx) = mpsc::channel();
        let (mut active, _, token) = ActiveTask::new(
            tx,
            TaskKind::Copy(Transfer {
                source: String::new(),
                destination: String::new(),
            }),
            1,
        );

        let (finished, errors) = copy_tree_leaving(
            &tree,
            &dst,
            &mut active,
            Box::new(move |left| {
                if left == deeper {
                    token.cancel();
                    fs::rename(&sub, &elsewhere).unwrap();
                }
            }),
        );
        active.cancelled();

        assert!(!finished);
        assert_eq!(
            vec![format!(
                "Cannot copy {}: it was relocated while it was being read",
                compact(&tree)
            )],
            errors
        );
    }

    /// A plain copy of a directory does not keep its modification time, as
    /// `cp -R` without `-p` does not.
    #[test]
    fn a_plain_copy_does_not_keep_a_directorys_modification_time() {
        let fx = TempDir::new("tasks_copy_dir_mtime");
        let src = fx.join("src");
        fs::create_dir(&src).unwrap();
        let old = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        File::open(&src).unwrap().set_modified(old).unwrap();
        let dst = fx.join("dst");

        let errors = copy_one(false, &src, &dst);

        assert_eq!(Vec::<String>::new(), errors);
        assert_ne!(old, fs::metadata(&dst).unwrap().modified().unwrap());
    }

    /// A granted overwrite whose source changed type since it was selected
    /// is refused, and the entry granted is left, which the message says.
    #[test]
    fn a_granted_overwrite_of_a_source_whose_type_changed_says_the_entry_was_left() {
        let fx = TempDir::new("tasks_type_changed_granted");
        let src = fx.join("src");
        std::os::unix::fs::symlink("target", &src).unwrap();
        let selected = listed(&src);
        fs::remove_file(&src).unwrap();
        fs::write(&src, b"now a file").unwrap();
        let dst = fx.join("dst");
        fs::write(&dst, b"dest").unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();

        assert!(copy_path(
            &mut context(false, &mut [0u8; 64]),
            &mut active,
            &mut errors,
            seen(&dst),
            &selected,
            &src,
            &dst,
        ));
        active.done();

        assert_eq!(
            vec![format!(
                "Cannot copy {}: its type changed since it was selected; {} was not replaced",
                compact(&src),
                compact(&dst)
            )],
            errors
        );
        assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
    }

    /// A replacement landing on a name freed since, whose rename fails, never
    /// says an entry was left there: none was.
    #[test]
    fn a_landing_on_a_name_freed_since_that_fails_says_nothing_was_left() {
        let fx = TempDir::new("tasks_land_freed_fails");
        let dest = fx.join("d");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("one"), b"granted").unwrap();
        let granted = seen(&dest.join("one")).unwrap();
        let handle = File::open(&dest).unwrap();
        let staging = staged_beside(&handle, &dest, b"staged");
        fs::remove_file(dest.join("one")).unwrap();
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(dest.join("probe"), b"").is_ok() {
            eprintln!("skipped: a directory without write permission can be written here");
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let mut buffer = [0u8; 8];
        let mut context = CopyContext::new(settings(false), &mut buffer, None, 0);
        let at = At {
            src: &handle,
            dst: &handle,
            src_name: c"one",
            dst_name: c"one",
        };
        let paths = Paths {
            old: fx.join("src"),
            new: dest.join("one"),
            depth: 0,
        };

        let landed = land(&mut context, &staging, granted, &at, &paths);
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
        staging.remove().unwrap();

        assert_eq!(
            Err(format!(
                "Failed to copy {} to {}: Permission denied (os error 13)",
                compact(&fx.join("src")),
                compact(&dest.join("one"))
            )),
            landed
        );
    }

    /// A umask that clears the owner's read bit leaves a new staging
    /// directory impossible to open. On Linux owner access is given back
    /// through a handle that needs none, and the replacement lands; elsewhere
    /// there is no such handle, and the replacement is refused with the entry
    /// left.
    #[test]
    #[ignore = "run under a umask of 0o477 by the test below"]
    fn a_replacement_under_a_umask_clearing_owner_read() {
        if !crate::test_support::alone() {
            return;
        }
        let fx = TempDir::new("tasks_replace_umask_read");
        let src = fx.join("src");
        fs::write(&src, b"x").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o644)).unwrap();
        let dest = fx.join("dest");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("src"), b"old").unwrap();
        nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o477));
        let probe = fx.join("probe");
        fs::create_dir(&probe).unwrap();
        if fs::read_dir(&probe).is_ok() {
            eprintln!("not exercised: a directory without owner read can be read here");
        }

        let (new, task) = paste_over(false, &dest, &src);

        #[cfg(target_os = "linux")]
        {
            assert_eq!(None, task.error_message());
            fs::set_permissions(&new, fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(b"x".to_vec(), fs::read(&new).unwrap());
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert_eq!(
                Some(format!(
                    "Failed to copy {} to {}: Permission denied (os error 13); {} was not replaced",
                    compact(&src),
                    compact(&new),
                    compact(&new)
                )),
                task.error_message()
            );
            assert_eq!(b"old".to_vec(), fs::read(&new).unwrap());
        }
        assert_eq!(Vec::<String>::new(), staging_left(&dest));
    }

    /// The umask is process-wide, so the replacement runs in a process of its
    /// own.
    #[test]
    fn a_replacement_opens_its_staging_directory_whatever_the_umask() {
        crate::test_support::run_alone(
            "file_system::tasks::copy::tests::a_replacement_under_a_umask_clearing_owner_read",
            "",
            &[],
        );
    }

    /// The fixture `a_replacement_that_fails_part_way_leaves_the_entry` makes,
    /// named by this variable in the process it runs this in.
    const FAILING_FIXTURE: &str = "FILECTRL_TEST_FAILING_REPLACEMENT";

    /// A replacement whose writing fails part way, under a file size limit set
    /// by the test below.
    #[test]
    #[ignore = "run under a file size limit by the test below"]
    fn a_replacement_that_fails_to_write() {
        if !crate::test_support::alone() {
            return;
        }
        let fixture = PathBuf::from(std::env::var_os(FAILING_FIXTURE).unwrap());
        let (src, dest) = (fixture.join("src"), fixture.join("dest"));

        let (new, task) = paste_over(false, &dest, &src);

        assert_eq!(
            Some(format!(
                "Failed to copy {} to {}: File too large (os error {}); {} was not replaced",
                compact(&src),
                compact(&new),
                nix::libc::EFBIG,
                compact(&new)
            )),
            task.error_message()
        );
        assert_eq!(b"old".to_vec(), fs::read(&new).unwrap());
        assert_eq!(Vec::<String>::new(), staging_left(&dest));
    }

    /// A replacement that fails part way leaves the entry it was to replace,
    /// and nothing of itself, whoever runs it: the write fails at a file size
    /// limit, in a process of its own that ignores the signal a write past it
    /// raises.
    #[test]
    fn a_replacement_that_fails_part_way_leaves_the_entry() {
        let fx = TempDir::new("tasks_replace_fails");
        let dest = fx.join("dest");
        fs::create_dir(&dest).unwrap();
        fs::write(fx.join("src"), vec![7u8; 64 * 1024]).unwrap();
        fs::write(dest.join("src"), b"old").unwrap();

        crate::test_support::run_alone(
            "file_system::tasks::copy::tests::a_replacement_that_fails_to_write",
            "trap '' XFSZ; ulimit -f 1;",
            &[(FAILING_FIXTURE, fx.path().as_os_str())],
        );
    }

    /// A default ACL taking owner write from new directories would leave the
    /// staging directory unwritable. Needs `setfacl` and ACLs on the temporary
    /// filesystem; skipped otherwise.
    #[test]
    fn a_replacement_writes_its_staging_directory_under_a_default_acl() {
        let fx = TempDir::new("tasks_replace_default_acl");
        let src = fx.join("src");
        fs::write(&src, b"x").unwrap();
        let dest = fx.join("dest");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("src"), b"old").unwrap();
        if !setfacl(&["-d", "-m", "u::r-x,g::r-x,o::---"], &dest) {
            eprintln!("skipped: no default ACLs here");
            return;
        }

        let (new, task) = paste_over(false, &dest, &src);

        assert_eq!(None, task.error_message());
        assert_eq!(b"x".to_vec(), fs::read(&new).unwrap());
        assert_eq!(Vec::<String>::new(), staging_left(&dest));
    }

    // A directory is created owner-writable so its children can be, then
    // given its mode: a copy's owner bits come back to what the source had.
    #[test_case(0o555, false ; "a copy of a read only directory")]
    #[test_case(0o1777, false ; "a copy drops the sticky bit and takes the umask")]
    #[test_case(0o2755, false ; "a copy drops the source's setgid bit")]
    #[test_case(0o555, true ; "a move of a read only directory")]
    #[test_case(0o1777, true ; "a move keeps the sticky bit")]
    fn a_copied_directory_ends_with_the_mode_of_its_kind_of_copy(mode: u32, is_move: bool) {
        let fx = TempDir::new("tasks_directory_mode");
        let (src, dst) = (fx.join("src"), fx.join("dst"));
        fs::create_dir(&src).unwrap();
        fs::write(src.join("child"), b"x").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();

        let errors = copy_one(is_move, &src, &dst);
        let copied = mode_of(&dst) & 0o7777;
        // Writable again before anything can fail, so the fixture can be
        // removed with the children a read-only mode would keep.
        for dir in [&src, &dst] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        }

        assert!(errors.is_empty(), "{errors:?}");
        assert!(dst.join("child").exists());
        let expected = if is_move {
            mode
        } else {
            mode & 0o777 & umask_leaves(fx.path())
        };
        assert_eq!(expected, copied);
    }

    /// A directory created in a setgid directory inherits its setgid bit, and a
    /// copy keeps it, like `cp -R`, so what is later created in the copy takes
    /// the shared group. A file created there does not inherit it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_copied_directory_keeps_the_setgid_bit_its_parent_gives_it() {
        let fx = TempDir::new("tasks_directory_setgid");
        let shared = fx.join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o2775)).unwrap();
        let src = fx.join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("child"), b"x").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();

        let errors = copy_one(false, &src, &shared.join("dst"));

        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(0o2000, mode_of(&shared.join("dst")) & 0o7000);
        assert_eq!(0, mode_of(&shared.join("dst").join("child")) & 0o7000);
    }

    /// A node is created with the umask applied, like any other entry, so a
    /// move puts the source's mode back afterwards.
    #[test_case(true ; "a move keeps the mode")]
    #[test_case(false ; "a copy takes the umask")]
    fn a_moved_fifo_keeps_its_mode_whatever_the_umask(is_move: bool) {
        let fx = TempDir::new("tasks_fifo_mode");
        let src = fx.join("fifo");
        nix::unistd::mkfifo(&src, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o666)).unwrap();
        let dst = fx.join("moved");

        let errors = copy_one(is_move, &src, &dst);

        assert!(errors.is_empty(), "{errors:?}");
        let expected = if is_move {
            0o666
        } else {
            0o666 & umask_leaves(fx.path())
        };
        assert_eq!(expected, mode_of(&dst) & 0o7777);
    }

    /// A moved node gets the source's group before its mode, like a moved
    /// file, so a group-writable FIFO is not writable by whichever group the
    /// destination assigned. Needs a supplementary group to tell the two apart,
    /// and proves nothing for a user without one. Linux only: macOS has no
    /// `getgroups` in nix.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_moved_fifo_keeps_its_group() {
        let primary = nix::unistd::getegid();
        let Some(other) = nix::unistd::getgroups()
            .unwrap()
            .into_iter()
            .find(|&group| group != primary)
        else {
            return;
        };
        let fx = TempDir::new("tasks_fifo_group");
        let src = fx.join("fifo");
        nix::unistd::mkfifo(&src, nix::sys::stat::Mode::from_bits_truncate(0o660)).unwrap();
        nix::unistd::chown(&src, None, Some(other)).unwrap();
        let dst = fx.join("moved");

        let errors = copy_one(true, &src, &dst);

        assert!(errors.is_empty(), "{errors:?}");
        let gid = std::os::unix::fs::MetadataExt::gid(&fs::symlink_metadata(&dst).unwrap());
        assert_eq!(other.as_raw(), gid);
    }

    /// `mv` clears setuid and setgid when it cannot carry the ownership over,
    /// or a program would run as whoever moved it. Another owner cannot be
    /// arranged without root, so the rule is driven with a source that names
    /// one.
    #[test]
    fn a_move_drops_setuid_and_setgid_from_another_users_file() {
        let fx = TempDir::new("tasks_move_other_owner");
        let file = File::create(fx.join("dst")).unwrap();
        let stat = fstat(&file).unwrap();
        let source = Source {
            mode: 0o100_6755,
            uid: stat.st_uid.wrapping_add(1),
            ..Source::of(&stat)
        };

        let paths = Paths {
            old: fx.join("src"),
            new: fx.join("dst"),
            depth: 0,
        };
        apply_final_mode(&context(true, &mut [0u8; 1]), &paths, &file, &source);

        assert_eq!(0o755, mode_of(&fx.join("dst")) & 0o7777);
    }

    /// A move keeps the group bits, like `mv`, so it keeps the group they were
    /// granted to as well: the copy is otherwise created with whichever group
    /// the destination assigns, and a file readable by one group would become
    /// readable by another. Needs a second group to belong to, which
    /// `getgroups` reports on Linux.
    #[cfg(target_os = "linux")]
    #[test_case(false, 0o640 ; "a file")]
    #[test_case(true, 0o750 ; "a directory")]
    fn a_move_keeps_the_group_of_its_source(is_directory: bool, mode: u32) {
        use std::os::unix::fs::MetadataExt;

        let own = nix::unistd::getegid();
        let Some(other) = nix::unistd::getgroups()
            .unwrap()
            .into_iter()
            .find(|group| *group != own)
        else {
            eprintln!("skipped: the user running the tests belongs to one group only");
            return;
        };
        let fx = TempDir::new("tasks_move_group");
        let src = fx.join("src");
        if is_directory {
            fs::create_dir(&src).unwrap();
        } else {
            fs::write(&src, b"x").unwrap();
        }
        std::os::unix::fs::chown(&src, None, Some(other.as_raw())).unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();

        let errors = copy_one(true, &src, &fx.join("dst"));

        assert!(errors.is_empty(), "{errors:?}");
        let copied = fs::symlink_metadata(fx.join("dst")).unwrap();
        assert_eq!(other.as_raw(), copied.gid());
        assert_eq!(mode, copied.mode() & 0o7777);
    }

    /// Another user renames a directory the copy created and leaves a link to
    /// one of the victim's in its place. The copy continues in the directory it
    /// created, and neither writes into nor changes the mode of the victim's.
    #[test]
    fn a_destination_directory_swapped_for_a_symlink_is_not_written_through() {
        let fx = TempDir::new("tasks_destination_swapped");
        let src = fx.join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("planted.txt"), b"x").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o777)).unwrap();
        let victim = fx.join("victim");
        fs::create_dir(&victim).unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o700)).unwrap();
        let dst = fx.join("dst");
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();
        let mut buffer = [0u8; 64];
        // A move, whose final mode is the source's 0o777 whatever the umask.
        let mut context = context(true, &mut buffer);
        let (src_parent, dst_parent) = (open_parent(&src).unwrap(), open_parent(&dst).unwrap());
        let source = fstatat(&src_parent, c"src", AtFlags::AT_SYMLINK_NOFOLLOW).unwrap();
        let mut paths = Paths {
            old: src.clone(),
            new: dst.clone(),
            depth: 0,
        };
        let at = At {
            src: &src_parent,
            dst: &dst_parent,
            src_name: c"src",
            dst_name: c"dst",
        };
        let level = enter_directory(&at, &paths, &mut errors, &mut context, &source)
            .expect("the directory should be entered");

        fs::rename(&dst, fx.join("moved")).unwrap();
        std::os::unix::fs::symlink(&victim, &dst).unwrap();
        assert!(copy_tree(
            level,
            &mut paths,
            &mut active,
            &mut errors,
            &mut context
        ));
        active.done();

        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(0o700, mode_of(&victim) & 0o7777);
        assert!(fs::read_dir(&victim).unwrap().next().is_none());
        assert!(fx.join("moved").join("planted.txt").exists());
        assert_eq!(0o777, mode_of(&fx.join("moved")) & 0o7777);
    }

    /// The source's own owner swaps a directory the copy has listed for a link
    /// to one outside the tree. The link is copied as a link, and nothing
    /// outside the tree is read.
    #[test]
    fn a_source_directory_swapped_for_a_symlink_is_copied_as_the_link() {
        let fx = TempDir::new("tasks_source_swapped");
        let src = fx.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        let outside = fx.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"secret").unwrap();
        let dst = fx.join("dst");
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();
        let mut buffer = [0u8; 64];
        let mut context = context(false, &mut buffer);
        let (src_parent, dst_parent) = (open_parent(&src).unwrap(), open_parent(&dst).unwrap());
        let source = fstatat(&src_parent, c"src", AtFlags::AT_SYMLINK_NOFOLLOW).unwrap();
        let mut paths = Paths {
            old: src.clone(),
            new: dst.clone(),
            depth: 0,
        };
        let at = At {
            src: &src_parent,
            dst: &dst_parent,
            src_name: c"src",
            dst_name: c"dst",
        };
        let level = enter_directory(&at, &paths, &mut errors, &mut context, &source)
            .expect("the directory should be entered");

        fs::rename(src.join("sub"), fx.join("sub.orig")).unwrap();
        std::os::unix::fs::symlink(&outside, src.join("sub")).unwrap();
        assert!(copy_tree(
            level,
            &mut paths,
            &mut active,
            &mut errors,
            &mut context
        ));
        active.done();

        assert!(errors.is_empty(), "{errors:?}");
        assert!(dst.join("sub").symlink_metadata().unwrap().is_symlink());
        assert_eq!(outside, fs::read_link(dst.join("sub")).unwrap());
    }

    /// A regular file swapped for something else between being listed and
    /// being opened. A FIFO would block the open, and with it the one worker
    /// every operation shares; a link to a device would be read without end.
    #[test_case("fifo" ; "a fifo")]
    #[test_case("link" ; "a symlink to a device")]
    #[test_case("file_link" ; "a symlink to a file outside the tree")]
    #[test_case("dir" ; "a directory")]
    fn a_source_file_that_is_no_longer_a_regular_file_is_refused_at_once(name: &'static str) {
        let fx = TempDir::new("tasks_source_not_regular");
        nix::unistd::mkfifo(
            &fx.join("fifo"),
            nix::sys::stat::Mode::from_bits_truncate(0o600),
        )
        .unwrap();
        std::os::unix::fs::symlink("/dev/zero", fx.join("link")).unwrap();
        fs::write(fx.join("outside.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(fx.join("outside.txt"), fx.join("file_link")).unwrap();
        fs::create_dir(fx.join("dir")).unwrap();
        let path = fx.join(name);
        let (tx, rx) = mpsc::channel();

        // On a thread, so an open that blocks fails the test instead of
        // hanging it.
        thread::spawn(move || {
            let (task_tx, _task_rx) = mpsc::channel();
            let dst = path.with_file_name("dst");
            let prepared = prepare_destination(
                copy_task(task_tx),
                settings(false),
                None,
                &path,
                &dst,
                0o100_644,
            );
            let _ = tx.send(prepared.is_none());
        });

        assert_eq!(Ok(true), rx.recv_timeout(Duration::from_secs(5)));
    }

    /// A FIFO selected for a move is a regular file by the time the worker
    /// runs. Recreating it as a FIFO and removing the file would lose its data.
    #[test]
    fn a_move_refuses_a_source_whose_type_changed_since_it_was_selected() {
        let fx = TempDir::new("tasks_move_type_changed");
        let old = fx.join("item");
        nix::unistd::mkfifo(&old, nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap();
        fs::create_dir(fx.join("dest")).unwrap();

        let (new_path, task) = paste_after(true, &fx.join("dest"), &old, || {
            fs::remove_file(&old).unwrap();
            fs::write(&old, b"data").unwrap();
        });

        assert_eq!(b"data".as_slice(), fs::read(&old).unwrap());
        assert!(new_path.symlink_metadata().is_err());
        assert_eq!(
            Some(format!(
                "Cannot move {}: its type changed since it was selected",
                compact(&old)
            )),
            task.error_message()
        );
    }

    /// Another file is renamed over the selected name before the worker runs.
    /// It is copied with its own mode, never the selected file's: a copy of a
    /// 0600 file must not come out readable by others, and a move of one must
    /// not get the other file's setuid or execute bits.
    #[test_case(false ; "a copy")]
    #[test_case(true ; "a move")]
    fn a_file_renamed_over_the_selection_is_copied_with_its_own_mode(is_move: bool) {
        let fx = TempDir::new("tasks_renamed_over");
        let old = fx.join("notes");
        fs::write(&old, b"selected").unwrap();
        fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
        let secret = fx.join("secret");
        fs::write(&secret, b"secret").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        fs::create_dir(fx.join("dest")).unwrap();

        let (new_path, task) = paste_after(is_move, &fx.join("dest"), &old, || {
            fs::rename(&secret, &old).unwrap();
        });

        assert_eq!(None, task.error_message());
        assert_eq!(b"secret".as_slice(), fs::read(&new_path).unwrap());
        assert_eq!(0o600, mode_of(&new_path) & 0o7777);
    }

    /// The destination directory is swapped for a link into the source after
    /// the paste was validated. The copy refuses to descend into the directory
    /// it created rather than copying its own output without end.
    #[test]
    fn a_copy_never_descends_into_a_directory_it_created() {
        let fx = TempDir::new("tasks_copy_into_itself");
        let src = fx.join("s");
        fs::create_dir_all(src.join("sub")).unwrap();
        let dest = fx.join("dest");
        fs::create_dir(&dest).unwrap();

        // A copy that did descend would nest `s/sub` without end; three
        // levels of it is proof enough, and shallow enough to remove.
        let runaway = src.join("sub/s/sub/s/sub/s/sub");
        let (_, task) = paste_after_unless(
            false,
            &dest,
            &src,
            || {
                fs::remove_dir(&dest).unwrap();
                std::os::unix::fs::symlink(src.join("sub"), &dest).unwrap();
            },
            || runaway.exists(),
        );

        let message = task.error_message().expect("the copy reports the refusal");
        assert!(
            message.ends_with("it is inside the destination being written"),
            "{message}"
        );
        assert!(src.join("sub/s/sub").is_dir());
        assert!(!src.join("sub/s/sub/s").exists());
        assert!(src.join("sub/s/sub").read_dir().unwrap().next().is_none());
    }

    /// Another directory is renamed over one the copy listed, between the
    /// listing and the open. It is refused rather than copied under metadata
    /// that describes a different directory.
    #[test]
    fn a_directory_replaced_after_it_was_listed_is_refused() {
        let fx = TempDir::new("tasks_directory_replaced");
        let src = fx.join("src");
        fs::create_dir(&src).unwrap();
        let other = fx.join("other");
        fs::create_dir(&other).unwrap();
        let (src_parent, dst_parent) = (open_parent(&src).unwrap(), open_parent(&src).unwrap());
        let stat = fstatat(&src_parent, c"src", AtFlags::AT_SYMLINK_NOFOLLOW).unwrap();
        fs::rename(&other, &src).unwrap();
        let (tx, _rx) = mpsc::channel();
        let active = copy_task(tx);
        let mut errors = Vec::new();
        let mut buffer = [0u8; 64];
        let mut context = context(false, &mut buffer);
        let at = At {
            src: &src_parent,
            dst: &dst_parent,
            src_name: c"src",
            dst_name: c"dst",
        };
        let paths = Paths {
            old: src.clone(),
            new: fx.join("dst"),
            depth: 0,
        };

        let level = enter_directory(&at, &paths, &mut errors, &mut context, &stat);
        active.done();

        assert!(level.is_none());
        assert_eq!(1, errors.len());
        assert!(
            errors[0].ends_with("it was replaced while it was being read"),
            "{errors:?}"
        );
    }

    /// The copy returns through both of its directories, and each has to be
    /// the one it left.
    #[test_case(false ; "the source moved")]
    #[test_case(true ; "the destination moved")]
    fn a_copy_returns_only_to_the_parents_it_left(destination_moved: bool) {
        let fx = TempDir::new("tasks_copy_reopen_pair");
        for side in ["src", "dst"] {
            fs::create_dir_all(fx.join(side).join("child")).unwrap();
        }
        fs::create_dir(fx.join("elsewhere")).unwrap();
        let open = |side: &str| {
            let parent = open_directory(CWD, &fx.join(side)).unwrap();
            let child = open_directory(&parent, "child").unwrap();
            (EntryId::of(&parent).unwrap(), child)
        };
        let ((src_id, src), (dst_id, dst)) = (open("src"), open("dst"));
        let child = Pair { src, dst };
        let parent = Pair {
            src: src_id,
            dst: dst_id,
        };
        let reopened = child.reopen_parent(parent, "moved").unwrap();
        assert_eq!(src_id, EntryId::of(&reopened.src).unwrap());
        assert_eq!(dst_id, EntryId::of(&reopened.dst).unwrap());

        let side = if destination_moved { "dst" } else { "src" };
        fs::rename(
            fx.join(side).join("child"),
            fx.join("elsewhere").join("child"),
        )
        .unwrap();

        let error = child.reopen_parent(parent, "moved").err().unwrap();
        assert_eq!("moved", error.to_string());
    }

    fn set_times(path: &Path, atime: i64, mtime: i64) {
        use nix::sys::stat::{UtimensatFlags, utimensat};
        utimensat(
            nix::fcntl::AT_FDCWD,
            path,
            &TimeSpec::new(atime, 0),
            &TimeSpec::new(mtime, 0),
            UtimensatFlags::NoFollowSymlink,
        )
        .unwrap();
    }

    fn times_of(path: &Path) -> (i64, i64) {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path).unwrap();
        (metadata.atime(), metadata.mtime())
    }

    #[test]
    fn a_move_keeps_the_access_time_of_a_file() {
        let fx = TempDir::new("tasks_move_file_atime");
        let old = fx.join("f");
        fs::write(&old, b"data").unwrap();
        // Older than the modification time, which `relatime` updates on the
        // first read.
        set_times(&old, 1_000_000_000, 1_000_000_100);
        fs::create_dir(fx.join("dest")).unwrap();

        let (new_path, task) = paste_after(true, &fx.join("dest"), &old, || {});

        assert_eq!(None, task.error_message());
        assert_eq!((1_000_000_000, 1_000_000_100), times_of(&new_path));
    }

    /// Linux only: macOS has no `O_NOATIME`, so the pre-scan's listing sets a
    /// directory's access time before the copy reads it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_move_keeps_the_access_times_of_a_tree_the_scan_listed() {
        let fx = TempDir::new("tasks_move_dir_atime");
        let old = fx.join("d");
        fs::create_dir_all(old.join("sub")).unwrap();
        fs::write(old.join("sub").join("x"), b"x").unwrap();
        // Deepest first, so setting one does not move its parent's again.
        set_times(&old.join("sub"), 1_000_000_000, 1_000_000_100);
        set_times(&old, 1_000_000_200, 1_000_000_300);
        fs::create_dir(fx.join("dest")).unwrap();

        // The progress scan lists every directory before the copy does.
        let (new_path, task) = paste_after(true, &fx.join("dest"), &old, || {});

        assert_eq!(None, task.error_message());
        assert_eq!((1_000_000_200, 1_000_000_300), times_of(&new_path));
        assert_eq!(
            (1_000_000_000, 1_000_000_100),
            times_of(&new_path.join("sub"))
        );
    }

    /// Runs `setfacl` with `args` on `path`. False where it is not installed
    /// or the filesystem has no ACLs, for a test to skip.
    fn setfacl(args: &[&str], path: &Path) -> bool {
        std::process::Command::new("setfacl")
            .args(args)
            .arg(path)
            .status()
            .is_ok_and(|status| status.success())
    }

    fn getfacl(path: &Path) -> String {
        let output = std::process::Command::new("getfacl")
            .args(["-c", "-p"])
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    /// A source of mode 600 with a named user granted `rw`, which makes the
    /// mask, and so the group bits of its mode, `rw` too.
    fn file_with_an_acl(label: &str) -> Option<(TempDir, PathBuf)> {
        let fx = TempDir::new(label);
        let old = fx.join("secret");
        fs::write(&old, b"secret").unwrap();
        fs::set_permissions(&old, fs::Permissions::from_mode(0o600)).unwrap();
        fs::create_dir(fx.join("dest")).unwrap();
        setfacl(&["-m", "u:nobody:rw"], &old).then_some((fx, old))
    }

    #[test]
    fn a_move_keeps_the_acl_so_the_owning_group_gains_nothing() {
        let Some((fx, old)) = file_with_an_acl("tasks_move_acl") else {
            eprintln!("skipped: no ACLs here");
            return;
        };
        let before = getfacl(&old);

        let (new_path, task) = paste_after(true, &fx.join("dest"), &old, || {});

        assert_eq!(None, task.error_message());
        // Without it the mask would be granted to the owning group instead.
        assert_eq!(before, getfacl(&new_path));
        assert!(before.contains("group::---"), "{before}");
        assert_eq!(0o660, mode_of(&new_path) & 0o7777);
    }

    #[test]
    fn a_copy_does_not_keep_the_acl() {
        let Some((fx, old)) = file_with_an_acl("tasks_copy_acl") else {
            eprintln!("skipped: no ACLs here");
            return;
        };

        let (new_path, task) = paste_after(false, &fx.join("dest"), &old, || {});

        // Like `cp` without `-p`.
        assert_eq!(None, task.error_message());
        assert!(!getfacl(&new_path).contains("nobody"));
    }

    /// Gives `path` the user attributes `user.filectrl`, and `user.empty`
    /// with an empty value. False where the filesystem has no user
    /// attributes, for a test to skip.
    fn set_user_attribute(path: &Path) -> bool {
        let set = |name, value: &[u8]| rustix::fs::setxattr(path, name, value, XattrFlags::empty());
        set("user.filectrl", b"kept").is_ok() && set("user.empty", b"").is_ok()
    }

    fn attribute(path: &Path, name: &str) -> Option<Vec<u8>> {
        let mut value = Vec::with_capacity(64);
        rustix::fs::getxattr(path, name, rustix::buffer::spare_capacity(&mut value)).ok()?;
        Some(value)
    }

    fn user_attribute(path: &Path) -> Option<Vec<u8>> {
        attribute(path, "user.filectrl")
    }

    /// Like `mv`, which keeps every attribute it can, not only the ACLs.
    #[test_case(false ; "a file")]
    #[test_case(true ; "a directory")]
    fn a_move_keeps_the_extended_attributes(is_directory: bool) {
        let fx = TempDir::new("tasks_move_xattr");
        let old = fx.join("item");
        if is_directory {
            fs::create_dir(&old).unwrap();
        } else {
            fs::write(&old, b"data").unwrap();
        }
        fs::create_dir(fx.join("dest")).unwrap();
        if !set_user_attribute(&old) {
            eprintln!("skipped: no user extended attributes here");
            return;
        }

        let (new_path, task) = paste_after(true, &fx.join("dest"), &old, || {});

        assert_eq!(None, task.error_message());
        assert_eq!(Some(b"kept".to_vec()), user_attribute(&new_path));
        assert_eq!(Some(Vec::new()), attribute(&new_path, "user.empty"));
    }

    #[test]
    fn a_copy_does_not_keep_the_extended_attributes() {
        let fx = TempDir::new("tasks_copy_xattr");
        let old = fx.join("item");
        fs::write(&old, b"data").unwrap();
        fs::create_dir(fx.join("dest")).unwrap();
        if !set_user_attribute(&old) {
            eprintln!("skipped: no user extended attributes here");
            return;
        }

        let (new_path, task) = paste_after(false, &fx.join("dest"), &old, || {});

        // Like `cp` without `-p`.
        assert_eq!(None, task.error_message());
        assert_eq!(None, user_attribute(&new_path));
    }

    /// A value read the way `flistxattr` and `fgetxattr` return one, as the
    /// `n`th call finds it: `grows` gives its length per call.
    fn reading(grows: impl Fn(usize) -> usize) -> impl Fn(&mut [u8]) -> rustix::io::Result<usize> {
        let calls = std::cell::Cell::new(0);
        move |buffer: &mut [u8]| {
            let len = grows(calls.replace(calls.get() + 1));
            if buffer.is_empty() {
                return Ok(len);
            }
            if buffer.len() < len {
                return Err(rustix::io::Errno::RANGE);
            }
            buffer[..len].fill(b'v');
            Ok(len)
        }
    }

    #[test]
    fn a_value_that_grew_while_it_was_read_is_measured_again() {
        // Measured at 4 bytes, 6 by the time it is read, then stable.
        let read = reading(|call| if call == 0 { 4 } else { 6 });

        assert_eq!(Ok(vec![b'v'; 6]), read_sized(read));
    }

    /// Measured empty, it is read as empty without asking again: an attribute
    /// added in between would otherwise be read into an empty buffer and lost.
    #[test]
    fn a_value_measured_empty_is_not_read_again() {
        let calls = std::cell::Cell::new(0);
        let read = |_: &mut [u8]| {
            calls.set(calls.get() + 1);
            Ok(if calls.get() == 1 { 0 } else { 5 })
        };

        assert_eq!(Ok(Vec::new()), read_sized(read));
        assert_eq!(1, calls.get());
    }

    #[test]
    fn a_value_that_keeps_growing_is_given_up_on() {
        let read = reading(|call| call + 1);

        assert_eq!(Err(rustix::io::Errno::RANGE), read_sized(read));
    }

    /// What is still asked for by name when the list cannot be read.
    #[cfg(target_os = "linux")]
    #[test_case(false => vec![c"system.posix_acl_access".to_owned()] ; "a file")]
    #[test_case(true => vec![
        c"system.posix_acl_access".to_owned(),
        c"system.posix_acl_default".to_owned(),
    ] ; "a directory")]
    fn the_acls_are_named_by_kind(is_directory: bool) -> Vec<CString> {
        acl_names(is_directory)
    }

    /// A list is read as its names. A list that cannot be read, including one
    /// the filesystem does not support listing while it still serves a named
    /// attribute, falls back to the ACL names.
    #[test_case(Ok(b"user.a\0user.b\0".to_vec()) => vec![
        c"user.a".to_owned(),
        c"user.b".to_owned(),
    ] ; "a list")]
    #[test_case(Err(rustix::io::Errno::NOTSUP) => acl_names(false) ; "listing unsupported")]
    #[test_case(Err(rustix::io::Errno::IO) => acl_names(false) ; "listing failed")]
    fn the_names_read_follow_the_listing(listed: rustix::io::Result<Vec<u8>>) -> Vec<CString> {
        attribute_names(Path::new("moved"), false, listed)
    }

    #[test]
    fn a_move_keeps_both_acls_of_a_directory() {
        let fx = TempDir::new("tasks_move_dir_acl");
        let old = fx.join("d");
        fs::create_dir(&old).unwrap();
        fs::set_permissions(&old, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(fx.join("dest")).unwrap();
        if !(setfacl(&["-m", "u:nobody:rx"], &old) && setfacl(&["-d", "-m", "u:nobody:rwx"], &old))
        {
            eprintln!("skipped: no ACLs here");
            return;
        }
        let before = getfacl(&old);

        let (new_path, task) = paste_after(true, &fx.join("dest"), &old, || {});

        assert_eq!(None, task.error_message());
        assert_eq!(before, getfacl(&new_path));
        assert!(before.contains("default:user:nobody:rwx"), "{before}");
    }

    /// `O_NOFOLLOW` refuses a symlink swapped in at the name, rather than
    /// reading its target.
    #[test]
    fn a_source_file_is_not_opened_through_a_symlink() {
        let fx = TempDir::new("tasks_source_symlink");
        std::fs::write(fx.join("target"), b"x").unwrap();
        std::os::unix::fs::symlink(fx.join("target"), fx.join("link")).unwrap();
        let dir = File::open(fx.path()).unwrap();

        let error = open_source_file(&dir, c"link").expect_err("a link is refused");

        assert_eq!(Some(nix::libc::ELOOP), error.raw_os_error());
    }

    /// Opened non-blocking so a FIFO cannot hang the open, then switched back
    /// so the copy's reads block as usual.
    #[test]
    fn a_source_file_is_read_blocking() {
        let fx = TempDir::new("tasks_source_blocking");
        std::fs::write(fx.join("file"), b"x").unwrap();
        let dir = File::open(fx.path()).unwrap();

        let file = open_source_file(&dir, c"file").unwrap();

        let status = OFlag::from_bits_retain(fcntl(&file, FcntlArg::F_GETFL).unwrap());
        assert!(!status.contains(OFlag::O_NONBLOCK), "{status:?}");
    }

    /// A name taken, by a file or by a symlink planted there, is reported as
    /// taken, which is what settles it as a raced collision.
    #[test_case(false ; "a file")]
    #[test_case(true ; "a symlink")]
    fn a_destination_file_is_not_created_over_a_taken_name(is_symlink: bool) {
        let fx = TempDir::new("tasks_create_taken");
        std::fs::write(fx.join("elsewhere"), b"keep").unwrap();
        if is_symlink {
            std::os::unix::fs::symlink(fx.join("elsewhere"), fx.join("dst")).unwrap();
        } else {
            std::fs::write(fx.join("dst"), b"keep").unwrap();
        }
        let dir = File::open(fx.path()).unwrap();

        let error = create_file_at(&dir, c"dst", 0o100_644, false).unwrap_err();

        assert_eq!(ErrorKind::AlreadyExists, error.kind());
        assert_eq!(
            b"keep".to_vec(),
            std::fs::read(fx.join("elsewhere")).unwrap()
        );
    }
}
