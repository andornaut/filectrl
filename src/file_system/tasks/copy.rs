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
    walk::{
        Handles, Level, Walk, c_name, list_names, open_directory, open_parent, scan_tree, unlink_at,
    },
};
use crate::{
    command::progress::ActiveTask,
    file_system::{
        conflicts::{Conflicts, same_name_refusal},
        debounce,
        entry_id::EntryId,
        paste::{Occupant, PasteStep, step},
        path_info::{PathInfo, compact},
    },
};

/// The settings and shared state one copy carries from the task down to every
/// entry of the tree, so that adding another does not lengthen every signature
/// in between.
struct CopyContext<'a> {
    /// One read buffer for the whole tree; see `copy_with_progress`.
    buffer: &'a mut [u8],
    /// The paste's standing `*All` answer, which settles a name already taken
    /// at a destination inside the tree. `None` for an operation that is not
    /// part of a paste, which records such a name instead.
    conflicts: Option<&'a Conflicts>,
    /// Keep each entry's full mode and its times, as `mv` does, so a
    /// cross-device move leaves what a same-device rename would have. A copy
    /// takes the umask and drops the special bits instead, as `cp` does.
    preserve: bool,
    /// The top-level source file, already opened by `prepare_destination`
    /// before it cleared the destination. `copy_file` takes it in place of
    /// opening the path again. `None` for any other source.
    source: Option<File>,
    /// Entries a standing "skip all" left alone. Counted separately from the
    /// errors: skipping is a choice rather than a failure, but a move still
    /// must not remove a source whose entries never reached the destination.
    skipped: usize,
    /// The directories this copy created, which it never descends into as a
    /// source: a destination swapped for a link into the source tree, or a
    /// bind mount of it, would otherwise copy the copy into itself without end.
    created: std::collections::HashSet<EntryId>,
    /// The top-level source that was copied, which is the entry a move removes
    /// afterwards and no other.
    root: Option<EntryId>,
    /// Set while the top-level entry is copied, so the entry created for it is
    /// recorded with the paste (`record_created`).
    creating_top: bool,
    /// One debouncer for the whole tree, against its total: one per file would
    /// send an update for every file, since a debouncer's first call triggers.
    progress: debounce::ProgressDebouncer,
}

impl<'a> CopyContext<'a> {
    fn new(
        buffer: &'a mut [u8],
        conflicts: Option<&'a Conflicts>,
        preserve: bool,
        source: Option<File>,
        total_size: u64,
    ) -> Self {
        Self {
            buffer,
            conflicts,
            preserve,
            source,
            skipped: 0,
            created: std::collections::HashSet::new(),
            root: None,
            creating_top: false,
            progress: debounce::ProgressDebouncer::new(
                PROGRESS_DEBOUNCE_PERCENTAGE,
                PROGRESS_MIN_INTERVAL,
                total_size,
            ),
        }
    }

    /// The copy's verb: `preserve` is set exactly for the copy a move across
    /// devices makes.
    fn verb_is_move(&self) -> bool {
        self.preserve
    }

    /// Records the entry just created for the top-level source with the
    /// paste, so no later source of it replaces this one. From the created
    /// entry itself, when it is created: a copy that fails or is cancelled
    /// afterwards still wrote it, and its name may hold another entry by then.
    fn record_created(&mut self, id: impl FnOnce() -> std::io::Result<EntryId>) {
        if !std::mem::take(&mut self.creating_top) {
            return;
        }
        if let (Some(conflicts), Ok(id)) = (self.conflicts, id()) {
            conflicts.record_pasted(id);
        }
    }
}

/// What a tree copy left behind: the entries that could not be written, how
/// many a standing "skip all" left alone, and which entry was copied.
#[derive(Default)]
pub(super) struct CopyOutcome {
    pub(super) errors: Vec<String>,
    pub(super) skipped: usize,
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
/// keeps what `mv` keeps (`CopyContext::preserve`), the paste's standing answer
/// (`CopyContext::conflicts`).
#[derive(Clone, Copy)]
pub(super) struct CopySettings<'a> {
    pub(super) preserve: bool,
    pub(super) conflicts: Option<&'a Conflicts>,
}

