//! Discovery of the applications that can open a given path, each with the
//! argv that launches it.
//!
//! The application database is read, never written: the picker cannot set a
//! default (use `gio mime <type> <application>` or `xdg-mime default`).

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use std::{collections::HashSet, ffi::OsString, path::Path};

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
    /// The program or bundle behind `name`.
    pub detail: String,
    pub is_default: bool,
    pub name: String,
    /// The `openers` key of the configured opener's row; `None` for an application.
    pub setting: Option<&'static str>,
}

impl AppCandidate {
    /// The name a failure to run this row uses.
    pub fn failure_name(&self) -> String {
        match self.setting {
            Some(key) => opener_setting(key),
            None => quoted_program(&self.name),
        }
    }
}

/// The applications that can open `path`, most preferred first, always followed
/// by the configured opener when one is set, and why the platform lookup could
/// not run, if it could not.
pub fn candidates_for(path: &Path) -> (Vec<AppCandidate>, Option<anyhow::Error>) {
    // Absolute, so the type is sniffed from a symlink's target and the path is
    // never read as a flag.
    let path = std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf());
    let (mut candidates, error) = match platform_candidates(&path) {
        Ok(candidates) => (candidates, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    // On the full names, so two that differ only past the cut both stay.
    dedupe_by_name(&mut candidates);
    for candidate in &mut candidates {
        candidate.name = shown_name(&candidate.name);
    }
    if let Some(fallback) = configured_opener(&Config::global().openers, &path) {
        candidates.push(fallback);
    }
    (candidates, error)
}

/// Longest application name shown, leaving room for the detail.
const MAX_NAME_CHARS: usize = 48;

/// An application's name escaped through `crate::visible` and cut at
/// `MAX_NAME_CHARS`.
fn shown_name(name: &str) -> String {
    let mut name = crate::visible(name).into_owned();
    if let Some((end, _)) = name.char_indices().nth(MAX_NAME_CHARS) {
        name.truncate(end);
        name.push('…');
    }
    name
}

/// Drop every candidate whose name a better ranked one already used.
fn dedupe_by_name(candidates: &mut Vec<AppCandidate>) {
    let mut seen = HashSet::new();
    candidates.retain(|candidate| seen.insert(candidate.name.to_lowercase()));
}

#[cfg(target_os = "linux")]
fn platform_candidates(path: &Path) -> anyhow::Result<Vec<AppCandidate>> {
    linux::candidates_for(path)
}

#[cfg(target_os = "macos")]
#[allow(clippy::unnecessary_wraps)] // Same signature as the Linux lookup, which can fail.
fn platform_candidates(path: &Path) -> anyhow::Result<Vec<AppCandidate>> {
    Ok(macos::candidates_for(path))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_candidates(_: &Path) -> anyhow::Result<Vec<AppCandidate>> {
    Ok(Vec::new())
}

/// The `openers` template for this kind of path, run through `sh`.
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
        detail: format!("openers.{key}"),
        is_default: false,
        name: shown_name(template),
        setting: Some(key),
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
        }
    }

    #[test_case("Text\u{2063} Editor" => "Text\\u{2063} Editor" ; "an invisible character is spelled out")]
    #[test_case(&"x".repeat(60) => format!("{}…", "x".repeat(48)) ; "a long name is cut")]
    fn shown_name_produces(name: &str) -> String {
        shown_name(name)
    }

    #[test]
    fn the_configured_opener_is_named_like_any_other_application() {
        let template = format!("{}\u{202e} %s", "x".repeat(60));
        let openers = Openers {
            open_directory: template.clone(),
            open_file: template,
            open_filectrl_window: String::new(),
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

    #[test]
    fn a_failure_names_the_setting_or_the_application() {
        let openers = Openers {
            open_directory: "cd %s && exec xterm".to_string(),
            open_file: "xdg-open %s".to_string(),
            open_filectrl_window: String::new(),
        };
        let directory = configured_opener(&openers, std::path::Path::new("/")).unwrap();
        assert_eq!("openers.open_directory", directory.failure_name());

        assert_eq!("\"gedit\"", candidate("gedit", "gedit").failure_name());
    }

    #[test_case("" ; "empty")]
    #[test_case("  \t" ; "only whitespace")]
    fn a_blank_opener_is_not_offered(template: &str) {
        let openers = Openers {
            open_directory: template.to_string(),
            open_file: template.to_string(),
            open_filectrl_window: String::new(),
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
