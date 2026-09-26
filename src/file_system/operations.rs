use std::{
    ffi::{OsStr, OsString},
    fs,
    io::ErrorKind,
    os::unix::process::{CommandExt, ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, ExitStatus, Stdio},
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
    tasks::{is_same_file, rename_no_replace, restat, set_mode_at},
};
use crate::{
    command::{Command, progress::CancellationToken},
    visible,
};

const CD_BATCH_SIZE: usize = 256;

/// Streams `directory`'s entries from a background thread as `ListingBatch`es,
/// ending with `DirectoryListingComplete`, all tagged with `generation`.
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
            // Superseded: the newer load owns the listing, so send no completion.
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
            match listed_entry(&path) {
                Ok(Some(info)) => {
                    if !batcher.push(info, &send) {
                        return; // channel closed
                    }
                }
                Ok(None) => {}
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
            let entries = if error_count == 1 { "entry" } else { "entries" };
            let _ = tx.send(Command::AlertWarn(format!(
                "{error_count} {entries} in {} could not be read",
                compact(&directory.path)
            )));
        }
        let _ = tx.send(Command::DirectoryListingComplete { generation });
    });
}

/// The entry a listing found at `path`, or `None` if it is gone by now, like
/// `rm -f`.
fn listed_entry(path: &Path) -> Result<Option<PathInfo>> {
    match PathInfo::try_from(path) {
        Ok(info) => Ok(Some(info)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Runs the opener `template` names on `path`. Failures name the `openers`
/// setting `key`, since a template's first word need not be what failed.
pub(super) fn open_in(
    key: &str,
    path: &PathInfo,
    template: &str,
    command_tx: Sender<Command>,
) -> Result<()> {
    info!("Opening \"{path:?}\" using template: \"{template}\"");
    let setting = opener_setting(key);
    if template.trim().is_empty() {
        return Err(anyhow!(
            "Cannot open {}: {setting} is empty",
            compact(&path.path)
        ));
    }
    let argv = shell::command(template, [path.path.as_os_str().to_os_string()]);
    let failure = run_failure(&setting, &path.path);
    let child = detached_command(&argv[0], &argv[1..])
        .spawn()
        .map_err(|error| anyhow!("{failure}: {error}"))?;
    watch_for_immediate_failure(child, failure, command_tx);
    Ok(())
}

/// The config setting of the opener `key`.
pub(crate) fn opener_setting(key: &str) -> String {
    format!("openers.{key}")
}

/// A program's name as a failure names it, quoted. Not `{:?}`: the name is
/// already shown text, and `visible` would not escape it a second time.
pub(crate) fn quoted_program(name: &str) -> String {
    format!("\"{}\"", visible(name))
}

/// `Failed to run "<program>" on <path>`, the start of every launch failure.
pub(crate) fn failure_prefix(program: &str, path: &Path) -> String {
    run_failure(&quoted_program(program), path)
}

fn run_failure(shown: &str, path: &Path) -> String {
    format!("Failed to run {shown} on {}", compact(path))
}

/// Why a program did not succeed: its exit code, or the signal that ended it.
pub(crate) fn exit_cause(status: ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exit code {code}"),
        (None, Some(signal)) => format!("killed by signal {signal}"),
        (None, None) => status.to_string(),
    }
}

/// Launches `argv` without a shell, so nothing in a file name is reinterpreted.
/// `label` is already rendered as a failure shows it.
pub(super) fn spawn_argv(
    label: &str,
    path: &Path,
    argv: &[OsString],
    command_tx: Sender<Command>,
) -> Result<()> {
    info!("Opening {label} using: {argv:?}");
    let Some((program, rest)) = argv.split_first() else {
        return Ok(());
    };
    let failure = run_failure(label, path);
    let child = detached_command(program, rest)
        .spawn()
        .map_err(|error| anyhow!("{failure}: {error}"))?;
    watch_for_immediate_failure(child, failure, command_tx);
    Ok(())
}

/// Reports a command that fails within 250ms (e.g. binary not found) without
/// blocking the TUI.
fn watch_for_immediate_failure(mut child: Child, failure: String, command_tx: Sender<Command>) {
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(250));
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let _ = command_tx.send(Command::AlertError(format!(
                        "{failure}: {}",
                        exit_cause(status)
                    )));
                }
            }
            // Still running: wait here so it is reaped rather than left as a zombie.
            _ => {
                let _ = child.wait();
            }
        }
    });
}

/// Refuses a symlink: `chmod(2)` follows the link, and Linux has no `lchmod`.
pub(super) fn chmod(path: &PathInfo, mode: u32) -> Result<()> {
    let p = path.as_path();
    if restat(path, "chmod")?.is_symlink() {
        return Err(symlink_refusal(p));
    }
    info!("Changing mode of {} to {mode:o}", p.display());
    set_mode_without_following(p, mode)
}

