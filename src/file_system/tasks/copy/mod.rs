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

/// Settings and shared state one copy carries from the task down to every entry of the tree.
struct CopyContext<'a> {
    active: &'a mut ActiveTask,
    /// Entries that could not be copied, in order. The copy continues past them, like `cp -R`.
    errors: Vec<String>,
    /// One read buffer for the whole tree.
    buffer: &'a mut [u8],
    /// The paste's standing `*All` answer.
    conflicts: &'a Conflicts,
    /// The copy a move across devices makes: it keeps full mode, times and extended attributes,
    /// like `mv`. A copy takes the umask and drops the special bits, like `cp`.
    is_move: bool,
    /// The top-level source file already opened by `prepare_destination`, with its metadata.
    source: Option<(File, Stat)>,
    /// The staging directory the top-level entry is written into while it replaces another.
    staging: Option<PathBuf>,
    /// The entry the copy created in the staging directory, the only one it lands or removes.
    staged: Option<EntryId>,
    /// Entries a standing "skip all" left alone. Not errors, but a move must still keep its source.
    skipped: usize,
    /// Something the copy made now holds the top-level destination name.
    wrote: bool,
    /// Directories this copy created, never descended into as a source: a destination swapped for a
    /// link into the source tree would otherwise copy into itself without end.
    created: std::collections::HashSet<EntryId>,
    /// The top-level source copied, the only entry a move removes afterwards.
    root: Option<EntryId>,
    /// One debouncer for the whole tree: a per-file debouncer sends an update for every file.
    progress: debounce::ProgressDebouncer,
    /// Called with each directory before the walk returns to its parent through it.
    #[cfg(test)]
    on_leave: Option<OnLeave<'a>>,
    /// The deepest level the walk may enter; past it the walk stops as if cancelled.
    #[cfg(test)]
    max_depth: Option<usize>,
}

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
            #[cfg(test)]
            max_depth: None,
        }
    }

    /// Records that the copy created the entry `at` names. While staged, the entry's identity is
    /// recorded instead, so only it is landed or removed.
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

    /// A failure to write `paths`: `own`, or while staged, the transfer that failed, since the
    /// entry at the name is left as it was.
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

    /// Where the entry is being created (the staging directory for a staged top-level entry), for
    /// `make_node` on macOS.
    fn node_path(&self, paths: &Paths) -> PathBuf {
        match (&self.staging, paths.new.file_name()) {
            (Some(staging), Some(name)) if paths.depth == 0 => staging.join(name),
            _ => paths.new.clone(),
        }
    }

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

/// What `prepare_destination` hands the copy: the opened top-level source file, and the entry a
/// granted overwrite replaces if it still held the name.
pub(super) struct Prepared {
    pub(super) source: Option<(File, Stat)>,
    pub(super) replace: Option<Seen>,
}

/// What a tree copy left behind: errors, how many entries were skipped, and which entry was copied.
#[derive(Default)]
pub(super) struct CopyOutcome {
    pub(super) errors: Vec<String>,
    pub(super) skipped: usize,
    pub(super) wrote: bool,
    pub(super) root: Option<EntryId>,
    /// The tree debouncer's threshold.
    #[cfg(test)]
    pub(super) progress_threshold: u64,
}

impl CopyOutcome {
    /// Whether the top-level entry itself was skipped.
    pub(super) fn top_skipped(&self) -> bool {
        self.skipped > 0 && !self.wrote
    }
}

/// The copy read buffer size, the same as coreutils `cp`. Keeps cancel latency near 10 ms under
/// writeback throttling.
const COPY_BUFFER_BYTES: usize = 128 * 1024;

/// What a copy was asked for, the same for every entry of its tree.
#[derive(Clone, Copy)]
pub(super) struct CopySettings<'a> {
    pub(super) is_move: bool,
    pub(super) conflicts: &'a Conflicts,
}

/// The byte-copy stage shared by copy and cross-device move: prepares the destination, sets the
/// progress total for a directory source, and copies the tree. `listed` is the source as the task
/// was started for it.
///
/// `None` when the task was finalized on the way (cancelled or refused); otherwise the task and
/// what the walk left behind, for the caller to finalize.
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

/// Best-effort recursive size for the progress total; unreadable entries are skipped. `None` when
/// cancelled.
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

/// Copies with `cp -R`/`mv` semantics: an entry that cannot be copied is recorded and the copy
/// continues. Returns `false` only when cancelled, and then the caller finalizes with
/// `active.cancelled()`. A cancelled copy leaves its partial destination, except a replacement
/// (`replace_entry`).
///
/// Every entry is accessed relative to an open directory and never through a symlink, so an entry
/// swapped for a link fails rather than leading the copy out of the tree. Only the top-level
/// parents are opened by path. Modes and owners come from the file actually copied; a source no
/// longer of its `listed` type is refused.
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
        // The file `prepare_destination` opened is the one copied.
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
    drop((src_parent, dst_parent));
    copy_tree(context, level, &mut paths)
}

struct At<'a> {
    src: &'a File,
    dst: &'a File,
    src_name: &'a CStr,
    dst_name: &'a CStr,
}

/// The paths of the entry being copied, for messages only. One pair for the whole walk, extended
/// and truncated in step with it.
struct Paths {
    old: PathBuf,
    new: PathBuf,
    /// Depth below the top-level entry: 0 for the entry itself.
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

/// What the copy needs of a source entry, taken before the copy reads it since reading moves its
/// access time.
struct Source {
    mode: u32,
    uid: u32,
    gid: u32,
    times: Option<Times>,
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

    /// Reads `file`'s metadata, and its extended attributes for a move. `path` names it in a
    /// warning.
    fn read(is_move: bool, path: &Path, file: &File, stat: &Stat) -> Self {
        let mut source = Self::of(stat);
        if is_move {
            let is_directory = FileType::of(stat) == FileType::Directory;
            source.attributes = read_attributes(path, is_directory, file);
        }
        source
    }
}

/// An error copying `object`: `Cannot` for filectrl's own refusal (no errno), `Failed to`
/// otherwise.
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

/// The object of a transfer's message. Below the top level it names the entry relative to both
/// sides, since `compact` keeps only the last components of a path.
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
