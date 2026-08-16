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
use log::debug;
use xdg_mime::SharedMimeInfo;

use super::{
    AppCandidate,
    exec::{ExecContext, expand, file_uri},
    mimeapps::{self, AppDirIndex, Level, MimeAppsList},
};
use crate::{app::config::Config, file_system::shell};

/// Matches every file, per the mime-apps spec's fallback types.
const ALL_FILES: &str = "all/allfiles";
const ALL: &str = "all/all";
const TEXT_PLAIN: &str = "text/plain";
/// `guess()` returns this for a zero byte file without ever consulting the
/// glob database, which would otherwise hide every handler for, say, a newly
/// created and still empty `notes.md`.
const ZEROSIZE: &str = "application/x-zerosize";

/// Parsing the shared MIME database reads every glob, magic, alias, and
/// subclass file, so it is done once for the life of the process.
static MIME_DB: OnceLock<SharedMimeInfo> = OnceLock::new();

fn mime_db() -> &'static SharedMimeInfo {
    MIME_DB.get_or_init(|| {
        let started = Instant::now();
        let db = SharedMimeInfo::new();
        debug!("Loaded the shared MIME database in {:?}", started.elapsed());
        db
    })
}

/// Child type to its direct parents.
///
/// `SharedMimeInfo::get_parents` cannot be used: it resolves through the alias
/// table first and returns `None` on a miss, which every type that is not itself
/// an alias produces, cutting those files off from their parents' handlers.
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
    // Every text/* type is a subclass of text/plain whether or not the database
    // says so explicitly.
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
    /// Build from the XDG environment. Returns empty sources rather than
    /// panicking when the home directory cannot be determined.
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

/// Building the sources walks every `applications` tree and reads every desktop
/// file in it, so it is done once for the life of the process. An application
/// installed while FileCTRL is running is not offered until the next start.
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
    associations
        .ordered
        .iter()
        .filter_map(|id| {
            let file = mimeapps::resolve(&sources.levels, id)?;
            let entry = DesktopEntry::from_path(file, None::<&[&str]>)
                .inspect_err(|error| debug!("Skipping {}: {error}", file.display()))
                .ok()?;
            let is_default = associations.default.as_deref() == Some(id);
            to_candidate(&locales, path, is_default, &entry)
        })
        .collect()
}

/// The name the glob rules are matched against. Lossy, because the rules are
/// patterns over a string and `guess` drops a name it cannot convert, leaving
/// `caf\xe9.txt` unmatched by `*.txt` and openable by nothing. A replacement
/// character cannot create a false match, since no rule contains one, and the
/// conversion stops here: the path reaches the program as its own bytes.
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
    // Set before the path, which fills the name in only when it is still
    // unset, so this is what the globs see.
    if let Some(file_name) = &file_name {
        builder.file_name(file_name);
    }
    let guessed = builder.path(path).guess();
    let mut queue: VecDeque<String> = VecDeque::new();
    // An empty file short circuits the guess before the globs are consulted,
    // so recover what the file name alone implies. The zero size type itself
    // is then dropped: it says nothing about an empty `notes.md`, and keeping
    // it would rank it above text/plain.
    let from_name = (guessed.mime_type().essence_str() == ZEROSIZE)
        .then_some(file_name.as_deref())
        .flatten()
        .map(|name| db.get_mime_types_from_file_name(name))
        .filter(|types| !types.is_empty());
    match from_name {
        Some(types) => queue.extend(types.iter().map(canonical)),
        None => queue.push_back(canonical(guessed.mime_type())),
    }

    // Breadth first, so nearer ancestors stay ahead of more distant ones.
    let mut chain = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(current) = queue.pop_front() {
        if !seen.insert(current.clone()) {
            continue;
        }
        queue.extend(parents_of(&current));
        chain.push(current);
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

/// The same, for a type read from a `MimeType=` line or a mimeapps.list key.
/// Those are matched against the lookup chain by string, so both sides have to
/// be canonical or an entry that declares an alias never matches.
fn canonical_str(mime: &str) -> String {
    mime.parse::<mime::Mime>()
        .map_or_else(|_| mime.to_string(), |parsed| canonical(&parsed))
}

fn to_candidate(
    locales: &[String],
    path: &Path,
    is_default: bool,
    entry: &DesktopEntry,
) -> Option<AppCandidate> {
    let file = entry.path.as_path();
    if entry.type_() != Some("Application") || entry.hidden() {
        return None;
    }
    // NoDisplay entries are deliberately kept: the spec says the key exists so
    // that an application can be associated with a MIME type without appearing
    // in menus.
    if let Some(try_exec) = entry.try_exec()
        && !is_installed(try_exec)
    {
        debug!(
            "Skipping {}: TryExec {try_exec:?} is not installed",
            file.display()
        );
        return None;
    }
    let name = entry
        .name(locales)
        .map_or_else(|| entry.appid.clone(), std::borrow::Cow::into_owned);
    let context = ExecContext {
        desktop_file: file,
        icon: entry.icon(),
        name: &name,
        path,
        uri: &file_uri(path),
    };
    let mut argv = expand(&context, entry.exec()?)
        .inspect_err(|error| debug!("Skipping {}: {error}", file.display()))
        .ok()?;
    if entry.terminal() {
        // A terminal application launched with null stdio does nothing at all,
        // so it is only worth offering when a terminal is configured to host it.
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
        // The desktop id is unique by construction, so it always tells two
        // similarly named entries apart. A program name need not: several
        // entries can share one wrapper such as `env` or `flatpak`.
        detail: entry.appid.clone(),
        is_default,
        name,
        working_dir: entry
            .path()
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from),
        argv,
    })
}

