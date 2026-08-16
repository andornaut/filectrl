use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::mpsc::Sender,
    thread,
    time::Duration,
};

use anyhow::{Result, anyhow};
use log::{info, warn};

use super::{
    path_info::{PathInfo, compact},
    shell,
    stream::{BATCH_FLUSH_INTERVAL, Batcher, batch_sender},
};
use crate::command::{Command, progress::CancellationToken};

const CD_BATCH_SIZE: usize = 256;

/// Spawns a background thread that reads `directory` and streams its entries as
/// `Command::ListingBatch`es, finishing with `Command::DirectoryListingComplete`.
/// `generation` tags every message so a superseded load (the user navigated away)
/// can be ignored, and `cancel` stops the walk when that happens. Off the UI
/// thread, so navigating into a very large directory stays responsive.
pub(super) fn stream_cd(
    directory: PathInfo,
    generation: u64,
    tx: Sender<Command>,
    cancel: CancellationToken,
) {
    info!("Streaming directory {directory:?}");
    thread::spawn(move || {
        let entries = match fs::read_dir(&directory.path) {
            Ok(entries) => entries,
            Err(error) => {
                let _ = tx.send(Command::AlertWarn(format!(
                    "Failed to read directory {}: {error}",
                    compact(&directory.path)
                )));
                let _ = tx.send(Command::DirectoryListingComplete { generation });
                return;
            }
        };

        let send = batch_sender(&tx, generation);
        let mut batcher = Batcher::new(CD_BATCH_SIZE, BATCH_FLUSH_INTERVAL);
        let mut error_count: usize = 0;

        for entry in entries {
            // A newer load has superseded this one: stop without sending a
            // completion (the newer load owns the listing now).
            if cancel.is_cancelled() {
                return;
            }
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(error) => {
                    warn!(
                        "Failed to read an entry in {}: {error}",
                        directory.path.display()
                    );
                    error_count += 1;
                    continue;
                }
            };
            match PathInfo::try_from(&path) {
                Ok(info) => {
                    if !batcher.push(info, &send) {
                        return; // channel closed
                    }
                }
                Err(error) => {
                    warn!("Failed to read metadata for {}: {error}", path.display());
                    error_count += 1;
                }
            }
        }

        if !batcher.flush(&send) {
            return;
        }
        if error_count > 0 {
            let _ = tx.send(Command::AlertWarn(format!(
                "{error_count} entries in {} could not be read",
                compact(&directory.path)
            )));
        }
        let _ = tx.send(Command::DirectoryListingComplete { generation });
    });
}

pub(super) fn open_in(path: &PathInfo, template: &str, command_tx: Sender<Command>) -> Result<()> {
    info!("Opening \"{path:?}\" using template: \"{template}\"");
    if template.is_empty() {
        return Ok(());
    }
    let command = shell::template(template, &shell::quote(path.path.as_os_str()));
    // Only for the message: the command line itself is passed to the shell as
    // raw bytes, so a path that is not valid UTF-8 still reaches the program.
    let label = format!("Command {:?}", command.to_string_lossy());
    let child = spawn_detached("sh", [OsStr::new("-c"), command.as_os_str()])
        .map_err(|error| anyhow!("Failed to run {label}: {error}"))?;
    watch_for_immediate_failure(child, label, command_tx);
    Ok(())
}

