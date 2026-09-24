//! Discovery of the applications that can open a given path.
//!
//! Each platform module answers "which applications handle this path", and
//! returns the concrete argv that launches each one, so that nothing
//! platform-specific has to travel through `Command` or `FileSystem`.
//!
//! Two limitations here are deliberate:
//!
//! - **The application database is read, never written.** The picker reports
//!   the default the desktop resolves to and cannot set one; use `xdg-mime
//!   default` or the desktop's own settings for that.
//! - **The index is built once per process** (the `OnceLock`s in `linux.rs`), so
//!   an application installed while FileCTRL runs is not picked up until it
//!   restarts. Rebuilding per open would pay the scan repeatedly to catch
//!   something that changes rarely.

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
    ffi::OsString,
    path::{Path, PathBuf},
};

use log::debug;

use super::{
    operations::{opener_setting, quoted_program},
    shell,
};
use crate::app::config::{Config, Openers};

/// An application offered by the "open with" picker, and the argv that launches
/// it against the chosen path.
#[derive(Clone, Debug, PartialEq)]
pub struct AppCandidate {
    pub argv: Vec<OsString>,
    /// The program or bundle behind `name`, which is what tells two similarly
    /// named applications apart.
    pub detail: String,
    /// Whether this is the platform's default handler for the path.
    pub is_default: bool,
    /// Human readable application name.
    pub name: String,
    /// The `openers` key of the configured opener's row, which names it in a
    /// failure: its `name` is a shell command, whose first word need not be
    /// what failed. `None` for an application.
    pub setting: Option<&'static str>,
    pub working_dir: Option<PathBuf>,
}

impl AppCandidate {
    /// What a failure to run this row calls it: the `openers` setting, or the
    /// application's quoted name.
    pub fn failure_name(&self) -> String {
        match self.setting {
            Some(key) => opener_setting(key),
            None => quoted_program(&self.name),
        }
    }
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
    // On the full names, so two that differ only past the cut both stay.
    dedupe_by_name(&mut candidates);
    for candidate in &mut candidates {
        candidate.name = shown_name(&candidate.name);
    }
    if let Some(fallback) = configured_opener(&Config::global().openers, &path) {
        candidates.push(fallback);
    }
    candidates
}

/// Longest application name shown, so the detail after it that tells two
/// entries apart always has room.
const MAX_NAME_CHARS: usize = 48;

/// An application's name as the picker shows it and compares it: passed
/// through `crate::visible`, so an invisible character cannot make two names
/// look the same, and cut at `MAX_NAME_CHARS`.
fn shown_name(name: &str) -> String {
    let mut name = crate::visible(name).into_owned();
    if let Some((end, _)) = name.char_indices().nth(MAX_NAME_CHARS) {
        name.truncate(end);
        name.push('…');
    }
    name
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
fn configured_opener(openers: &Openers, path: &Path) -> Option<AppCandidate> {
    let (key, template) = if path.is_dir() {
        ("open_directory", &openers.open_directory)
    } else {
        ("open_file", &openers.open_file)
    };
    if template.trim().is_empty() {
        debug!("No configured opener for {}", path.display());
        return None;
    }
    Some(AppCandidate {
        argv: shell::command(template, [path.as_os_str().to_os_string()]),
        // The setting it comes from, so it is obvious which config key to
        // change.
        detail: format!("openers.{key}"),
        is_default: false,
        name: shown_name(template),
        setting: Some(key),
        working_dir: None,
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use test_case::test_case;

    use super::{AppCandidate, configured_opener, dedupe_by_name, shown_name};
    use crate::app::config::Openers;

    fn candidate(name: &str, detail: &str) -> AppCandidate {
        AppCandidate {
            argv: vec![OsString::from(detail)],
            detail: detail.to_string(),
            is_default: false,
            name: name.to_string(),
            setting: None,
            working_dir: None,
        }
    }

    /// An invisible character would make two rows read the same, and a long
    /// name would push the detail that tells them apart off screen.
    #[test_case("Text\u{2063} Editor" => "Text\\u{2063} Editor" ; "an invisible character is spelled out")]
    #[test_case(&"x".repeat(60) => format!("{}…", "x".repeat(48)) ; "a long name is cut")]
    fn shown_name_produces(name: &str) -> String {
        shown_name(name)
    }

    /// The template is shown as the application's name, so it is shown the
    /// way every other name is.
    #[test]
    fn the_configured_opener_is_named_like_any_other_application() {
        let template = format!("{}\u{202e} %s", "x".repeat(60));
        let openers = Openers {
            open_directory: template.clone(),
            open_file: template,
            open_filectrl_window: String::new(),
            run_in_terminal: String::new(),
        };
        let opener = configured_opener(&openers, std::path::Path::new("/")).unwrap();
        assert_eq!(format!("{}…", "x".repeat(48)), opener.name);

        let openers = Openers {
            open_directory: "a\u{202e} %s".to_string(),
            ..openers
        };
        let opener = configured_opener(&openers, std::path::Path::new("/")).unwrap();
        assert_eq!("a\\u{202e} %s", opener.name);
    }

    /// The configured opener's name is a shell command, so a failure names
    /// the setting it came from; an application keeps its quoted name.
    #[test]
    fn a_failure_names_the_setting_or_the_application() {
        let openers = Openers {
            open_directory: "cd %s && exec xterm".to_string(),
            open_file: "xdg-open %s".to_string(),
            open_filectrl_window: String::new(),
            run_in_terminal: String::new(),
        };
        let directory = configured_opener(&openers, std::path::Path::new("/")).unwrap();
        assert_eq!("openers.open_directory", directory.failure_name());

        assert_eq!("\"gedit\"", candidate("gedit", "gedit").failure_name());
    }

    /// Blank reads as unset, as it does when the opener is run directly, so
    /// the picker does not offer a row that runs nothing.
    #[test_case("" ; "empty")]
    #[test_case("  \t" ; "only whitespace")]
    fn a_blank_opener_is_not_offered(template: &str) {
        let openers = Openers {
            open_directory: template.to_string(),
            open_file: template.to_string(),
            open_filectrl_window: String::new(),
            run_in_terminal: String::new(),
        };
        assert!(configured_opener(&openers, std::path::Path::new("/")).is_none());
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
