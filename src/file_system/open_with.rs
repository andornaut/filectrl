//! Discovery of the applications that can open a given path.
//!
//! Each platform module answers "which applications handle this path", and
//! returns the concrete argv that launches each one, so that nothing
//! platform-specific has to travel through `Command` or `FileSystem`.

// The desktop entry and mime-apps specs are Linux only; macOS answers the same
// question through Launch Services.
#[cfg(target_os = "linux")]
mod exec;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod mimeapps;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use log::debug;

use crate::app::config::Config;

/// An application offered by the "open with" picker, and the argv that launches
/// it against the chosen path.
#[derive(Clone, Debug, PartialEq)]
pub struct AppCandidate {
    pub argv: Vec<String>,
    /// The program or bundle behind `name`, which is what tells two similarly
    /// named applications apart.
    pub detail: String,
    /// Whether this is the platform's default handler for the path.
    pub is_default: bool,
    /// Human readable application name.
    pub name: String,
    pub working_dir: Option<PathBuf>,
}

/// The applications that can open `path`, most preferred first, always followed
/// by the configured opener when one is set.
pub fn candidates_for(path: &Path) -> Vec<AppCandidate> {
    // Resolve symlinks so that the type is sniffed from the target. The result
    // is always absolute, which desktop entries expect and which keeps a path
    // from ever being read as a command line flag.
    let path = std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf());
    let mut candidates = platform_candidates(&path);
    dedupe_by_name(&mut candidates);
    if let Some(fallback) = configured_opener(&path) {
        candidates.push(fallback);
    }
    candidates
}

/// Drop every candidate whose name a better ranked one already used. Several
/// desktop entries can share a `Name=`, and rows that read identically are not
/// a choice.
fn dedupe_by_name(candidates: &mut Vec<AppCandidate>) {
    let mut seen = HashSet::new();
    candidates.retain(|candidate| seen.insert(candidate.name.to_lowercase()));
}

#[cfg(target_os = "linux")]
fn platform_candidates(path: &Path) -> Vec<AppCandidate> {
    linux::candidates_for(path)
}

#[cfg(target_os = "macos")]
fn platform_candidates(path: &Path) -> Vec<AppCandidate> {
    macos::candidates_for(path)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_candidates(_: &Path) -> Vec<AppCandidate> {
    Vec::new()
}

/// The `openers` template for this kind of path, offered last so that the
/// picker still works on a system with no application database at all. The
/// template is a shell command rather than an argv, so it runs through `sh`.
fn configured_opener(path: &Path) -> Option<AppCandidate> {
    let openers = &Config::global().openers;
    let (key, template) = if path.is_dir() {
        ("open_current_directory", &openers.open_current_directory)
    } else {
        ("open_selected_file", &openers.open_selected_file)
    };
    if template.is_empty() {
        debug!("No configured opener for {path:?}");
        return None;
    }
    let command = template.replace("%s", &shell_words::quote(&path.to_string_lossy()));
    Some(AppCandidate {
        argv: vec!["sh".to_string(), "-c".to_string(), command],
        // The setting it comes from, so it is obvious which config key to
        // change.
        detail: format!("openers.{key}"),
        is_default: false,
        name: template.clone(),
        working_dir: None,
    })
}

#[cfg(test)]
mod tests {
    use super::{AppCandidate, dedupe_by_name};

    fn candidate(name: &str, detail: &str) -> AppCandidate {
        AppCandidate {
            argv: vec![detail.to_string()],
            detail: detail.to_string(),
            is_default: false,
            name: name.to_string(),
            working_dir: None,
        }
    }

    #[test]
    fn dedupe_by_name_keeps_the_best_ranked_of_each_name() {
        let mut candidates = vec![
            candidate("notepad", "wine-extension-ini"),
            candidate("gedit", "org.gnome.gedit"),
            candidate("notepad", "wine-extension-txt"),
            candidate("Notepad", "wine-extension-log"),
        ];

        dedupe_by_name(&mut candidates);

        assert_eq!(
            vec!["wine-extension-ini", "org.gnome.gedit"],
            candidates
                .iter()
                .map(|candidate| candidate.detail.as_str())
                .collect::<Vec<_>>()
        );
    }
}
