//! Expansion of a desktop entry's `Exec=` value into an argv.
//!
//! <https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html>

use std::{ffi::OsString, os::unix::ffi::OsStrExt, path::Path};

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
pub(super) fn expand(context: &ExecContext<'_>, exec: &str) -> Result<Vec<OsString>> {
    // The desktop entry string escapes are undone before the quoting rules are
    // applied, so a literal backslash inside a quoted argument is written as
    // four backslashes.
    let unescaped = unescape_value(exec);

    // The spec's quoting (double quotes, backslash-escaping of " ` $ \) is a
    // subset of POSIX quoting.
    let tokens = shell_words::split(&unescaped)
        .map_err(|error| anyhow!("Malformed Exec {exec:?}: {error}"))?;

    // Only ever one path, so %F and %U behave as %f and %u.
    let mut argv: Vec<OsString> = Vec::with_capacity(tokens.len() + 1);
    let mut consumed_path = false;
    for token in tokens {
        match token.as_str() {
            // The only code that expands to more than one argument.
            "%i" => {
                if let Some(icon) = context.icon {
                    argv.push(OsString::from("--icon"));
                    argv.push(OsString::from(icon));
                }
            }
            // Deprecated codes expand to nothing.
            "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
            _ => {
                let (expanded, used_path) = expand_in_token(context, &token);
                consumed_path |= used_path;
                // A token that was nothing but dropped field codes is not an
                // empty argument, but a literal "" is.
                if !expanded.is_empty() || !token.contains('%') {
                    argv.push(expanded);
                }
            }
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
        match chars.next() {
            Some('%') => expanded.push("%"),
            // Pushed as an `OsStr`, so a name that is not valid UTF-8 reaches
            // the program intact rather than as replacement characters.
            Some('f' | 'F') => {
                expanded.push(context.path.as_os_str());
                consumed_path = true;
            }
            Some('u' | 'U') => {
                expanded.push(context.uri);
                consumed_path = true;
            }
            Some('c') => expanded.push(context.name),
            Some('k') => expanded.push(context.desktop_file.as_os_str()),
            // Deprecated, unrecognized, %i in a position where it cannot expand
            // to two arguments, and a trailing '%' are all dropped.
            _ => {}
        }
    }
    (expanded, consumed_path)
}

/// The `file://` URI of an absolute path, for the `%u` and `%U` field codes.
/// Everything outside the RFC 3986 unreserved set is percent encoded.
///
/// Encoded from the raw bytes: a lossy conversion would turn a name that is
/// not valid UTF-8 into replacement characters, and the URI would then be
/// percent encoding those rather than the name.
pub(super) fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(byte as char)
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
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
            Some('\\') => unescaped.push('\\'),
            Some(other) => {
                unescaped.push('\\');
                unescaped.push(other);
            }
            None => unescaped.push('\\'),
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
