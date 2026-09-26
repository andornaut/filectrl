//! Application lookup via the freedesktop.org shared MIME database, the desktop
//! entry spec, and the mime-apps spec.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Instant,
};

use freedesktop_desktop_entry::{DesktopEntry, get_languages_from_env};
use log::{debug, warn};
use xdg_mime::SharedMimeInfo;

use super::{
    AppCandidate,
    exec::{expand, unescape_value},
    mimeapps::{self, AppDirIndex, Level, MimeAppsList},
};
use crate::{
    app::config::Config,
    file_system::{path_info::compact, shell},
};

/// Matches every file, per the mime-apps spec's fallback types.
const ALL_FILES: &str = "all/allfiles";
const ALL: &str = "all/all";
const TEXT_PLAIN: &str = "text/plain";
const OCTET_STREAM: &str = "application/octet-stream";
/// `guess()` returns this for an empty file without consulting the globs.
const ZEROSIZE: &str = "application/x-zerosize";

/// Parsed once per process.
static MIME_DB: OnceLock<SharedMimeInfo> = OnceLock::new();

fn mime_db() -> &'static SharedMimeInfo {
    MIME_DB.get_or_init(|| {
        let started = Instant::now();
        let db = SharedMimeInfo::new();
        debug!("Loaded the shared MIME database in {:?}", started.elapsed());
        db
    })
}

/// Child type to its direct parents. Not `SharedMimeInfo::get_parents`, which
/// returns `None` for any type that is not an alias.
static SUBCLASSES: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();

fn subclasses() -> &'static HashMap<String, Vec<String>> {
    SUBCLASSES.get_or_init(|| {
        let mut graph: HashMap<String, Vec<String>> = HashMap::new();
        for dir in data_dirs() {
            let Ok(text) = fs::read_to_string(dir.join("mime").join("subclasses")) else {
                continue;
            };
            for (child, parent) in parse_subclasses(&text) {
                graph.entry(child).or_default().push(parent);
            }
        }
        graph
    })
}

/// Each line of a `subclasses` file is `<child> <parent>`.
fn parse_subclasses(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| line.trim().split_once(' '))
        .map(|(child, parent)| (child.trim().to_string(), parent.trim().to_string()))
        .filter(|(child, parent)| !child.is_empty() && !parent.is_empty())
        .collect()
}

fn parents_of(mime: &str) -> Vec<String> {
    let mut parents = subclasses().get(mime).cloned().unwrap_or_default();
    // Every text/* type is a subclass of text/plain, declared or not.
    if mime.starts_with("text/") && mime != TEXT_PLAIN && !parents.iter().any(|p| p == TEXT_PLAIN) {
        parents.push(TEXT_PLAIN.to_string());
    }
    parents
}

/// The mimeapps.list files and applications directories to consult, ordered
/// highest precedence first.
pub(super) struct Sources {
    levels: Vec<Level>,
}

impl Sources {
    /// Build from the XDG environment.
    pub(super) fn system() -> Self {
        Self::from_dirs(&config_dirs(), &data_dirs())
    }

    /// `config_dirs` contribute mimeapps.list files only; `data_dirs`
    /// contribute both a mimeapps.list and an `applications` directory.
    fn from_dirs(config_dirs: &[PathBuf], data_dirs: &[PathBuf]) -> Self {
        let desktops = current_desktops();
        let mut levels: Vec<Level> = config_dirs
            .iter()
            .map(|dir| Level {
                apps: None,
                lists: lists_in(&desktops, dir),
            })
            .collect();
        levels.extend(data_dirs.iter().map(|dir| {
            let applications = dir.join("applications");
            Level {
                lists: lists_in(&desktops, &applications),
                apps: Some(index_applications(&applications)),
            }
        }));
        Self { levels }
    }
}

/// Built once per process.
static SOURCES: OnceLock<Sources> = OnceLock::new();

fn sources() -> &'static Sources {
    SOURCES.get_or_init(|| {
        let started = Instant::now();
        let sources = Sources::system();
        debug!(
            "Indexed the application directories in {:?}",
            started.elapsed()
        );
        sources
    })
}

pub(super) fn candidates_for(path: &Path) -> Vec<AppCandidate> {
    let started = Instant::now();
    let candidates = candidates_from(sources(), path);
    debug!(
        "Found {} application(s) for {} in {:?}",
        candidates.len(),
        path.display(),
        started.elapsed()
    );
    candidates
}

