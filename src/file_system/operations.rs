use std::{
    ffi::{OsStr, OsString},
    fs,
    io::ErrorKind,
    os::unix::process::CommandExt,
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
    tasks::{is_same_file, rename_no_replace, restat},
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
    let argv = shell::command(template, [path.path.as_os_str().to_os_string()]);
    let label = format!("Command {template:?}");
    let child = detached_command(&argv[0], &argv[1..])
        .spawn()
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

/// Refuses a symlink rather than changing its target: `chmod(2)` follows the
/// link, and Linux has no `lchmod`. The type checked is the one the path has
/// now, which is what the mode is set on.
pub(super) fn chmod(path: &PathInfo, mode: u32) -> Result<()> {
    let p = path.as_path();
    if restat(path, "chmod")?.is_symlink() {
        return Err(symlink_refusal(p));
    }
    info!("Changing mode of {} to {mode:o}", p.display());
    set_mode_without_following(p, mode)
}

/// Sets the mode with `fchmodat(AT_SYMLINK_NOFOLLOW)`, which the C library
/// implements without following the final component and refuses on Linux with
/// `EOPNOTSUPP` for a symlink, so a path swapped for one after `chmod` checked
/// it is refused rather than followed. macOS sets the link's own mode instead,
/// which leaves the target alone too.
///
/// An `EOPNOTSUPP` is never retried with a chmod that follows links: a link
/// swapped in again before the retry would have its target changed.
fn set_mode_without_following(p: &Path, mode: u32) -> Result<()> {
    use nix::{
        errno::Errno,
        fcntl::AT_FDCWD,
        sys::stat::{FchmodatFlags, Mode, fchmodat},
    };

    // `mode_t` is u32 on Linux but u16 on macOS; the permission bits
    // `from_bits_truncate` keeps fit in either.
    #[allow(clippy::cast_possible_truncation)]
    let bits = Mode::from_bits_truncate(mode as nix::libc::mode_t);
    match fchmodat(AT_FDCWD, p, bits, FchmodatFlags::NoFollowSymlink) {
        Ok(()) => Ok(()),
        Err(Errno::EOPNOTSUPP) if is_symlink(p, mode)? => Err(symlink_refusal(p)),
        Err(errno) => Err(chmod_failure(p, mode, &std::io::Error::from(errno))),
    }
}

fn is_symlink(p: &Path, mode: u32) -> Result<bool> {
    p.symlink_metadata()
        .map(|metadata| metadata.is_symlink())
        .map_err(|error| chmod_failure(p, mode, &error))
}

fn symlink_refusal(p: &Path) -> anyhow::Error {
    anyhow!("Cannot chmod {}: it is a symlink", compact(p))
}

fn chmod_failure(p: &Path, mode: u32, error: &dyn std::fmt::Display) -> anyhow::Error {
    anyhow!("Failed to chmod {} to {mode:o}: {error}", compact(p))
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

/// Renames `path` to `new_basename` in the same directory, never replacing an
/// existing entry. Every error is the whole message: `Cannot rename` for a
/// refusal filectrl decided, `Failed to rename` for one the system reported.
pub(super) fn rename(path: &PathInfo, new_basename: &str) -> Result<()> {
    let old_path = path.as_path();
    let refuse = |reason: &dyn std::fmt::Display| {
        anyhow!(
            "Cannot rename {} to {new_basename:?}: {reason}",
            compact(old_path)
        )
    };
    let fail = |error: &dyn std::fmt::Display| {
        anyhow!(
            "Failed to rename {} to {new_basename:?}: {error}",
            compact(old_path)
        )
    };
    validate_basename("New name", new_basename).map_err(|error| refuse(&error))?;
    let new_path = join_parent(old_path, new_basename);
    if old_path == new_path {
        return Ok(());
    }
    info!("Renaming {} to {}", old_path.display(), new_path.display());
    match rename_no_replace(old_path, &new_path) {
        // Only NotFound means vanished; other errors (e.g. permission denied)
        // must not claim the file is gone.
        Err(error) if error.kind() == ErrorKind::NotFound => Err(refuse(&"it no longer exists")),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            if !is_same_file(old_path, &new_path) {
                return Err(refuse(&format_args!(
                    "{} already exists",
                    compact(&new_path)
                )));
            }
            // Same underlying file. A case-only change is a real rename on a
            // case-insensitive filesystem, where the new name resolves to the
            // source and the directory still lists the old spelling, so it is
            // let through. Renaming onto another hard link of the same inode
            // is a POSIX no-op, so it is reported instead, including a link
            // whose name differs only in case on a case-sensitive filesystem,
            // which the directory lists exactly as typed.
            if !is_case_only_change(old_path, &new_path) || is_listed(&new_path) {
                return Err(refuse(&"both names are the same file"));
            }
            fs::rename(old_path, new_path).map_err(|error| fail(&error))
        }
        result => result.map_err(|error| fail(&error)),
    }
}

/// Whether the directory `path` is in lists an entry of exactly its name.
fn is_listed(path: &Path) -> bool {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return false;
    };
    fs::read_dir(parent)
        .is_ok_and(|mut entries| entries.any(|entry| entry.is_ok_and(|e| e.file_name() == name)))
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

/// The single place the detach strategy is defined, so that both spawn paths
/// stay in step.
///
/// The child gets a process group of its own, outside the terminal's
/// foreground group: signals the terminal sends that group (the hangup when
/// it closes) do not reach it, and it is stopped rather than obeyed if it
/// tries to read from or reconfigure the terminal filectrl is drawing on.
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
        .stderr(Stdio::null())
        .process_group(0);
    command
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

    #[test]
    fn renaming_a_vanished_source_says_so_rather_than_blaming_the_destination() {
        let dir = TempDir::new("ops_rename_vanished");
        let gone = PathInfo::try_from(dir.path()).unwrap();
        std::fs::remove_dir_all(dir.path()).unwrap();

        let error = rename(&gone, "new-name").unwrap_err().to_string();

        // Only NotFound means vanished. Any other errno has to keep its own
        // message, so a permission problem is not reported as a missing file.
        assert!(error.contains("no longer exists"), "{error}");
    }

    #[test_case("/b", "/a", "b"; "a sibling of a top-level entry")]
    #[test_case("/a/b", "/a/aa", "b"; "a sibling of a nested entry")]
    #[test_case("b", "", "b"; "a path with no parent")]
    fn join_parent_names_a_sibling_of(expected: &str, left: &str, right: &str) {
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

    #[test_case("" => "Directory name cannot be empty" ; "empty")]
    #[test_case("." => "Directory name cannot be \".\"" ; "current directory")]
    #[test_case(".." => "Directory name cannot be \"..\"" ; "parent directory")]
    #[test_case("nested/name" => "Directory name cannot contain '/'" ; "relative path")]
    fn create_directory_rejects_a_name_that_is_not_a_basename(name: &str) -> String {
        let dir = TempDir::new("ops_create");
        let parent = PathInfo::try_from(dir.path()).unwrap();

        // Every one of these names also fails at `create_dir`, so which rule
        // refused is the assertion: `is_err` alone holds with no validation.
        let error = create_directory(&parent, name)
            .expect_err("a name that is not a basename must be refused")
            .to_string();
        assert!(is_empty(&dir));
        error
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
        // Which refusal matters: a destination wrongly taken for the source's
        // own file is refused too, with the "same file" message instead.
        let error = rename(&info, "b.txt")
            .expect_err("an existing destination must be refused")
            .to_string();
        assert!(error.ends_with("already exists"), "{error}");
        // The existing destination must be untouched.
        assert_eq!(b"b".to_vec(), fs::read(&b).unwrap());

        assert!(rename(&info, "c.txt").is_ok());
        assert!(dir.join("c.txt").exists());
    }

    #[test]
    fn rename_of_an_unreachable_entry_is_a_failure_not_a_vanishing() {
        let dir = TempDir::new("ops_rename_unreachable");
        let sub = dir.join("sub");
        fs::create_dir(&sub).unwrap();
        let a = sub.join("a.txt");
        fs::write(&a, b"a").unwrap();
        let listed = PathInfo::try_from(a.as_path()).unwrap();
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o000)).unwrap();
        // Root reaches through a mode-000 directory anyway; probe rather than
        // inspect the euid.
        let is_unreachable = a.symlink_metadata().is_err();

        let result = rename(&listed, "b.txt");
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();

        if !is_unreachable {
            return;
        }
        // EACCES keeps its own message: the entry is still there.
        let error = result.unwrap_err().to_string();
        assert!(error.starts_with("Failed to rename"), "{error}");
        assert!(error.contains("Permission denied"), "{error}");
        assert!(a.exists());
    }

    #[test]
    fn rename_refuses_a_name_that_leaves_the_directory() {
        let dir = TempDir::new("ops_rename_escape");
        fs::create_dir(dir.join("sub")).unwrap();
        let a = dir.join("sub").join("a.txt");
        fs::write(&a, b"a").unwrap();
        let info = PathInfo::try_from(a.as_path()).unwrap();

        // Joined onto the parent unvalidated, this renames the file into the
        // directory above the one on screen.
        let error = rename(&info, "../escaped.txt")
            .expect_err("a name that is not a basename must be refused")
            .to_string();

        assert!(error.starts_with("Cannot rename"), "{error}");
        assert!(error.ends_with("New name cannot contain '/'"), "{error}");
        assert!(a.exists());
        assert!(!dir.join("escaped.txt").exists());
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

    /// Two hard links whose names differ only in case, which only a
    /// case-sensitive filesystem can hold. Renaming one onto the other would
    /// change nothing, like any other pair of hard links.
    #[cfg(target_os = "linux")]
    #[test]
    fn rename_refuses_a_hard_link_whose_name_differs_only_in_case() {
        let dir = TempDir::new("ops_case_hard_link");
        let upper = dir.join("Foo");
        fs::write(&upper, b"x").unwrap();
        fs::hard_link(&upper, dir.join("foo")).unwrap();

        let error = rename(&PathInfo::try_from(upper.as_path()).unwrap(), "foo")
            .unwrap_err()
            .to_string();

        assert!(error.ends_with("both names are the same file"), "{error}");
        assert!(upper.exists());
        assert!(dir.join("foo").exists());
    }

    /// macOS only: the default APFS volume is case-insensitive, where the new
    /// spelling resolves to the entry itself and the rename changes the name.
    #[cfg(target_os = "macos")]
    #[test]
    fn rename_changes_only_the_case_of_a_name_on_a_case_insensitive_filesystem() {
        let dir = TempDir::new("ops_case_change");
        let lower = dir.join("a.txt");
        fs::write(&lower, b"a").unwrap();

        rename(&PathInfo::try_from(lower.as_path()).unwrap(), "A.TXT").unwrap();

        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(vec![OsString::from("A.TXT")], names);
    }

    // ── chmod ───────────────────────────────────────────────────────────────

    fn mode_of(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
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

    #[test]
    fn chmod_refuses_a_symlink_and_leaves_its_target_alone() {
        let dir = TempDir::new("ops_chmod_symlink");
        let target = dir.join("target.txt");
        fs::write(&target, b"x").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let info = PathInfo::try_from(link.as_path()).unwrap();

        // `set_permissions` follows the link, so without the refusal this
        // succeeds and makes the target world-writable.
        let error = chmod(&info, 0o777)
            .expect_err("a symlink must be refused")
            .to_string();

        assert!(error.starts_with("Cannot chmod"), "{error}");
        assert!(error.ends_with("it is a symlink"), "{error}");
        assert_eq!(0o600, mode_of(&target));
    }

    /// The check in `chmod` runs before the mode is set, so a path swapped for
    /// a symlink in between reaches this step as a symlink. Linux refuses to
    /// set a symlink's own mode; macOS sets it. Either way the target is left
    /// alone.
    #[test]
    fn setting_the_mode_does_not_follow_a_symlink_that_passed_the_check() {
        let dir = TempDir::new("ops_chmod_swapped");
        let target = dir.join("target.txt");
        fs::write(&target, b"x").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let result = set_mode_without_following(&link, 0o777);

        assert_eq!(0o600, mode_of(&target));
        if cfg!(target_os = "linux") {
            let error = result.expect_err("a symlink must be refused").to_string();
            assert!(error.ends_with("it is a symlink"), "{error}");
        }
    }

    #[test]
    fn setting_the_mode_changes_a_regular_file() {
        let dir = TempDir::new("ops_chmod_plain");
        let file = dir.join("a.txt");
        fs::write(&file, b"a").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();

        set_mode_without_following(&file, 0o640).unwrap();

        assert_eq!(0o640, mode_of(&file));
    }

    #[test]
    fn setting_the_mode_reports_a_failure_it_did_not_decide() {
        let dir = TempDir::new("ops_chmod_gone");
        let file = dir.join("a.txt");

        let error = set_mode_without_following(&file, 0o600)
            .unwrap_err()
            .to_string();

        assert!(error.starts_with("Failed to chmod"), "{error}");
        assert!(error.contains("to 600: "), "{error}");
    }

    #[test]
    fn chmod_reports_an_entry_that_vanished_as_a_failure_rather_than_a_change() {
        let dir = TempDir::new("ops_chmod_vanished");
        let file = dir.join("a.txt");
        fs::write(&file, b"a").unwrap();
        let info = PathInfo::try_from(file.as_path()).unwrap();
        fs::remove_file(&file).unwrap();

        let error = chmod(&info, 0o600).unwrap_err().to_string();

        // The errno is reported, not a refusal filectrl decided.
        assert!(error.starts_with("Failed to chmod"), "{error}");
        assert!(error.contains("No such file"), "{error}");
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
        fs::create_dir(dir.join("taken")).unwrap();

        // `create_dir` rather than `create_dir_all`, so an existing directory
        // is an error instead of silently adopted.
        assert!(create_directory(&parent, "taken").is_err());
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

    #[test_case("" => "Bookmark name cannot be empty" ; "empty")]
    #[test_case("   " => "Bookmark name cannot be empty" ; "only whitespace, which trims to empty")]
    #[test_case("nested/name" => "Bookmark name cannot contain '/'" ; "a path rather than a name")]
    fn add_bookmark_refuses_a_name_that_is_not_a_basename(name: &str) -> String {
        let base = TempDir::new("ops_bookmark_bad_name");
        let bookmarks = base.join("bookmarks");
        let target = PathInfo::try_from(base.path()).unwrap();

        // An empty name resolves to the bookmarks directory and a nested one
        // to a missing parent, so both fail later on anyway: the message is
        // what says the name was refused rather than the symlink call failing.
        add_bookmark(&bookmarks, &target, name)
            .expect_err("a name that is not a basename must be refused")
            .to_string()
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

    // ── launching and listing ───────────────────────────────────────────────

    #[test]
    fn spawn_argv_refuses_a_working_directory_that_is_not_one() {
        let dir = TempDir::new("ops_spawn_cwd");
        let missing = dir.join("missing");
        let (tx, _rx) = std::sync::mpsc::channel();

        // `spawn` would fail with the same ENOENT as a missing program, which
        // would send the user looking for the wrong thing.
        let error = spawn_argv(Some(&missing), "App", &[OsString::from("true")], tx)
            .expect_err("a missing working directory must be refused")
            .to_string();

        assert!(error.starts_with("Cannot run \"App\""), "{error}");
        assert!(error.ends_with("is not a directory"), "{error}");
    }

    #[test_case("false" => Some("\"false\" failed (exit code 1)".to_string()) ; "a failure is reported")]
    #[test_case("true" => None ; "a success is not")]
    fn a_program_that_exits_at_once_is_reported_only_when_it_failed(
        program: &str,
    ) -> Option<String> {
        let (tx, rx) = std::sync::mpsc::channel();

        spawn_argv(None, program, &[OsString::from(program)], tx).unwrap();

        // The watcher thread holds the only sender, so this returns once it
        // has either reported or dropped it.
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Command::AlertError(message)) => Some(message),
            Ok(other) => panic!("unexpected command {other:?}"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
            Err(error) => panic!("the watcher never finished: {error}"),
        }
    }

    /// Linux only: the group is read from `/proc`.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_launched_program_runs_in_a_process_group_of_its_own() {
        let mut child = detached_command("sleep", ["10"]).spawn().unwrap();
        let stat = fs::read_to_string(format!("/proc/{}/stat", child.id()));
        let _ = child.kill();
        let _ = child.wait();

        // After the parenthesised command name: state, parent, then group.
        let stat = stat.unwrap();
        let group: u32 = stat
            .rsplit_once(')')
            .and_then(|(_, fields)| fields.split_whitespace().nth(2))
            .and_then(|group| group.parse().ok())
            .unwrap_or_else(|| panic!("unexpected /proc stat line {stat:?}"));
        assert_eq!(child.id(), group);
    }

    #[test]
    fn an_unreadable_directory_still_completes_its_load() {
        let dir = TempDir::new("ops_stream_missing");
        let mut missing = PathInfo::try_from(dir.path()).unwrap();
        missing.path = dir.join("missing");
        let (tx, rx) = std::sync::mpsc::channel();

        stream_cd(missing, 7, tx, CancellationToken::new());

        // Without the completion the listing would stay in its loading state.
        let commands: Vec<Command> = rx.iter().collect();
        let [
            Command::AlertWarn(message),
            Command::DirectoryListingComplete { generation: 7 },
        ] = commands.as_slice()
        else {
            panic!("expected a warning and the completion, got {commands:?}");
        };
        assert!(message.starts_with("Failed to read directory"), "{message}");
    }

    #[test]
    fn a_superseded_load_sends_nothing() {
        let dir = TempDir::new("ops_stream_cancelled");
        fs::write(dir.join("a.txt"), b"a").unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (tx, rx) = std::sync::mpsc::channel();

        stream_cd(PathInfo::try_from(dir.path()).unwrap(), 7, tx, cancel);

        // The newer load owns the listing, so neither this one's entries nor
        // its completion may reach it.
        let commands: Vec<Command> = rx.iter().collect();
        assert!(commands.is_empty(), "{commands:?}");
    }
}
