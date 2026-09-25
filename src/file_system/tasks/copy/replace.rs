use super::super::sys::{Errno, Stat, UnlinkatFlags, fstat, mkdirat, mode_bits, stat_mode};
use super::super::validate::{Holds, look_again, rename_no_replace_at, still_holds};
use super::super::walk::{c_name, open_directory, open_parent, unlink_at};
use super::entry::{copy_entry, open_source_file};
use super::{At, CopyContext, CopySettings, Paths, Prepared, refused_or_failed, transfer_object};
use crate::command::progress::ActiveTask;
use crate::file_system::conflicts::{
    changed_refusal, failed_replacement, not_replaced, rename_failure, verb,
};
use crate::file_system::entry_id::{EntryId, Seen};
use crate::file_system::path_info::compact;
use log::warn;
use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

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
pub(super) fn replace_entry(
    context: &mut CopyContext<'_>,
    granted: Seen,
    at: &At<'_>,
    paths: &Paths,
    stat: &Stat,
) -> bool {
    let parent = paths.new.parent().unwrap_or(Path::new("."));
    match Staging::create(at.dst, parent, staging_names(), at.dst_name) {
        Ok(staging) => {
            let replacement = Replacement { granted, staging };
            replace_through(context, replacement, at, paths, stat)
        }
        Err(error) => {
            context.errors.push(
                refused_or_failed(context.is_move, &transfer_object(paths), &error)
                    + &not_replaced(&paths.new),
            );
            true
        }
    }
}

/// What a granted overwrite replaces an entry with: the entry `granted`
/// describes, and the directory its replacement is written into.
pub(super) struct Replacement<'a> {
    pub(super) granted: Seen,
    pub(super) staging: Staging<'a>,
}

/// `replace_entry` once its staging directory is made: copies the entry into
/// it, lands the copy when it is whole, and removes the staging directory.
pub(super) fn replace_through(
    context: &mut CopyContext<'_>,
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
    let errors_before = context.errors.len();
    context.staging = Some(staging.path.clone());
    let finished = copy_entry(context, &staged, paths, stat);
    context.staging = None;
    staging.staged = context.staged.take();
    let kept = not_replaced(&paths.new);
    for error in &mut context.errors[errors_before..] {
        error.push_str(&kept);
    }
    // A staged copy is never skipped: a name taken in its staging directory
    // is refused (`resolve_raced`), so only an error or a cancel stops it.
    if finished && context.errors.len() == errors_before {
        match land(context, &staging, granted, at, paths) {
            Ok(true) => {
                context.wrote = true;
                // Landed, so nothing of it is left in staging to remove.
                staging.staged = None;
            }
            Ok(false) => {}
            Err(message) => context.errors.push(message),
        }
    }
    clear_staging(context.active, staging);
    finished
}

/// The refusal of an entry whose staging directory holds something the copy
/// did not put there, which is neither landed nor removed.
pub(super) fn staging_changed(is_move: bool, paths: &Paths) -> String {
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
pub(super) fn clear_staging(active: &ActiveTask, staging: Staging<'_>) {
    let path = staging.path.clone();
    if let Err(error) = staging.remove() {
        let message = staging_left_behind(&path, &error);
        warn!("{message}");
        active.warn(message);
    }
}

/// What a staging directory at `path` that could not be removed says.
pub(super) fn staging_left_behind(path: &Path, error: &std::io::Error) -> String {
    format!(
        "Failed to remove the staging directory {}: {error}",
        compact(path)
    )
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
pub(super) fn land(
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
        failed_replacement(true, context.is_move, &paths.old, &paths.new, &error)
    })?;
    let replaces = match still_holds(granted, found) {
        Holds::Granted => true,
        Holds::Free => false,
        Holds::Changed => return Err(changed_refusal(context.is_move, &paths.old, &paths.new)),
    };
    let renamed = rename_staged(replaces, &staging.dir, at.dst, at.dst_name);
    landed(context, paths, replaces, renamed)
}

