//! Running a configured shell template on a path.
//!
//! The path is never written into the script. `%s` becomes a quoted reference
//! to a positional parameter and the path is passed after the script as an
//! argument, so the shell only ever expands it and never parses it: no file
//! name can run as a command, whatever quoting, here-document or backticks the
//! template puts around `%s`. Only a template that hands the text to another
//! parser can still run it: `eval`, a nested `sh -c`, `ssh`, or bash
//! arithmetic such as `$(( %s ))`, which evaluates `x[$(cmd)]`.
//!
//! `%s` must be written unquoted, as its own word. The reference carries its
//! own double quotes, so inside double quotes it ends up unquoted and the value
//! is split into words, and inside single quotes it stays the literal text
//! `"$1"`. Neither is supported, and neither runs the value.
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
    /// The reference that expands to exactly these parameters when unquoted.
    fn reference(self) -> &'static str {
        match self {
            Parameters::One => "\"$1\"",
            Parameters::All => "\"$@\"",
        }
    }
}

/// The argv that runs `template` with `sh -c`, each `%s` replaced by a quoted
/// reference to `values`.
pub(crate) fn command(
    template: &str,
    parameters: Parameters,
    values: impl IntoIterator<Item = OsString>,
) -> Vec<OsString> {
    let mut argv = vec![
        OsString::from("sh"),
        OsString::from("-c"),
        OsString::from(template.replace("%s", parameters.reference())),
        // `$0`, which the shell names itself by in its messages.
        OsString::from("sh"),
    ];
    argv.extend(values);
    argv
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
    // That holds for the quoted placements too, which are unsupported.
    #[test_case("./rec %s" ; "unquoted")]
    #[test_case("./rec \"%s\"" ; "inside double quotes")]
    #[test_case("./rec '%s'" ; "inside single quotes")]
    #[test_case("./rec \"--file=%s\"" ; "embedded in a double quoted word")]
    #[test_case("./rec '--file=%s'" ; "embedded in a single quoted word")]
    #[test_case("./rec \"$(./rec %s)\"" ; "in a command substitution")]
    #[test_case("./rec `./rec %s`" ; "in backticks")]
    #[test_case("cat <<EOF\n%s\nEOF" ; "in a here document")]
    #[test_case("true # it's\n./rec %s" ; "after a comment holding a quote")]
    fn a_hostile_name_is_never_run(template: &str) {
        // Written into most of these scripts, `HOSTILE` leaves a quote open
        // and is a syntax error that runs nothing, so balanced names are tried
        // too: one that runs outside single quotes, one that runs inside them.
        for name in [HOSTILE, "$(touch pwned)", "'$(touch pwned)'"] {
            for parameters in [Parameters::One, Parameters::All] {
                let (_, ran) = run(template, parameters, &[OsStr::new(name)]);
                assert!(!ran, "{name:?}");
            }
        }
    }

    #[test_case("./rec %s", "" ; "on its own")]
    #[test_case("./rec --file=%s", "--file=" ; "embedded in a word")]
    fn an_unquoted_value_arrives_as_one_word(template: &str, prefix: &str) {
        let (words, _) = run(template, Parameters::One, &[OsStr::new(HOSTILE)]);
        assert_eq!(vec![format!("{prefix}{HOSTILE}").into_bytes()], words);
    }

    // The reference's own quotes close the surrounding double quotes, leaving
    // it unquoted and split on the space, and are literal inside single quotes.
    #[test_case("./rec \"%s\"", &["a", "b'\"$(touch", "pwned)`touch", "pwned`"] ; "inside double quotes")]
    #[test_case("./rec \"--file=%s\"", &["--file=a", "b'\"$(touch", "pwned)`touch", "pwned`"] ; "embedded in a double quoted word")]
    #[test_case("./rec '%s'", &["\"$1\""] ; "inside single quotes")]
    #[test_case("./rec '--file=%s'", &["--file=\"$1\""] ; "embedded in a single quoted word")]
    fn a_quoted_value_is_split_or_left_literal(template: &str, expected: &[&str]) {
        let (words, _) = run(template, Parameters::One, &[OsStr::new(HOSTILE)]);
        let expected: Vec<Vec<u8>> = expected
            .iter()
            .map(|word| word.as_bytes().to_vec())
            .collect();
        assert_eq!(expected, words);
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

    #[test]
    fn every_placeholder_is_replaced() {
        let argv = command("diff %s %s.orig", Parameters::One, []);
        assert_eq!("diff \"$1\" \"$1\".orig", argv[2]);
    }
}
