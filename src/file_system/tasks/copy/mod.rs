use std::{
    ffi::{CStr, CString, OsStr},
    fs::File,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use super::{
    PROGRESS_DEBOUNCE_PERCENTAGE, PROGRESS_MIN_INTERVAL, cancel_logging,
    sys::{AtFlags, FileType, Stat, fstatat, stat_mode},
    walk::{c_name, open_parent, scan_tree},
};
use crate::{
    command::progress::ActiveTask,
    file_system::{
        conflicts::{Conflicts, failed_replacement, failed_transfer, not_replaced, verb},
        debounce,
        entry_id::{EntryId, Seen},
        path_info::{PathInfo, compact},
    },
};

mod attributes;
mod entry;
mod replace;
#[cfg(test)]
mod tests;
mod tree;

#[cfg(test)]
pub(super) use self::replace::STAGING_PREFIX;
use self::{
    attributes::{Times, read_attributes, times_of},
    entry::{copy_entry, type_bits},
    replace::{prepare_destination, replace_entry},
    tree::{copy_tree, enter_directory},
};

/// The settings and shared state one copy carries from the task down to every
/// entry of the tree, so that adding another does not lengthen every signature
/// in between.
struct CopyContext<'a> {
    /// The task the copy reports progress and a cancel through.
    active: &'a mut ActiveTask,
    /// Each entry that could not be copied, in the order met: the copy carries
    /// on past them, like `cp -R`, and the task reports them when it ends.
    errors: Vec<String>,
    /// One read buffer for the whole tree; see `copy_with_progress`.
    buffer: &'a mut [u8],
    /// The paste's standing `*All` answer (`Conflicts`).
    conflicts: &'a Conflicts,
    /// The copy is the one a move across devices makes. It keeps each entry's
    /// full mode, times and extended attributes, as `mv` does, so the move
    /// leaves what a same-device rename would have, and its messages call it
    /// a move. A copy takes the umask and drops the special bits instead, as
    /// `cp` does.
    is_move: bool,
    /// The top-level source file, already opened by `prepare_destination`,
    /// with its metadata. `copy_file` takes it in place of opening the path
    /// again. `None` for any other source.
    source: Option<(File, Stat)>,
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
}

/// What `CopyContext::on_leave` calls.
#[cfg(test)]
type OnLeave<'a> = Box<dyn FnMut(&Path) + 'a>;