fn candidates_from(sources: &Sources, path: &Path) -> Vec<AppCandidate> {
    let chain = mime_chain(path);
    debug!("Resolved {} to MIME types: {chain:?}", path.display());
    let associations = mimeapps::associations(&sources.levels, &chain);
    let locales = get_languages_from_env();
    let candidate = |id: &str| {
        let file = mimeapps::resolve(&sources.levels, id)?;
        let entry = DesktopEntry::from_path(file, None::<&[&str]>)
            .inspect_err(|error| debug!("Skipping {}: {error}", file.display()))
            .ok()?;
        to_candidate(&locales, path, &entry)
    };
    // The first offerable configured default goes first; one that cannot be
    // offered falls through to the next, as in `xdg-mime` and `gio`. Each id is
    // built at most once.
    let mut tried: HashSet<&str> = HashSet::new();
    let mut candidates: Vec<AppCandidate> = associations
        .defaults
        .iter()
        .filter(|id| tried.insert(id))
        .find_map(|id| candidate(id))
        .into_iter()
        .collect();
    candidates.extend(
        associations
            .ordered
            .iter()
            .filter(|id| tried.insert(id))
            .filter_map(|id| candidate(id)),
    );
    // Without an offerable default, the spec falls back to the first association.
    if let Some(first) = candidates.first_mut() {
        first.is_default = true;
    }
    candidates
}

/// The name the glob rules are matched against. Deliberately lossy: `guess`
/// drops a name it cannot convert, so `caf\xe9.txt` would match no `*.txt` rule.
/// The program still receives the path's own bytes.
fn glob_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// The MIME types to look up, most specific first: the guessed type, then its
/// ancestors in the subclass graph, then the spec's fallback types.
fn mime_chain(path: &Path) -> Vec<String> {
    let db = mime_db();
    let file_name = glob_name(path);
    let mut builder = db.guess_mime_type();
    // Set before `path`, which fills in the name only when unset.
    if let Some(file_name) = &file_name {
        builder.file_name(file_name);
    }
    let guessed = builder.path(path).guess();
    let mut queue: VecDeque<String> = VecDeque::new();
    // For an empty file, use the types the name implies instead of the zero
    // size type.
    let from_name = (guessed.mime_type().essence_str() == ZEROSIZE)
        .then_some(file_name.as_deref())
        .flatten()
        .map(|name| db.get_mime_types_from_file_name(name))
        .filter(|types| !types.is_empty());
    match from_name {
        Some(types) => queue.extend(types.iter().map(canonical)),
        None => queue.push_back(canonical(guessed.mime_type())),
    }

    // Breadth first, so nearer ancestors rank higher.
    let mut chain = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(current) = queue.pop_front() {
        if !seen.insert(current.clone()) {
            continue;
        }
        queue.extend(parents_of(&current));
        chain.push(current);
    }
    // Every non-inode type is a subclass of octet-stream; added last so it
    // never ranks above a declared ancestor.
    if chain
        .first()
        .is_some_and(|mime| !mime.starts_with("inode/"))
        && !seen.contains(OCTET_STREAM)
    {
        chain.push(OCTET_STREAM.to_string());
    }

    if path.is_file() {
        chain.push(ALL_FILES.to_string());
    }
    chain.push(ALL.to_string());
    chain
}

/// Resolve an alias to the type it stands for.
fn canonical(mime: &mime::Mime) -> String {
    mime_db()
        .unalias_mime_type(mime)
        .as_ref()
        .unwrap_or(mime)
        .essence_str()
        .to_string()
}

/// `canonical` for a type read as text, since the chain is matched by string.
fn canonical_str(mime: &str) -> String {
    mime.parse::<mime::Mime>()
        .map_or_else(|_| mime.to_string(), |parsed| canonical(&parsed))
}

/// Whether an entry is a non-hidden application whose `TryExec` is installed.
/// `NoDisplay` entries are kept: the key hides an application from menus only.
fn is_offerable(entry: &DesktopEntry) -> bool {
    if entry.type_() != Some("Application") || entry.hidden() {
        return false;
    }
    entry
        .try_exec()
        .is_none_or(|program| is_installed(&unescape_value(program)))
}

