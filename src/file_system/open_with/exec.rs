//! Expansion of a desktop entry's `Exec=` value into an argv.
//!
//! <https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html>

use std::{
    ffi::{OsStr, OsString},
    fmt::Write as _,
    os::unix::ffi::OsStrExt,
    path::Path,
};

use anyhow::{Result, anyhow};

use crate::file_system::shell;

/// Follows the '%' of a field code that `mark_quoted_codes` found inside
/// quotes. A private use character, so no meaningful `Exec` contains one.
const QUOTED_CODE: char = '\u{E000}';

/// The values substituted into a desktop entry's `Exec=` field codes.
pub(super) struct ExecContext<'a> {
    /// Path of the `.desktop` file itself (`%k`).
    pub(super) desktop_file: &'a Path,
    /// `Icon=` value, if any (`%i`).
    pub(super) icon: Option<&'a str>,
    /// Localized `Name=` (`%c`).
    pub(super) name: &'a str,
    /// The file or directory being opened (`%f`, `%F`).
    pub(super) path: &'a Path,
    /// `file://` URI of `path` (`%u`, `%U`).
    pub(super) uri: &'a str,
}

/// Expand `exec` into an argv suitable for `std::process::Command`.
///
/// `DesktopEntry::parse_exec` is deliberately not used: it splits on ASCII
/// whitespace (which tears apart a quoted program path), substitutes a field
/// code only when it is an entire token (so `--file=%f` is passed through
/// literally), and rejects any `Exec` whose first token contains '='
/// (`env FOO=1 app %f`).
pub(super) fn expand(context: &ExecContext<'_>, exec: &str) -> Result<Vec<OsString>> {
    // The desktop entry string escapes are undone before the quoting rules are
    // applied, so a literal backslash inside a quoted argument is written as
    // four backslashes.
    let unescaped = mark_quoted_codes(&unescape_value(exec));

    // The spec's quoting (double quotes, backslash-escaping of " ` $ \) is a
    // subset of POSIX quoting.
    let tokens = shell_words::split(&unescaped)
        .map_err(|error| anyhow!("Malformed Exec {exec:?}: {error}"))?;

    if let Some(script) = shell_script(&tokens)
        && has_substituting_code(script)
    {
        return Err(anyhow!(
            "Exec {exec:?}: a field code inside a shell's -c script cannot be passed safely"
        ));
    }

    // Only ever one path, so %F and %U behave as %f and %u.
    let mut argv: Vec<OsString> = Vec::with_capacity(tokens.len() + 1);
    let mut consumed_path = false;
    for mut token in tokens {
        // A quoted argument that is nothing but one field code (`app "%f"`)
        // becomes a single argv element that no shell reads, so the value is
        // passed raw, as it would be unquoted.
        if let [first, QUOTED_CODE, _] = token.chars().collect::<Vec<_>>()[..]
            && first == '%'
        {
            token.remove(first.len_utf8());
        }
        // The only code that expands to more than one argument.
        if token == "%i" {
            if let Some(icon) = context.icon {
                argv.push(OsString::from("--icon"));
                argv.push(OsString::from(icon));
            }
            continue;
        }
        let (expanded, used_path) =
            expand_in_token(context, &token).map_err(|error| anyhow!("Exec {exec:?}: {error}"))?;
        consumed_path |= used_path;
        // A token that was nothing but dropped field codes (deprecated ones
        // included) is not an empty argument, but a literal "" is.
        if !expanded.is_empty() || !token.contains('%') {
            argv.push(expanded);
        }
    }

    if argv.is_empty() {
        return Err(anyhow!("Exec {exec:?} is empty"));
    }
    // An entry that declares no file field code takes no argument, but the user
    // picked it to open this path, so append it rather than launch the
    // application against nothing. A program that rejects the extra argument
    // exits straight away and is reported.
    if !consumed_path {
        argv.push(context.path.as_os_str().to_os_string());
    }
    Ok(argv)
}