/// Sets the mode without following a symlink, so a path swapped for one after
/// the check is refused.
fn set_mode_without_following(p: &Path, mode: u32) -> Result<()> {
    match set_mode_at(nix::fcntl::AT_FDCWD, p, mode) {
        Ok(()) => Ok(()),
        Err(error)
            if error.raw_os_error() == Some(nix::libc::EOPNOTSUPP) && is_symlink(p, mode)? =>
        {
            Err(symlink_refusal(p))
        }
        Err(error) => Err(chmod_failure(p, mode, &error)),
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

/// Takes the bookmarks directory so writes resolve against the directory
/// `read_bookmarks` reads.
pub(super) fn add_bookmark(dir: &Path, target: &PathInfo, name: &str) -> Result<()> {
    let refuse = |reason: &dyn std::fmt::Display| anyhow!("Cannot add bookmark {name:?}: {reason}");
    let fail = |error: &dyn std::fmt::Display| anyhow!("Failed to add bookmark {name:?}: {error}");
    validate_basename(name).map_err(|reason| refuse(&reason))?;
    fs::create_dir_all(dir).map_err(|error| fail(&error))?;
    let link = dir.join(name);
    info!(
        "Creating bookmark {} -> {}",
        link.display(),
        target.path.display()
    );
    // `symlink(2)` refuses any entry at the name, a broken symlink included.
    std::os::unix::fs::symlink(&target.path, &link).map_err(|error| {
        if error.kind() == ErrorKind::AlreadyExists {
            refuse(&"it already exists")
        } else {
            fail(&error)
        }
    })
}

pub(super) fn create_directory(parent: &PathInfo, name: &str) -> Result<()> {
    validate_basename(name)
        .map_err(|reason| anyhow!("Cannot create directory {name:?}: {reason}"))?;
    let path = parent.as_path().join(name);
    info!("Creating directory {}", path.display());
    fs::create_dir(&path).map_err(|error| anyhow!("Failed to create directory {name:?}: {error}"))
}

/// Rejects a name that is not a single path component. `Path::join` discards
/// the base for an absolute path, so this keeps entries in the directory shown.
fn validate_basename(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a name cannot be empty".into());
    }
    if name == "." || name == ".." {
        return Err(format!("a name cannot be {name:?}"));
    }
    if name.contains(std::path::MAIN_SEPARATOR) {
        return Err(format!(
            "a name cannot contain {:?}",
            std::path::MAIN_SEPARATOR
        ));
    }
    Ok(())
}

/// Renames `path` to `new_basename` in the same directory, never replacing an
/// existing entry.
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
    validate_basename(new_basename).map_err(|reason| refuse(&reason))?;
    let new_path = join_parent(old_path, new_basename);
    if old_path == new_path {
        return Ok(());
    }
    info!("Renaming {} to {}", old_path.display(), new_path.display());
    match rename_no_replace(old_path, &new_path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Err(refuse(&"it no longer exists")),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            if !is_same_file(old_path, &new_path) {
                return Err(refuse(&format_args!(
                    "{} already exists",
                    compact(&new_path)
                )));
            }
            // Same file. A case-only change is a real rename on a case-insensitive
            // filesystem, so it is let through; a rename onto another hard link is a POSIX
            // no-op, so it is refused.
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

fn is_case_only_change(a: &Path, b: &Path) -> bool {
    match (a.file_name(), b.file_name()) {
        (Some(a), Some(b)) => {
            a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
        }
        _ => false,
    }
}

/// The child gets its own process group, outside the terminal's foreground
/// group: it misses the terminal's hangup, and is stopped if it touches the
/// terminal.
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
    crate::app::events::unblock_in_child(&mut command);
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

        assert!(error.contains("no longer exists"), "{error}");
    }

    #[test]
    fn a_listed_entry_that_vanished_is_left_out_rather_than_unreadable() {
        let dir = TempDir::new("ops_listed_vanished");
        let there = dir.join("there");
        fs::write(&there, b"x").unwrap();
        let locked = dir.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("inside"), b"x").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = listed_entry(&locked.join("inside"));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            Some(there.clone()),
            listed_entry(&there).unwrap().map(|info| info.path)
        );
        assert!(listed_entry(&dir.join("gone")).unwrap().is_none());
        if nix::unistd::geteuid().is_root() {
            eprintln!("skipped: root reads a mode-000 directory");
        } else {
            assert!(unreadable.is_err(), "{unreadable:?}");
        }
    }

    #[test_case("/b", "/a", "b"; "a sibling of a top-level entry")]
    #[test_case("/a/b", "/a/aa", "b"; "a sibling of a nested entry")]
    #[test_case("b", "", "b"; "a path with no parent")]
    fn join_parent_names_a_sibling_of(expected: &str, left: &str, right: &str) {
        let old_path = Path::new(left);
        let result = join_parent(old_path, right);

        assert_eq!(expected, result.to_string_lossy());
    }

    /// Whether `dir` is still empty: a name that escaped validation creates
    /// something.
    fn is_empty(dir: &TempDir) -> bool {
        fs::read_dir(dir.path()).unwrap().next().is_none()
    }

    #[test_case("" => "Cannot create directory \"\": a name cannot be empty" ; "empty")]
    #[test_case("." => "Cannot create directory \".\": a name cannot be \".\"" ; "current directory")]
    #[test_case(".." => "Cannot create directory \"..\": a name cannot be \"..\"" ; "parent directory")]
    #[test_case("nested/name" => "Cannot create directory \"nested/name\": a name cannot contain '/'" ; "relative path")]
    fn create_directory_rejects_a_name_that_is_not_a_basename(name: &str) -> String {
        let dir = TempDir::new("ops_create");
        let parent = PathInfo::try_from(dir.path()).unwrap();

        // Each name also fails at `create_dir`, so the message is what is asserted.
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
        // A reserved path, so its absence is this call's doing.
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
        let error = rename(&info, "b.txt")
            .expect_err("an existing destination must be refused")
            .to_string();
        assert!(error.ends_with("already exists"), "{error}");
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
        // Root reaches through a mode-000 directory; probe rather than check the euid.
        let is_unreachable = a.symlink_metadata().is_err();

        let result = rename(&listed, "b.txt");
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();

        if !is_unreachable {
            return;
        }
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

        let error = rename(&info, "../escaped.txt")
            .expect_err("a name that is not a basename must be refused")
            .to_string();

        assert!(error.starts_with("Cannot rename"), "{error}");
        assert!(
            error.ends_with("to \"../escaped.txt\": a name cannot contain '/'"),
            "{error}"
        );
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
        let error = rename(&info, "b.txt").unwrap_err().to_string();
        assert!(error.contains("same file"), "unexpected error: {error}");
        assert!(a.exists());
        assert!(b.exists());
    }

    /// Only a case-sensitive filesystem can hold both names.
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

    /// The default APFS volume is case-insensitive.
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

        let error = chmod(&info, 0o777)
            .expect_err("a symlink must be refused")
            .to_string();

        assert!(error.starts_with("Cannot chmod"), "{error}");
        assert!(error.ends_with("it is a symlink"), "{error}");
        assert_eq!(0o600, mode_of(&target));
    }

    /// Linux refuses to set a symlink's own mode; macOS sets it. Either way the
    /// target is left alone.
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

        assert!(error.starts_with("Failed to chmod"), "{error}");
        assert!(error.contains("No such file"), "{error}");
    }

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

        let error = create_directory(&parent, "taken")
            .expect_err("an existing directory must be refused")
            .to_string();
        assert_eq!(
            "Failed to create directory \"taken\": File exists (os error 17)",
            error
        );
    }

    #[test]
    fn add_bookmark_symlinks_the_named_directory() {
        let base = TempDir::new("ops_bookmark");
        let bookmarks = base.join("bookmarks");
        let target = PathInfo::try_from(base.path()).unwrap();

        add_bookmark(&bookmarks, &target, "favs").unwrap();

        assert_eq!(base.path(), fs::read_link(bookmarks.join("favs")).unwrap());
    }

    #[test_case("" => "Cannot add bookmark \"\": a name cannot be empty" ; "empty")]
    #[test_case("nested/name" => "Cannot add bookmark \"nested/name\": a name cannot contain '/'" ; "a path rather than a name")]
    fn add_bookmark_refuses_a_name_that_is_not_a_basename(name: &str) -> String {
        let base = TempDir::new("ops_bookmark_bad_name");
        let bookmarks = base.join("bookmarks");
        let target = PathInfo::try_from(base.path()).unwrap();

        // Both also fail later, so the message is what shows the name was refused.
        add_bookmark(&bookmarks, &target, name)
            .expect_err("a name that is not a basename must be refused")
            .to_string()
    }

    #[test]
    fn add_bookmark_refuses_an_absolute_name() {
        let base = TempDir::new("ops_bookmark_absolute");
        let bookmarks = base.join("bookmarks");
        let target = PathInfo::try_from(base.path()).unwrap();
        // A reserved path, so its absence is this call's doing.
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

        assert_eq!("Cannot add bookmark \"favs\": it already exists", error);
    }

    #[test]
    fn add_bookmark_names_the_bookmark_when_its_directory_cannot_be_created() {
        let base = TempDir::new("ops_bookmark_no_dir");
        // A file where the directory should be, which `create_dir_all` refuses even for root.
        let bookmarks = base.join("bookmarks");
        fs::write(&bookmarks, b"").unwrap();
        let target = PathInfo::try_from(base.path()).unwrap();

        let error = add_bookmark(&bookmarks, &target, "favs")
            .expect_err("a bookmarks directory that is a file must fail")
            .to_string();

        assert_eq!(
            "Failed to add bookmark \"favs\": File exists (os error 17)",
            error
        );
    }

    #[test]
    fn add_bookmark_refuses_a_name_held_by_a_broken_symlink() {
        let base = TempDir::new("ops_bookmark_broken");
        let bookmarks = base.join("bookmarks");
        fs::create_dir_all(&bookmarks).unwrap();
        std::os::unix::fs::symlink(base.join("gone"), bookmarks.join("favs")).unwrap();
        let target = PathInfo::try_from(base.path()).unwrap();

        let error = add_bookmark(&bookmarks, &target, "favs")
            .expect_err("a name held by a broken symlink must be refused")
            .to_string();

        assert_eq!("Cannot add bookmark \"favs\": it already exists", error);
    }

    #[test_case(&["false"] => Some("Failed to run \"App\" on \"/f\": exit code 1".to_string()) ; "a failure is reported")]
    #[test_case(&["sh", "-c", "kill -KILL $$"] => Some("Failed to run \"App\" on \"/f\": killed by signal 9".to_string()) ; "a death by signal is named")]
    #[test_case(&["true"] => None ; "a success is not")]
    fn a_program_that_exits_at_once_is_reported_only_when_it_failed(
        argv: &[&str],
    ) -> Option<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let argv: Vec<OsString> = argv.iter().map(OsString::from).collect();

        spawn_argv("\"App\"", Path::new("/f"), &argv, tx).unwrap();

        // The watcher thread holds the only sender.
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Command::AlertError(message)) => Some(message),
            Ok(other) => panic!("unexpected command {other:?}"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
            Err(error) => panic!("the watcher never finished: {error}"),
        }
    }

    #[test_case("Text\\u{2063} Editor" => "Failed to run \"Text\\u{2063} Editor\" on \"/f\"" ; "a shown name is kept as it is")]
    #[test_case("Text\u{2063} Editor" => "Failed to run \"Text\\u{2063} Editor\" on \"/f\"" ; "a raw name is escaped once")]
    fn failure_prefix_names(program: &str) -> String {
        failure_prefix(program, Path::new("/f"))
    }

    #[test]
    fn an_opener_template_is_reported_by_its_setting() {
        let (tx, rx) = std::sync::mpsc::channel();
        let path = PathInfo::try_from(Path::new("/")).unwrap();

        open_in("open_directory", &path, "cd %s && false", tx).unwrap();

        let Ok(Command::AlertError(message)) = rx.recv_timeout(Duration::from_secs(5)) else {
            panic!("expected an alert");
        };
        assert_eq!(
            "Failed to run openers.open_directory on \"/\": exit code 1",
            message
        );
    }

    #[test_case("" ; "empty")]
    #[test_case("  " ; "blank")]
    fn an_empty_opener_is_refused_by_its_key(template: &str) {
        let dir = TempDir::new("ops_empty_opener");
        let file = dir.join("notes.txt");
        fs::write(&file, "").unwrap();
        let path = PathInfo::try_from(&file).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();

        let error = open_in("open_file", &path, template, tx)
            .expect_err("an empty template must be refused")
            .to_string();

        assert_eq!(
            format!("Cannot open {}: openers.open_file is empty", compact(&file)),
            error
        );
    }

    /// The signals are blocked on this test's thread only, as filectrl blocks
    /// them for the whole process.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_launched_program_receives_the_signals_filectrl_blocks() {
        let mut blocked = nix::sys::signal::SigSet::empty();
        blocked.add(nix::sys::signal::Signal::SIGTERM);
        blocked.thread_block().unwrap();
        let spawned = detached_command("sleep", ["10"]).spawn();
        blocked.thread_unblock().unwrap();
        let mut child = spawned.unwrap();
        let status = fs::read_to_string(format!("/proc/{}/status", child.id()));
        let _ = child.kill();
        let _ = child.wait();

        let status = status.unwrap();
        assert!(status.contains("SigBlk:\t0000000000000000\n"), "{status}");
    }

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

        let commands: Vec<Command> = rx.iter().collect();
        assert!(commands.is_empty(), "{commands:?}");
    }
}
