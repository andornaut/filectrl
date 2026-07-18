use std::{
    ffi::OsStr,
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
    path_info::PathInfo,
    stream::{BATCH_FLUSH_INTERVAL, Batcher},
};
use crate::{
    app::config::Config,
    command::{Command, progress::CancellationToken},
};

const CD_BATCH_SIZE: usize = 256;

/// Spawns a background thread that reads `directory` and streams its entries as
/// `Command::DirectoryListing` batches, finishing with a
/// `Command::DirectoryListingComplete`. `generation` tags every message so a
/// superseded load (the user navigated away) can be ignored; `cancel` stops the
/// walk early when that happens. Reading off the UI thread keeps navigation into
/// very large directories responsive.
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
                    "Failed to read directory {:?}: {error}",
                    directory.path
                )));
                let _ = tx.send(Command::DirectoryListingComplete { generation });
                return;
            }
        };

        let send = |items| {
            tx.send(Command::DirectoryListing { items, generation })
                .is_ok()
        };
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
                    warn!("Could not read an entry in {:?}: {error}", directory.path);
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
                    warn!("Could not read metadata for {path:?}: {error}");
                    error_count += 1;
                }
            }
        }

        if !batcher.flush(&send) {
            return;
        }
        if error_count > 0 {
            let _ = tx.send(Command::AlertWarn(format!(
                "{error_count} entries in {:?} could not be read",
                directory.path
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
    let command = template.replace("%s", &shell_words::quote(&path.path.to_string_lossy()));
    let mut child = spawn_detached("sh", ["-c", &command])
        .map_err(|error| anyhow!("Failed to run command \"{command}\": {error}"))?;

    // Catch commands that fail immediately (e.g. binary not found) without
    // blocking the TUI. Long-lived processes (e.g. a terminal window) will
    // still be running after 250ms and are silently ignored.
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(250));
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let code = status
                        .code()
                        .map_or("unknown".to_string(), |c| c.to_string());
                    let _ = command_tx.send(Command::AlertError(format!(
                        "Command \"{command}\" failed (exit code {code})"
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

    Ok(())
}

pub(super) fn chmod(path: &PathInfo, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let p = path.as_path();
    info!("Changing mode of {p:?} to {mode:o}");
    let permissions = fs::Permissions::from_mode(mode);
    fs::set_permissions(p, permissions)?;
    Ok(())
}

pub(super) fn add_bookmark(target: &PathInfo, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow!("Bookmark name cannot be empty"));
    }
    if name.contains(std::path::MAIN_SEPARATOR) {
        return Err(anyhow!(
            "Bookmark name cannot contain {:?}",
            std::path::MAIN_SEPARATOR
        ));
    }
    let dir = Config::global().bookmarks_dir();
    fs::create_dir_all(&dir)?;
    let link = dir.join(name);
    // Reject duplicates, including a pre-existing broken symlink.
    if link.symlink_metadata().is_ok() {
        return Err(anyhow!("A bookmark named {name:?} already exists"));
    }
    info!("Creating bookmark {link:?} -> {:?}", target.path);
    std::os::unix::fs::symlink(&target.path, &link)?;
    Ok(())
}

pub(super) fn create_directory(parent: &PathInfo, name: &str) -> Result<()> {
    let path = parent.as_path().join(name);
    info!("Creating directory {path:?}");
    fs::create_dir(&path)?;
    Ok(())
}

pub(super) fn rename(path: &PathInfo, new_basename: &str) -> Result<()> {
    if new_basename.contains(std::path::MAIN_SEPARATOR) {
        return Err(anyhow!(
            "New name cannot contain {:?}",
            std::path::MAIN_SEPARATOR
        ));
    }
    let old_path = path.as_path();
    let new_path = join_parent(old_path, new_basename);
    info!("Renaming {old_path:?} to {new_path:?}");
    if old_path != new_path {
        // Diagnose a vanished source up front; otherwise an existing
        // destination would be misreported as the problem. Only NotFound
        // means vanished; other errors (e.g. permission denied) must not
        // claim the file is gone.
        if let Err(error) = old_path.symlink_metadata() {
            return Err(if error.kind() == std::io::ErrorKind::NotFound {
                anyhow!("{old_path:?} no longer exists")
            } else {
                anyhow!("Cannot rename {old_path:?}: {error}")
            });
        }
        // Refuse to overwrite: `fs::rename` would silently replace an
        // existing destination.
        if new_path.symlink_metadata().is_ok() {
            if !is_same_file(old_path, &new_path) {
                return Err(anyhow!("{new_path:?} already exists"));
            }
            // Same underlying file. A case-only change of the name is a real
            // rename on a case-insensitive filesystem (where the destination
            // path resolves to the source itself), so let it through.
            // Renaming onto another hard link of the same inode is a POSIX
            // no-op that would silently change nothing, so report it instead.
            if !is_case_only_change(old_path, &new_path) {
                return Err(anyhow!("{old_path:?} and {new_path:?} are the same file"));
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

fn spawn_detached<I, S>(program: &str, args: I) -> Result<Child>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Into::into)
}

fn join_parent(left: &Path, right: &str) -> PathBuf {
    match left.parent() {
        Some(parent) => parent.join(right),
        None => PathBuf::from(right),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

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

    #[test]
    fn rename_refuses_existing_destination() {
        let dir = std::env::temp_dir().join(format!("filectrl_ops_rename_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
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

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_reports_same_file_for_hard_link_destination() {
        let dir =
            std::env::temp_dir().join(format!("filectrl_ops_samefile_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
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

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_allows_case_only_change_to_same_file() {
        let dir =
            std::env::temp_dir().join(format!("filectrl_ops_casechange_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
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

        let _ = fs::remove_dir_all(&dir);
    }
}
