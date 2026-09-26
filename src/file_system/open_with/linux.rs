//! Application lookup through GLib's `gio` command: `gio info` for the content
//! type, `gio mime` for the applications registered for it, and `gio launch` to
//! run the chosen one. Only each application's name is read here, from its
//! desktop file.

use std::{
    env,
    ffi::OsString,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use anyhow::{Result, anyhow};
use log::debug;

use super::AppCandidate;

const GIO: &str = "gio";

/// The applications `gio` offers for `path`, in its order. `Err` only when
/// `gio` cannot be run; a lookup that fails for this path offers nothing.
pub(super) fn candidates_for(path: &Path) -> Result<Vec<AppCandidate>> {
    let started = Instant::now();
    let Some(content_type) = gio(&[
        OsString::from("info"),
        "-a".into(),
        "standard::content-type".into(),
        "--".into(),
        path.into(),
    ])?
    .as_deref()
    .and_then(parse_content_type)
    .map(str::to_string) else {
        return Ok(Vec::new());
    };
    let ids = gio(&["mime".into(), "--".into(), content_type.clone().into()])?
        .as_deref()
        .map(parse_mime_apps)
        .unwrap_or_default();
    let data_dirs = data_dirs();
    let mut candidates: Vec<AppCandidate> = ids
        .iter()
        .filter_map(|id| to_candidate(&data_dirs, path, id))
        .collect();
    if let Some(first) = candidates.first_mut() {
        first.is_default = true;
    }
    debug!(
        "Found {} application(s) for {} ({content_type}) in {:?}",
        candidates.len(),
        path.display(),
        started.elapsed()
    );
    Ok(candidates)
}

/// `gio`'s standard output, or `None` when it exits with an error. `Err` when
/// it cannot be started.
fn gio(args: &[OsString]) -> Result<Option<String>> {
    let mut command = Command::new(GIO);
    command
        .args(args)
        // Untranslated headers, so `parse_mime_apps` reads the same text in
        // every locale.
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    crate::app::events::unblock_in_child(&mut command);
    let output = command.output().map_err(|error| match error.kind() {
        ErrorKind::NotFound => {
            anyhow!("Cannot list applications: \"{GIO}\" (GLib) is required and was not found")
        }
        _ => anyhow!("Failed to run \"{GIO}\": {error}"),
    })?;
    if !output.status.success() {
        debug!("{GIO} {args:?} exited with {}", output.status);
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}

/// The value of the `standard::content-type` line of `gio info`.
fn parse_content_type(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("standard::content-type:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// The desktop ids in `gio mime` output: the default application, then the
/// registered and the recommended ones, each once. The default follows `": "`
/// on its header line; the others are indented one per line.
fn parse_mime_apps(text: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for line in text.lines() {
        let id = match line.strip_prefix('\t') {
            Some(indented) => indented.trim(),
            None => line.rsplit_once(": ").map_or("", |(_, id)| id.trim()),
        };
        if id.ends_with(".desktop") && !ids.iter().any(|seen| seen == id) {
            ids.push(id.to_string());
        }
    }
    ids
}

/// `None` when the desktop file cannot be found, since `gio launch` needs its
/// path.
fn to_candidate(data_dirs: &[PathBuf], path: &Path, id: &str) -> Option<AppCandidate> {
    let Some(file) = find_desktop_file(data_dirs, id) else {
        debug!("Skipping {id}: no desktop file found");
        return None;
    };
    let name = fs::read_to_string(&file)
        .ok()
        .and_then(|text| desktop_name(&text))
        .unwrap_or_else(|| id.to_string());
    Some(AppCandidate {
        argv: vec![
            GIO.into(),
            "launch".into(),
            file.into_os_string(),
            path.as_os_str().to_os_string(),
        ],
        // The desktop id is unique; a name can be shared.
        detail: id.to_string(),
        is_default: false,
        name,
        setting: None,
    })
}

/// The first `applications` directory holding the desktop file `id` names.
fn find_desktop_file(data_dirs: &[PathBuf], id: &str) -> Option<PathBuf> {
    if id.contains('/') {
        return None;
    }
    data_dirs
        .iter()
        .find_map(|dir| find_in(&dir.join("applications"), id))
}

/// A desktop id is the file's path below `applications` with each `/` written
/// as `-`, so `kde4-foo.desktop` may be `kde4/foo.desktop`.
fn find_in(dir: &Path, id: &str) -> Option<PathBuf> {
    let file = dir.join(id);
    if file.is_file() {
        return Some(file);
    }
    id.match_indices('-').find_map(|(index, _)| {
        let subdir = dir.join(&id[..index]);
        if subdir.is_dir() {
            find_in(&subdir, &id[index + 1..])
        } else {
            None
        }
    })
}

/// The unlocalized `Name=` of the `[Desktop Entry]` group.
fn desktop_name(text: &str) -> Option<String> {
    let mut in_desktop_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(group) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_desktop_entry = group == "Desktop Entry";
            continue;
        }
        if !in_desktop_entry {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim_end() == "Name"
        {
            let value = unescape(value.trim_start());
            return (!value.is_empty()).then_some(value);
        }
    }
    None
}

/// Undo the desktop entry spec's escapes: `\s`, `\n`, `\t`, `\r` and `\\`.
fn unescape(value: &str) -> String {
    let mut unescaped = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            unescaped.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => unescaped.push(' '),
            Some('n') => unescaped.push('\n'),
            Some('t') => unescaped.push('\t'),
            Some('r') => unescaped.push('\r'),
            Some(other) => unescaped.push(other),
            None => unescaped.push('\\'),
        }
    }
    unescaped
}

/// `$XDG_DATA_HOME` then `$XDG_DATA_DIRS`, where GLib looks for desktop files.
fn data_dirs() -> Vec<PathBuf> {
    let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
    data_dirs_of(
        home.as_deref(),
        env::var_os("XDG_DATA_HOME"),
        env::var_os("XDG_DATA_DIRS"),
    )
}

/// Relative and empty entries are ignored, as the XDG Base Directory spec
/// requires, and an unset or empty variable takes its default.
fn data_dirs_of(
    home: Option<&Path>,
    data_home: Option<OsString>,
    data_dirs: Option<OsString>,
) -> Vec<PathBuf> {
    let data_home = data_home
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home.map(|home| home.join(".local/share")));
    let data_dirs = data_dirs
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    data_home
        .into_iter()
        .chain(env::split_paths(&data_dirs).filter(|dir| dir.is_absolute()))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, path::Path};

    use test_case::test_case;

    use super::{
        data_dirs_of, desktop_name, find_desktop_file, parse_content_type, parse_mime_apps,
        to_candidate,
    };
    use crate::test_support::TempDir;

    const GIO_MIME: &str = "Default application for \"text/plain\": gedit.desktop\n\
        Registered applications:\n\
        \tvim.desktop\n\
        \tgedit.desktop\n\
        \tkde4-kate.desktop\n\
        Recommended applications:\n\
        \tvim.desktop\n\
        \tmousepad.desktop\n";

    #[test]
    fn parse_mime_apps_puts_the_default_first_and_lists_each_once() {
        assert_eq!(
            vec![
                "gedit.desktop",
                "vim.desktop",
                "kde4-kate.desktop",
                "mousepad.desktop"
            ],
            parse_mime_apps(GIO_MIME)
        );
    }

    #[test]
    fn parse_mime_apps_without_a_default_keeps_the_registered_order() {
        let text = "No default applications for \"text/x-foo\"\n\
            Registered applications:\n\tb.desktop\n\ta.desktop\n";
        assert_eq!(vec!["b.desktop", "a.desktop"], parse_mime_apps(text));
    }

    #[test_case("No default applications for \"application/x-foo\"\n" ; "no applications")]
    #[test_case("" ; "no output")]
    fn parse_mime_apps_finds_nothing(text: &str) {
        assert!(parse_mime_apps(text).is_empty());
    }

    #[test]
    fn parse_content_type_reads_the_attribute_line() {
        let text = "uri: file:///a/b.md\nlocal path: /a/b.md\n\
            unix mount: /dev/x / ext4 rw,relatime\nattributes:\n  standard::content-type: text/markdown\n";
        assert_eq!(Some("text/markdown"), parse_content_type(text));
    }

    #[test_case("uri: file:///a\nattributes:\n" ; "no attribute")]
    #[test_case("attributes:\n  standard::content-type: \n" ; "an empty value")]
    fn parse_content_type_finds_nothing(text: &str) {
        assert_eq!(None, parse_content_type(text));
    }

    #[test_case("[Desktop Entry]\nName[fr]=Éditeur\nName = Text\\sEditor\n" => Some("Text Editor".to_string())
        ; "the unlocalized key, trimmed and unescaped")]
    #[test_case("[Desktop Action new]\nName=New Window\n[Desktop Entry]\nName=Editor\n" => Some("Editor".to_string())
        ; "only the desktop entry group")]
    #[test_case("[Desktop Entry]\nExec=edit\n[Desktop Action new]\nName=New Window\n" => None
        ; "not from a later group")]
    #[test_case("[Desktop Entry]\nName=\n" => None ; "an empty name")]
    fn desktop_name_reads(text: &str) -> Option<String> {
        desktop_name(text)
    }

    fn write(dir: &Path, relative: &str, text: &str) {
        let file = dir.join(relative);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    #[test]
    fn find_desktop_file_takes_the_first_data_dir_that_has_it() {
        let dir = TempDir::new("gio_first_data_dir");
        let (user, system) = (dir.join("user"), dir.join("system"));
        write(&system, "applications/vim.desktop", "");
        write(&user, "applications/vim.desktop", "");

        assert_eq!(
            Some(user.join("applications/vim.desktop")),
            find_desktop_file(&[user, system], "vim.desktop")
        );
    }

    #[test_case("kde4-kate.desktop", "applications/kde4/kate.desktop" ; "a dash is a subdirectory")]
    #[test_case("org.kde-kate.desktop", "applications/org.kde-kate.desktop" ; "a dash in the file name is kept")]
    #[test_case("a-b-c.desktop", "applications/a/b-c.desktop" ; "any dash may be the subdirectory")]
    fn find_desktop_file_resolves(id: &str, relative: &str) {
        let dir = TempDir::new("gio_desktop_id");
        write(dir.path(), relative, "");

        assert_eq!(
            Some(dir.join(relative)),
            find_desktop_file(&[dir.path().to_path_buf()], id)
        );
    }

    #[test_case("../x.desktop" ; "a parent directory")]
    #[test_case("x/y.desktop" ; "a slash")]
    #[test_case("missing.desktop" ; "a missing file")]
    fn find_desktop_file_does_not_resolve(id: &str) {
        let dir = TempDir::new("gio_desktop_id_refused");
        write(dir.path(), "x.desktop", "");
        write(dir.path(), "applications/x/y.desktop", "");

        assert_eq!(None, find_desktop_file(&[dir.path().to_path_buf()], id));
    }

    #[test]
    fn a_candidate_is_named_by_its_desktop_file_and_launched_through_gio() {
        let dir = TempDir::new("gio_candidate");
        write(
            dir.path(),
            "applications/gedit.desktop",
            "[Desktop Entry]\nName=Text Editor\n",
        );
        write(
            dir.path(),
            "applications/nameless.desktop",
            "[Desktop Entry]\n",
        );
        let data_dirs = [dir.path().to_path_buf()];

        let gedit = to_candidate(&data_dirs, Path::new("/f"), "gedit.desktop").unwrap();
        assert_eq!("Text Editor", gedit.name);
        assert_eq!("gedit.desktop", gedit.detail);
        assert_eq!(
            vec![
                OsString::from("gio"),
                "launch".into(),
                dir.join("applications/gedit.desktop").into(),
                "/f".into()
            ],
            gedit.argv
        );
        let nameless = to_candidate(&data_dirs, Path::new("/f"), "nameless.desktop").unwrap();
        assert_eq!("nameless.desktop", nameless.name);
        assert!(to_candidate(&data_dirs, Path::new("/f"), "missing.desktop").is_none());
    }

    #[test_case(None, None, &["/home/u/.local/share", "/usr/local/share", "/usr/share"] ; "defaults")]
    #[test_case(Some("/d"), Some("/a:rel:/b"), &["/d", "/a", "/b"] ; "set, a relative entry dropped")]
    #[test_case(Some("rel"), Some(""), &["/home/u/.local/share", "/usr/local/share", "/usr/share"] ; "relative or empty takes the default")]
    fn data_dirs_of_reads(data_home: Option<&str>, data_dirs: Option<&str>, expected: &[&str]) {
        let dirs = data_dirs_of(
            Some(Path::new("/home/u")),
            data_home.map(OsString::from),
            data_dirs.map(OsString::from),
        );
        assert_eq!(
            expected
                .iter()
                .map(std::path::PathBuf::from)
                .collect::<Vec<_>>(),
            dirs
        );
    }
}
