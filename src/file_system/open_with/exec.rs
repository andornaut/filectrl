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
///
/// Two shapes are refused with `Refused`, so the entry is not offered: a
/// field code in an argument written with quotes or escapes, unless the
/// argument is nothing but that code (`app "%f"`), and one in the script a
/// shell is given with `-c`, however it is written. The spec leaves a quoted
/// code undefined, and in practice such an argument is a script for some
/// interpreter (`sh -c`, `python3 -c`, `env -S`), where the name would run as
/// code and no quoting is right for every language that might read it. A
/// shell's script is recognized even unquoted (`sh -c %f`), the one place an
/// unquoted code is read as code.
pub(super) fn expand(context: &ExecContext<'_>, exec: &str) -> Result<Vec<OsString>> {
    // The desktop entry string escapes are undone before the quoting rules are
    // applied, so a literal backslash inside a quoted argument is written as
    // four backslashes.
    let tokens = split(&unescape_value(exec))
        .map_err(|error| anyhow!("Malformed Exec {exec:?}: {error}"))?;
    if shell_scripts(&tokens).any(has_substituting_code) {
        return Err(Refused(format!(
            "Exec {exec:?}: a field code in a shell's -c script cannot be passed safely"
        ))
        .into());
    }

    // Only ever one path, so %F and %U behave as %f and %u.
    let mut argv: Vec<OsString> = Vec::with_capacity(tokens.len() + 1);
    let mut consumed_path = false;
    for Token { text, quoted } in tokens {
        // The only code that expands to more than one argument.
        if text == "%i" {
            if let Some(icon) = context.icon {
                argv.push(OsString::from("--icon"));
                argv.push(OsString::from(icon));
            }
            continue;
        }
        let is_one_code = text.len() == 2 && text.starts_with('%');
        if quoted && !is_one_code && has_substituting_code(&text) {
            return Err(Refused(format!(
                "Exec {exec:?}: a field code in a quoted argument cannot be passed safely"
            ))
            .into());
        }
        let (expanded, used_path) = expand_in_token(context, &text);
        consumed_path |= used_path;
        // A token that was nothing but dropped field codes (deprecated ones
        // included) is not an empty argument, but a literal "" is.
        if !expanded.is_empty() || !text.contains('%') {
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

/// An `Exec` that `expand` refuses because it would let a file name run as
/// code, as opposed to one that is malformed. Worth telling the user about:
/// the application is installed and would otherwise have been offered.
#[derive(Debug)]
pub(super) struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// Programs that read the argument after `-c` as a shell script.
const SHELLS: [&str; 9] = [
    "ash", "bash", "csh", "dash", "fish", "ksh", "mksh", "sh", "zsh",
];

/// Long options of the shells above that take the next argument as their
/// value.
const SHELL_OPTIONS_WITH_VALUE: [&str; 2] = ["--init-file", "--rcfile"];

/// The scripts shells among `tokens` are given with `-c`. Every token naming a
/// shell is tried, since an earlier one may be another program's argument
/// (`env -u sh bash -c ...`).
fn shell_scripts(tokens: &[Token]) -> impl Iterator<Item = &str> {
    tokens.iter().enumerate().filter_map(|(start, token)| {
        let program = token.text.rsplit('/').next().unwrap_or(&token.text);
        if SHELLS.contains(&program) {
            script_operand(&tokens[start + 1..])
        } else {
            None
        }
    })
}

/// The script among a shell's `arguments`, when `-c` is given: the first
/// operand after the options.
fn script_operand(arguments: &[Token]) -> Option<&str> {
    let mut has_script_option = false;
    let mut rest = arguments.iter().map(|token| token.text.as_str());
    while let Some(token) = rest.next() {
        let Some(options) = token.strip_prefix(['-', '+']) else {
            return has_script_option.then_some(token);
        };
        if options.starts_with('-') {
            if SHELL_OPTIONS_WITH_VALUE.contains(&token) {
                rest.next();
            }
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

/// One argument of an `Exec` line, with its quotes and escapes removed.
#[derive(Default)]
struct Token {
    text: String,
    /// Whether any of it was quoted or escaped.
    quoted: bool,
}

/// Splits an `Exec` line into arguments. The spec's quoting (double quotes,
/// backslash-escaping of " ` $ \) is a subset of POSIX quoting, which this
/// follows: single quotes, double quotes, backslashes, and a `#` that starts a
/// word begins a comment.
fn split(line: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut current: Option<Token> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => tokens.extend(current.take()),
            '#' if current.is_none() => {
                chars.by_ref().find(|&c| c == '\n');
            }
            '\\' => match chars.next() {
                // A line continuation, which is not part of any argument.
                Some('\n') => {}
                Some(c) => {
                    let token = current.get_or_insert_with(Token::default);
                    token.quoted = true;
                    token.text.push(c);
                }
                None => return Err(anyhow!("it ends with a backslash")),
            },
            '\'' => {
                let token = current.get_or_insert_with(Token::default);
                token.quoted = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => token.text.push(c),
                        None => return Err(anyhow!("a single quote is not closed")),
                    }
                }
            }
            '"' => {
                let token = current.get_or_insert_with(Token::default);
                token.quoted = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '`' | '$' | '\\')) => token.text.push(c),
                            Some('\n') => {}
                            Some(c) => {
                                token.text.push('\\');
                                token.text.push(c);
                            }
                            None => return Err(anyhow!("a double quote is not closed")),
                        },
                        Some(c) => token.text.push(c),
                        None => return Err(anyhow!("a double quote is not closed")),
                    }
                }
            }
            c => current.get_or_insert_with(Token::default).text.push(c),
        }
    }
    tokens.extend(current);
    Ok(tokens)
}

