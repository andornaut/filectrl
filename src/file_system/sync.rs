use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{anyhow, Result};
use log::{info, warn};

use super::path_info::PathInfo;

pub(super) fn cd(directory: &PathInfo) -> Result<Vec<PathInfo>> {
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

    if !errors.is_empty() {
        warn!("Some paths could not be read: {:?}", errors);
    }

    Ok(children.into_iter().flatten().collect())
}

pub(super) fn open_in(path: &PathInfo, template: &String) -> Result<()> {
    info!("Opening the program defined in template:\"{template}\", %s:\"{path:?}\"");
    let mut it = template.split_whitespace();

    let Some(program) = it.next() else {
        return Ok(());
    };

    let args = it.map(|arg| arg.replace("%s", &path.path));
    run_detached(program, args)
        .map_err(|error| anyhow!("Failed to open program \"{program}\": {error}"))
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
        .spawn()?;

    Ok(())
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