/// The byte-copy stage shared by copy and cross-device move: for a directory
/// source, scans the real transfer total (a directory entry's own size is not
/// the transfer size) and applies it via `set_total`, and copies the tree. `source` is the handle `prepare_destination` opened,
/// and `listed` the source as the task was started for it: its type, mode and
/// size.
///
/// Returns `None` when the task was cancelled, in which case it has already
/// been finalized via `active.cancelled()`. Otherwise returns the task and
/// what the walk left behind, for the caller to finalize.
pub(super) fn copy_with_progress(
    old_path: &Path,
    new_path: &Path,
    mut active: ActiveTask,
    source: Option<File>,
    listed: &PathInfo,
    settings: CopySettings<'_>,
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
    let mut context = CopyContext::new(
        &mut buffer,
        settings.conflicts,
        settings.preserve,
        source,
        total_size,
    );
    let mut errors = Vec::new();
    if !copy_path(
        old_path,
        new_path,
        &mut active,
        &mut errors,
        &mut context,
        is_directory,
        listed.mode(),
    ) {
        cancel_logging(&errors, active);
        return None;
    }
    Some((
        active,
        CopyOutcome {
            errors,
            skipped: context.skipped,
            root: context.root,
            #[cfg(test)]
            progress_threshold: context.progress.threshold(),
        },
    ))
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
/// interrupted `cp`; the destination is not removed.
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
/// between would otherwise be given the first one's mode. `listed_mode` is the
/// type the task was started for, and a source that is no longer of that type
/// is refused.
fn copy_path(
    old_path: &Path,
    new_path: &Path,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
    is_directory: bool,
    listed_mode: u32,
) -> bool {
    let failed = |error: &dyn std::fmt::Display| {
        format!(
            "Failed to copy {} to {}: {error}",
            compact(old_path),
            compact(new_path)
        )
    };
    let (Some(src_name), Some(dst_name)) = (c_name(old_path), c_name(new_path)) else {
        errors.push(format!(
            "Cannot copy {}: path has no file name",
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
    if type_bits(stat_mode(&stat)) != type_bits(listed_mode) {
        errors.push(format!(
            "Cannot copy {}: its type changed since it was selected",
            compact(old_path)
        ));
        return true;
    }
    let mut paths = Paths {
        old: old_path.to_path_buf(),
        new: new_path.to_path_buf(),
    };
    let at = At {
        src: &src_parent,
        dst: &dst_parent,
        src_name: &src_name,
        dst_name: &dst_name,
    };
    if !is_directory {
        context.creating_top = true;
        let finished = copy_entry(&at, &paths, active, errors, context, &stat);
        context.creating_top = false;
        return finished;
    }
    // A directory needs no record: nothing ever replaces one.
    let Some(level) = enter_directory(&at, &paths, errors, context, &stat) else {
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
            finish_directory(&handles.dst, &level.source, level.umask_left, context);
            if walk.is_empty() {
                break;
            }
            paths.pop();
            if let Err(error) = walk.reopen(&handles, "it was moved while it was being copied") {
                // Neither this directory nor any above it can be reached
                // again, so the walk ends here. They keep the owner-only mode
                // they were created with.
                errors.push(format!("Failed to copy {}: {error}", compact(&paths.old)));
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
/// can be created, and `finish_directory` gives it its mode once they are.
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
            "Cannot copy {}: it is inside the copy being made",
            compact(&paths.old)
        ));
        return None;
    }
    let creation = if context.preserve {
        0o700
    } else {
        (stat_mode(stat) & 0o777) | 0o700
    };
    let created: std::io::Result<()> = match mkdirat(at.dst, at.dst_name, mode_bits(creation)) {
        // Something took this name while the copy was running: the destination
        // was free when the task started, so this is another process writing
        // into the tree. A directory never replaces what holds its name, so
        // only a standing "skip all" settles this one, and skipping drops the
        // subtree.
        Err(Errno::EEXIST) => {
            resolve_nested(context, errors, true, at, paths);
            return None;
        }
        result => result.map_err(Into::into),
    };
    let opened = created.and_then(|()| {
        let dst = open_directory(at.dst, at.dst_name)?;
        Ok((EntryId::of(&dst)?, dst))
    });
    let (dst_id, dst) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            // The subtree cannot be copied at all; skip it and continue with
            // the siblings.
            errors.push(format!(
                "Failed to create directory {}: {error}",
                compact(&paths.new)
            ));
            return None;
        }
    };
    context.created.insert(dst_id);
    let umask_left = grant_owner_access(&dst);
    // Abandons the subtree, leaving the destination directory empty and with
    // its final mode.
    let give_up = |errors: &mut Vec<String>, context: &CopyContext<'_>, message: String| {
        errors.push(message);
        finish_directory(&dst, &Source::of(stat), umask_left, context);
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
            "Cannot copy {}: it was replaced while being copied",
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
    let source = Source::read(&src, &opened, context.preserve);
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

/// Adds owner access to the directory `dst` a copy just created, when the
/// umask took it away: a umask such as `0o277` leaves it unwritable, and no
/// child could be created in it. `cp` does the same. Returns the mode the
/// umask left, which `finish_directory` puts back before computing the final
/// mode from it.
fn grant_owner_access(dst: &File) -> Option<u32> {
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
fn finish_directory(
    dst: &File,
    source: &Source,
    umask_left: Option<u32>,
    context: &CopyContext<'_>,
) {
    if let Some(mode) = umask_left {
        let _ = nix::sys::stat::fchmod(dst, mode_bits(mode));
    }
    if context.preserve {
        apply_times(source, dst);
    }
    apply_final_mode(dst, source, context);
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
    let created = match symlinkat(target.as_os_str(), at.dst, at.dst_name) {
        Ok(()) => true,
        Err(Errno::EEXIST) => {
            let create = || Ok(symlinkat(target.as_os_str(), at.dst, at.dst_name)?);
            replace_raced(at, paths, errors, context, create).is_some()
        }
        Err(error) => {
            errors.push(format!(
                "Failed to create symlink {}: {error}",
                compact(&paths.new)
            ));
            false
        }
    };
    if created {
        context.record_created(|| created_id(at));
    }
}

/// The entry just created at `at`'s destination name, which cannot be opened
/// to ask (a symlink, or a FIFO that would block).
fn created_id(at: &At<'_>) -> std::io::Result<EntryId> {
    let stat = fstatat(at.dst, at.dst_name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
    Ok(EntryId::of_stat(&stat))
}

/// Settles a name taken inside the tree being copied, which is a race: the
/// top-level collision was answered before the task started. When the paste's
/// standing answer is to overwrite, removes what holds the name and runs
/// `create` again. `None` when nothing was created, having recorded why.
fn replace_raced<T>(
    at: &At<'_>,
    paths: &Paths,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
    create: impl FnOnce() -> std::io::Result<T>,
) -> Option<T> {
    if !resolve_nested(context, errors, false, at, paths) {
        return None;
    }
    match unlink_at(at.dst, at.dst_name, UnlinkatFlags::NoRemoveDir).and_then(|()| create()) {
        Ok(created) => Some(created),
        Err(error) => {
            errors.push(format!(
                "Failed to replace {}: {error}",
                compact(&paths.new)
            ));
            None
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
        format!(
            "Failed to copy {} to {}: {error}",
            compact(&paths.old),
            compact(&paths.new)
        )
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
    let source = &Source::read(&old_file, &stat, context.preserve);
    let mut new_file = match create_file_at(at.dst, at.dst_name, source.mode, context.preserve) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let preserve = context.preserve;
            let create = || create_file_at(at.dst, at.dst_name, source.mode, preserve);
            let Some(file) = replace_raced(at, paths, errors, context, create) else {
                return true;
            };
            file
        }
        Err(error) => {
            errors.push(failed(&error));
            return true;
        }
    };
    context.record_created(|| EntryId::of(&new_file));

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
                if context.preserve {
                    apply_times(source, &new_file);
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
                    errors.push(format!("Failed to write {}: {error}", compact(&paths.new)));
                    break true;
                }
            },
            Err(error) => {
                errors.push(format!("Failed to read {}: {error}", compact(&paths.old)));
                break true;
            }
        }
    };
    apply_final_mode(&new_file, source, context);
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
            "Cannot copy {}: unsupported file type",
            compact(&paths.old)
        ));
        return;
    }
    // Everything comes from the one `lstat` the type came from. Device nodes
    // need the source's device numbers; the rest take zero.
    let make = || make_node(at, paths, file_type, stat_mode(stat), stat.st_rdev);
    match make() {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            if replace_raced(at, paths, errors, context, make).is_none() {
                return;
            }
        }
        Err(error) => {
            errors.push(format!(
                "Failed to create special file {}: {error}",
                compact(&paths.new)
            ));
            return;
        }
    }
    context.record_created(|| created_id(at));
    if context.preserve {
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
        restore_node_mode(at, paths, errors, stat_mode(stat));
    }
}

