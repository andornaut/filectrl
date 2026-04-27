use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::mpsc::Sender,
    thread,
    time::Duration,
};

use anyhow::{anyhow, Result};
use log::{info, warn};

use super::path_info::PathInfo;
use crate::command::Command;

pub(super) fn cd(directory: &PathInfo) -> Result<(Vec<PathInfo>, usize)> {
    info!("Changing directory to {directory:?}");
    let entries = fs::read_dir(&directory.path)?;

    // Use collect to gather results, then partition into successes and failures
    let results: Vec<Result<PathInfo>> = entries
        .map(|entry| {
            entry
                .map_err(Into::into)
                .and_then(|e| PathInfo::try_from(&e.path()))
        })
        .collect();

    let (children, errors): (Vec<_>, Vec<_>) = results.into_iter().partition(Result::is_ok);

    let error_count = errors.len();
    if error_count > 0 {
        warn!("Some paths could not be read: {:?}", errors);
    }

    Ok((children.into_iter().flatten().collect(), error_count))
}

pub(super) fn open_in(path: &PathInfo, template: &str, command_tx: Sender<Command>) -> Result<()> {
    info!("Opening \"{path:?}\" using template: \"{template}\"");
    if template.is_empty() {
        return Ok(());
    }
    let command = template.replace("%s", &shell_words::quote(&path.path));
    let mut child = spawn_detached("sh", ["-c", &command])
        .map_err(|error| anyhow!("Failed to run command \"{command}\": {error}"))?;

    // Catch commands that fail immediately (e.g. binary not found) without
    // blocking the TUI. Long-lived processes (e.g. a terminal window) will
    // still be running after 250ms and are silently ignored.
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(250));
        if let Ok(Some(status)) = child.try_wait() {
            if !status.success() {
                let code = status
                    .code()
                    .map_or("unknown".to_string(), |c| c.to_string());
                let _ = command_tx.send(Command::AlertError(format!(
                    "Command \"{command}\" failed (exit code {code})"
                )));
            }
        }
    });

    Ok(())
}

pub(super) fn rename(path: &PathInfo, new_basename: &str) -> Result<()> {
    let old_path = path.as_path();
    let new_path = join_parent(old_path, new_basename);
    info!("Renaming {old_path:?} to {new_path:?}");
    if old_path != new_path {
        fs::rename(old_path, new_path)?;
    }
    Ok(())
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
        let result = join_parent(&old_path, right);

        assert_eq!(expected, result.to_string_lossy());
    }
}