/// A refused or malformed `Exec` is logged as a warning, any other skip at
/// debug. `candidates_from` decides `is_default`.
fn to_candidate(locales: &[String], path: &Path, entry: &DesktopEntry) -> Option<AppCandidate> {
    let file = entry.path.as_path();
    if !is_offerable(entry) {
        debug!(
            "Skipping {}: not an installed, visible application",
            file.display()
        );
        return None;
    }
    let name = entry
        .name(locales)
        .map_or_else(|| entry.appid.clone(), std::borrow::Cow::into_owned);
    let mut argv = expand(path, entry.exec()?)
        .inspect_err(|error| warn!("Cannot offer {}: {error}", compact(file)))
        .ok()?;
    if entry.terminal() {
        // Offered only when a terminal is configured to host it.
        let template = &Config::global().openers.run_in_terminal;
        argv = in_terminal(template, &argv).or_else(|| {
            debug!(
                "Skipping {}: Terminal=true and openers.run_in_terminal is empty",
                file.display()
            );
            None
        })?;
    }
    Some(AppCandidate {
        // The desktop id is unique; a program name can be a shared wrapper.
        detail: entry.appid.clone(),
        is_default: false,
        name,
        setting: None,
        working_dir: entry
            .path()
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from),
        argv,
    })
}

/// Wrap `argv` in the configured terminal, `%s` taking each word as its own
/// argument. `None` when no terminal is configured.
fn in_terminal(template: &str, argv: &[OsString]) -> Option<Vec<OsString>> {
    if template.trim().is_empty() {
        return None;
    }
    Some(shell::command(template, argv.iter().cloned()))
}

/// Whether a `TryExec` value names an executable, absolute or on `$PATH`.
fn is_installed(program: &str) -> bool {
    let path = Path::new(program);
    if path.is_absolute() {
        return is_executable(path);
    }
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|dir| is_executable(&dir.join(program))))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Index the `.desktop` files under `dir`, scanning only for `MimeType=`.
fn index_applications(dir: &Path) -> AppDirIndex {
    let mut index = AppDirIndex::default();
    let mut queue = VecDeque::from([dir.to_path_buf()]);
    // `is_dir` follows symlinks, so a link to an ancestor would loop.
    let mut visited: HashSet<PathBuf> = HashSet::from([canonical_dir(dir)]);
    while let Some(current) = queue.pop_front() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let file = entry.path();
            if file.is_dir() {
                if visited.insert(canonical_dir(&file)) {
                    queue.push_back(file);
                }
                continue;
            }
            let Some(id) = mimeapps::desktop_id(dir, &file) else {
                continue;
            };
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            let types = scan_mime_types(&text)
                .iter()
                .map(|mime| canonical_str(mime))
                .collect();
            index.mime_types.insert(id.clone(), types);
            index.by_id.insert(id, file);
        }
    }
    index
}

fn canonical_dir(dir: &Path) -> PathBuf {
    fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// Read the `MimeType=` values from the `[Desktop Entry]` group.
fn scan_mime_types(text: &str) -> Vec<String> {
    let mut in_desktop_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(group) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if in_desktop_entry {
                break;
            }
            in_desktop_entry = group.trim() == "Desktop Entry";
            continue;
        }
        if !in_desktop_entry {
            continue;
        }
        if let Some((key, values)) = line.split_once('=')
            && key.trim_end() == "MimeType"
        {
            return values
                .split(';')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect();
        }
    }
    Vec::new()
}

/// `$XDG_CONFIG_HOME` then `$XDG_CONFIG_DIRS`.
fn config_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = env_dir("XDG_CONFIG_HOME").or_else(|| home_dir().map(|h| h.join(".config")))
    {
        dirs.push(dir);
    }
    dirs.extend(env_dirs("XDG_CONFIG_DIRS", "/etc/xdg"));
    dedupe_dirs(dirs)
}

/// `$XDG_DATA_HOME` then `$XDG_DATA_DIRS`.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) =
        env_dir("XDG_DATA_HOME").or_else(|| home_dir().map(|h| h.join(".local").join("share")))
    {
        dirs.push(dir);
    }
    dirs.extend(env_dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share"));
    dedupe_dirs(dirs)
}

/// Drop repeated directories, comparing through symlinks.
fn dedupe_dirs(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    dirs.into_iter()
        .filter(|dir| seen.insert(canonical_dir(dir)))
        .collect()
}

fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

fn env_dir(name: &str) -> Option<PathBuf> {
    dir_of(env::var_os(name))
}

/// `None` for an unset, empty or relative value, as the XDG Base Directory spec
/// requires.
fn dir_of(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|dir| dir.is_absolute())
}

