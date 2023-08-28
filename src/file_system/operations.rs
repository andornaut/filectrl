use super::human::HumanPath;
use anyhow::{anyhow, Result};
use std::ffi::OsStr;
use std::process::Stdio;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn cd(directory: &HumanPath) -> Result<Vec<HumanPath>> {
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
    if let Some(template) = template {
        if !template.contains("%s") {
            // TODO: Early up this validation
            return Err(anyhow!("Invalid program template: {template}"));
        }
        let command = template.replace("%s", path);

        let mut it: std::str::SplitWhitespace<'_> = command.split_whitespace();

        it.next().map_or_else(
            || Ok(()),
            |program| {
                let args: Vec<_> = it.collect();
                if args.len() == 0 {
                    return Err(anyhow!("Invalid program template: {template}"));
                }
                run_detached(program, args).map_or_else(
                    |error| Err(anyhow!("Failed to open program \"{command}\": {error}")),
                    |_| Ok(()),
                )
            },
        )
    } else {
        Ok(())
    }
}

pub(super) fn copy(old_path: &HumanPath, new_dir: &HumanPath) -> Result<()> {
    let new_path = Path::new(&new_dir.path).join(&old_path.basename);
    let old_path = Path::new(&old_path.path);
    fs::copy(old_path, new_path)?;
    Ok(())
}

pub(super) fn mv(old_path: &HumanPath, new_dir: &HumanPath) -> Result<()> {
    let new_path = Path::new(&new_dir.path).join(&old_path.basename);
    let old_path = Path::new(&old_path.path);
    fs::rename(old_path, new_path)?;
    Ok(())
}

pub(super) fn rename(old_path: &HumanPath, new_basename: &str) -> Result<()> {
    let old_path = Path::new(&old_path.path);
    let new_path = join_parent(old_path, new_basename);
    fs::rename(old_path, new_path)?;
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
}