/// Launch `argv` directly, without a shell, so that nothing in a file name can
/// be reinterpreted. An empty `argv` is a no-op, mirroring `open_in`'s empty
/// template guard.
pub(super) fn spawn_argv(
    working_dir: Option<&Path>,
    label: &str,
    argv: &[OsString],
    command_tx: Sender<Command>,
) -> Result<()> {
    info!("Opening {label:?} using: {argv:?}");
    let Some((program, rest)) = argv.split_first() else {
        return Ok(());
    };
    let mut command = detached_command(program, rest);
    if let Some(working_dir) = working_dir {
        // A desktop entry's `Path=` key names the directory to run in. Check it
        // here so a stale one is reported as what it is: `spawn` would fail
        // with the same ENOENT as a missing program, and the alert would send
        // the user looking for the wrong thing.
        if !working_dir.is_dir() {
            return Err(anyhow!(
                "Cannot run {label:?}: its working directory {} is not a directory",
                compact(working_dir)
            ));
        }
        command.current_dir(working_dir);
    }
    let child = command
        .spawn()
        .map_err(|error| anyhow!("Failed to run {label:?}: {error}"))?;
    watch_for_immediate_failure(child, format!("{label:?}"), command_tx);
    Ok(())
}

/// Catch commands that fail immediately (e.g. binary not found) without
/// blocking the TUI. Long-lived processes (e.g. a terminal window) will still
/// be running after 250ms and are silently ignored.
fn watch_for_immediate_failure(mut child: Child, label: String, command_tx: Sender<Command>) {
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(250));
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let code = status
                        .code()
                        .map_or("unknown".to_string(), |c| c.to_string());
                    let _ = command_tx.send(Command::AlertError(format!(
                        "{label} failed (exit code {code})"
                    )));
                }
            }
            // Still running: block in this detached thread until it exits so
            // it is reaped rather than left as a zombie.
            _ => {
                let _ = child.wait();
            }
        }
    });
}

pub(super) fn chmod(path: &PathInfo, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let p = path.as_path();
    info!("Changing mode of {} to {mode:o}", p.display());
    let permissions = fs::Permissions::from_mode(mode);
    fs::set_permissions(p, permissions)?;
    Ok(())
}

/// Takes the bookmarks directory rather than reading it from the global
/// `Config`, so writes resolve against the same directory `read_bookmarks`
/// reads (`FileSystem::bookmarks_dir`).
pub(super) fn add_bookmark(dir: &Path, target: &PathInfo, name: &str) -> Result<()> {
    let name = name.trim();
    validate_basename("Bookmark name", name)?;
    fs::create_dir_all(dir)?;
    let link = dir.join(name);
    // Reject duplicates, including a pre-existing broken symlink.
    if link.symlink_metadata().is_ok() {
        return Err(anyhow!("A bookmark named {name:?} already exists"));
    }
    info!(
        "Creating bookmark {} -> {}",
        link.display(),
        target.path.display()
    );
    std::os::unix::fs::symlink(&target.path, &link)?;
    Ok(())
}

pub(super) fn create_directory(parent: &PathInfo, name: &str) -> Result<()> {
    validate_basename("Directory name", name)?;
    let path = parent.as_path().join(name);
    info!("Creating directory {}", path.display());
    fs::create_dir(&path)?;
    Ok(())
}

/// Rejects a name that cannot denote a new entry inside the directory it is
/// joined to. `Path::join` discards the base when handed an absolute path, so
/// without this a prompt value can create or rename an entry anywhere on the
/// filesystem rather than in the directory the user is looking at.
fn validate_basename(kind: &str, name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(anyhow!("{kind} cannot be empty"));
    }
    if name == "." || name == ".." {
        return Err(anyhow!("{kind} cannot be {name:?}"));
    }
    if name.contains(std::path::MAIN_SEPARATOR) {
        return Err(anyhow!(
            "{kind} cannot contain {:?}",
            std::path::MAIN_SEPARATOR
        ));
    }
    Ok(())
}