/// Wrap `argv` in the configured terminal. Unlike the path openers, `%s` stands
/// for a command line, so it takes the arguments joined and quoted individually
/// where needed: `xterm -e %s` becomes `xterm -e vim '/a b'`, not
/// `xterm -e 'vim /a b'`. `None` when no terminal is configured.
fn in_terminal(template: &str, argv: &[OsString]) -> Option<Vec<OsString>> {
    if template.is_empty() {
        return None;
    }
    let command = shell::template(template, &shell::join(argv));
    Some(vec![OsString::from("sh"), OsString::from("-c"), command])
}

/// Whether a `TryExec` value names an executable that exists. Only an absolute
/// value is a path; the spec looks anything else up in `$PATH`.
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

/// Index the `.desktop` files under `dir`, scanning for `MimeType=` rather than
/// fully parsing each one: only the handful that end up being offered are
/// parsed in full.
fn index_applications(dir: &Path) -> AppDirIndex {
    let mut index = AppDirIndex::default();
    let mut queue = VecDeque::from([dir.to_path_buf()]);
    // `is_dir` follows symlinks, so without this a link back to an ancestor
    // would spin forever on the thread that draws the UI.
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

/// The identity a directory is tracked by while walking, so that two paths
/// reaching the same directory through a symlink compare equal.
fn canonical_dir(dir: &Path) -> PathBuf {
    fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// Read the `MimeType=` values from the `[Desktop Entry]` group of a desktop
/// file. Later groups (`[Desktop Action ...]`) are not associations.
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
        if let Some(values) = line.strip_prefix("MimeType=") {
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

/// Drop repeated directories, comparing through symlinks. `$XDG_DATA_DIRS`
/// commonly names the same directory twice, which would otherwise be walked and
/// indexed once per occurrence.
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
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn env_dirs(name: &str, fallback: &str) -> Vec<PathBuf> {
    let value = env::var_os(name).filter(|value| !value.is_empty());
    let value = value.unwrap_or_else(|| fallback.into());
    env::split_paths(&value)
        .filter(|dir| !dir.as_os_str().is_empty())
        .collect()
}

/// Each entry of `$XDG_CURRENT_DESKTOP`, lowercased, in order. These name the
/// `$desktop-mimeapps.list` files that take precedence over the generic one.
fn current_desktops() -> Vec<String> {
    env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
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

    use std::{ffi::OsString, path::PathBuf};

    use super::{dedupe_dirs, glob_name, in_terminal, parse_subclasses, scan_mime_types};

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn glob_name_keeps_the_extension_of_a_name_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt;

        let path = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/caf\xe9.txt"));

        // The glob rules are patterns over a string, so a name that cannot be
        // converted has to be converted lossily rather than dropped: dropping
        // it leaves `*.txt` unmatched and the picker offers nothing.
        let name = glob_name(&path).expect("a file name");
        // The assertion is over the glob string this builds, not over a
        // path's extension, so the case-insensitive Path::extension check
        // the lint suggests would test something else.
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        let ends_with_txt = name.ends_with(".txt");
        assert!(ends_with_txt, "{name}");
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

    #[test_case(&["vim", "/a/b.txt"], "sh -c xterm -e vim /a/b.txt" ; "no quoting needed")]
    #[test_case(&["vim", "/a b.txt"], "sh -c xterm -e vim '/a b.txt'" ; "only the argument that needs it is quoted")]
    #[test_case(&["vim"], "sh -c xterm -e vim" ; "a single argument")]
    fn in_terminal_substitutes_a_command_line(argv: &[&str], expected: &str) {
        let argv: Vec<OsString> = argv.iter().map(OsString::from).collect();
        let wrapped = in_terminal("xterm -e %s", &argv).unwrap();
        let words: Vec<String> = wrapped
            .iter()
            .map(|word| word.to_string_lossy().into_owned())
            .collect();
        assert_eq!(expected, words.join(" "));
    }

    #[test]
    fn in_terminal_declines_when_no_terminal_is_configured() {
        assert_eq!(None, in_terminal("", &[OsString::from("vim")]));
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

    #[test_case("Name=Viewer" ; "no group header")]
    #[test_case("[Desktop Entry]\nName=Viewer" ; "no MimeType key")]
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