/// Programs that read the argument after `-c` as a shell script.
const SHELLS: [&str; 9] = [
    "ash", "bash", "csh", "dash", "fish", "ksh", "mksh", "sh", "zsh",
];

/// The script a shell among `tokens` is given with `-c`, if any. A field code
/// inside it is refused rather than quoted: quoting cannot follow what a script
/// does with the text (a here-document, `eval`, a nested `sh -c`), and the name
/// can be passed safely as an argument after the script instead
/// (`sh -c 'mpv "$1"' sh %f`).
fn shell_script(tokens: &[String]) -> Option<&str> {
    let start = tokens.iter().position(|token| {
        let program = token.rsplit('/').next().unwrap_or(token);
        SHELLS.contains(&program)
    })?;
    let mut has_script_option = false;
    let mut rest = tokens[start + 1..].iter();
    while let Some(token) = rest.next() {
        let Some(options) = token.strip_prefix(['-', '+']) else {
            // The first operand: the script when `-c` was given.
            return has_script_option.then_some(token.as_str());
        };
        if options.starts_with('-') {
            // A long option such as `--login` or `--norc`.
            continue;
        }
        has_script_option |= options.contains('c');
        // `-o` and `-O` take the next argument as their value.
        if options.ends_with(['o', 'O']) {
            rest.next();
        }
    }
    None
}

/// Whether `token` holds a field code that substitutes a value, as opposed to a
/// literal percent or a code that expands to nothing.
fn has_substituting_code(token: &str) -> bool {
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            continue;
        }
        let mut code = chars.next();
        if code == Some(QUOTED_CODE) {
            code = chars.next();
        }
        if matches!(code, Some('f' | 'F' | 'u' | 'U' | 'c' | 'k')) {
            return true;
        }
    }
    false
}

/// Substitute the field codes appearing anywhere within a single argument, so
/// that `--file=%f` works as well as a bare `%f`. Returns the expansion and
/// whether it consumed the path.
///
/// A code that was inside quotes is substituted shell quoted, as glib does. The
/// spec leaves that case undefined, and in practice the quoted argument is a
/// script, where a raw name would be run as shell code. A shell that
/// `shell_script` knows is refused before this; the quoting covers one it does
/// not, such as a shell run under another name (`runner -c "mpv %f"`).
///
/// Single quoting is only inert where the script itself has no quote open: in
/// `runner -c "echo \"%f\""` the script's double quotes make the single quotes
/// literal, and a `$(...)` in the name would run. The script's own quote state
/// is therefore tracked, and a code inside it is refused rather than guessed
/// at, so the entry is not offered.
fn expand_in_token(context: &ExecContext<'_>, token: &str) -> Result<(OsString, bool)> {
    let mut expanded = OsString::with_capacity(token.len());
    let mut consumed_path = false;
    let mut script = shell::Quotes::default();
    let mut chars = token.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            script.advance(c);
            expanded.push(c.encode_utf8(&mut [0u8; 4]));
            continue;
        }
        let quoted = chars.next_if_eq(&QUOTED_CODE).is_some();
        // Pushed as an `OsStr`, so a name that is not valid UTF-8 reaches the
        // program intact rather than as replacement characters.
        let value = match chars.next() {
            Some('%') => OsStr::new("%"),
            Some('f' | 'F') => {
                consumed_path = true;
                context.path.as_os_str()
            }
            Some('u' | 'U') => {
                consumed_path = true;
                OsStr::new(context.uri)
            }
            Some('c') => OsStr::new(context.name),
            Some('k') => context.desktop_file.as_os_str(),
            // Deprecated, unrecognized, %i in a position where it cannot expand
            // to two arguments, and a trailing '%' are all dropped, so they
            // put nothing into the script that could need quoting.
            _ => continue,
        };
        if !quoted {
            expanded.push(value);
        } else if script.is_open() {
            return Err(anyhow!(
                "a field code inside quotes within a quoted argument cannot be quoted safely"
            ));
        } else {
            expanded.push(shell::quote(value));
        }
    }
    Ok((expanded, consumed_path))
}