impl<'a> CopyContext<'a> {
    fn new(
        settings: CopySettings<'a>,
        active: &'a mut ActiveTask,
        buffer: &'a mut [u8],
        source: Option<(File, Stat)>,
        total_size: u64,
    ) -> Self {
        Self {
            active,
            errors: Vec::new(),
            buffer,
            conflicts: settings.conflicts,
            is_move: settings.is_move,
            source,
            staging: None,
            staged: None,
            skipped: 0,
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
            self.staged = EntryId::at(at.dst, at.dst_name).ok();
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

    /// What the copy left behind, with the errors it recorded.
    fn into_outcome(self) -> CopyOutcome {
        CopyOutcome {
            errors: self.errors,
            skipped: self.skipped,
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
pub(super) struct Prepared {
    pub(super) source: Option<(File, Stat)>,
    pub(super) replace: Option<Seen>,
}

/// What a tree copy left behind: the entries that could not be written, how
/// many a standing "skip all" left alone, and which entry was copied.
#[derive(Default)]
pub(super) struct CopyOutcome {
    pub(super) errors: Vec<String>,
    pub(super) skipped: usize,
    /// Something the copy made holds the top-level destination name.
    pub(super) wrote: bool,
    pub(super) root: Option<EntryId>,
    /// The tree debouncer's threshold, which a copy too quick to outlast the
    /// time floor gives no other way to observe.
    #[cfg(test)]
    pub(super) progress_threshold: u64,
}

impl CopyOutcome {
    /// Whether the top-level entry itself was skipped, so nothing was copied:
    /// any skip inside the tree comes after the copy made its top directory.
    pub(super) fn top_skipped(&self) -> bool {
        self.skipped > 0 && !self.wrote
    }
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

/// The byte-copy stage shared by copy and cross-device move: checks the
/// destination and opens the source (`prepare_destination`, with the entry
/// `overwrite` grants replacing), for a directory source scans the real
/// transfer total (a directory entry's own size is not the transfer size) and
/// applies it via `set_total`, and copies the tree. `listed` is the source as
/// the task was started for it: its type, mode and size.
///
/// Returns `None` when the task was finalized on the way: cancelled, via
/// `active.cancelled()`, or refused by `prepare_destination`. Otherwise
/// returns the task and what the walk left behind, for the caller to finalize.
pub(super) fn copy_with_progress(
    settings: CopySettings<'_>,
    overwrite: Option<Seen>,
    listed: &PathInfo,
    active: ActiveTask,
    old_path: &Path,
    new_path: &Path,
) -> Option<(ActiveTask, CopyOutcome)> {
    let (mut active, prepared) = prepare_destination(
        active,
        settings,
        overwrite,
        old_path,
        new_path,
        listed.mode(),
    )?;
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
        settings,
        &mut active,
        &mut buffer,
        prepared.source,
        total_size,
    );
    let finished = copy_path(&mut context, prepared.replace, listed, old_path, new_path);
    let outcome = context.into_outcome();
    if !finished {
        cancel_logging(&outcome.errors, active);
        return None;
    }
    Some((active, outcome))
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
/// that cannot be copied is recorded in `CopyContext::errors` and the copy
/// continues with the remaining entries. Each returns `false` only when the
/// task was cancelled, in which case the caller must finalize with
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
    let failed = |error: &dyn std::fmt::Display| {
        failed_replacement(replace.is_some(), is_move, old_path, new_path, error)
    };
    let (Some(src_name), Some(dst_name)) = (c_name(old_path), c_name(new_path)) else {
        context.errors.push(format!(
            "Cannot {verb} {}: path has no file name{kept}",
            compact(old_path)
        ));
        return true;
    };
    let opened = open_parent(old_path).and_then(|src| {
        // The file `prepare_destination` opened is the one copied, so its
        // metadata is the one that counts.
        let stat = match &context.source {
            Some((_, stat)) => *stat,
            None => fstatat(&src, src_name.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW)?,
        };
        Ok((src, open_parent(new_path)?, stat))
    });
    let (src_parent, dst_parent, stat) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            context.errors.push(failed(&error));
            return true;
        }
    };
    context.root = Some(EntryId::of_stat(&stat));
    if type_bits(stat_mode(&stat)) != type_bits(listed.mode()) {
        context.errors.push(format!(
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
    if !listed.is_directory() {
        return match replace {
            Some(granted) => replace_entry(context, granted, &at, &paths, &stat),
            None => copy_entry(context, &at, &paths, &stat),
        };
    }
    let Some(level) = enter_directory(context, &at, &paths, &stat) else {
        return true;
    };
    // Only the directories being worked in are held open.
    drop((src_parent, dst_parent));
    copy_tree(context, level, &mut paths)
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

/// How an error copying `object` reads: filectrl's own refusal (no errno) in
/// the `Cannot` form, anything else in the `Failed to` form.
fn refused_or_failed(
    is_move: bool,
    object: &dyn std::fmt::Display,
    error: &std::io::Error,
) -> String {
    let shape = if error.raw_os_error().is_none() {
        "Cannot"
    } else {
        "Failed to"
    };
    format!("{shape} {} {object}: {error}", verb(is_move))
}

/// The object of a transfer's message: its source and destination. Below the
/// top-level entry it names the entry relative to both, since `compact` keeps
/// only the last components of a path, which are the same on either side.
fn transfer_object(paths: &Paths) -> String {
    let roots = (paths.depth > 0).then(|| {
        (
            paths.old.ancestors().nth(paths.depth),
            paths.new.ancestors().nth(paths.depth),
        )
    });
    let Some((Some(old_root), Some(new_root))) = roots else {
        return format!("{} to {}", compact(&paths.old), compact(&paths.new));
    };
    let entry = paths.old.strip_prefix(old_root).unwrap_or(&paths.old);
    format!(
        "{} from {} to {}",
        compact(entry),
        compact(old_root),
        compact(new_root)
    )
}