fn env_dirs(name: &str, fallback: &str) -> Vec<PathBuf> {
    dirs_of(env::var_os(name), fallback)
}

/// Split a `$PATH`-shaped value, falling back to `fallback` when unset or
/// empty. Empty and relative components are dropped.
fn dirs_of(value: Option<OsString>, fallback: &str) -> Vec<PathBuf> {
    let value = value.filter(|value| !value.is_empty());
    let value = value.unwrap_or_else(|| fallback.into());
    env::split_paths(&value)
        .filter(|dir| dir.is_absolute())
        .collect()
}

/// Each entry of `$XDG_CURRENT_DESKTOP`, lowercased, in order.
fn current_desktops() -> Vec<String> {
    desktops_of(&env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
}

/// The non-empty names in a `$XDG_CURRENT_DESKTOP` value.
fn desktops_of(value: &str) -> Vec<String> {
    value
        .split(':')
        .map(|desktop| desktop.trim().to_lowercase())
        .filter(|desktop| !desktop.is_empty())
        .collect()
}

/// The desktop-specific lists in `dir`, highest precedence first, followed by
/// the generic one.
fn lists_in(desktops: &[String], dir: &Path) -> Vec<MimeAppsList> {
    let mut lists = Vec::new();
    for desktop in desktops {
        if let Ok(text) = fs::read_to_string(dir.join(format!("{desktop}-mimeapps.list"))) {
            lists.push(MimeAppsList::parse(true, &text));
        }
    }
    if let Ok(text) = fs::read_to_string(dir.join("mimeapps.list")) {
        lists.push(MimeAppsList::parse(false, &text));
    }
    for list in &mut lists {
        list.canonicalize_keys(canonical_str);
    }
    lists
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use std::{
        ffi::OsString,
        path::{Path, PathBuf},
    };

    use super::{
        ALL, ALL_FILES, DesktopEntry, OCTET_STREAM, Sources, TEXT_PLAIN, candidates_from,
        dedupe_dirs, desktops_of, dir_of, dirs_of, glob_name, in_terminal, index_applications,
        is_executable, is_offerable, lists_in, mime_chain, parents_of, parse_subclasses,
        scan_mime_types, to_candidate,
    };
    use crate::{app::config::Config, test_support::TempDir};

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    fn desktop_entry(dir: &TempDir, name: &str, body: &str) -> DesktopEntry {
        let file = dir.join(name);
        std::fs::write(&file, body).unwrap();
        DesktopEntry::from_path(file, None::<&[&str]>).unwrap()
    }

    #[test_case("[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\n", true
        ; "an application is offered")]
    #[test_case("[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\nNoDisplay=true\n", true
        ; "NoDisplay is kept, since the key exists to associate without appearing in menus")]
    #[test_case("[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\nHidden=true\n", false
        ; "Hidden means the entry was deleted and must not be offered")]
    #[test_case("[Desktop Entry]\nType=Link\nName=Viewer\nExec=view %f\nURL=http://x\n", false
        ; "only an Application can open a file")]
    #[test_case("[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\nTryExec=/nonexistent/program\n", false
        ; "TryExec naming a program that is not installed")]
    #[test_case("[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\nTryExec=sh\n", true
        ; "TryExec naming a program on $PATH")]
    fn is_offerable_accepts_an_installed_visible_application(body: &str, expected: bool) {
        let dir = TempDir::new("open_with_offerable");
        let entry = desktop_entry(&dir, "viewer.desktop", body);

        assert_eq!(expected, is_offerable(&entry), "{body:?}");
    }

    #[test]
    fn a_try_exec_path_with_an_escaped_space_is_found() {
        let dir = TempDir::new("open_with_try_exec_escaped");
        let program = dir.join("my app");
        crate::test_support::write_executable(&program, "#!/bin/sh\n");
        let escaped = program.to_str().unwrap().replace(' ', "\\s");
        let body = format!(
            "[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\nTryExec={escaped}\n"
        );
        let entry = desktop_entry(&dir, "viewer.desktop", &body);

        assert!(is_offerable(&entry), "{:?}", entry.try_exec());
    }

    #[test_case("[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\n", true
        ; "an application is offered")]
    #[test_case("[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\nHidden=true\n", false
        ; "an entry the filter rejects is not built")]
    #[test_case("[Desktop Entry]\nType=Application\nName=Viewer\n", false
        ; "an entry with no Exec has nothing to run")]
    fn to_candidate_offers_only_a_runnable_application(body: &str, expected: bool) {
        Config::init_test();
        let dir = TempDir::new("open_with_entry");
        let entry = desktop_entry(&dir, "viewer.desktop", body);

        let candidate = to_candidate(&[], Path::new("/tmp/file.txt"), &entry);

        assert_eq!(expected, candidate.is_some(), "{body:?}");
    }

    #[test]
    fn a_terminal_application_runs_inside_the_configured_terminal() {
        Config::init_test();
        let dir = TempDir::new("open_with_terminal");
        let entry = desktop_entry(
            &dir,
            "viewer.desktop",
            "[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\nTerminal=true\n",
        );

        let candidate = to_candidate(&[], Path::new("/tmp/file.txt"), &entry).unwrap();

        assert_eq!(
            vec![
                OsString::from("sh"),
                OsString::from("-c"),
                OsString::from("xterm -e \"$@\""),
                OsString::from("sh"),
                OsString::from("view"),
                OsString::from("/tmp/file.txt"),
            ],
            candidate.argv
        );
    }

    #[test]
    fn a_symlink_back_to_an_ancestor_is_indexed_once() {
        let dir = TempDir::new("open_with_index_loop");
        std::fs::write(
            dir.join("viewer.desktop"),
            "[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.join("loop")).unwrap();

        let index = index_applications(dir.path());

        assert_eq!(
            vec!["viewer.desktop"],
            index.by_id.keys().map(String::as_str).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_desktop_specific_list_outranks_the_generic_one_and_only_sets_a_default() {
        use super::mimeapps::{AppDirIndex, Level, associations};

        let dir = TempDir::new("open_with_lists");
        std::fs::write(
            dir.join("gnome-mimeapps.list"),
            "[Default Applications]\ntext/plain=b.desktop\n\
             [Added Associations]\ntext/plain=ignored.desktop\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("mimeapps.list"),
            "[Default Applications]\ntext/plain=a.desktop\n",
        )
        .unwrap();
        let mut apps = AppDirIndex::default();
        for id in ["a.desktop", "b.desktop", "ignored.desktop"] {
            apps.by_id.insert(id.to_string(), dir.join(id));
        }
        let levels = vec![Level {
            apps: Some(apps),
            lists: lists_in(&strings(&["gnome"]), dir.path()),
        }];

        let result = associations(&levels, &strings(&[TEXT_PLAIN]));

        assert_eq!(strings(&["b.desktop", "a.desktop"]), result.defaults);
        assert!(result.ordered.is_empty(), "{:?}", result.ordered);
    }

    #[test]
    fn the_chain_ends_with_the_fallback_types() {
        let dir = TempDir::new("open_with_chain");
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"x").unwrap();

        assert!(mime_chain(&file).ends_with(&strings(&[ALL_FILES, ALL])));
        let directory = mime_chain(dir.path());
        assert_eq!(Some(&ALL.to_string()), directory.last());
        assert!(!directory.iter().any(|mime| mime == ALL_FILES));
    }

    #[test]
    fn a_file_inherits_octet_stream_after_its_other_ancestors() {
        let dir = TempDir::new("open_with_octet_stream");
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"x").unwrap();

        assert!(
            mime_chain(&file).ends_with(&strings(&[OCTET_STREAM, ALL_FILES, ALL])),
            "{:?}",
            mime_chain(&file)
        );
        assert!(
            !mime_chain(dir.path())
                .iter()
                .any(|mime| mime == OCTET_STREAM)
        );
    }

    #[test]
    fn glob_name_keeps_the_extension_of_a_name_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt;

        let path = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/caf\xe9.txt"));

        let name = glob_name(&path).expect("a file name");
        // Asserts on the glob string, not a path's extension.
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        let ends_with_txt = name.ends_with(".txt");
        assert!(ends_with_txt, "{name}");
    }

    #[test]
    fn every_text_type_inherits_text_plain() {
        // A type unknown to the database, so the parent comes from the rule.
        let parents = parents_of("text/x-filectrl-test");

        assert!(
            parents.iter().any(|p| p == TEXT_PLAIN),
            "an editor registered for text/plain must be offered for any text type: {parents:?}"
        );
    }

    #[test]
    fn text_plain_is_not_its_own_parent() {
        assert!(!parents_of(TEXT_PLAIN).iter().any(|p| p == TEXT_PLAIN));
    }

    #[test_case(0o644, false ; "a readable file with no execute bit")]
    #[test_case(0o755, true  ; "an executable file")]
    #[test_case(0o100, true  ; "executable by its owner alone")]
    #[test_case(0o001, true  ; "executable by others alone")]
    fn is_executable_reads_the_execute_bits(mode: u32, expected: bool) {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("open_with_exec");
        let file = dir.join("program");
        std::fs::write(&file, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();

        assert_eq!(expected, is_executable(&file));
    }

    #[test]
    fn a_directory_with_the_execute_bit_is_not_a_program() {
        let dir = TempDir::new("open_with_exec_dir");

        assert!(!is_executable(dir.path()));
    }

    /// Three applications for `all/all`, with `editor.desktop` the default.
    fn data_dir_with_applications(dir: &TempDir) -> std::path::PathBuf {
        let applications = dir.join("applications");
        std::fs::create_dir_all(&applications).unwrap();
        let write = |name: &str, body: &str| std::fs::write(applications.join(name), body).unwrap();
        write(
            "viewer.desktop",
            "[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\nMimeType=all/all;\n",
        );
        write(
            "editor.desktop",
            "[Desktop Entry]\nType=Application\nName=Editor\nExec=edit %f\nMimeType=all/all;\nPath=/var/empty\n",
        );
        write(
            "blank.desktop",
            "[Desktop Entry]\nType=Application\nName=Blank\nExec=blank %f\nMimeType=all/all;\nPath=\n",
        );
        std::fs::write(
            applications.join("mimeapps.list"),
            "[Default Applications]\nall/all=editor.desktop\n",
        )
        .unwrap();
        dir.path().to_path_buf()
    }

    #[test]
    fn the_configured_default_is_the_one_marked_default() {
        Config::init_test();
        let dir = TempDir::new("open_with_sources");
        let data = data_dir_with_applications(&dir);
        let file = dir.join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        let sources = Sources::from_dirs(&[], &[data]);

        let candidates = candidates_from(&sources, &file);

        let named = |name: &str| {
            candidates
                .iter()
                .find(|candidate| candidate.name == name)
                .unwrap_or_else(|| panic!("{name} should be offered: {candidates:?}"))
        };
        assert!(named("Editor").is_default);
        assert!(!named("Viewer").is_default);
        assert!(!named("Blank").is_default);
    }

    // Viewer is not associated, so only the fall-through can list it.
    #[test_case(Some("Exec=gone %f\nHidden=true\n") ; "hidden")]
    #[test_case(Some("Exec=gone %f\nTryExec=/nonexistent/program\n") ; "its TryExec is not installed")]
    #[test_case(Some("Exec=sh -c %f\n") ; "its Exec is refused")]
    #[test_case(Some("Exec=gone \"%f\n") ; "its Exec is malformed")]
    #[test_case(Some("") ; "it has no Exec")]
    #[test_case(None ; "it is not installed")]
    fn a_default_that_cannot_be_offered_falls_through_to_the_next_one(gone: Option<&str>) {
        Config::init_test();
        let dir = TempDir::new("open_with_default_fallthrough");
        let applications = dir.join("applications");
        std::fs::create_dir_all(&applications).unwrap();
        let write = |name: &str, body: &str| std::fs::write(applications.join(name), body).unwrap();
        if let Some(keys) = gone {
            write(
                "gone.desktop",
                &format!("[Desktop Entry]\nType=Application\nName=Gone\nMimeType=all/all;\n{keys}"),
            );
        }
        write(
            "viewer.desktop",
            "[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\n",
        );
        write(
            "editor.desktop",
            "[Desktop Entry]\nType=Application\nName=Editor\nExec=edit %f\nMimeType=all/all;\n",
        );
        write(
            "mimeapps.list",
            "[Default Applications]\nall/all=gone.desktop;viewer.desktop\n",
        );
        let file = dir.join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        let sources = Sources::from_dirs(&[], &[dir.path().to_path_buf()]);

        let candidates = candidates_from(&sources, &file);

        let rows: Vec<(&str, bool)> = candidates
            .iter()
            .map(|candidate| (candidate.name.as_str(), candidate.is_default))
            .collect();
        assert_eq!(vec![("Viewer", true), ("Editor", false)], rows);
    }

    // First and Second are not associated, so only the defaults can list them.
    #[test_case("first.desktop;second.desktop", &[("First", true), ("Viewer", false), ("Editor", false)] ; "only the first offerable default is hoisted")]
    #[test_case("editor.desktop", &[("Editor", true), ("Viewer", false)] ; "an associated default is listed once")]
    fn candidates_from_lists_each_entry_once(defaults: &str, expected: &[(&str, bool)]) {
        Config::init_test();
        let dir = TempDir::new("open_with_default_rows");
        let applications = dir.join("applications");
        std::fs::create_dir_all(&applications).unwrap();
        let write = |name: &str, body: &str| std::fs::write(applications.join(name), body).unwrap();
        write(
            "first.desktop",
            "[Desktop Entry]\nType=Application\nName=First\nExec=first %f\n",
        );
        write(
            "second.desktop",
            "[Desktop Entry]\nType=Application\nName=Second\nExec=second %f\n",
        );
        write(
            "viewer.desktop",
            "[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\nMimeType=all/all;\n",
        );
        write(
            "editor.desktop",
            "[Desktop Entry]\nType=Application\nName=Editor\nExec=edit %f\nMimeType=all/all;\n",
        );
        write(
            "mimeapps.list",
            &format!(
                "[Default Applications]\nall/all={defaults}\n[Added Associations]\nall/all=viewer.desktop;editor.desktop\n"
            ),
        );
        let file = dir.join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        let sources = Sources::from_dirs(&[], &[dir.path().to_path_buf()]);

        let candidates = candidates_from(&sources, &file);

        let rows: Vec<(&str, bool)> = candidates
            .iter()
            .map(|candidate| (candidate.name.as_str(), candidate.is_default))
            .collect();
        assert_eq!(expected, rows.as_slice());
    }

    #[test]
    fn a_hidden_top_association_is_not_the_fallback_default() {
        Config::init_test();
        let dir = TempDir::new("open_with_hidden_fallback");
        let applications = dir.join("applications");
        std::fs::create_dir_all(&applications).unwrap();
        let write = |name: &str, body: &str| std::fs::write(applications.join(name), body).unwrap();
        write(
            "gone.desktop",
            "[Desktop Entry]\nType=Application\nName=Gone\nExec=gone %f\nMimeType=all/all;\nHidden=true\n",
        );
        write(
            "viewer.desktop",
            "[Desktop Entry]\nType=Application\nName=Viewer\nExec=view %f\nMimeType=all/all;\n",
        );
        write(
            "mimeapps.list",
            "[Added Associations]\nall/all=gone.desktop;viewer.desktop\n",
        );
        let file = dir.join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        let sources = Sources::from_dirs(&[], &[dir.path().to_path_buf()]);

        let candidates = candidates_from(&sources, &file);

        let defaults: Vec<&str> = candidates
            .iter()
            .filter(|candidate| candidate.is_default)
            .map(|candidate| candidate.name.as_str())
            .collect();
        assert_eq!(vec!["Viewer"], defaults);
    }

    #[test]
    fn an_entrys_working_directory_is_taken_only_when_it_names_one() {
        Config::init_test();
        let dir = TempDir::new("open_with_working_dir");
        let data = data_dir_with_applications(&dir);
        let file = dir.join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        let sources = Sources::from_dirs(&[], &[data]);

        let candidates = candidates_from(&sources, &file);

        let working_dir = |name: &str| {
            candidates
                .iter()
                .find(|candidate| candidate.name == name)
                .and_then(|candidate| candidate.working_dir.clone())
        };
        assert_eq!(Some(PathBuf::from("/var/empty")), working_dir("Editor"));
        assert_eq!(None, working_dir("Viewer"));
        assert_eq!(None, working_dir("Blank"));
    }

    #[test_case(None, None                          ; "unset falls back to the spec default")]
    #[test_case(Some(""), None                      ; "empty falls back rather than naming the current directory")]
    #[test_case(Some("/x/data"), Some("/x/data")    ; "a directory is taken as given")]
    #[test_case(Some("data"), None                  ; "a relative directory is ignored")]
    fn dir_of_reads_a_single_directory_variable(value: Option<&str>, expected: Option<&str>) {
        let expected = expected.map(PathBuf::from);

        assert_eq!(expected, dir_of(value.map(OsString::from)));
    }

    #[test_case(None, &["/usr/share"]                 ; "unset uses the fallback")]
    #[test_case(Some(""), &["/usr/share"]             ; "empty uses the fallback")]
    #[test_case(Some("/a:/b"), &["/a", "/b"]          ; "each component is a directory")]
    #[test_case(Some("/a::/b"), &["/a", "/b"]         ; "an empty component is dropped")]
    #[test_case(Some(":/a"), &["/a"]                  ; "a leading separator is not a directory")]
    #[test_case(Some("share:/a"), &["/a"]             ; "a relative component is dropped")]
    fn dirs_of_splits_a_search_path(value: Option<&str>, expected: &[&str]) {
        let expected: Vec<PathBuf> = expected.iter().map(PathBuf::from).collect();

        assert_eq!(expected, dirs_of(value.map(OsString::from), "/usr/share"));
    }

    #[test_case("", &[]                             ; "unset names no desktop")]
    #[test_case("GNOME", &["gnome"]                  ; "a single name is lowercased")]
    #[test_case("ubuntu:GNOME", &["ubuntu", "gnome"] ; "each name in order")]
    #[test_case("GNOME:", &["gnome"]                 ; "a trailing separator names nothing")]
    #[test_case(" GNOME ", &["gnome"]                ; "surrounding space is not part of the name")]
    fn desktops_of_names_the_lists_to_read(value: &str, expected: &[&str]) {
        assert_eq!(strings(expected), desktops_of(value));
    }

    #[test]
    fn dedupe_dirs_keeps_the_first_of_each_directory() {
        let dirs = ["/usr/share", "/usr/local/share", "/usr/share", "/opt/share"]
            .iter()
            .map(PathBuf::from)
            .collect();

        assert_eq!(
            vec![
                PathBuf::from("/usr/share"),
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/opt/share"),
            ],
            dedupe_dirs(dirs)
        );
    }

    #[test]
    fn in_terminal_passes_the_command_as_arguments() {
        let argv = [OsString::from("vim"), OsString::from("/a b.txt")];
        let wrapped = in_terminal("xterm -e %s", &argv).unwrap();
        let expected: Vec<OsString> = ["sh", "-c", "xterm -e \"$@\"", "sh", "vim", "/a b.txt"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(expected, wrapped);
    }

    #[test_case("" ; "empty")]
    #[test_case(" \t" ; "only whitespace")]
    fn in_terminal_declines_when_no_terminal_is_configured(template: &str) {
        assert_eq!(None, in_terminal(template, &[OsString::from("vim")]));
    }

    #[test]
    fn scan_mime_types_reads_the_desktop_entry_group() {
        let text = "[Desktop Entry]\n\
                    Name=Viewer\n\
                    MimeType=application/pdf;image/png;\n\
                    \n\
                    [Desktop Action new]\n\
                    Name=New\n";
        assert_eq!(
            strings(&["application/pdf", "image/png"]),
            scan_mime_types(text)
        );
    }

    #[test_case("MimeType=text/plain;" ; "no space")]
    #[test_case("MimeType = text/plain;" ; "spaces around the equals sign")]
    #[test_case("MimeType\t=text/plain" ; "a tab before the equals sign")]
    fn scan_mime_types_ignores_whitespace_around_the_equals_sign(line: &str) {
        let text = format!("[Desktop Entry]\n{line}\n");
        assert_eq!(strings(&["text/plain"]), scan_mime_types(&text));
    }

    #[test_case("Name=Viewer" ; "no group header")]
    #[test_case("[Desktop Entry]\nName=Viewer" ; "no MimeType key")]
    #[test_case("[Desktop Entry]\nMimeTypes=text/plain" ; "a longer key")]
    #[test_case("[Desktop Entry]\nX-MimeType=text/plain" ; "a key ending in MimeType")]
    #[test_case("[Desktop Entry]\nMimeType=;;" ; "no usable values")]
    #[test_case("[Desktop Action new]\nMimeType=application/pdf" ; "only in a later group")]
    fn scan_mime_types_returns_nothing(text: &str) {
        assert!(scan_mime_types(text).is_empty());
    }

    #[test]
    fn parse_subclasses_reads_child_parent_pairs() {
        let text = "text/markdown text/plain\n\
                    \n\
                    application/toml text/plain\n\
                    malformed\n";
        assert_eq!(
            vec![
                ("text/markdown".to_string(), "text/plain".to_string()),
                ("application/toml".to_string(), "text/plain".to_string()),
            ],
            parse_subclasses(text)
        );
    }
}
