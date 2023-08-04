use super::path::HumanPath;
use anyhow::{anyhow, Result};
use std::fs;

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
    eprintln!("TODO: Delete: {path:?}");
    Ok(())
}

pub(super) fn rename(old_path: &HumanPath, new_basename: &str) -> Result<()> {
    eprintln!(
        "TODO: Rename: {old_basename} -> {new_basename}",
        old_basename = old_path.basename
    );
    Ok(())
}