pub(super) fn rename(path: &PathInfo, new_basename: &str) -> Result<()> {
    validate_basename("New name", new_basename)?;
    let old_path = path.as_path();
    let new_path = join_parent(old_path, new_basename);
    info!("Renaming {} to {}", old_path.display(), new_path.display());
    if old_path != new_path {
        // Diagnose a vanished source up front; otherwise an existing
        // destination would be misreported as the problem. Only NotFound
        // means vanished; other errors (e.g. permission denied) must not
        // claim the file is gone.
        if let Err(error) = old_path.symlink_metadata() {
            return Err(if error.kind() == std::io::ErrorKind::NotFound {
                anyhow!("{} no longer exists", compact(old_path))
            } else {
                anyhow!("Failed to rename {}: {error}", compact(old_path))
            });
        }
        // Refuse to overwrite: `fs::rename` would silently replace an
        // existing destination.
        if new_path.symlink_metadata().is_ok() {
            if !is_same_file(old_path, &new_path) {
                return Err(anyhow!("{} already exists", compact(&new_path)));
            }
            // Same underlying file. A case-only change is a real rename on a
            // case-insensitive filesystem, where the destination resolves to the
            // source, so it is let through. Renaming onto another hard link of
            // the same inode is a POSIX no-op, so it is reported instead.
            if !is_case_only_change(old_path, &new_path) {
                return Err(anyhow!(
                    "{} and {} are the same file",
                    compact(old_path),
                    compact(&new_path)
                ));
            }
        }
        fs::rename(old_path, new_path)?;
    }
    Ok(())
}

/// True when the two paths' file names differ only by letter case.
fn is_case_only_change(a: &Path, b: &Path) -> bool {
    match (a.file_name(), b.file_name()) {
        (Some(a), Some(b)) => {
            a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
        }
        _ => false,
    }
}

/// True when both paths resolve to the same underlying file (device and
/// inode). Links are not followed, so a symlink is compared as the link
/// itself.
fn is_same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (a.symlink_metadata(), b.symlink_metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// The single place the detach strategy is defined, so that both spawn paths
/// stay in step.
fn detached_command<P, I, S>(program: P, args: I) -> std::process::Command
where
    P: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn spawn_detached<P, I, S>(program: P, args: I) -> Result<Child>
where
    P: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    detached_command(program, args).spawn().map_err(Into::into)
}

