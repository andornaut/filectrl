//! Building shell command lines that carry a path unchanged.
//!
//! Everything here works on raw bytes rather than `str`. A path need not be
//! valid UTF-8, and `to_string_lossy` replaces the offending bytes with U+FFFD,
//! which hands the program a path that does not exist. The rest of the file
//! system code already takes care to pass names around as `OsStr` for the same
//! reason.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// Bytes that need no quoting to survive word splitting and expansion.
fn is_safe(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'%' | b'+' | b',' | b'-' | b'.' | b'/' | b':' | b'=' | b'@' | b'_'
        )
}

/// Quotes `value` so a shell reads it as exactly one word, leaving it alone
/// when it needs no quoting so that a logged command line stays readable.
pub(crate) fn quote(value: &OsStr) -> OsString {
    let bytes = value.as_bytes();
    if !bytes.is_empty() && bytes.iter().copied().all(is_safe) {
        return value.to_os_string();
    }
    let mut quoted = Vec::with_capacity(bytes.len() + 2);
    quoted.push(b'\'');
    for &byte in bytes {
        // A single quote cannot appear inside single quotes, so close the
        // quoting, emit an escaped quote, and reopen it.
        if byte == b'\'' {
            quoted.extend_from_slice(b"'\\''");
        } else {
            quoted.push(byte);
        }
    }
    quoted.push(b'\'');
    OsString::from_vec(quoted)
}

/// Substitutes every `%s` in a shell template with `replacement`, which the
/// caller has already quoted if it needs to be.
pub(crate) fn template(template: &str, replacement: &OsStr) -> OsString {
    let mut expanded = OsString::with_capacity(template.len() + replacement.len());
    let mut rest = template;
    while let Some(index) = rest.find("%s") {
        expanded.push(&rest[..index]);
        expanded.push(replacement);
        rest = &rest[index + 2..];
    }
    expanded.push(rest);
    expanded
}

/// Joins `argv` into one shell command line, quoting each word that needs it.
///
/// Only the Linux terminal wrapper needs this: macOS launches through `open`
/// with an argv and never builds a command line.
#[cfg(target_os = "linux")]
pub(crate) fn join(argv: &[OsString]) -> OsString {
    let mut joined = OsString::new();
    for (index, word) in argv.iter().enumerate() {
        if index > 0 {
            joined.push(" ");
        }
        joined.push(quote(word));
    }
    joined
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use test_case::test_case;

    use super::*;

    /// An `OsStr` holding a byte sequence that is not valid UTF-8, which is
    /// what a lossy conversion would destroy.
    fn invalid_utf8() -> &'static OsStr {
        OsStr::from_bytes(b"caf\xe9.txt")
    }

    #[test_case("plain" => "plain" ; "a safe word is left unquoted")]
    #[test_case("/a/b.txt" => "/a/b.txt" ; "a plain path needs no quoting")]
    #[test_case("/a b.txt" => "'/a b.txt'" ; "a space forces quoting")]
    #[test_case("" => "''" ; "an empty value is quoted")]
    #[test_case("a$b" => "'a$b'" ; "an expansion character is quoted")]
    #[test_case("it's" => "'it'\\''s'" ; "an embedded single quote is escaped")]
    #[test_case("a;rm -rf /" => "'a;rm -rf /'" ; "a command separator is quoted")]
    fn quote_produces(value: &str) -> String {
        quote(OsStr::new(value)).to_string_lossy().into_owned()
    }

    #[test]
    fn quote_preserves_bytes_that_are_not_utf8() {
        let quoted = quote(invalid_utf8());
        // Quoted because 0xe9 is not in the safe set, and the byte itself has
        // to survive: this is the whole reason the module works on bytes.
        assert_eq!(b"'caf\xe9.txt'".as_slice(), quoted.as_bytes());
    }

    #[test_case("cp %s /dest", "/a b" => "cp '/a b' /dest" ; "one placeholder")]
    #[test_case("%s and %s", "x" => "x and x" ; "every placeholder is substituted")]
    #[test_case("no placeholder", "x" => "no placeholder" ; "no placeholder is a no-op")]
    #[test_case("", "x" => "" ; "an empty template stays empty")]
    fn template_produces(text: &str, replacement: &str) -> String {
        template(text, &quote(OsStr::new(replacement)))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn template_preserves_bytes_that_are_not_utf8() {
        let expanded = template("cp %s /dest", &quote(invalid_utf8()));
        assert_eq!(b"cp 'caf\xe9.txt' /dest".as_slice(), expanded.as_bytes());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn join_quotes_only_the_words_that_need_it() {
        let argv = [
            OsString::from("vim"),
            OsString::from("/a b.txt"),
            OsString::from("--flag=1"),
        ];
        assert_eq!("vim '/a b.txt' --flag=1", join(&argv).to_string_lossy());
    }
}