/// Follow the '%' of every field code that sits inside single or double quotes
/// with `QUOTED_CODE`, so `expand_in_token` can still tell after the quotes are
/// removed. The quote state is tracked the way `shell_words::split` tracks it.
///
/// A code outside quotes is marked too when its argument holds whitespace that
/// was quoted or escaped (`runner -c echo\ %f`, `runner -c "echo "%f`): only a
/// script is written as one argument with spaces in it, and the shell reading
/// that script would run a raw name as code.
fn mark_quoted_codes(exec: &str) -> String {
    enum State {
        Delimiter,
        Unquoted,
        Backslash { in_word: bool },
        Single,
        Double,
        DoubleBackslash,
        Comment,
    }

    let mut marked = String::with_capacity(exec.len());
    // The argument being read, with the offsets just after each '%' of a code
    // outside quotes, which are marked once the argument turns out to be a
    // script.
    let mut word = String::new();
    let mut unquoted_codes = Vec::new();
    let mut is_script = false;
    let mut state = State::Delimiter;
    let mut chars = exec.chars().peekable();
    loop {
        let next = chars.next();
        if next.is_none() || matches!(state, State::Delimiter) {
            if is_script {
                for &offset in unquoted_codes.iter().rev() {
                    word.insert(offset, QUOTED_CODE);
                }
            }
            marked.push_str(&word);
            word.clear();
            unquoted_codes.clear();
            is_script = false;
        }
        let Some(c) = next else {
            break;
        };
        word.push(c);
        let quoted = matches!(
            state,
            State::Single | State::Double | State::DoubleBackslash
        );
        if c == '%' {
            // "%%" is a literal percent rather than a code.
            if let Some(percent) = chars.next_if_eq(&'%') {
                word.push(percent);
            } else if quoted {
                word.push(QUOTED_CODE);
            } else {
                unquoted_codes.push(word.len());
            }
        }
        is_script |= match state {
            State::Single | State::Double | State::DoubleBackslash => {
                matches!(c, ' ' | '\t' | '\n')
            }
            // An escaped newline continues the line rather than escaping it.
            State::Backslash { .. } => matches!(c, ' ' | '\t'),
            _ => false,
        };
        state = match (state, c) {
            (State::Delimiter | State::Unquoted, ' ' | '\t' | '\n')
            | (State::Backslash { in_word: false } | State::Comment, '\n') => State::Delimiter,
            (State::Delimiter, '#') | (State::Comment, _) => State::Comment,
            (State::Delimiter | State::Unquoted, '\'') => State::Single,
            (State::Delimiter | State::Unquoted, '"') => State::Double,
            (State::Delimiter, '\\') => State::Backslash { in_word: false },
            (State::Unquoted, '\\') => State::Backslash { in_word: true },
            (State::Delimiter | State::Unquoted | State::Backslash { .. }, _)
            | (State::Single, '\'')
            | (State::Double, '"') => State::Unquoted,
            (State::Single, _) => State::Single,
            (State::Double, '\\') => State::DoubleBackslash,
            (State::Double | State::DoubleBackslash, _) => State::Double,
        };
    }
    marked
}

/// The `file://` URI of an absolute path, for the `%u` and `%U` field codes.
/// Everything outside the RFC 3986 unreserved set is percent encoded, from the
/// raw bytes: a lossy conversion would percent encode replacement characters
/// rather than the name they stood in for.
pub(super) fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(byte as char);
            }
            // Writing into the string rather than formatting a new one per byte.
            // Writing to a String is infallible, so the Result cannot be an error.
            _ => {
                let _ = write!(uri, "%{byte:02X}");
            }
        }
    }
    uri
}

