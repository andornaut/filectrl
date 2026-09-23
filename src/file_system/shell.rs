//! Running a configured shell template on a path.
//!
//! The path is never written into the script. `%s` becomes a reference to a
//! positional parameter and the path is passed after the script as an
//! argument, so the shell only ever expands it and never parses it: no file
//! name can run as a command, whatever quoting, here-document or backticks the
//! template puts around `%s`. Only a template that hands the text to another
//! parser can still run it: `eval`, a nested `sh -c`, `ssh`, or bash
//! arithmetic such as `$(( %s ))`, which evaluates `x[$(cmd)]`.
//!
//! The values are passed as raw bytes, so a path that is not valid UTF-8
//! reaches the program intact.

use std::ffi::OsString;

/// Which positional parameters `%s` stands for.
#[derive(Clone, Copy)]
pub(crate) enum Parameters {
    /// One value: `$1`.
    One,
    /// Every value, each its own word: `$@`. Only the Linux terminal wrapper
    /// passes a command; macOS launches through `open`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    All,
}

impl Parameters {
    /// The text that expands to exactly these parameters within `quote`.
    fn reference(self, quote: &Quote) -> &'static str {
        match (self, quote) {
            (Parameters::One, Quote::None) => "\"$1\"",
            (Parameters::One, Quote::Double) => "$1",
            (Parameters::One, Quote::Single) => "'\"$1\"'",
            (Parameters::All, Quote::None) => "\"$@\"",
            (Parameters::All, Quote::Double) => "$@",
            (Parameters::All, Quote::Single) => "'\"$@\"'",
        }
    }
}

/// The argv that runs `template` with `sh -c`, `%s` standing for `values`. The
/// reference is written to expand to exactly the value wherever `%s` sits:
/// quoted when it is outside quotes, bare inside double quotes, and closing and
/// reopening single quotes around itself inside them. Backslashes are not
/// tracked, so a misjudged quote state (after an escaped quote, or inside
/// `$(...)`) can only split the value into words or garble it, never run it.
pub(crate) fn command(
    template: &str,
    parameters: Parameters,
    values: impl IntoIterator<Item = OsString>,
) -> Vec<OsString> {
    let mut script = String::with_capacity(template.len() + 8);
    let mut quote = Quote::None;
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' && chars.next_if_eq(&'s').is_some() {
            script.push_str(parameters.reference(&quote));
            continue;
        }
        quote.advance(c);
        script.push(c);
    }
    let mut argv = vec![
        OsString::from("sh"),
        OsString::from("-c"),
        OsString::from(script),
        // `$0`, which the shell names itself by in its messages.
        OsString::from("sh"),
    ];
    argv.extend(values);
    argv
}

/// The quote state of a script as `sh` reads it, advanced over its characters.
enum Quote {
    None,
    Single,
    Double,
}

impl Quote {
    fn advance(&mut self, c: char) {
        *self = match (&*self, c) {
            (Quote::None, '\'') => Quote::Single,
            (Quote::None, '"') => Quote::Double,
            (Quote::Single, '\'') | (Quote::Double, '"') => Quote::None,
            _ => return,
        };
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::{OsStr, OsString},
        os::unix::ffi::OsStrExt,
        process::Command,
    };

    use test_case::test_case;

    use super::*;

    /// A name that runs `touch pwned` if a shell ever parses it, and closes
    /// either kind of quote first.
    const HOSTILE: &str = "a b'\"$(touch pwned)`touch pwned`";

    /// Runs `template` on `values` in a scratch directory holding `rec`, a
    /// program that prints its arguments NUL-terminated. Returns the words it
    /// printed and whether anything ran `touch`.
    fn run(template: &str, parameters: Parameters, values: &[&OsStr]) -> (Vec<Vec<u8>>, bool) {
        let dir = crate::test_support::TempDir::new("shell_command");
        crate::test_support::write_executable(
            &dir.join("rec"),
            "#!/bin/sh\nprintf '%s\\0' \"$@\"\n",
        );
        let argv = command(
            template,
            parameters,
            values.iter().map(|value| value.to_os_string()),
        );
        let output = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let words = output
            .stdout
            .split(|&byte| byte == 0)
            .filter(|word| !word.is_empty())
            .map(<[u8]>::to_vec)
            .collect();
        (words, dir.join("pwned").exists())
    }

    // Wherever `%s` sits, the value is only expanded, so nothing in it runs.
    // Some of these do not pass it intact (a here-document keeps the quotes
    // around the reference as text), which is the template's own concern.
    #[test_case("./rec %s" ; "unquoted")]
    #[test_case("./rec \"%s\"" ; "inside double quotes")]
    #[test_case("./rec '%s'" ; "inside single quotes")]
    #[test_case("./rec \"$(./rec %s)\"" ; "in a command substitution")]
    #[test_case("./rec `./rec %s`" ; "in backticks")]
    #[test_case("cat <<EOF\n%s\nEOF" ; "in a here document")]
    #[test_case("true # it's\n./rec %s" ; "after a comment holding a quote")]
    #[test_case("./rec $'\\'' %s" ; "after an ansi c quote")]
    fn a_hostile_name_is_never_run(template: &str) {
        let (_, ran) = run(template, Parameters::One, &[OsStr::new(HOSTILE)]);
        assert!(!ran);
    }

    #[test_case("./rec %s", "" ; "unquoted")]
    #[test_case("./rec \"%s\"", "" ; "inside double quotes")]
    #[test_case("./rec '%s'", "" ; "inside single quotes")]
    #[test_case("./rec \"--file=%s\"", "--file=" ; "embedded in a double quoted word")]
    #[test_case("./rec '--file=%s'", "--file=" ; "embedded in a single quoted word")]
    fn the_value_arrives_as_one_word(template: &str, prefix: &str) {
        let (words, _) = run(template, Parameters::One, &[OsStr::new(HOSTILE)]);
        assert_eq!(vec![format!("{prefix}{HOSTILE}").into_bytes()], words);
    }

    // Had the backslash ended the comment, the value `./rec` would run and
    // print `x`.
    #[test]
    fn a_backslash_before_the_reference_does_not_end_a_comment() {
        let (words, _) = run("true # see \\%s x", Parameters::One, &[OsStr::new("./rec")]);
        assert!(words.is_empty(), "the opened file ran: {words:?}");
    }

    #[test]
    fn every_value_arrives_as_its_own_word() {
        let values = [OsStr::new("vim"), OsStr::new(HOSTILE)];
        let (words, ran) = run("./rec %s", Parameters::All, &values);
        assert_eq!(vec![b"vim".to_vec(), HOSTILE.as_bytes().to_vec()], words);
        assert!(!ran);
    }

    #[test]
    fn a_value_that_is_not_utf8_arrives_intact() {
        let name = OsStr::from_bytes(b"caf\xe9.txt");
        let (words, _) = run("./rec %s", Parameters::One, &[name]);
        assert_eq!(vec![name.as_bytes().to_vec()], words);
    }

    #[test]
    fn the_script_is_passed_before_the_values() {
        let argv = command("xdg-open %s", Parameters::One, [OsString::from("/a b")]);
        let expected: Vec<OsString> = ["sh", "-c", "xdg-open \"$1\"", "sh", "/a b"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(expected, argv);
    }
}