/// Gives a node a move created the source's permission bits, which the umask
/// trimmed at creation and `mv` keeps. By name, since a FIFO cannot be opened
/// without blocking, and without following a link swapped in at the name since,
/// as `operations::set_mode_without_following` does. A filesystem that cannot
/// set a mode that way leaves the node as created, with a warning.
fn restore_node_mode(at: &At<'_>, paths: &Paths, errors: &mut Vec<String>, source_mode: u32) {
    use nix::sys::stat::{FchmodatFlags, fchmodat};

    let bits = mode_bits(source_mode & 0o777);
    let Err(errno) = fchmodat(at.dst, at.dst_name, bits, FchmodatFlags::NoFollowSymlink) else {
        return;
    };
    let error = std::io::Error::from(errno);
    if errno == Errno::EOPNOTSUPP {
        warn!("Failed to set permissions on a moved entry: {error}");
    } else {
        errors.push(format!(
            "Failed to chmod {} to {:o}: {error}",
            compact(&paths.new),
            source_mode & 0o777
        ));
    }
}

/// Creates the node `at` names in the destination directory.
#[cfg(not(target_os = "macos"))]
fn make_node(
    at: &At<'_>,
    _paths: &Paths,
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

/// Creates the node `at` names, by path: macOS has no `mknodat`. A parent
/// swapped for a symlink could therefore place the node outside the tree, but
/// an empty FIFO or socket there carries nothing, and a device node needs root.
#[cfg(target_os = "macos")]
fn make_node(
    _at: &At<'_>,
    paths: &Paths,
    file_type: FileType,
    source_mode: u32,
    device: nix::libc::dev_t,
) -> std::io::Result<()> {
    let (kind, device) = node_kind(file_type, device);
    Ok(nix::sys::stat::mknod(
        &paths.new,
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
    preserve: bool,
) -> std::io::Result<File> {
    let creation = if preserve {
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
fn apply_final_mode(file: &File, source: &Source, context: &CopyContext<'_>) {
    if context.preserve {
        let _ = fchown(file, None, Some(Gid::from_raw(source.gid)));
        for (name, value) in &source.attributes {
            if let Err(error) = rustix::fs::fsetxattr(file, name, value, XattrFlags::empty()) {
                warn!(
                    "Failed to copy the extended attribute {} of a moved entry: {error}",
                    name.to_string_lossy()
                );
            }
        }
    }
    let Ok(created) = fstat(file) else {
        return;
    };
    let created_mode = stat_mode(&created) & 0o777;
    let mode = if context.preserve {
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
        warn!("Failed to set permissions on a copied entry: {error}");
    }
}

/// Gives the open `target` `source`'s access and modification times. Best
/// effort: a filesystem that cannot record them is not a reason to fail the
/// operation.
fn apply_times(source: &Source, target: &File) {
    let Some(times) = &source.times else {
        return;
    };
    if let Err(error) = nix::sys::stat::futimens(target, &times.access, &times.modification) {
        warn!("Failed to set times on a copied entry: {error}");
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
}

impl Paths {
    fn push(&mut self, name: &CStr) {
        let name = OsStr::from_bytes(name.to_bytes());
        self.old.push(name);
        self.new.push(name);
    }

    fn pop(&mut self) {
        self.old.pop();
        self.new.pop();
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
    /// for a move.
    fn read(file: &File, stat: &Stat, preserve: bool) -> Self {
        let mut source = Self::of(stat);
        if preserve {
            let is_directory = FileType::of(stat) == FileType::Directory;
            source.attributes = read_attributes(is_directory, file);
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
/// are still read by name where the platform has them.
fn read_attributes(is_directory: bool, file: &File) -> Vec<(CString, Vec<u8>)> {
    let names = match read_sized(|buffer| rustix::fs::flistxattr(file, buffer)) {
        Ok(list) => list
            .split(|&byte| byte == 0)
            .filter_map(|name| CString::new(name).ok().filter(|name| !name.is_empty()))
            .collect(),
        // A filesystem without extended attributes has none, ACLs included.
        Err(rustix::io::Errno::NOTSUP) => return Vec::new(),
        Err(error) => {
            let names = acl_names(is_directory);
            let copied = if names.is_empty() {
                "none are copied"
            } else {
                "only its ACLs are copied"
            };
            warn!("Failed to list the extended attributes of a moved entry, so {copied}: {error}");
            names
        }
    };
    names
        .into_iter()
        .filter_map(|name| {
            let value = read_attribute(file, &name)?;
            Some((name, value))
        })
        .collect()
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

/// What to do about `at`'s destination name (`paths.new`), which another
/// process took at a destination inside the tree being copied: the destination was free when the task
/// started. Returns whether to replace it, having counted or recorded it
/// otherwise.
///
/// The same decision the queue makes for a top-level collision, minus the one
/// outcome a worker cannot produce: it never asks, so a collision the paste's
/// standing answer does not settle is recorded like any other entry that could
/// not be written.
fn resolve_nested(
    context: &mut CopyContext<'_>,
    errors: &mut Vec<String>,
    source_is_directory: bool,
    at: &At<'_>,
    paths: &Paths,
) -> bool {
    let new_path = &paths.new;
    let stat = fstatat(at.dst, at.dst_name, AtFlags::AT_SYMLINK_NOFOLLOW).ok();
    // Classified exactly as at the top level, so only the skip choices apply
    // to a directory on either side.
    let is_directory = stat
        .as_ref()
        .is_some_and(|stat| FileType::of(stat) == FileType::Directory);
    let occupant = Occupant::of(source_is_directory, is_directory);
    let standing = context.conflicts.and_then(Conflicts::standing);
    // Whatever the standing answer, an entry an earlier source of the same
    // paste wrote is never replaced: the two names are one entry here. Read
    // from the same `fstatat`, relative to the directory being written in.
    let pasted = context
        .conflicts
        .zip(stat.as_ref())
        .is_some_and(|(conflicts, stat)| conflicts.was_pasted_stat(stat));
    match step(standing, Some(occupant)) {
        PasteStep::Run { overwrite: true } if pasted => {
            errors.push(same_name_refusal(
                context.verb_is_move(),
                &paths.old,
                new_path.parent().unwrap_or(new_path),
            ));
            false
        }
        PasteStep::Run { overwrite } => overwrite,
        PasteStep::Skip => {
            context.skipped += 1;
            false
        }
        PasteStep::Ask { .. } => {
            errors.push(format!("{} already exists", compact(new_path)));
            false
        }
    }
}

/// Opens a regular-file source, then clears a destination the paste granted
/// permission to replace. `verb` names the operation in the error, "copy" or
/// "move". Returns the task and the opened source, or `None`
/// when the task was finalized here and must not continue.
///
/// Opening first means a source that cannot be read (mode 000, or gone) fails
/// the task with the destination still in place, and the copy then reads the
/// handle that was checked rather than reopening the path. Other types are not
/// opened: a FIFO would block, and a directory or symlink is not read as bytes.
pub(super) fn prepare_destination(
    active: ActiveTask,
    verb: &str,
    old_path: &Path,
    new_path: &Path,
    overwrite: bool,
    source_mode: u32,
) -> Option<(ActiveTask, Option<File>)> {
    let source = if unix_mode::is_file(source_mode) {
        let name = c_name(old_path).ok_or_else(|| std::io::Error::from(ErrorKind::InvalidInput));
        match open_parent(old_path).and_then(|parent| open_source_file(&parent, &name?)) {
            Ok(file) => Some(file),
            Err(error) => {
                active.error(format!(
                    "Failed to {verb} {} to {}: {error}",
                    compact(old_path),
                    compact(new_path)
                ));
                return None;
            }
        }
    } else {
        None
    };
    clear_destination(active, old_path, new_path, overwrite).map(|active| (active, source))
}

/// Clears a destination the paste granted permission to replace. Returns `None`
/// when the task was finalized here and must not continue. Only a
/// non-directory source gets here with `overwrite`: `validate_paths` refuses
/// one for a directory.
///
/// Runs in the worker, once the operation is about to write. The caller queues
/// every source of an "overwrite all" paste in one pass, so clearing there would
/// delete every destination up front and leave a hole wherever a later task is
/// cancelled or fails.
///
/// The source is re-checked first for the same reason: removing the destination
/// for a copy that then finds nothing to read would leave neither entry. It
/// narrows the window rather than closing it.
fn clear_destination(
    active: ActiveTask,
    old_path: &Path,
    new_path: &Path,
    overwrite: bool,
) -> Option<ActiveTask> {
    if !overwrite {
        return Some(active);
    }
    if let Err(error) = old_path.symlink_metadata() {
        active.error(format!(
            "Failed to replace {}: failed to read {}: {error}",
            compact(new_path),
            compact(old_path)
        ));
        return None;
    }
    let removed = open_parent(new_path).and_then(|dir| {
        unlink_at(
            &dir,
            &c_name(new_path).ok_or(ErrorKind::InvalidInput)?,
            UnlinkatFlags::NoRemoveDir,
        )
    });
    if let Err(error) = removed {
        active.error(format!("Failed to replace {}: {error}", compact(new_path)));
        return None;
    }
    Some(active)
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use test_case::test_case;

    use super::{
        super::sys::CWD,
        super::test_support::{
            copy_task, destination, finished_task, mode_of, paste_after, paste_after_unless,
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

    /// A copy context for a test: no paste behind it, so a nested collision
    /// records an error rather than asking.
    fn context(preserve_times: bool, buffer: &mut [u8]) -> CopyContext<'_> {
        CopyContext::new(buffer, None, preserve_times, None, 0)
    }

    /// Copies `src` to `dst` as a copy, or as a move's copy stage when
    /// `preserve`, returning the errors recorded.
    fn copy_one(src: &Path, dst: &Path, preserve: bool) -> Vec<String> {
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();
        let is_directory = fs::symlink_metadata(src).unwrap().is_dir();
        assert!(copy_path(
            src,
            dst,
            &mut active,
            &mut errors,
            &mut context(preserve, &mut [0u8; 64]),
            is_directory,
            mode_of(src),
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

        let errors = copy_one(&src, &dst, false);
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

        let errors = copy_one(&src, &dst, false);
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

        let errors = copy_one(&src, &dst, false);

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
        let errors = copy_one(&src, &dst, false);
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
        let errors = copy_one(&src, &dst, true);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");

        assert_eq!(before, modified_times(&dst));
    }

    /// A copy context whose collisions are settled by a standing choice, which
    /// is the only kind of answer that reaches a worker.
    fn answered_context<'a>(
        standing: ConflictChoice,
        buffer: &'a mut [u8],
        conflicts: &'a Conflicts,
    ) -> CopyContext<'a> {
        conflicts.answer(standing);
        CopyContext::new(buffer, Some(conflicts), false, None, 0)
    }

    /// A source entry and a destination path that another process took while
    /// the copy was already running. A copy's destination is free when it
    /// starts, so a race part way through is the only way a collision appears
    /// underneath it, and each entry is reached individually.
    fn raced(label: &str) -> (TempDir, PathBuf, PathBuf) {
        let fx = TempDir::new(label);
        let src = fx.join("src");
        let dst = fx.join("dst");
        fs::create_dir_all(&src).unwrap();
        // What the copy had created before the other process interfered.
        fs::create_dir_all(&dst).unwrap();
        (fx, src, dst)
    }

    /// The three pieces every raced-entry test needs. The receiver is leaked
    /// rather than returned as a fourth: nothing here reads it, and it only has
    /// to outlive the task, whose sends are best-effort anyway.
    fn raced_parts() -> (ActiveTask, Vec<String>, Conflicts) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        (copy_task(tx), Vec::new(), Conflicts::default())
    }

    #[test]
    fn a_raced_file_is_replaced_when_that_is_the_answer() {
        let (_fx, src, dst) = raced("tasks_raced_overwrite");
        fs::write(src.join("a.txt"), b"src").unwrap();
        fs::write(dst.join("a.txt"), b"raced").unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];

        assert!(copy_path(
            &src.join("a.txt"),
            &dst.join("a.txt"),
            &mut active,
            &mut errors,
            &mut answered_context(ConflictChoice::OverwriteAll, &mut buffer, &conflicts),
            false,
            mode_of(&src.join("a.txt")),
        ));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        active.done();

        assert_eq!(b"src".to_vec(), fs::read(dst.join("a.txt")).unwrap());
    }

    /// Makes `b.txt` in `dst` name what was copied to `a.txt`, as two names are
    /// one entry on a filesystem that folds case or normalization: a second
    /// link, then the first name removed, so it keeps one link.
    fn alias_a_as_b(dst: &Path) {
        fs::hard_link(dst.join("a.txt"), dst.join("b.txt")).unwrap();
        fs::remove_file(dst.join("a.txt")).unwrap();
    }

    /// Copies `a.txt`, then `b.txt`, from `src` into `dst` as two sources of one
    /// paste under a standing overwrite, with `b.txt` made an alias of what the
    /// first wrote in between. `cancel_first` cancels the first copy once it
    /// has created its file. Returns the errors.
    fn copy_onto_an_alias(src: &Path, dst: &Path, cancel_first: bool) -> Vec<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        let conflicts = Conflicts::default();
        let mut buffer = [0u8; 64];
        let mut context = answered_context(ConflictChoice::OverwriteAll, &mut buffer, &conflicts);
        let mut errors = Vec::new();
        let (mut active, _, token) = ActiveTask::new(
            tx.clone(),
            TaskKind::Copy(Transfer {
                source: String::new(),
                destination: String::new(),
            }),
            1,
        );
        if cancel_first {
            token.cancel();
        }
        let finished = copy_path(
            &src.join("a.txt"),
            &dst.join("a.txt"),
            &mut active,
            &mut errors,
            &mut context,
            false,
            mode_of(&src.join("a.txt")),
        );
        assert_eq!(!cancel_first, finished);
        active.done();
        alias_a_as_b(dst);

        let mut active = copy_task(tx);
        copy_path(
            &src.join("b.txt"),
            &dst.join("b.txt"),
            &mut active,
            &mut errors,
            &mut context,
            false,
            mode_of(&src.join("b.txt")),
        );
        active.done();
        errors
    }

    // The name was free when the second source was queued, so the refusal
    // comes from the raced-name path.
    #[test_case(false ; "a finished copy")]
    #[test_case(true ; "a copy cancelled after it created its file")]
    fn a_raced_name_an_earlier_source_wrote_is_never_replaced(cancel_first: bool) {
        let (_fx, src, dst) = raced("tasks_raced_pasted");
        fs::write(src.join("a.txt"), b"first").unwrap();
        fs::write(src.join("b.txt"), b"second").unwrap();

        let errors = copy_onto_an_alias(&src, &dst, cancel_first);

        assert_eq!(
            vec![format!(
                "Cannot copy {} into {}: another source in this paste has the same name",
                compact(&src.join("b.txt")),
                compact(&dst)
            )],
            errors
        );
        let kept = if cancel_first {
            b"".to_vec()
        } else {
            b"first".to_vec()
        };
        assert_eq!(kept, fs::read(dst.join("b.txt")).unwrap());
    }

    #[test]
    fn a_raced_file_is_left_alone_when_that_is_the_answer() {
        let (_fx, src, dst) = raced("tasks_raced_skip");
        fs::write(src.join("a.txt"), b"src").unwrap();
        fs::write(dst.join("a.txt"), b"raced").unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];
        let mut context = answered_context(ConflictChoice::SkipAll, &mut buffer, &conflicts);

        assert!(copy_path(
            &src.join("a.txt"),
            &dst.join("a.txt"),
            &mut active,
            &mut errors,
            &mut context,
            false,
            mode_of(&src.join("a.txt")),
        ));
        // A skipped name is a choice, not a failure, so nothing is reported.
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        // It is still counted: a move must not remove a source whose entries
        // are not all at the destination.
        assert_eq!(1, context.skipped);
        active.done();

        assert_eq!(b"raced".to_vec(), fs::read(dst.join("a.txt")).unwrap());
    }

    #[test]
    fn a_raced_name_with_no_standing_answer_is_recorded_rather_than_asked_about() {
        let (_fx, src, dst) = raced("tasks_raced_unanswered");
        fs::write(src.join("a.txt"), b"src").unwrap();
        fs::write(dst.join("a.txt"), b"raced").unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];
        let mut context = CopyContext::new(&mut buffer, Some(&conflicts), false, None, 0);

        // A worker never asks: the queue that could have prompted is gone by
        // the time it runs.
        assert!(copy_path(
            &src.join("a.txt"),
            &dst.join("a.txt"),
            &mut active,
            &mut errors,
            &mut context,
            false,
            mode_of(&src.join("a.txt")),
        ));
        active.done();

        assert_eq!(1, errors.len(), "expected the collision: {errors:?}");
        assert!(errors[0].contains("already exists"), "{}", errors[0]);
        // Recorded, not skipped: the entry was left behind by a failure to
        // settle it, so a move must report it rather than call it a choice.
        assert_eq!(0, context.skipped);
        assert_eq!(b"raced".to_vec(), fs::read(dst.join("a.txt")).unwrap());
    }

    #[test]
    fn a_raced_directory_is_recorded_when_only_overwrite_all_stands() {
        let (_fx, src, dst) = raced("tasks_raced_directory_overwrite");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::create_dir_all(dst.join("sub")).unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];

        // A directory is never replaced, so "overwrite all" cannot settle this
        // one and there is nobody to ask.
        assert!(copy_path(
            &src.join("sub"),
            &dst.join("sub"),
            &mut active,
            &mut errors,
            &mut answered_context(ConflictChoice::OverwriteAll, &mut buffer, &conflicts),
            true,
            mode_of(&src.join("sub")),
        ));
        active.done();

        assert_eq!(1, errors.len(), "expected the collision: {errors:?}");
        assert!(errors[0].contains("already exists"), "{}", errors[0]);
    }

    #[test]
    fn a_raced_symlink_is_replaced_when_that_is_the_answer() {
        let (_fx, src, dst) = raced("tasks_raced_symlink");
        std::os::unix::fs::symlink("target", src.join("link")).unwrap();
        std::os::unix::fs::symlink("raced", dst.join("link")).unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];

        assert!(copy_path(
            &src.join("link"),
            &dst.join("link"),
            &mut active,
            &mut errors,
            &mut answered_context(ConflictChoice::OverwriteAll, &mut buffer, &conflicts),
            false,
            mode_of(&src.join("link")),
        ));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        active.done();

        assert_eq!(
            PathBuf::from("target"),
            fs::read_link(dst.join("link")).unwrap()
        );
    }

    #[test]
    fn a_raced_symlink_with_no_standing_answer_is_recorded() {
        let (_fx, src, dst) = raced("tasks_raced_symlink_unanswered");
        std::os::unix::fs::symlink("target", src.join("link")).unwrap();
        std::os::unix::fs::symlink("raced", dst.join("link")).unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];
        let mut context = CopyContext::new(&mut buffer, Some(&conflicts), false, None, 0);

        assert!(copy_path(
            &src.join("link"),
            &dst.join("link"),
            &mut active,
            &mut errors,
            &mut context,
            false,
            mode_of(&src.join("link")),
        ));
        active.done();

        assert_eq!(1, errors.len(), "expected the collision: {errors:?}");
        assert!(errors[0].contains("already exists"), "{}", errors[0]);
        assert_eq!(
            PathBuf::from("raced"),
            fs::read_link(dst.join("link")).unwrap()
        );
    }

    #[test]
    fn a_raced_special_file_is_left_alone_when_that_is_the_answer() {
        use nix::sys::stat::Mode;

        let (_fx, src, dst) = raced("tasks_raced_fifo");
        nix::unistd::mkfifo(&src.join("pipe"), Mode::from_bits_truncate(0o644)).unwrap();
        nix::unistd::mkfifo(&dst.join("pipe"), Mode::from_bits_truncate(0o600)).unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];
        let mut context = answered_context(ConflictChoice::SkipAll, &mut buffer, &conflicts);

        assert!(copy_path(
            &src.join("pipe"),
            &dst.join("pipe"),
            &mut active,
            &mut errors,
            &mut context,
            false,
            mode_of(&src.join("pipe")),
        ));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(1, context.skipped);
        active.done();

        // Left exactly as the other process made it.
        assert_eq!(0o600, mode_of(&dst.join("pipe")) & 0o7777);
    }

    #[test]
    fn a_raced_file_where_a_directory_goes_is_never_replaced() {
        let (_fx, src, dst) = raced("tasks_raced_file_for_directory");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub").join("deep.txt"), b"src").unwrap();
        fs::write(dst.join("sub"), b"raced").unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];

        // A directory never replaces a file, like `cp -R` and `mv`, so
        // "overwrite all" cannot settle this one and there is nobody to ask.
        assert!(copy_path(
            &src.join("sub"),
            &dst.join("sub"),
            &mut active,
            &mut errors,
            &mut answered_context(ConflictChoice::OverwriteAll, &mut buffer, &conflicts),
            true,
            mode_of(&src.join("sub")),
        ));
        active.done();

        assert_eq!(1, errors.len(), "expected the collision: {errors:?}");
        assert!(errors[0].contains("already exists"), "{}", errors[0]);
        assert_eq!(b"raced".to_vec(), fs::read(dst.join("sub")).unwrap());
    }

    #[test]
    fn a_raced_directory_is_never_replaced_so_its_subtree_is_dropped() {
        let (_fx, src, dst) = raced("tasks_raced_directory");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub").join("deep.txt"), b"src").unwrap();
        fs::create_dir_all(dst.join("sub")).unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];

        // Only the skip choices apply to a directory, so "skip all" is what can
        // answer this one; "overwrite all" would still have to ask.
        assert!(copy_path(
            &src.join("sub"),
            &dst.join("sub"),
            &mut active,
            &mut errors,
            &mut answered_context(ConflictChoice::SkipAll, &mut buffer, &conflicts),
            true,
            mode_of(&src.join("sub")),
        ));
        active.done();

        // Dropped, not merged into: a directory is never replaced, and the copy
        // does not descend into one it did not create.
        assert!(!dst.join("sub").join("deep.txt").exists());
    }

    #[test]
    fn a_raced_name_with_no_paste_behind_it_is_recorded_rather_than_asked_about() {
        let (_fx, src, dst) = raced("tasks_raced_no_paste");
        fs::write(src.join("a.txt"), b"src").unwrap();
        fs::write(dst.join("a.txt"), b"raced").unwrap();

        // No paste at all (an operation that is not one), so there is not even
        // a standing answer to consult: the entry is left alone and reported.
        let errors = copy_one(&src.join("a.txt"), &dst.join("a.txt"), false);

        assert_eq!(1, errors.len(), "expected the collision: {errors:?}");
        assert!(errors[0].contains("already exists"), "{}", errors[0]);
        assert_eq!(b"raced".to_vec(), fs::read(dst.join("a.txt")).unwrap());
    }

    /// `copy_path_continues_past_unreadable_entries` degrades to a plain full
    /// copy under root, so the error-recording path is pinned here instead,
    /// with a failure the kernel enforces for every user.
    #[test]
    fn copy_path_records_a_directory_it_cannot_create() {
        let fx = TempDir::new("tasks");
        let src = fx.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), b"a").unwrap();
        // A regular file already occupies the destination, so creating the
        // directory fails with EEXIST regardless of privileges.
        let dst = fx.join("dst");
        std::fs::write(&dst, b"in the way").unwrap();

        // The subtree is skipped rather than failing the whole task, so a
        // multi-source paste still copies its other sources.
        let errors = copy_one(&src, &dst, false);

        assert_eq!(1, errors.len(), "expected one error: {errors:?}");
        assert!(errors[0].contains("dst"), "unexpected error: {}", errors[0]);
        // The occupying file must be left exactly as it was.
        assert_eq!(b"in the way".to_vec(), std::fs::read(&dst).unwrap());
    }

    // ── prepare_destination: the ordering every queued operation depends on ──

    #[test]
    fn a_granted_overwrite_clears_the_destination() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_overwrite");

        let active = clear_destination(active, &src, &dst, true).expect("the task should continue");

        assert!(!dst.exists());
        active.done();
    }

    #[test]
    fn a_destination_survives_when_overwrite_was_not_granted() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_no_overwrite");

        let active =
            clear_destination(active, &src, &dst, false).expect("the task should continue");

        assert_eq!(b"dest".to_vec(), std::fs::read(&dst).unwrap());
        active.done();
    }

    #[test]
    fn a_destination_that_already_vanished_is_not_an_error() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_vanished");
        std::fs::remove_file(&dst).unwrap();

        // Something else removed it between the prompt and the worker, which
        // is the outcome the user asked for anyway.
        assert!(clear_destination(active, &src, &dst, true).is_some());
    }

    #[test]
    fn an_unreadable_source_leaves_the_destination_it_would_have_replaced() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_source_unreadable");
        fs::set_permissions(&src, fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads a mode-000 file anyway (CAP_DAC_OVERRIDE), as do mounts
        // that ignore permissions; probe rather than inspect the euid.
        let is_unreadable = File::open(&src).is_err();

        // The source still exists, so only opening it can tell that the copy
        // is bound to fail. Clearing first would leave the user with neither
        // file's contents.
        let prepared = prepare_destination(active, "copy", &src, &dst, true, mode_of(&src));

        if is_unreadable {
            assert!(prepared.is_none());
            assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
        } else {
            let (active, source) = prepared.expect("a readable source should continue");
            assert!(source.is_some());
            assert!(!dst.exists());
            active.done();
        }
    }

    #[test_case("copy" ; "a copy")]
    #[test_case("move" ; "a move")]
    fn an_unreadable_source_is_reported_under_the_operation_that_failed(verb: &str) {
        let fx = TempDir::new("tasks_prepare_verb");
        let src = fx.join("missing.txt");
        let dst = fx.join("dest.txt");
        let (tx, rx) = mpsc::channel();
        // A regular-file mode with no file behind it, so opening fails for
        // root as well.
        let prepared = prepare_destination(copy_task(tx), verb, &src, &dst, false, 0o100_644);

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
    fn a_copied_file_is_never_more_readable_than_its_source_while_written(preserve: bool) -> u32 {
        let fx = TempDir::new("tasks_create_file_mode");
        let dir = open_parent(&fx.join("dst.txt")).unwrap();

        let _file = create_file_at(&dir, c"dst.txt", 0o100_640, preserve).unwrap();

        let mode = mode_of(&fx.join("dst.txt")) & 0o7777;
        if preserve { mode } else { mode & !0o640 }
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
            &src,
            &dst,
            &mut active,
            &mut errors,
            &mut context(false, &mut [0u8; 64]),
            false,
            mode_of(&src),
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
            &src,
            &dst,
            &mut active,
            &mut errors,
            &mut context(false, &mut [0u8; 64]),
            true,
            mode_of(&src),
        ));
        active.cancelled();

        assert!(!dst.join("a.txt").exists());
        assert_eq!(0o750, mode_of(&dst) & 0o7777);
    }

    #[test]
    fn a_vanished_source_leaves_the_destination_it_would_have_replaced() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_source_gone");
        std::fs::remove_file(&src).unwrap();

        // A task can wait behind a long operation on the shared worker, so the
        // source it was going to copy may be gone by the time it runs. Clearing
        // the destination for a copy that can no longer happen would leave the
        // user with neither entry.
        assert!(clear_destination(active, &src, &dst, true).is_none());
        assert_eq!(b"dest".to_vec(), std::fs::read(&dst).unwrap());
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
            &src,
            &fx.join("dst.bin"),
            &mut active,
            &mut errors,
            &mut context(false, &mut [0u8; 64]),
            false,
            mode_of(&src),
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
            &src,
            &fx.join("dst"),
            active,
            None,
            &PathInfo::try_from(src.as_path()).unwrap(),
            CopySettings {
                preserve: false,
                conflicts: None,
            },
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

        let errors = copy_one(&link, &dst, false);
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
        preserve: bool,
    ) {
        let fx = TempDir::new("tasks_file_mode");
        let src = fx.join("src");
        fs::write(&src, b"x").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();

        let errors = copy_one(&src, &fx.join("dst"), preserve);

        assert!(errors.is_empty(), "{errors:?}");
        let expected = if preserve {
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
        let fx = TempDir::new("tasks_file_mode_umask");
        let src = fx.join("src");
        fs::write(&src, b"x").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o777)).unwrap();
        let src_dir = fx.join("src_dir");
        fs::create_dir(&src_dir).unwrap();
        fs::write(src_dir.join("child"), b"x").unwrap();
        fs::set_permissions(&src_dir, fs::Permissions::from_mode(0o777)).unwrap();
        nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o277));

        let errors = copy_one(&src, &fx.join("dst"), false);
        let dir_errors = copy_one(&src_dir, &fx.join("dst_dir"), false);

        assert!(errors.is_empty(), "{errors:?}");
        assert!(dir_errors.is_empty(), "{dir_errors:?}");
        let leaves = 0o777 & umask_leaves(fx.path());
        assert_eq!(leaves, mode_of(&fx.join("dst")) & 0o7777);
        assert_eq!(leaves, mode_of(&fx.join("dst_dir")) & 0o7777);
        assert_eq!(
            b"x",
            &fs::read(fx.join("dst_dir").join("child")).unwrap()[..]
        );
        fs::set_permissions(fx.join("dst_dir"), fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// A copy's owner bits are the source's as the umask leaves them, like
    /// `cp`: a umask that clears the owner's write bit clears it on the copy.
    /// The umask is process-wide, so the copy runs in a process of its own.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_copy_takes_the_umask_on_the_owner_bits_too() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "file_system::tasks::copy::tests::a_copy_under_a_umask_clearing_owner_bits",
                "--ignored",
                "--test-threads=1",
            ])
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("1 passed"), "{stdout}");
    }

    // A directory is created owner-writable so its children can be, then
    // given its mode: a copy's owner bits come back to what the source had.
    #[test_case(0o555, false ; "a copy of a read only directory")]
    #[test_case(0o1777, false ; "a copy drops the sticky bit and takes the umask")]
    #[test_case(0o2755, false ; "a copy drops the source's setgid bit")]
    #[test_case(0o555, true ; "a move of a read only directory")]
    #[test_case(0o1777, true ; "a move keeps the sticky bit")]
    fn a_copied_directory_ends_with_the_mode_of_its_kind_of_copy(mode: u32, preserve: bool) {
        let fx = TempDir::new("tasks_directory_mode");
        let src = fx.join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("child"), b"x").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();

        let errors = copy_one(&src, &fx.join("dst"), preserve);

        assert!(errors.is_empty(), "{errors:?}");
        assert!(fx.join("dst").join("child").exists());
        let expected = if preserve {
            mode
        } else {
            mode & 0o777 & umask_leaves(fx.path())
        };
        assert_eq!(expected, mode_of(&fx.join("dst")) & 0o7777);
        fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
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

        let errors = copy_one(&src, &shared.join("dst"), false);

        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(0o2000, mode_of(&shared.join("dst")) & 0o7000);
        assert_eq!(0, mode_of(&shared.join("dst").join("child")) & 0o7000);
    }

    /// A node is created with the umask applied, like any other entry, so a
    /// move puts the source's mode back afterwards.
    #[test_case(true ; "a move keeps the mode")]
    #[test_case(false ; "a copy takes the umask")]
    fn a_moved_fifo_keeps_its_mode_whatever_the_umask(preserve: bool) {
        let fx = TempDir::new("tasks_fifo_mode");
        let src = fx.join("fifo");
        nix::unistd::mkfifo(&src, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o666)).unwrap();
        let dst = fx.join("moved");

        let errors = copy_one(&src, &dst, preserve);

        assert!(errors.is_empty(), "{errors:?}");
        let expected = if preserve {
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

        let errors = copy_one(&src, &dst, true);

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

        apply_final_mode(&file, &source, &context(true, &mut [0u8; 1]));

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

        let errors = copy_one(&src, &fx.join("dst"), true);

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
            let prepared =
                prepare_destination(copy_task(task_tx), "copy", &path, &dst, false, 0o100_644);
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

        let (new_path, task) = paste_after(&old, &fx.join("dest"), true, || {
            fs::remove_file(&old).unwrap();
            fs::write(&old, b"data").unwrap();
        });

        assert_eq!(b"data".as_slice(), fs::read(&old).unwrap());
        assert!(new_path.symlink_metadata().is_err());
        let message = task.error_message().expect("the move fails");
        assert!(
            message.ends_with("its type changed since it was selected"),
            "{message}"
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

        let (new_path, task) = paste_after(&old, &fx.join("dest"), is_move, || {
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
            &src,
            &dest,
            false,
            || {
                fs::remove_dir(&dest).unwrap();
                std::os::unix::fs::symlink(src.join("sub"), &dest).unwrap();
            },
            || runaway.exists(),
        );

        let message = task.error_message().expect("the copy reports the refusal");
        assert!(
            message.ends_with("it is inside the copy being made"),
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
        };

        let level = enter_directory(&at, &paths, &mut errors, &mut context, &stat);
        active.done();

        assert!(level.is_none());
        assert_eq!(1, errors.len());
        assert!(
            errors[0].ends_with("it was replaced while being copied"),
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

        let (new_path, task) = paste_after(&old, &fx.join("dest"), true, || {});

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
        let (new_path, task) = paste_after(&old, &fx.join("dest"), true, || {});

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

        let (new_path, task) = paste_after(&old, &fx.join("dest"), true, || {});

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

        let (new_path, task) = paste_after(&old, &fx.join("dest"), false, || {});

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

        let (new_path, task) = paste_after(&old, &fx.join("dest"), true, || {});

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

        let (new_path, task) = paste_after(&old, &fx.join("dest"), false, || {});

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

        let (new_path, task) = paste_after(&old, &fx.join("dest"), true, || {});

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
