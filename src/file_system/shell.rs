//! Running a configured shell template on a path.
//!
//! The path is never written into the script: `%s` becomes `"$@"` and the path
//! is passed as an argument, so the shell expands it and never parses it. Only a
//! template that hands the text to another parser can run it (`eval`, a nested
//! `sh -c`, `ssh`, bash arithmetic such as `$(( %s ))`).
//!
//! `%s` must be unquoted, as its own word: inside double quotes the value is
//! split into words, and inside single quotes it stays the literal `"$@"`.
//! Values are passed as raw bytes, so a path that is not UTF-8 arrives intact.

use std::ffi::OsString;

/// The argv that runs `template` with `sh -c`, each `%s` replaced by `"$@"`.
pub(crate) fn command(template: &str, values: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut argv = vec![
        OsString::from("sh"),
        OsString::from("-c"),
        OsString::from(template.replace("%s", "\"$@\"")),
        // `$0`, which the shell names itself by in its messages.
        OsString::from("sh"),
    ];
    argv.extend(values);
    argv
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt, process::Command};

    use test_case::test_case;

    use super::*;

    /// A name that runs `touch pwned` if a shell parses it, closing either quote first.
    const HOSTILE: &str = "a b'\"$(touch pwned)`touch pwned`";

    /// Runs `template` on `values` in a scratch directory holding `rec`, which
    /// prints its arguments NUL-terminated. Returns the words and whether `touch` ran.
    fn run(template: &str, values: &[&OsStr]) -> (Vec<Vec<u8>>, bool) {
        let dir = crate::test_support::TempDir::new("shell_command");
        crate::test_support::write_executable(
            &dir.join("rec"),
            "#!/bin/sh\nprintf '%s\\0' \"$@\"\n",
        );
        let argv = command(template, values.iter().map(|value| value.to_os_string()));
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

    // The quoted placements are unsupported, but still run nothing.
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
        // `HOSTILE` leaves a quote open in most of these, a syntax error that runs
        // nothing, so balanced names are tried too.
        for name in [HOSTILE, "$(touch pwned)", "'$(touch pwned)'"] {
            let (_, ran) = run(template, &[OsStr::new(name)]);
            assert!(!ran, "{name:?}");
        }
    }

    #[test_case("./rec %s", "" ; "on its own")]
    #[test_case("./rec --file=%s", "--file=" ; "embedded in a word")]
    fn an_unquoted_value_arrives_as_one_word(template: &str, prefix: &str) {
        let (words, _) = run(template, &[OsStr::new(HOSTILE)]);
        assert_eq!(vec![format!("{prefix}{HOSTILE}").into_bytes()], words);
    }

    #[test_case("./rec \"%s\"", &["a", "b'\"$(touch", "pwned)`touch", "pwned`"] ; "inside double quotes")]
    #[test_case("./rec \"--file=%s\"", &["--file=a", "b'\"$(touch", "pwned)`touch", "pwned`"] ; "embedded in a double quoted word")]
    #[test_case("./rec '%s'", &["\"$@\""] ; "inside single quotes")]
    #[test_case("./rec '--file=%s'", &["--file=\"$@\""] ; "embedded in a single quoted word")]
    fn a_quoted_value_is_split_or_left_literal(template: &str, expected: &[&str]) {
        let (words, _) = run(template, &[OsStr::new(HOSTILE)]);
        let expected: Vec<Vec<u8>> = expected
            .iter()
            .map(|word| word.as_bytes().to_vec())
            .collect();
        assert_eq!(expected, words);
    }

    #[test]
    fn every_value_arrives_as_its_own_word() {
        let values = [OsStr::new("vim"), OsStr::new(HOSTILE)];
        let (words, ran) = run("./rec %s", &values);
        assert_eq!(vec![b"vim".to_vec(), HOSTILE.as_bytes().to_vec()], words);
        assert!(!ran);
    }

    #[test]
    fn a_value_that_is_not_utf8_arrives_intact() {
        let name = OsStr::from_bytes(b"caf\xe9.txt");
        let (words, _) = run("./rec %s", &[name]);
        assert_eq!(vec![name.as_bytes().to_vec()], words);
    }

    #[test]
    fn the_script_is_passed_before_the_values() {
        let argv = command("xdg-open %s", [OsString::from("/a b")]);
        let expected: Vec<OsString> = ["sh", "-c", "xdg-open \"$@\"", "sh", "/a b"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(expected, argv);
    }

    #[test]
    fn every_placeholder_is_replaced() {
        let argv = command("diff %s %s.orig", []);
        assert_eq!("diff \"$@\" \"$@\".orig", argv[2]);
    }
}
