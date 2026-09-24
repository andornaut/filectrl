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

/// Expand `exec` into an argv suitable for `std::process::Command`, opening
/// `path`. Only the file and URI codes are substituted. `%i`, `%c` and `%k`
/// are removed like the deprecated codes, as the spec directs for a code that
/// is not supported.
///
/// `DesktopEntry::parse_exec` is deliberately not used: it splits on ASCII
/// whitespace (which tears apart a quoted program path), substitutes a field
/// code only when it is an entire token (so `--file=%f` is passed through
/// literally), and rejects any `Exec` whose first token contains '='
/// (`env FOO=1 app %f`).
///
/// Two shapes are refused with `Refused`, so the entry is not offered: a
/// field code in an argument written with quotes or escapes, unless the
/// argument is nothing but that code (`app "%f"`), and a value in or after an
/// option that may take code, however it is written, whether a field code or
/// the path appended for want of one. The spec leaves a quoted code
/// undefined, and in practice such an argument is a script for some
/// interpreter (`sh -c`, `python3 -c`, `env -S`), where the name would run as
/// code and no quoting is right for every language that might read it. An
/// unquoted code is read as code only in or after such an option (`sh -c %f`,
/// `env -S%f`), which is recognized by its shape alone, whatever the program.
/// The rule is deliberately broad and refuses some safe entries (`sh -c 'mpv
/// "$1"' sh %f`).
pub(super) fn expand(path: &Path, exec: &str) -> Result<Vec<OsString>> {
    // The desktop entry string escapes are undone before the quoting rules are
    // applied, so a literal backslash inside a quoted argument is written as
    // four backslashes.
    let tokens = split(&unescape_value(exec))
        .map_err(|error| anyhow!("Malformed Exec {exec:?}: {error}"))?;
    let takes_path = tokens
        .iter()
        .any(|token| has_substituting_code(&token.text));
    // Without a field code the path is appended, so it follows the option too.
    if let Some(start) = code_option(&tokens)
        && (!takes_path
            || tokens[start..]
                .iter()
                .any(|token| has_substituting_code(&token.text)))
    {
        return Err(Refused(format!(
            "Exec {exec:?}: a path in or after an option that takes code cannot be passed safely"
        ))
        .into());
    }

    // Only ever one path, so %F and %U behave as %f and %u.
    let uri = file_uri(path);
    let mut argv: Vec<OsString> = Vec::with_capacity(tokens.len() + 1);
    for Token { text, quoted } in tokens {
        let is_one_code = text.len() == 2 && text.starts_with('%');
        if quoted && !is_one_code && has_substituting_code(&text) {
            return Err(Refused(format!(
                "Exec {exec:?}: a field code in a quoted argument cannot be passed safely"
            ))
            .into());
        }
        let expanded = expand_in_token(path, &uri, &text);
        // A token that was nothing but removed field codes is not an empty
        // argument, but a literal "" is.
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
    if !takes_path {
        argv.push(path.as_os_str().to_os_string());
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

/// The index of the first option cluster among `tokens` whose leading run of
/// letters and digits holds `c`, `e`, `E` or `S` (`-c`, `-lc`, `-cx`, `-e`,
/// `-E`, `-S`, `-cprint(1)`, `-S%f`). That is how shells, `env`, `python3`,
/// `perl`, `node` and the like are given code to run, so it is matched for any
/// program. The code may be attached to the option, and every later argument
/// counts, since options may stand between the cluster and the code (`sh -c -x
/// %f`) and a program may read an argument after the code as code too (`eval
/// "$1"`). A `--` option has no leading run, so it never matches.
fn code_option(tokens: &[Token]) -> Option<usize> {
    tokens.iter().position(|token| {
        token.text.strip_prefix('-').is_some_and(|options| {
            options
                .bytes()
                .take_while(u8::is_ascii_alphanumeric)
                .any(|byte| matches!(byte, b'c' | b'e' | b'E' | b'S'))
        })
    })
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
/// literal percent or a code that is removed.
fn has_substituting_code(token: &str) -> bool {
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c == '%' && matches!(chars.next(), Some('f' | 'F' | 'u' | 'U')) {
            return true;
        }
    }
    false
}

/// Substitute the field codes appearing anywhere within a single argument, so
/// that `--file=%f` works as well as a bare `%f`.
fn expand_in_token(path: &Path, uri: &str, token: &str) -> OsString {
    let mut expanded = OsString::with_capacity(token.len());
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
            Some('f' | 'F') => path.as_os_str(),
            Some('u' | 'U') => OsStr::new(uri),
            // Unsupported (%i, %c, %k), deprecated, unrecognized, and a
            // trailing '%' are all removed.
            _ => continue,
        };
        expanded.push(value);
    }
    expanded
}

/// The `file://` URI of an absolute path, for the `%u` and `%U` field codes.
/// Everything outside the RFC 3986 unreserved set is percent encoded, from the
/// raw bytes: a lossy conversion would percent encode replacement characters
/// rather than the name they stood in for.
fn file_uri(path: &Path) -> String {
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
pub(super) fn unescape_value(value: &str) -> String {
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

    use super::{Refused, expand, file_uri, split, unescape_value};

    /// The expansion as plain strings, for comparing against the expected argv.
    fn expanded(path: &str, exec: &str) -> Vec<String> {
        expand(Path::new(path), exec)
            .unwrap()
            .iter()
            .map(|word| word.to_string_lossy().into_owned())
            .collect()
    }

    const PATH: &str = "/home/u/report.pdf";
    const URI: &str = "file:///home/u/report.pdf";

    #[test_case("app %f", &["app", PATH] ; "bare file code")]
    #[test_case("app %F", &["app", PATH] ; "multi file code takes the one path")]
    #[test_case("app %u", &["app", URI] ; "bare uri code")]
    #[test_case("app %U", &["app", URI] ; "multi uri code takes the one uri")]
    #[test_case("app --file=%f", &["app", "--file=/home/u/report.pdf"] ; "code inside a token")]
    #[test_case("\"/opt/my app/bin\" %U", &["/opt/my app/bin", URI] ; "quoted program path")]
    #[test_case("app", &["app", PATH] ; "no field code appends the path")]
    #[test_case("app %i %f", &["app", PATH] ; "the icon code is dropped")]
    #[test_case("app %c %f", &["app", PATH] ; "the name code is dropped")]
    #[test_case("app %k %f", &["app", PATH] ; "the desktop file code is dropped")]
    #[test_case("app --name=%c %f", &["app", "--name=", PATH] ; "an unsupported code inside a token is dropped")]
    #[test_case("app %c %k %i", &["app", PATH] ; "unsupported codes alone append the path")]
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
        assert_eq!(expected, expanded(PATH, exec).as_slice());
    }

    #[test_case("app \"unmatched" ; "unmatched quote")]
    #[test_case("" ; "empty")]
    #[test_case("   " ; "only whitespace")]
    fn expand_rejects(exec: &str) {
        assert!(expand(Path::new(PATH), exec).is_err());
    }

    #[test]
    fn expand_preserves_a_name_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::OsStr::from_bytes(b"/tmp/caf\xe9.txt");

        // A lossy conversion would hand the program U+FFFD instead of 0xe9,
        // and it would open nothing.
        let argv = expand(Path::new(name), "app %f %u").unwrap();
        assert_eq!(name, argv[1]);
        // The URI encodes the byte itself rather than a replacement character.
        assert_eq!("file:///tmp/caf%E9.txt", argv[2]);
    }

    /// A name that runs a command if a shell ever reads it unquoted.
    const HOSTILE: &str = "/v/x$(touch pwned).mp4";
    const HOSTILE_URI: &str = "file:///v/x%24%28touch%20pwned%29.mp4";

    /// The rules `expand` refuses by, as the tail of their messages.
    const CODE_OPTION: &str =
        "a path in or after an option that takes code cannot be passed safely";
    const QUOTED: &str = "a field code in a quoted argument cannot be passed safely";

    // Any value in or after an option cluster whose leading letters hold c, e,
    // E or S may be read as code, by any program, however it is written. So
    // may the path appended to an entry with no field code. A code in an
    // argument with any quoting or escape in it is in a script, whatever reads
    // it.
    #[test_case("sh -c %f", CODE_OPTION ; "c directly before the code")]
    #[test_case("foo -e %f", CODE_OPTION ; "e directly before the code")]
    #[test_case("perl -E %f", CODE_OPTION ; "upper case e directly before the code")]
    #[test_case("env -S %f", CODE_OPTION ; "s directly before the code")]
    #[test_case("sh -lc %f", CODE_OPTION ; "c last in a cluster")]
    #[test_case("sh -cx %f", CODE_OPTION ; "c first in a cluster")]
    #[test_case("foo -xey %f", CODE_OPTION ; "e inside a cluster")]
    #[test_case("foo -xE %f", CODE_OPTION ; "upper case e last in a cluster")]
    #[test_case("foo -verbose %f", CODE_OPTION ; "a single dash long option holding e")]
    #[test_case("python3 \"-cimport os,sys; os.system('echo ' + sys.argv[1])\" %f", CODE_OPTION ; "a script attached to c")]
    #[test_case("perl \"-esystem('echo ' . $ARGV[0])\" %f", CODE_OPTION ; "a script attached to e")]
    #[test_case("env -S%f", CODE_OPTION ; "the code attached to s")]
    #[test_case("python3 \"-cprint('%f')\"", CODE_OPTION ; "the code inside a script attached to c")]
    #[test_case("sh -c -x %f", CODE_OPTION ; "after a further option")]
    #[test_case("bash -c -o pipefail %f", CODE_OPTION ; "after a further option with a value")]
    #[test_case("sh -c -- %f", CODE_OPTION ; "after a double dash")]
    #[test_case("foo -c -- -- %f", CODE_OPTION ; "after several double dashes")]
    #[test_case("sh -c 'mpv \"$1\"' sh %f", CODE_OPTION ; "after the script as its parameter")]
    #[test_case("sh -c 'mpv %f'", CODE_OPTION ; "in a single quoted script")]
    #[test_case("sh -c \"mpv %f\"", CODE_OPTION ; "in a double quoted script")]
    #[test_case("foo -c \"%f\"", CODE_OPTION ; "a quoted code on its own")]
    #[test_case("sh -c \"cat <<EOF\n%f\nEOF\"", CODE_OPTION ; "in a here document")]
    #[test_case("sh -c cat${IFS}%f", CODE_OPTION ; "embedded in an unquoted script")]
    #[test_case("foo -c --file=%f", CODE_OPTION ; "embedded in an option value")]
    #[test_case("sh -c %F", CODE_OPTION ; "the file list code")]
    #[test_case("sh -c %u", CODE_OPTION ; "the uri code")]
    #[test_case("sh -c %U", CODE_OPTION ; "the uri list code")]
    #[test_case("/bin/bash -lc 'mpv %u'", CODE_OPTION ; "a path to the program")]
    #[test_case("env FOO=1 dash -c 'mpv %f'", CODE_OPTION ; "a shell run through env")]
    #[test_case("bash -o pipefail -c 'cat %f'", CODE_OPTION ; "an option before the cluster")]
    #[test_case("zsh --login -c 'cat %f'", CODE_OPTION ; "a long option before the cluster")]
    #[test_case("rbash -c %f", CODE_OPTION ; "a restricted shell")]
    #[test_case("tcsh -c %f", CODE_OPTION ; "a csh")]
    #[test_case("env -S \"sh -c %f\"", CODE_OPTION ; "a command line split by env")]
    #[test_case("python3 -c %f", CODE_OPTION ; "python")]
    #[test_case("perl -e %f", CODE_OPTION ; "perl")]
    #[test_case("node -e %f", CODE_OPTION ; "node")]
    #[test_case("ruby -e %f", CODE_OPTION ; "ruby")]
    #[test_case("sh -c", CODE_OPTION ; "no code so the path would be the script")]
    #[test_case("foo -c %i", CODE_OPTION ; "only a removed code so the path is appended")]
    #[test_case("run --command \"mpv %f\"", QUOTED ; "double quoted script")]
    #[test_case("run --command 'mpv %f'", QUOTED ; "single quoted script")]
    #[test_case("run --command \"%f --flag\"", QUOTED ; "a quoted script that starts with the code")]
    #[test_case(r"run --command echo\\ %f", QUOTED ; "after an escaped space")]
    #[test_case(r#"app "x"%f"#, QUOTED ; "after a quoted word")]
    #[test_case("app \"--file=%f\"", QUOTED ; "a quoted option value")]
    #[test_case("env --split-string \"sh -c %f\"", QUOTED ; "a long option that takes code")]
    fn expand_refuses(exec: &str, rule: &str) {
        let error = expand(Path::new(HOSTILE), exec).expect_err("the entry must not be offered");
        assert!(error.is::<Refused>(), "{error}");
        assert!(error.to_string().ends_with(rule), "{error}");
    }

    // Outside quotes the value is one argv element and no shell reads it, so it
    // is passed raw, embedded or not. So is a quoted argument that is only the
    // code. A code that expands to nothing puts nothing into a script, and a
    // literal percent is not a code.
    #[test_case("mpv %f", &["mpv", HOSTILE] ; "a bare code")]
    #[test_case("vlc %U", &["vlc", HOSTILE_URI] ; "a bare uri code")]
    #[test_case("app \"%f\"", &["app", HOSTILE] ; "a quoted code on its own")]
    #[test_case("app '%f'", &["app", HOSTILE] ; "a single quoted code on its own")]
    #[test_case("app \"it's\" %f", &["app", "it's", HOSTILE] ; "a code after a quoted argument")]
    #[test_case("sh %f", &["sh", HOSTILE] ; "a shell given a file rather than a script")]
    #[test_case("foo %f -c bar", &["foo", HOSTILE, "-c", "bar"] ; "a code before the cluster")]
    #[test_case("foo %f -c %i", &["foo", HOSTILE, "-c"] ; "the icon code after the cluster")]
    #[test_case("foo %f -c %c", &["foo", HOSTILE, "-c"] ; "the name code after the cluster")]
    #[test_case("foo %f -c %k", &["foo", HOSTILE, "-c"] ; "the desktop file code after the cluster")]
    #[test_case("foo %f -c %% %d", &["foo", HOSTILE, "-c", "%"] ; "a literal percent and a deprecated code after the cluster")]
    #[test_case("foo --config %f", &["foo", "--config", HOSTILE] ; "a long option")]
    #[test_case("foo --exec %f", &["foo", "--exec", HOSTILE] ; "a long option holding e and c")]
    #[test_case("foo --c=%f", &["foo", "--c=/v/x$(touch pwned).mp4"] ; "a long option named c with the code attached")]
    #[test_case("bash --norc %f", &["bash", "--norc", HOSTILE] ; "a long option ending in c")]
    #[test_case("foo -a-c %f", &["foo", "-a-c", HOSTILE] ; "a word that is not a cluster")]
    #[test_case("foo -xvf %f", &["foo", "-xvf", HOSTILE] ; "a cluster without c e or s")]
    #[test_case("foo -s %f", &["foo", "-s", HOSTILE] ; "a lower case s")]
    #[test_case("foo -C %f", &["foo", "-C", HOSTILE] ; "an upper case c")]
    #[test_case(r#"run --command "echo '%d'" %f"#, &["run", "--command", "echo ''", HOSTILE] ; "a deprecated code in a quoted argument")]
    #[test_case(r#"run --command "echo '%i'" %f"#, &["run", "--command", "echo ''", HOSTILE] ; "an icon code in a quoted argument")]
    #[test_case(r#"run --command "echo '%c'" %f"#, &["run", "--command", "echo ''", HOSTILE] ; "a name code in a quoted argument")]
    #[test_case(r#"run --command "echo '%k'" %f"#, &["run", "--command", "echo ''", HOSTILE] ; "a desktop file code in a quoted argument")]
    #[test_case(r#"run --command "printf 100%%" %f"#, &["run", "--command", "printf 100%", HOSTILE] ; "a literal percent in a quoted argument")]
    fn expand_offers(exec: &str, expected: &[&str]) {
        assert_eq!(expected, expanded(HOSTILE, exec).as_slice());
    }

    #[test]
    fn a_malformed_exec_is_not_reported_as_refused() {
        let error = expand(Path::new(PATH), "app \"unmatched").unwrap_err();
        assert!(!error.is::<Refused>(), "{error}");
    }

    #[test_case("app \\\n%f", &["app", HOSTILE] ; "a line continuation")]
    #[test_case("app # %f", &["app", HOSTILE] ; "a comment")]
    #[test_case("app x#y", &["app", "x#y", HOSTILE] ; "a hash inside a word")]
    fn split_follows_the_shell(exec: &str, expected: &[&str]) {
        assert_eq!(expected, expanded(HOSTILE, exec).as_slice());
    }

    // Called directly, since the escapes of an `Exec` value are undone before
    // splitting and a quoted field code is refused.
    #[test_case("a\tb", &["a", "b"] ; "a tab separates")]
    #[test_case("a\nb", &["a", "b"] ; "a newline separates")]
    #[test_case(r#""a\`b""#, &["a`b"] ; "an escaped backtick in double quotes")]
    #[test_case(r#""a\$b""#, &["a$b"] ; "an escaped dollar in double quotes")]
    #[test_case(r#""a\"b""#, &["a\"b"] ; "an escaped double quote in double quotes")]
    #[test_case(r#""a\\b""#, &["a\\b"] ; "an escaped backslash in double quotes")]
    #[test_case("\"a\\\nb\"", &["ab"] ; "a line continuation in double quotes")]
    #[test_case(r#""a\qb""#, &["a\\qb"] ; "a backslash before any other character in double quotes is kept")]
    fn split_produces(line: &str, expected: &[&str]) {
        let words: Vec<String> = split(line)
            .unwrap()
            .into_iter()
            .map(|token| token.text)
            .collect();
        assert_eq!(expected, words.as_slice());
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