/// Undo the escape sequences that the desktop entry spec defines for values of
/// type string. Any other backslash sequence is left alone.
fn unescape_value(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_string();
    }
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
            // An escaped backslash, and a trailing one at the end of the
            // value, both yield a single backslash.
            Some('\\') | None => unescaped.push('\\'),
            Some(other) => {
                unescaped.push('\\');
                unescaped.push(other);
            }
        }
    }
    unescaped
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use test_case::test_case;

    use super::{ExecContext, expand, file_uri, unescape_value};

    /// The expansion as plain strings, for comparing against the expected argv.
    fn expanded(exec: &str, context: &ExecContext<'_>) -> Vec<String> {
        expand(context, exec)
            .unwrap()
            .iter()
            .map(|word| word.to_string_lossy().into_owned())
            .collect()
    }

    const PATH: &str = "/home/u/report.pdf";
    const URI: &str = "file:///home/u/report.pdf";

    fn context() -> ExecContext<'static> {
        ExecContext {
            desktop_file: Path::new("/usr/share/applications/viewer.desktop"),
            icon: Some("viewer-icon"),
            name: "Viewer",
            path: Path::new(PATH),
            uri: URI,
        }
    }

    #[test_case("app %f", &["app", PATH] ; "bare file code")]
    #[test_case("app %F", &["app", PATH] ; "multi file code takes the one path")]
    #[test_case("app %u", &["app", URI] ; "bare uri code")]
    #[test_case("app %U", &["app", URI] ; "multi uri code takes the one uri")]
    #[test_case("app --file=%f", &["app", "--file=/home/u/report.pdf"] ; "code inside a token")]
    #[test_case("\"/opt/my app/bin\" %U", &["/opt/my app/bin", URI] ; "quoted program path")]
    #[test_case("app", &["app", PATH] ; "no field code appends the path")]
    #[test_case("app %c %k", &["app", "Viewer", "/usr/share/applications/viewer.desktop", PATH] ; "name and desktop file codes")]
    #[test_case("app %i %f", &["app", "--icon", "viewer-icon", PATH] ; "icon expands to two arguments")]
    #[test_case("app %% %f", &["app", "%", PATH] ; "escaped percent")]
    #[test_case("env FOO=1 app %f", &["env", "FOO=1", "app", PATH] ; "equals sign in the first token")]
    #[test_case("app %d %v %f", &["app", PATH] ; "deprecated codes are dropped")]
    #[test_case("app %z %f", &["app", PATH] ; "unknown code is dropped")]
    // Escapes are undone before the quoting rules are applied, so \s becomes a
    // real separator; quoting is the only way to get a space inside one token.
    #[test_case("app\\sname %f", &["app", "name", PATH] ; "escaped space separates tokens")]
    #[test_case("\"app name\" %f", &["app name", PATH] ; "quoting keeps a space inside one token")]
    #[test_case("app \"a\\\\\\\\b\" %f", &["app", "a\\b", PATH] ; "four backslashes are one literal backslash")]
    #[test_case("app \"\" %f", &["app", "", PATH] ; "an explicitly empty argument is kept")]
    #[test_case("flatpak run --command=\"foo bar\" org.x %U", &["flatpak", "run", "--command=foo bar", "org.x", URI] ; "quoting inside a token")]
    fn expand_produces(exec: &str, expected: &[&str]) {
        assert_eq!(expected, expanded(exec, &context()).as_slice());
    }

    #[test_case("app \"unmatched" ; "unmatched quote")]
    #[test_case("" ; "empty")]
    #[test_case("   " ; "only whitespace")]
    fn expand_rejects(exec: &str) {
        assert!(expand(&context(), exec).is_err());
    }

    #[test]
    fn expand_drops_the_icon_code_when_there_is_no_icon() {
        let mut context = context();
        context.icon = None;
        assert_eq!(
            vec!["app".to_string(), PATH.to_string()],
            expanded("app %i %f", &context)
        );
    }

    #[test]
    fn expand_preserves_a_name_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::OsStr::from_bytes(b"/tmp/caf\xe9.txt");
        let path = Path::new(name);
        let uri = file_uri(path);
        let context = ExecContext {
            path,
            uri: &uri,
            ..context()
        };

        // A lossy conversion would hand the program U+FFFD instead of 0xe9,
        // and it would open nothing.
        let argv = expand(&context, "app %f").unwrap();
        assert_eq!(name, argv[1]);
        // The URI encodes the byte itself rather than a replacement character.
        assert_eq!("file:///tmp/caf%E9.txt", uri);
    }

    /// A name that runs a command if a shell ever reads it unquoted.
    const HOSTILE: &str = "/v/x$(touch pwned).mp4";

    fn hostile_context() -> ExecContext<'static> {
        ExecContext {
            path: Path::new(HOSTILE),
            ..context()
        }
    }

    // `run` stands for an interpreter that is not known to be a shell, whose
    // script is quoted for rather than refused. Inside quotes the argument is
    // typically a script, so the name is quoted for the shell that will read it.
    #[test_case("run -c \"mpv %f\"", &["run", "-c", "mpv '/v/x$(touch pwned).mp4'"] ; "double quoted script")]
    #[test_case("run -c 'mpv %f'", &["run", "-c", "mpv '/v/x$(touch pwned).mp4'"] ; "single quoted script")]
    // A quoted argument that is only the code is one argv element, so it is
    // passed raw.
    #[test_case("app \"%f\"", &["app", HOSTILE] ; "a quoted code on its own")]
    #[test_case("app '%f'", &["app", HOSTILE] ; "a single quoted code on its own")]
    // Only a token that is exactly the code is passed raw; one that starts with
    // it is still a script.
    #[test_case("run -c \"%f --flag\"", &["run", "-c", "'/v/x$(touch pwned).mp4' --flag"] ; "a quoted script that starts with the code")]
    #[test_case("run -c \"printf %%s %f\"", &["run", "-c", "printf %s '/v/x$(touch pwned).mp4'"] ; "quoted percent stays literal")]
    // Outside quotes the value is one argv element and no shell reads it, so it
    // is passed raw, embedded or not.
    #[test_case("mpv %f", &["mpv", HOSTILE] ; "bare code is raw")]
    #[test_case("mpv --file=%f", &["mpv", "--file=/v/x$(touch pwned).mp4"] ; "unquoted embedded code is raw")]
    // A single quote inside double quotes opens nothing, so the code after the
    // closing double quote is outside quotes.
    #[test_case("app \"it's\" %f", &["app", "it's", HOSTILE] ; "a quote inside the other kind")]
    #[test_case("app 'x' --file=%f", &["app", "x", "--file=/v/x$(touch pwned).mp4"] ; "a code after a closed single quote")]
    #[test_case("app \\'%f", &["app", "'/v/x$(touch pwned).mp4"] ; "an escaped quote opens nothing")]
    fn expand_quotes_only_codes_inside_quotes(exec: &str, expected: &[&str]) {
        assert_eq!(expected, expanded(exec, &hostile_context()).as_slice());
    }

    // Whitespace that was quoted or escaped makes the argument a script, so a
    // code outside the quotes is quoted for the shell that reads it too.
    #[test_case(r"run -c echo\\ %f", &["run", "-c", "echo '/v/x$(touch pwned).mp4'"] ; "after an escaped space")]
    #[test_case(r"run -c %f\\ --flag", &["run", "-c", "'/v/x$(touch pwned).mp4' --flag"] ; "before an escaped space")]
    #[test_case(r"run -c echo\\ %c\\ %f", &["run", "-c", "echo Viewer '/v/x$(touch pwned).mp4'"] ; "two codes in one script")]
    #[test_case(r#"run -c "echo "%f"#, &["run", "-c", "echo '/v/x$(touch pwned).mp4'"] ; "after a quoted space")]
    #[test_case(r"run -c printf\\ %%s\\ %f", &["run", "-c", "printf %s '/v/x$(touch pwned).mp4'"] ; "an unquoted percent stays literal")]
    // Quoting with no whitespace in it leaves the argument a plain value.
    #[test_case(r#"app "x"%f"#, &["app", "x/v/x$(touch pwned).mp4"] ; "after a quoted word")]
    fn a_code_in_an_argument_with_quoted_whitespace_is_quoted(exec: &str, expected: &[&str]) {
        assert_eq!(expected, expanded(exec, &hostile_context()).as_slice());
    }

    // Inside the script's own quotes a single-quoted value is literal text, so
    // the name would be read as shell code: the entry is refused instead.
    #[test_case(r#"run -c "echo \"%f\"""# ; "inside the script's double quotes")]
    #[test_case(r#"run -c "echo '%f'""# ; "inside the script's single quotes")]
    #[test_case(r#"run -c "echo \\%f""# ; "after a backslash in the script")]
    fn a_code_quoted_within_a_quoted_script_is_refused(exec: &str) {
        let error = expand(&hostile_context(), exec)
            .expect_err("the entry must not be offered")
            .to_string();
        assert!(error.ends_with("cannot be quoted safely"), "{error}");
    }

    // A code that expands to nothing puts nothing into the script, so where it
    // sits does not matter.
    #[test_case(r#"run -c "echo '%d' %f""#, &["run", "-c", "echo '' '/v/x$(touch pwned).mp4'"] ; "a deprecated code")]
    #[test_case(r#"run -c "echo '%i' %f""#, &["run", "-c", "echo '' '/v/x$(touch pwned).mp4'"] ; "an icon code inside an argument")]
    fn a_code_that_expands_to_nothing_inside_the_scripts_quotes_is_dropped(
        exec: &str,
        expected: &[&str],
    ) {
        assert_eq!(expected, expanded(exec, &hostile_context()).as_slice());
    }

    // The script's quotes are closed again before the code, so single quoting
    // is inert there.
    #[test_case(r#"run -c "echo \"a\" %f""#, &["run", "-c", "echo \"a\" '/v/x$(touch pwned).mp4'"] ; "after a closed double quote")]
    #[test_case(r#"run -c "echo 'a' %f""#, &["run", "-c", "echo 'a' '/v/x$(touch pwned).mp4'"] ; "after a closed single quote")]
    fn a_code_after_the_script_closes_its_quotes_is_quoted(exec: &str, expected: &[&str]) {
        assert_eq!(expected, expanded(exec, &hostile_context()).as_slice());
    }

    /// Runs `exec` expanded against a file whose name runs a command if a
    /// shell reads it unquoted, returning what it printed and whether the
    /// command ran.
    fn run_against_a_hostile_name(exec: impl Fn(&Path) -> String) -> (Vec<u8>, Vec<u8>, bool) {
        let dir = crate::test_support::TempDir::new("exec_hostile");
        let path = dir.join("x$(touch pwned).mp4");
        let uri = file_uri(&path);
        let context = ExecContext {
            path: &path,
            uri: &uri,
            ..context()
        };
        let argv = expand(&context, &exec(dir.path())).unwrap();

        let output = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(dir.path())
            .output()
            .unwrap();

        assert!(output.status.success());
        let expected = path.as_os_str().as_encoded_bytes().to_vec();
        (expected, output.stdout, dir.join("pwned").exists())
    }

    /// A shell under a name filectrl does not know, so its script is quoted
    /// for rather than refused. Runs the expanded argv, so the check is what
    /// the shell does with the name rather than what the string looks like.
    #[test]
    fn a_quoted_script_receives_the_name_as_inert_text() {
        let (expected, printed, ran) = run_against_a_hostile_name(|dir| {
            let runner = dir.join("runner");
            std::os::unix::fs::symlink("/bin/sh", &runner).unwrap();
            format!("{} -c \"printf %%s %f\"", runner.display())
        });
        assert_eq!(expected, printed);
        assert!(!ran);
    }

    #[test]
    fn a_name_passed_to_a_shell_after_its_script_is_inert() {
        let (expected, printed, ran) =
            run_against_a_hostile_name(|_| "sh -c 'printf %%s \"$1\"' sh %f".to_string());
        assert_eq!(expected, printed);
        assert!(!ran);
    }

    // Quoting cannot follow what a shell script does with the text, so a code
    // in the script is refused wherever it sits.
    #[test_case("sh -c \"mpv %f\"" ; "a double quoted script")]
    #[test_case("sh -c %f" ; "the code as the whole script")]
    #[test_case("sh -c \"cat <<EOF\n%f\nEOF\"" ; "a here document")]
    #[test_case("sh -c \"eval cat %f\"" ; "eval")]
    #[test_case("sh -c cat${IFS}%f" ; "no whitespace in the script")]
    #[test_case("/bin/bash -lc 'mpv %u'" ; "a path to the shell and clustered options")]
    #[test_case("env FOO=1 dash -c 'mpv %c'" ; "a shell run through env")]
    #[test_case("bash -o pipefail -c 'cat %f'" ; "an option that takes a value")]
    #[test_case("zsh --login -c 'cat %k'" ; "a long option")]
    fn a_code_in_a_shell_script_is_refused(exec: &str) {
        let error = expand(&hostile_context(), exec)
            .expect_err("the entry must not be offered")
            .to_string();
        assert!(error.ends_with("cannot be passed safely"), "{error}");
    }

    #[test_case("sh -c 'mpv \"$1\"' sh %f", &["sh", "-c", "mpv \"$1\"", "sh", HOSTILE] ; "the code after the script")]
    #[test_case("sh -c 'printf 100%%' %f", &["sh", "-c", "printf 100%", HOSTILE] ; "a literal percent in the script")]
    #[test_case("sh -c 'echo %i' %f", &["sh", "-c", "echo ", HOSTILE] ; "a code that expands to nothing")]
    #[test_case("sh %f", &["sh", HOSTILE] ; "a shell given a file rather than a script")]
    #[test_case("bash --norc %f", &["bash", "--norc", HOSTILE] ; "a long option with a c in it")]
    #[test_case("app -c %f", &["app", "-c", HOSTILE] ; "a -c option of a program that is not a shell")]
    fn a_shell_whose_script_holds_no_code_is_offered(exec: &str, expected: &[&str]) {
        assert_eq!(expected, expanded(exec, &hostile_context()).as_slice());
    }

    #[test_case("/a/b.txt", "file:///a/b.txt" ; "unreserved characters pass through")]
    #[test_case("/a/my file.txt", "file:///a/my%20file.txt" ; "space")]
    #[test_case("/a/~-._x", "file:///a/~-._x" ; "the rest of the unreserved set")]
    #[test_case("/a/100%", "file:///a/100%25" ; "percent")]
    #[test_case("/a/caf\u{e9}", "file:///a/caf%C3%A9" ; "multi byte utf8")]
    fn file_uri_encodes(path: &str, expected: &str) {
        assert_eq!(expected, file_uri(Path::new(path)));
    }

    #[test_case("plain", "plain" ; "no escapes")]
    #[test_case("a\\sb", "a b" ; "space")]
    #[test_case("a\\nb", "a\nb" ; "newline")]
    #[test_case("a\\tb", "a\tb" ; "tab")]
    #[test_case("a\\rb", "a\rb" ; "carriage return")]
    #[test_case("a\\\\b", "a\\b" ; "backslash")]
    #[test_case("a\\qb", "a\\qb" ; "unknown escape is left alone")]
    #[test_case("trailing\\", "trailing\\" ; "trailing backslash")]
    fn unescape_value_produces(value: &str, expected: &str) {
        assert_eq!(expected, unescape_value(value));
    }
}