/// What the rename that lands a replacement (`renamed`) means
/// (`conflicts::rename_failure`): `Ok(false)` when a standing "skip all"
/// skipped a name taken since the check.
pub(super) fn landed(
    context: &mut CopyContext<'_>,
    paths: &Paths,
    replaces: bool,
    renamed: std::io::Result<()>,
) -> Result<bool, String> {
    let Err(error) = renamed else {
        return Ok(true);
    };
    let (conflicts, is_move) = (context.conflicts, context.is_move);
    match rename_failure(conflicts, is_move, replaces, &paths.old, &paths.new, &error) {
        None => {
            context.skipped += 1;
            Ok(false)
        }
        Some(message) => Err(message),
    }
}

/// Renames the entry `name` in `staging` to `name` in `dst`: over what holds
/// it when `replaces`, with a plain `renameat`, which works where `renameat2`
/// is missing, and otherwise without replacing anything (`AlreadyExists` when
/// the name is taken).
pub(super) fn rename_staged(
    replaces: bool,
    staging: &File,
    dst: &File,
    name: &CStr,
) -> std::io::Result<()> {
    if replaces {
        Ok(rustix::fs::renameat(staging, name, dst, name)?)
    } else {
        rename_no_replace_at(staging, name, dst, name)
    }
}

/// What every staging directory's name starts with.
pub(in crate::file_system::tasks) const STAGING_PREFIX: &str = ".filectrl-";

/// How many names `staging_names` offers before giving up.
pub(super) const STAGING_ATTEMPTS: u64 = 16;

