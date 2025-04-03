use anyhow::{anyhow, Result};
use log::{info, warn};

use std::{
    cmp::max,
    ffi::OsStr,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::mpsc::Sender,
    thread,
};

use super::{handler::TaskCommand, human::HumanPath};
use crate::command::{task::Task, Command};

const MAX_BUFFER_BYTES: u64 = 64_000_000;
const MIN_DYNAMIC_BUFFER_BYTES: u64 = 64_000;

pub(super) fn cd(directory: &HumanPath) -> Result<Vec<HumanPath>> {
    info!("Changing directory to {directory:?}");
    let entries = fs::read_dir(&directory.path)?;
    let (children, errors): (Vec<_>, Vec<_>) = entries
        .map(|entry| -> Result<HumanPath> { HumanPath::try_from(&entry?.path()) })
        .partition(Result::is_ok);
    if !errors.is_empty() {
        return Err(anyhow!("Some paths could not be read: {:?}", errors));
    }
    Ok(children.into_iter().map(Result::unwrap).collect())
}

pub(super) fn delete(path: &HumanPath) -> Result<()> {
    info!("Deleting {path:?}");
    let pathname = &path.path;
    if path.is_directory() {
        fs::remove_dir_all(pathname)?;
    } else {
        // File or Symlink
        fs::remove_file(pathname)?;
    }
    Ok(())
}

pub(super) fn open_in(template: Option<String>, path: &str) -> Result<()> {
    match template {
        Some(template) => {
            info!("Opening the program defined in template:\"{template}\", %s:\"{path}\"");
            let mut it: std::str::SplitWhitespace<'_> = template.split_whitespace();

            it.next().map_or_else(
                || Ok(()),
                |program| {
                    let args = it.map(|arg| arg.replace("%s", path));
                    run_detached(program, args).map_or_else(
                        |error| Err(anyhow!("Failed to open program \"{program}\": {error}")),
                        |_| Ok(()),
                    )
                },
            )
        }
        None => {
            warn!("Cannot open the program, because a template is not configured");
            Ok(())
        }
    }
}

pub(super) fn mv(path: &HumanPath, dir: &HumanPath) -> Result<()> {
    let new_path = Path::new(&dir.path).join(&path.basename);
    let old_path = Path::new(&path.path);
    info!("Moving {old_path:?} to {new_path:?}");
    if old_path != new_path {
        fs::rename(old_path, new_path)?;
    }
    Ok(())
}

pub(super) fn rename(old_path: &HumanPath, new_basename: &str) -> Result<()> {
    let old_path = Path::new(&old_path.path);
    let new_path = join_parent(old_path, new_basename);
    info!("Renaming {old_path:?} to {new_path:?}");
    if old_path != new_path {
        fs::rename(old_path, new_path)?;
    }
    Ok(())
}

pub(super) fn run_task(tx: Sender<Command>, task_command: TaskCommand) -> Result<Task> {
    match task_command {
        TaskCommand::Copy(path, dir) => {
            let mut buffer = vec![0; buffer_bytes(path.size)];
            let old_path = PathBuf::from(&path.path);
            let new_path = Path::new(&dir.path).join(&path.basename);
            if old_path == new_path {
                return Err(anyhow!("Cannot copy {old_path:?} to {new_path:?}: Source and destination paths must be different"));
            }
            info!("Copying {old_path:?} to {new_path:?}");
            let mut task = Task::new(path.size);
            let original_task = task.clone();

            thread::spawn(move || match open_files(&old_path, &new_path) {
                Err(error) => {
                    task.error(format!(
                        "Failed to copy {old_path:?} to {new_path:?}: {error}"
                    ));
                    tx.send(Command::Progress(task)).expect("Can send command");
                    return;
                }
                Ok((old_file, new_file)) => {
                    let mut reader = BufReader::new(old_file);
                    let mut writer = BufWriter::new(new_file);
                    loop {
                        match reader.read(&mut buffer) {
                            Err(error) => {
                                task.error(format!("Failed to read {old_path:?}: {error}"))
                            }
                            Ok(0) => break,
                            Ok(bytes) => match writer.write_all(&buffer[..bytes]) {
                                Err(error) => {
                                    task.error(format!("Failed to write {new_path:?}: {error}"))
                                }
                                Ok(()) => task.increment(bytes as u64),
                            },
                        }
                        tx.send(Command::Progress(task.clone()))
                            .expect("Can send command");
                        if task.is_done() {
                            break;
                        }
                    }
                }
            });

            // Must return a task with `status==New` for the logic in `StatusView.update_task()` to work
            Ok(original_task)
        }
    }
}

fn open_files(source: &PathBuf, target: &PathBuf) -> Result<(File, File)> {
    let source = File::open(&source)?;
    // `File::create` will truncate a file if it already exist, which may take a few seconds
    let target = File::create(&target)?;
    Ok((source, target))
}

fn run_detached<I, S>(program: &str, args: I) -> Result<()>
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
        .map(|_| ())
        .map_err(|error| error.into())
}

fn join_parent(left: &Path, right: &str) -> PathBuf {
    match left.parent() {
        Some(parent) => parent.join(right),
        None => PathBuf::from(right),
    }
}

fn buffer_bytes(len: u64) -> usize {
    let bytes = if len <= MIN_DYNAMIC_BUFFER_BYTES {
        len
    } else if len >= (MAX_BUFFER_BYTES * 10) {
        MAX_BUFFER_BYTES
    } else {
        max(MIN_DYNAMIC_BUFFER_BYTES, len / 10)
    };
    bytes as usize
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
    fn join_is_correct(expected: &str, left: &str, right: &str) {
        let old_path = Path::new(left);
        let result = join_parent(&old_path, right);

        assert_eq!(expected, result.to_string_lossy());
    }

    #[test_case(64_000_000, 1_000_000_000_000; "1tb")]
    #[test_case(64_000_000, 1_000_000_000; "1gb")]
    #[test_case(10_000_000, 100_000_000; "100mb")]
    #[test_case(1_000_000, 10_000_000; "10mb")]
    #[test_case(200_000, 2_000_000; "2mb")]
    #[test_case(64_000, 640_000; "640k")]
    #[test_case(64_000, 320_000; "320k")]
    #[test_case(64_000, 64_000; "min")]
    #[test_case(63_999, 63_999; "below min")]
    fn buffer_size_is_correct(expected: usize, len: u64) {
        let result = buffer_bytes(len);

        assert_eq!(expected, result);
    }
}