/// Whether `token` holds a field code that substitutes a value, as opposed to a
/// literal percent or a code that expands to nothing.
fn has_substituting_code(token: &str) -> bool {
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c == '%' && matches!(chars.next(), Some('f' | 'F' | 'u' | 'U' | 'c' | 'k')) {
            return true;
        }
    }
    false
}

/// Substitute the field codes appearing anywhere within a single argument, so
/// that `--file=%f` works as well as a bare `%f`. Returns the expansion and
/// whether it consumed the path.
fn expand_in_token(context: &ExecContext<'_>, token: &str) -> (OsString, bool) {
    let mut expanded = OsString::with_capacity(token.len());
    let mut consumed_path = false;
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            expanded.push(c.encode_utf8(&mut [0u8; 4]));
            continue;
        }
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
            // to two arguments, and a trailing '%' are all dropped.
            _ => continue,
        };
        expanded.push(value);
    }
    (expanded, consumed_path)
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

    use super::{ExecContext, Refused, expand, file_uri, unescape_value};

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

    // Outside quotes the value is one argv element and no shell reads it, so it
    // is passed raw, embedded or not. So is a quoted argument that is only the
    // code.
    #[test_case("mpv %f", &["mpv", HOSTILE] ; "bare code is raw")]
    #[test_case("mpv --file=%f", &["mpv", "--file=/v/x$(touch pwned).mp4"] ; "unquoted embedded code is raw")]
    #[test_case("app \"%f\"", &["app", HOSTILE] ; "a quoted code on its own")]
    #[test_case("app '%f'", &["app", HOSTILE] ; "a single quoted code on its own")]
    #[test_case("app \"it's\" %f", &["app", "it's", HOSTILE] ; "a code after a quoted argument")]
    fn a_code_outside_quotes_is_passed_raw(exec: &str, expected: &[&str]) {
        assert_eq!(expected, expanded(exec, &hostile_context()).as_slice());
    }

    // A code in an argument with any quoting or escape in it is in a script,
    // whatever reads it, so the entry is refused.
    #[test_case("run -c \"mpv %f\"" ; "double quoted script")]
    #[test_case("run -c 'mpv %f'" ; "single quoted script")]
    #[test_case("run -c \"%f --flag\"" ; "a quoted script that starts with the code")]
    #[test_case(r#"run -c "echo \"%f\"""# ; "inside the script's double quotes")]
    #[test_case(r"run -c echo\\ %f" ; "after an escaped space")]
    #[test_case(r#"run -c "echo "%f"# ; "after a quoted space")]
    #[test_case(r#"app "x"%f"# ; "after a quoted word")]
    #[test_case("app \\'%f" ; "after an escaped quote")]
    #[test_case("app \"--file=%f\"" ; "a quoted option value")]
    #[test_case("python3 -c \"print(%f)\"" ; "another language")]
    #[test_case("env -S \"sh -c %f\"" ; "a command line split by env")]
    fn a_code_in_a_quoted_argument_is_refused(exec: &str) {
        let error = expand(&hostile_context(), exec).expect_err("the entry must not be offered");
        assert!(error.is::<Refused>(), "{error}");
        let error = error.to_string();
        assert!(
            error.ends_with("a field code in a quoted argument cannot be passed safely"),
            "{error}"
        );
    }

    // A code that expands to nothing puts nothing into the script, so where it
    // sits does not matter, and a literal percent is not a code.
    #[test_case(r#"run -c "echo '%d'""#, &["run", "-c", "echo ''", HOSTILE] ; "a deprecated code")]
    #[test_case(r#"run -c "echo '%i'""#, &["run", "-c", "echo ''", HOSTILE] ; "an icon code inside an argument")]
    #[test_case(r#"run -c "printf 100%%""#, &["run", "-c", "printf 100%", HOSTILE] ; "a literal percent")]
    #[test_case("sh -c 'mpv \"$1\"' sh %f", &["sh", "-c", "mpv \"$1\"", "sh", HOSTILE] ; "the code after the script")]
    fn a_script_with_no_substituting_code_is_offered(exec: &str, expected: &[&str]) {
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

    #[test]
    fn a_name_passed_to_a_shell_after_its_script_is_inert() {
        let (expected, printed, ran) =
            run_against_a_hostile_name(|_| "sh -c 'printf %%s \"$1\"' sh %f".to_string());
        assert_eq!(expected, printed);
        assert!(!ran);
    }

    // A shell reads its -c operand as a script however it is written, so a
    // code there is refused even unquoted.
    #[test_case("sh -c %f" ; "the code as the whole script")]
    #[test_case("sh -c cat${IFS}%f" ; "no whitespace in the script")]
    #[test_case("sh -c \"cat <<EOF\n%f\nEOF\"" ; "a here document")]
    #[test_case("/bin/bash -lc 'mpv %u'" ; "a path to the shell and clustered options")]
    #[test_case("env FOO=1 dash -c 'mpv %c'" ; "a shell run through env")]
    #[test_case("bash -o pipefail -c 'cat %f'" ; "an option that takes a value")]
    #[test_case("bash --rcfile /dev/null -c %f" ; "a long option that takes a value")]
    #[test_case("zsh --login -c 'cat %k'" ; "a long option")]
    #[test_case("env -u sh bash -c %f" ; "an earlier argument that names a shell")]
    fn a_code_in_a_shell_script_is_refused(exec: &str) {
        let error = expand(&hostile_context(), exec).expect_err("the entry must not be offered");
        assert!(error.is::<Refused>(), "{error}");
        assert!(
            error
                .to_string()
                .ends_with("a field code in a shell's -c script cannot be passed safely"),
            "{error}"
        );
    }

    #[test_case("sh %f", &["sh", HOSTILE] ; "a shell given a file rather than a script")]
    #[test_case("bash --norc %f", &["bash", "--norc", HOSTILE] ; "a long option with a c in it")]
    #[test_case("app -c %f", &["app", "-c", HOSTILE] ; "a -c option of a program that is not a shell")]
    fn a_shell_whose_script_holds_no_code_is_offered(exec: &str, expected: &[&str]) {
        assert_eq!(expected, expanded(exec, &hostile_context()).as_slice());
    }

    #[test]
    fn a_malformed_exec_is_not_reported_as_refused() {
        let error = expand(&context(), "app \"unmatched").unwrap_err();
        assert!(!error.is::<Refused>(), "{error}");
    }

    #[test_case("app \\\n%f", &["app", HOSTILE] ; "a line continuation")]
    #[test_case("app # %f", &["app", HOSTILE] ; "a comment")]
    #[test_case("app x#y", &["app", "x#y", HOSTILE] ; "a hash inside a word")]
    fn split_follows_the_shell(exec: &str, expected: &[&str]) {
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