/// Names for a staging directory, hidden and unique to this process and call.
pub(super) fn staging_names() -> impl Iterator<Item = CString> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let pid = std::process::id();
    (0..STAGING_ATTEMPTS).map(move |_| {
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        CString::new(format!("{STAGING_PREFIX}{pid}-{serial}")).expect("the name holds no NUL")
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
pub(super) struct Staging<'a> {
    pub(super) parent: &'a File,
    pub(super) name: CString,
    /// Where it is, for what is made by path and for a warning.
    pub(super) path: PathBuf,
    pub(super) dir: File,
    /// Which directory `dir` is.
    pub(super) id: EntryId,
    pub(super) entry: CString,
    /// The entry the copy created at `entry`, the only one landed or removed.
    pub(super) staged: Option<EntryId>,
    pub(super) removed: bool,
}

impl<'a> Staging<'a> {
    /// Creates a staging directory in `parent`, which is at `parent_path`.
    /// Having no free name is filectrl's own refusal, an error with no errno.
    pub(super) fn create(
        parent: &'a File,
        parent_path: &Path,
        names: impl IntoIterator<Item = CString>,
        entry: &CStr,
    ) -> std::io::Result<Self> {
        for name in names {
            match mkdirat(parent, name.as_c_str(), mode_bits(0o700)) {
                Ok(()) => {}
                Err(Errno::EEXIST) => continue,
                Err(errno) => return Err(errno.into()),
            }
            let path = parent_path.join(OsStr::from_bytes(name.to_bytes()));
            return Self::adopt(parent, name, path, entry);
        }
        Err(std::io::Error::new(
            ErrorKind::AlreadyExists,
            "no free name for a staging directory",
        ))
    }

    /// Takes the directory just made at `name` in `parent` (at `path`) as the
    /// staging directory (`open_owned`). When it cannot be opened, whatever
    /// holds the name is removed if it is an empty directory, which anyone who
    /// can write `parent` could remove as well, and otherwise left where it is.
    pub(super) fn adopt(
        parent: &'a File,
        name: CString,
        path: PathBuf,
        entry: &CStr,
    ) -> std::io::Result<Self> {
        match open_owned(parent, &name, &path) {
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
    pub(super) fn holds_staged(&self) -> Option<EntryId> {
        EntryId::at(&self.dir, &self.entry).ok()
    }

    /// Removes the staging directory, with its entry if that is still there.
    pub(super) fn remove(mut self) -> std::io::Result<()> {
        self.removed = true;
        self.unlink_all()
    }

    /// Unlinks the entry the copy made, if it still holds its name, through
    /// the directory's own handle, then the directory by name, which only
    /// succeeds when it is empty, and only while the name still holds this
    /// directory: another swapped in at it is left alone.
    pub(super) fn unlink_all(&self) -> std::io::Result<()> {
        if self.staged.is_some() && self.holds_staged() == self.staged {
            unlink_at(&self.dir, &self.entry, UnlinkatFlags::NoRemoveDir)?;
        }
        if EntryId::at(self.parent, &self.name)? != self.id {
            return Err(std::io::Error::other(
                "another entry holds its name, and was left alone",
            ));
        }
        unlink_at(self.parent, &self.name, UnlinkatFlags::RemoveDir)
    }
}

/// Opens the directory `name` just created in `parent`, at `path`, without
/// following a symlink there, and gives it owner access when a umask
/// (`0o277`) or a default ACL took that away: without it nothing could be
/// written into it. Returns it and which directory it is. It is not checked to
/// be the one just made: the copy acts in it only on the entry it creates
/// there, by identity (`land`, `unlink_all`), so a directory another user
/// swaps in at the name loses nothing but, at most, the owner access given
/// to it. One left without owner read (a umask of `0o477`) cannot be opened,
/// and the replacement fails. A mount that refuses a mode change to the user
/// who created the directory (vfat or exfat without `uid=`, CIFS without unix
/// extensions) has no per-user permissions to keep, so that refusal is only
/// logged: a directory that really cannot be written still fails the write
/// into it.
pub(super) fn open_owned(
    parent: &File,
    name: &CStr,
    path: &Path,
) -> std::io::Result<(File, EntryId)> {
    let dir = open_directory(parent, name)?;
    let id = EntryId::of(&dir)?;
    let mode = stat_mode(&fstat(&dir)?) & 0o7777;
    if mode & 0o700 != 0o700 {
        match nix::sys::stat::fchmod(&dir, mode_bits(mode | 0o700)) {
            Err(Errno::EPERM) => warn!(
                "Failed to give the staging directory {} owner access: {}",
                compact(path),
                std::io::Error::from(Errno::EPERM)
            ),
            result => result?,
        }
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
            warn!("{}", staging_left_behind(&self.path, &error));
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
pub(in crate::file_system::tasks) fn prepare_destination(
    active: ActiveTask,
    settings: CopySettings<'_>,
    overwrite: Option<Seen>,
    old_path: &Path,
    new_path: &Path,
    source_mode: u32,
) -> Option<(ActiveTask, Prepared)> {
    let failed = |kept: bool, error: &dyn std::fmt::Display| {
        failed_replacement(kept, settings.is_move, old_path, new_path, error)
    };
    let replace = match overwrite.map(|granted| (granted, look_again(granted, new_path))) {
        None | Some((_, Ok((Holds::Free, _)))) => None,
        Some((granted, Ok((Holds::Granted, _)))) => Some(granted),
        Some((_, Ok((Holds::Changed, _)))) => {
            active.error(changed_refusal(settings.is_move, old_path, new_path));
            return None;
        }
        Some((_, Err(error))) => {
            active.error(failed(true, &error));
            return None;
        }
    };
    let source = if unix_mode::is_file(source_mode) {
        let name = c_name(old_path).ok_or_else(|| std::io::Error::from(ErrorKind::InvalidInput));
        match open_parent(old_path).and_then(|parent| open_source_file(&parent, &name?)) {
            Ok(file) => Some(file),
            Err(error) => {
                active.error(failed(replace.is_some(), &error));
                return None;
            }
        }
    } else {
        None
    };
    Some((active, Prepared { source, replace }))
}