fn join_parent(left: &Path, right: &str) -> PathBuf {
    match left.parent() {
        Some(parent) => parent.join(right),
        None => PathBuf::from(right),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use test_case::test_case;

    use super::*;
    use crate::test_support::TempDir;

    #[test_case("/b", "/a", "b"; "/a to b relative")]
    #[test_case("/b", "/a", "/b"; "/a to /b absolute")]
    #[test_case("/b", "/a/aa", "/b"; "/a/aa to /b absolute")]
    #[test_case("/a/aa", "/b", "/a/aa"; "/b to /a/aa absolute")]
    #[test_case("/b", "/", "/b"; "root to /b absolute")]
    #[test_case("/b", "", "/b"; "empty to /b absolute")]
    fn join_is_correct_when(expected: &str, left: &str, right: &str) {
        let old_path = Path::new(left);
        let result = join_parent(old_path, right);

        assert_eq!(expected, result.to_string_lossy());
    }

    /// Whether `dir` gained an entry, which is how a name that escaped
    /// validation shows up: it creates something, just not where it was asked
    /// to.
    fn is_empty(dir: &TempDir) -> bool {
        fs::read_dir(dir.path()).unwrap().next().is_none()
    }

    #[test_case("" ; "empty")]
    #[test_case("." ; "current directory")]
    #[test_case(".." ; "parent directory")]
    #[test_case("nested/name" ; "relative path")]
    fn create_directory_rejects_a_name_that_is_not_a_basename(name: &str) {
        let dir = TempDir::new("ops_create");
        let parent = PathInfo::try_from(dir.path()).unwrap();

        assert!(create_directory(&parent, name).is_err());
        assert!(is_empty(&dir));
    }

    #[test]
    fn create_directory_rejects_an_absolute_name() {
        let dir = TempDir::new("ops_create_absolute");
        let parent = PathInfo::try_from(dir.path()).unwrap();
        // `Path::join` drops the parent entirely for an absolute name, so an
        // unvalidated one creates a directory outside the one on screen. The
        // escape target is a reserved fixture path, so its absence is this
        // call's doing and not another process's.
        let escape = TempDir::reserved("ops_create_escape");

        assert!(create_directory(&parent, escape.path().to_str().unwrap()).is_err());
        assert!(!escape.path().exists());
        assert!(is_empty(&dir));
    }

    #[test]
    fn rename_refuses_existing_destination() {
        let dir = TempDir::new("ops_rename");
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();

        let info = PathInfo::try_from(a.as_path()).unwrap();
        assert!(rename(&info, "b.txt").is_err());
        // The existing destination must be untouched.
        assert_eq!(b"b".to_vec(), fs::read(&b).unwrap());

        assert!(rename(&info, "c.txt").is_ok());
        assert!(dir.join("c.txt").exists());
    }

    #[test]
    fn rename_reports_same_file_for_hard_link_destination() {
        let dir = TempDir::new("ops_samefile");
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        fs::write(&a, b"a").unwrap();
        fs::hard_link(&a, &b).unwrap();

        let info = PathInfo::try_from(a.as_path()).unwrap();
        // Renaming onto another hard link of the same inode would be a POSIX
        // no-op; report it rather than silently succeeding.
        let error = rename(&info, "b.txt").unwrap_err().to_string();
        assert!(error.contains("same file"), "unexpected error: {error}");
        assert!(a.exists());
        assert!(b.exists());
    }

    // Linux only: the case-variant hard link this builds is what stands in for
    // a case-insensitive filesystem, and it cannot be created on one, where
    // "A.TXT" already resolves to "a.txt".
    #[cfg(target_os = "linux")]
    #[test]
    fn rename_allows_case_only_change_to_same_file() {
        let dir = TempDir::new("ops_casechange");
        let a = dir.join("a.txt");
        // On a case-insensitive filesystem the destination of a case-only
        // rename resolves to the source itself; a case-variant hard link is
        // the closest equivalent constructible on a case-sensitive one.
        let upper = dir.join("A.TXT");
        fs::write(&a, b"a").unwrap();
        fs::hard_link(&a, &upper).unwrap();

        let info = PathInfo::try_from(a.as_path()).unwrap();
        assert!(rename(&info, "A.TXT").is_ok());
        assert!(upper.exists());
    }

    // ── chmod ───────────────────────────────────────────────────────────────

    fn mode_of(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn chmod_sets_the_mode() {
        let dir = TempDir::new("ops_chmod");
        let file = dir.join("a.txt");
        fs::write(&file, b"a").unwrap();
        let info = PathInfo::try_from(file.as_path()).unwrap();

        chmod(&info, 0o600).unwrap();

        assert_eq!(0o600, mode_of(&file));
    }

    #[test]
    fn chmod_on_a_directory_leaves_its_contents_alone() {
        let dir = TempDir::new("ops_chmod_dir");
        let target = dir.join("sub");
        fs::create_dir(&target).unwrap();
        let inner = target.join("inner.txt");
        fs::write(&inner, b"x").unwrap();
        fs::set_permissions(&inner, fs::Permissions::from_mode(0o644)).unwrap();
        let info = PathInfo::try_from(target.as_path()).unwrap();

        // Deliberately not recursive: it applies to exactly the marked
        // entries, so a two-keystroke prompt cannot rewrite a whole subtree.
        chmod(&info, 0o700).unwrap();

        assert_eq!(0o700, mode_of(&target));
        assert_eq!(0o644, mode_of(&inner));
    }

    // ── create_directory ────────────────────────────────────────────────────

    #[test]
    fn create_directory_creates_it_inside_the_parent() {
        let dir = TempDir::new("ops_mkdir");
        let parent = PathInfo::try_from(dir.path()).unwrap();

        create_directory(&parent, "brand_new").unwrap();

        assert!(dir.join("brand_new").is_dir());
    }

    #[test]
    fn create_directory_refuses_a_name_already_taken() {
        let dir = TempDir::new("ops_mkdir_exists");
        let parent = PathInfo::try_from(dir.path()).unwrap();
        fs::write(dir.join("taken"), b"x").unwrap();

        // `create_dir` rather than `create_dir_all`, so an existing entry is
        // an error instead of silently adopting whatever is already there.
        assert!(create_directory(&parent, "taken").is_err());
        assert!(dir.join("taken").is_file());
    }

    // ── add_bookmark ────────────────────────────────────────────────────────

    #[test]
    fn add_bookmark_symlinks_the_named_directory() {
        let base = TempDir::new("ops_bookmark");
        let bookmarks = base.join("bookmarks");
        let target = PathInfo::try_from(base.path()).unwrap();

        // The bookmarks directory does not exist yet; adding one creates it.
        add_bookmark(&bookmarks, &target, "favs").unwrap();

        assert_eq!(base.path(), fs::read_link(bookmarks.join("favs")).unwrap());
    }

    #[test]
    fn add_bookmark_trims_the_name_it_is_given() {
        let base = TempDir::new("ops_bookmark_trim");
        let bookmarks = base.join("bookmarks");
        let target = PathInfo::try_from(base.path()).unwrap();

        add_bookmark(&bookmarks, &target, "  favs  ").unwrap();

        assert!(bookmarks.join("favs").symlink_metadata().is_ok());
    }

    #[test_case("" ; "empty")]
    #[test_case("   " ; "only whitespace, which trims to empty")]
    #[test_case("nested/name" ; "a path rather than a name")]
    fn add_bookmark_refuses_a_name_that_is_not_a_basename(name: &str) {
        let base = TempDir::new("ops_bookmark_bad_name");
        let bookmarks = base.join("bookmarks");
        let target = PathInfo::try_from(base.path()).unwrap();

        assert!(add_bookmark(&bookmarks, &target, name).is_err());
    }

    #[test]
    fn add_bookmark_refuses_an_absolute_name() {
        let base = TempDir::new("ops_bookmark_absolute");
        let bookmarks = base.join("bookmarks");
        let target = PathInfo::try_from(base.path()).unwrap();
        // Joined onto the bookmarks directory, an absolute name replaces it,
        // so the symlink would be planted wherever the name pointed. A
        // reserved fixture path makes its absence attributable to this call.
        let escape = TempDir::reserved("ops_bookmark_escape");

        assert!(add_bookmark(&bookmarks, &target, escape.path().to_str().unwrap()).is_err());
        assert!(!escape.path().exists());
    }

    #[test]
    fn add_bookmark_refuses_a_name_already_taken() {
        let base = TempDir::new("ops_bookmark_dup");
        let bookmarks = base.join("bookmarks");
        let target = PathInfo::try_from(base.path()).unwrap();
        add_bookmark(&bookmarks, &target, "favs").unwrap();

        let error = add_bookmark(&bookmarks, &target, "favs")
            .expect_err("a duplicate name must be refused")
            .to_string();

        assert!(error.contains("already exists"), "{error}");
    }

    #[test]
    fn add_bookmark_refuses_a_name_held_by_a_broken_symlink() {
        let base = TempDir::new("ops_bookmark_broken");
        let bookmarks = base.join("bookmarks");
        fs::create_dir_all(&bookmarks).unwrap();
        // What a bookmark becomes once its target is removed. `exists()`
        // follows the link and reports false, so the check has to be
        // `symlink_metadata` or the name is silently reused.
        std::os::unix::fs::symlink(base.join("gone"), bookmarks.join("favs")).unwrap();
        let target = PathInfo::try_from(base.path()).unwrap();

        let error = add_bookmark(&bookmarks, &target, "favs")
            .expect_err("a name held by a broken symlink must be refused")
            .to_string();

        // filectrl's own refusal, not the EEXIST `symlink` would raise a line
        // later: asserting only `is_err` cannot tell the two apart, and the
        // errno one means the duplicate check let it through.
        assert!(error.contains("already exists"), "{error}");
    }
}
